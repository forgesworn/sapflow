import Foundation
import Network
import Security
import Sapflow

/// One explicit manual publishing attempt. Rust owns RTMP/FLV; Network.framework
/// owns TLS, certificate/hostname validation and asynchronous TCP operations.
/// No network callback is executed on capture, codec, recording or UI queues.
final class ManualRtmpPublisher: EncodedPublishingSink {
    private let observe: (PublishingAttemptEvent, UInt64) -> Void
    private let destination: ManualDestination
    private let queue = DispatchQueue(label: "sapflow.manual-rtmp", qos: .userInitiated)
    private let lock = NSLock()
    // lock owns only admission/generation/status, never transport or FFI calls.
    private var generation: UInt64 = 0
    private var accepting = false
    private var released = true
    private var currentStatus = "Manual destination ready · single attempt"
    private var admission = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
    // Everything below is owned exclusively by queue.
    private var connection: NWConnection?
    private var publisher: NativeRtmpPublisher?
    private lazy var timer = NativeDeadlineTimer(queue: queue)
    private var connectDeadline: UInt64?
    private var writeDeadline: UInt64?
    private var stopDeadline: UInt64?
    private var writeInFlight = false
    private var finalizingTransport = false
    private var stopping = false
    private var cancelling = false
    private var acceptedNotified = false
    private let progressLatch = PublishingProgressLatch()
    private var protocolDeadline: UInt64?
    private var stopCompletions: [() -> Void] = []
    private var waitingForKeyframe = true
    private var completedBytes: UInt64 = 0
    private var inflightBytes = 0
    private var bitrateWindowMs: UInt64 = 0
    private var bitrateWindowBytes: UInt64 = 0
    private var kilobitsPerSecond: UInt64 = 0

    init(destination: ManualDestination, observe: @escaping (PublishingAttemptEvent, UInt64) -> Void = { _, _ in }) { self.destination = destination; self.observe = observe }
    deinit { timer.cancel(); connection?.cancel(); try? publisher?.cancel() }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return released }
    var status: String { lock.lock(); defer { lock.unlock() }; return currentStatus }

    func start() throws {
        lock.lock()
        guard released, !accepting else { lock.unlock(); throw ReconnectFailure.cleanupRetained }
        guard generation < UInt64.max else { lock.unlock(); throw CaptureFailure(message: "Publisher generation exhausted") }
        generation += 1
        let token = generation
        admission.close()
        admission = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
        accepting = true; released = false
        currentStatus = destination.encrypted ? "Connecting with TLS · single attempt" : "Connecting with explicit plaintext RTMP · single attempt"
        lock.unlock()
        queue.async {
            guard self.isCurrent(token), self.isAccepting() else { return }
            self.stopping = false; self.waitingForKeyframe = true
            self.completedBytes = 0; self.bitrateWindowBytes = 0; self.kilobitsPerSecond = 0; self.bitrateWindowMs = self.nowMs()
            self.connectDeadline = self.nowMs() + 15_000
            let parameters: NWParameters
            if self.destination.encrypted {
                let tls = NWProtocolTLS.Options()
                sec_protocol_options_set_tls_server_name(tls.securityProtocolOptions, self.destination.host)
                sec_protocol_options_set_peer_authentication_required(tls.securityProtocolOptions, true)
                sec_protocol_options_set_min_tls_protocol_version(tls.securityProtocolOptions, .TLSv12)
                // No verify block or trust override: system trust and hostname
                // validation remain enabled for this TLS client.
                parameters = NWParameters(tls: tls, tcp: NWProtocolTCP.Options())
            } else { parameters = .tcp }
            guard let port = NWEndpoint.Port(rawValue: self.destination.port) else {
                self.fail(token, "Invalid destination port"); return
            }
            let connection = NWConnection(host: NWEndpoint.Host(self.destination.host), port: port, using: parameters)
            self.connection = connection
            connection.stateUpdateHandler = { [weak self] state in
                guard let self, self.isCurrent(token) else { return }
                switch state {
                case .ready:
                    guard !self.cancelling else { return }
                    guard !self.stopping else { self.finishTransport(token); return }
                    self.connectDeadline = nil
                    if self.publisher != nil {
                        do { try self.updateProtocolStatus(token); try self.pumpOutput(token) }
                        catch { self.failProtocol(token, error) }
                        return
                    }
                    do {
                        self.publisher = try NativeRtmpPublisher(app: self.destination.app, streamKey: self.destination.streamKey,
                            tcUrl: self.destination.endpoint, mediaEpochNs: 0, nowMs: self.nowMs())
                        try self.publisher?.declareTracks(hasVideo: true, hasAudio: true)
                        try self.updateProtocolStatus(token)
                        try self.pumpOutput(token)
                        self.receive(token)
                        self.armDeadline(token)
                    } catch { self.failProtocol(token, error) }
                case let .failed(error): self.failNetwork(token, error)
                case .waiting:
                    self.setStatus(token, "Network path unavailable · local recording continues")
                case .cancelled: self.completeRelease()
                default: break
                }
            }
            connection.start(queue: self.queue)
            self.armDeadline(token)
        }
    }

    func offer(_ sample: MediaSample) throws {
        lock.lock()
        let active = accepting; let token = generation; let budget = admission
        lock.unlock()
        guard active else { return } // Recording continues after publisher failure.
        let length = sample.len()
        guard length <= UInt64(Int.max), let reservation = budget.reserve(bytes: Int(length)) else {
            if budget.close() { queue.async { self.fail(token, "Publishing media backlog exceeded 4 MiB / 128 samples / 1 s; local recording continues") } }
            return
        }
        let admittedAt = nowMs()
        queue.async {
            defer { budget.release(reservation) }
            guard self.isCurrent(token), !self.stopping, !self.cancelling else { return }
            guard self.nowMs() - admittedAt <= 1_000 else { self.fail(token, "Publishing media became stale; local recording continues"); return }
            guard let publisher = self.publisher else { return } // Pre-handshake media is never buffered indefinitely.
            do {
                guard self.acceptedNotified else { return }
                if self.waitingForKeyframe {
                    guard sample.isVideo(), sample.isKeyframe() else { return }
                    self.waitingForKeyframe = false
                }
                if self.writeInFlight {
                    try publisher.tick(nowMs: self.nowMs())
                    try publisher.send(sample: sample)
                    self.protocolDeadline = try publisher.nextDeadlineMs()
                } else {
                    let result = try publisher.sendAndTakeOutput(sample: sample, nowMs: self.nowMs())
                    self.protocolDeadline = result.nextDeadlineMs
                    self.progress(result.startupSubmitted, result.peerMediaProgress)
                    try self.pumpOutput(token, provided: result.output)
                }
                self.armDeadline(token)
            } catch { self.failProtocol(token, error) }
        }
    }

    /// Explicit user stop only. Best-effort closeStream drains with a fixed
    /// deadline; this does not assert that destination end was acknowledged.
    func stop() { requestStop(nil) }
    func stop(completion: @escaping () -> Void) { requestStop(completion) }
    private func requestStop(_ completion: (() -> Void)?) {
        lock.lock(); let token = generation; accepting = false; admission.close(); lock.unlock()
        queue.async {
            guard self.isCurrent(token) else { completion?(); return }
            if let completion, self.stopCompletions.count < 2 { self.stopCompletions.append(completion) }
            if self.cancelling { return }
            if self.stopping && self.connection != nil { return }
            self.stopping = true
            self.setStatus(token, "Stopping publisher · destination end unconfirmed")
            self.connectDeadline = nil
            self.stopDeadline = self.nowMs() + 3_000
            guard let publisher = self.publisher else { self.finishTransport(token); return }
            do { try publisher.finish(); try self.pumpOutput(token); self.armDeadline(token) }
            catch { self.finishTransport(token) }
        }
    }

    private func receive(_ token: UInt64) {
        guard let connection, isCurrent(token) else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, complete, error in
            guard let self, self.isCurrent(token), self.connection != nil, !self.cancelling else { return }
            do {
                if let data, !data.isEmpty, let publisher = self.publisher {
                    try publisher.feed(bytes: data, nowMs: self.nowMs())
                    try self.updateProtocolStatus(token)
                    try self.pollProgress()
                    try self.pumpOutput(token)
                }
                if let error { self.failNetwork(token, error); return }
                if complete {
                    if self.stopping { self.finishTransport(token) }
                    else { self.fail(token, "Destination closed the uplink · local recording continues") }
                    return
                }
                self.receive(token)
                self.armDeadline(token)
            } catch { self.failProtocol(token, error) }
        }
    }

    private func pumpOutput(_ token: UInt64, provided: Data? = nil) throws {
        defer { armDeadline(token) }
        guard isCurrent(token), !cancelling, let connection, let publisher else { return }
        // Legacy send while a write is outstanding can create Sending without
        // inbound ACK traffic. Drain from existing cold output/completion work,
        // including an outstanding write, rather than adding sample hot-path calls.
        if provided == nil { try pollProgress() }
        guard !writeInFlight, !finalizingTransport else { return }
        let data: Data
        if let provided { data = provided }
        else { data = try publisher.takeOutput(); protocolDeadline = try publisher.nextDeadlineMs() }
        guard data.count <= 8 * 1024 * 1024 else { fail(token, "RTMP output exceeded its native bound"); return }
        guard !data.isEmpty else {
            if stopping { finishTransport(token, graceful: true) }
            return
        }
        writeInFlight = true; inflightBytes = data.count
        writeDeadline = nowMs() + 5_000
        connection.send(content: data, completion: .contentProcessed { [weak self] error in
            guard let self, self.isCurrent(token), self.connection != nil, !self.cancelling else { return }
            let bytes = self.inflightBytes
            self.writeInFlight = false; self.inflightBytes = 0; self.writeDeadline = nil
            if let error { self.failNetwork(token, error); return }
            let sum = self.completedBytes.addingReportingOverflow(UInt64(bytes))
            self.completedBytes = sum.overflow ? UInt64.max : sum.partialValue
            self.bitrateWindowBytes += UInt64(bytes)
            let elapsed = self.nowMs() - self.bitrateWindowMs
            if elapsed >= 1_000 {
                self.kilobitsPerSecond = self.bitrateWindowBytes * 8 / elapsed
                self.bitrateWindowBytes = 0; self.bitrateWindowMs = self.nowMs()
            }
            do {
                try self.pumpOutput(token)
                try self.updateProtocolStatus(token)
                self.armDeadline(token)
            } catch { self.failProtocol(token, error) }
        })
        armDeadline(token)
    }

    private func updateProtocolStatus(_ token: UInt64) throws {
        guard let publisher else { return }
        switch try publisher.state() {
        case .publishing:
            if !acceptedNotified { acceptedNotified = true; observe(.accepted, DispatchTime.now().uptimeNanoseconds) }
            if waitingForKeyframe {
                setStatus(token, "Destination accepted · waiting for video keyframe · playback unconfirmed")
                return
            }
            let transport = destination.encrypted ? "RTMPS" : "plaintext RTMP"
            setStatus(token, "Sending \(transport) · \(kilobitsPerSecond) kbit/s accepted by socket · playback unconfirmed")
        case .handshaking: setStatus(token, "RTMP handshake · local recording active")
        case .connecting, .creatingStream, .awaitingPublish: setStatus(token, "Awaiting destination publish acceptance · local recording active")
        case .finishing: break
        case .closed: if stopping { finishTransport(token) } else { fail(token, "Destination ended publication · local recording continues") }
        case .failed: fail(token, "RTMP publishing failed · local recording continues")
        }
    }
    private func armDeadline(_ token: UInt64) {
        guard isCurrent(token), connection != nil else { return }
        guard !cancelling else { timer.cancel(); return }
        let deadline = [connectDeadline, protocolDeadline, writeDeadline, stopDeadline].compactMap({ $0 }).min()
        timer.schedule(atNs: deadline.map { $0 * 1_000_000 }, generation: token) { [weak self] in self?.deadlineFired($0) }
    }

    private func deadlineFired(_ token: UInt64) {
        guard isCurrent(token), connection != nil else { return }
        let now = nowMs()
        if stopDeadline.map({ now >= $0 }) == true { finishTransport(token); return }
        if connectDeadline.map({ now >= $0 }) == true { fail(token, "Connection timed out · local recording continues"); return }
        if writeDeadline.map({ now >= $0 }) == true { fail(token, "Uplink write timed out · local recording continues"); return }
        do {
            try publisher?.tick(nowMs: now)
            protocolDeadline = try publisher?.nextDeadlineMs()
            try updateProtocolStatus(token)
            try pumpOutput(token)
            armDeadline(token)
        } catch { failProtocol(token, error) }
    }
    private func failProtocol(_ token: UInt64, _ error: Error) {
        // Never include a destination-supplied error string or publish key.
        if let error = error as? NativeRtmpError {
            switch error {
            case .UnsupportedHandshake: fail(token, "Destination requires an unsupported RTMP handshake · local recording continues")
            case .Backpressure: fail(token, "Uplink cannot keep up · local recording continues")
            case .Timeout: fail(token, "RTMP protocol or queued-media deadline elapsed · local recording continues")
            case .Refused: fail(token, "Destination refused publishing · local recording continues")
            default: fail(token, "RTMP protocol failed · local recording continues")
            }
        } else { fail(token, "RTMP adapter failed · local recording continues") }
    }
    private func failNetwork(_ token: UInt64, _ error: NWError) {
        if stopping { finishTransport(token); return }
        switch error {
        case .tls: fail(token, "TLS certificate or handshake failed · local recording continues")
        default: fail(token, "Uplink disconnected · local recording continues")
        }
    }
    private func fail(_ token: UInt64, _ message: String) {
        lock.lock()
        guard generation == token, accepting else { lock.unlock(); return }
        accepting = false; admission.close(); currentStatus = message
        lock.unlock()
        observe(.failed, DispatchTime.now().uptimeNanoseconds)
        releaseTransport()
    }
    private func finishTransport(_ token: UInt64, graceful: Bool = false) {
        guard isCurrent(token) else { return }
        if graceful, let connection {
            guard !finalizingTransport else { return }
            finalizingTransport = true
            // Send TCP/TLS end-of-stream after closeStream bytes have completed.
            // The existing fixed stop deadline still bounds this final write.
            connection.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { [weak self] _ in
                guard let self, self.isCurrent(token), self.connection != nil, !self.cancelling else { return }
                self.releaseTransport()
                self.setStatus(token, "Publisher stopped · destination end unconfirmed")
            })
            armDeadline(token)
            return
        }
        releaseTransport()
        setStatus(token, "Publisher stopped · destination end unconfirmed")
    }
    private func progress(_ sending: Bool, _ peer: Bool) {
        progressLatch.observe(sending: sending, peer: peer) { event in
            observe(event, DispatchTime.now().uptimeNanoseconds)
        }
    }
    private func pollProgress() throws {
        try progressLatch.drain(next: {
            guard let event = try publisher?.pollEvent() else { return nil }
            switch event {
            case .sending: return .sending
            case .peerMediaProgress: return .peerMediaProgress
            }
        }, notify: { event in observe(event, DispatchTime.now().uptimeNanoseconds) })
    }
    private func releaseTransport() {
        timer.cancel()
        try? publisher?.cancel(); publisher = nil
        connectDeadline = nil; protocolDeadline = nil; writeDeadline = nil; stopDeadline = nil
        writeInFlight = false; finalizingTransport = false; inflightBytes = 0
        guard let connection else { completeRelease(); return }
        guard !cancelling else { return }
        cancelling = true
        connection.cancel() // success is reported only from state .cancelled
    }
    private func completeRelease() {
        connection?.stateUpdateHandler = nil; connection = nil; cancelling = false
        lock.lock(); released = true; accepting = false; lock.unlock()
        let completions = stopCompletions; stopCompletions.removeAll()
        for completion in completions { completion() }
    }
    private func setStatus(_ token: UInt64, _ value: String) {
        lock.lock(); defer { lock.unlock() }
        if generation == token { currentStatus = value }
    }
    private func isCurrent(_ token: UInt64) -> Bool { lock.lock(); defer { lock.unlock() }; return token == generation }
    private func isAccepting() -> Bool { lock.lock(); defer { lock.unlock() }; return accepting }
    private func nowMs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }
}
