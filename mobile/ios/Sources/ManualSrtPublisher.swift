import Foundation
import Network
import Sapflow

/// Network.framework owns one connected UDP socket. Rust owns MPEG-TS, SRT,
/// encryption, pacing, ACK/retry windows and deadlines. The capture profile is
/// currently fixed A/V; its immutable codec handles are configured once.
final class ManualSrtPublisher: EncodedPublishingSink {
    private struct Pending {
        let sample: MediaSample
        let budget: EncodedWorkBudget
        let reservation: EncodedWorkBudget.Reservation
        let admittedUs: UInt64
    }
    private let observe: (PublishingAttemptEvent, UInt64) -> Void
    private let destination: ManualSrtDestination
    private let queue = DispatchQueue(label: "sapflow.manual-srt", qos: .userInitiated)
    private let lock = NSLock()
    private var generation: UInt64 = 0
    private var accepting = false
    private var released = true
    private var currentStatus = "SRT destination ready · single attempt"
    private var admission = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
    // Serial network queue owns all fields below and all FFI calls.
    private lazy var timer = NativeDeadlineTimer(queue: queue)
    private var connection: NWConnection?
    private var publisher: NativeSrtPublisher?
    private var video: MediaCodec?
    private var audio: MediaCodec?
    private var socketReady = false
    private var connected = false
    private var waitingForKeyframe = true
    private var stopping = false
    private var cancelling = false
    private var stopCompletions: [() -> Void] = []
    private var writing = false
    private var continuationQueued = false
    private var pending: [Pending] = []
    private var connectDeadlineUs: UInt64?
    private var writeDeadlineUs: UInt64?
    private var stopDeadlineUs: UInt64?
    private var statusAtUs: UInt64 = 0

    init(destination: ManualSrtDestination, observe: @escaping (PublishingAttemptEvent, UInt64) -> Void = { _, _ in }) { self.destination = destination; self.observe = observe }
    deinit { timer.cancel(); connection?.cancel(); try? publisher?.stop(nowUs: Self.nowUs()) }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return released }
    var status: String { lock.lock(); defer { lock.unlock() }; return currentStatus }

    func start() throws {
        lock.lock()
        guard released, !accepting else { lock.unlock(); throw ReconnectFailure.cleanupRetained }
        guard generation < UInt64.max else { lock.unlock(); throw CaptureFailure(message: "Publisher generation exhausted") }
        generation += 1
        let token = generation
        admission.close(); admission = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
        accepting = true; released = false; currentStatus = "Preparing SRT UDP caller · single attempt"
        lock.unlock()
        queue.async {
            guard self.isCurrent(token), self.isAccepting() else { return }
            self.stopping = false; self.waitingForKeyframe = true
            self.connectDeadlineUs = Self.nowUs() + 15_000_000
            self.statusAtUs = 0
            guard let port = NWEndpoint.Port(rawValue: self.destination.port) else {
                self.fail(token, "Invalid SRT destination port"); return
            }
            let connection = NWConnection(host: NWEndpoint.Host(self.destination.host), port: port, using: .udp)
            self.connection = connection
            connection.stateUpdateHandler = { [weak self] state in
                guard let self, self.isCurrent(token), self.connection != nil else { return }
                // Cancellation completion releases this exact owner even after
                // ordinary readiness/failure callbacks have been fenced.
                if case .cancelled = state { self.completeRelease(); return }
                guard !self.cancelling else { return }
                switch state {
                case .ready:
                    guard !self.cancelling else { return }
                    guard !self.stopping else { self.finishStop(token); return }
                    if self.socketReady {
                        do { try self.drive(token) } catch { self.failProtocol(token, error) }
                        return
                    }
                    self.socketReady = true
                    do { try self.createPublisher(token); self.receive(token); try self.drive(token) }
                    catch { self.failProtocol(token, error) }
                case .waiting: self.setStatus(token, "SRT network path unavailable · local recording continues")
                case .failed: self.fail(token, "SRT UDP path failed · local recording continues")
                case .cancelled: self.completeRelease()
                default: break
                }
            }
            connection.start(queue: self.queue)
            self.armDeadline(token)
        }
    }
    func configure(video: MediaCodec, audio: MediaCodec) throws {
        lock.lock(); let token = generation; let active = accepting; lock.unlock()
        guard active else { return }
        queue.async {
            guard self.isCurrent(token), self.isAccepting(), !self.stopping else { return }
            guard self.video == nil, self.audio == nil else {
                self.fail(token, "SRT configuration changed; prepare a new session · local recording continues"); return
            }
            self.video = video; self.audio = audio
            do { try self.createPublisher(token); try self.drive(token) }
            catch { self.failProtocol(token, error) }
        }
    }
    private func createPublisher(_ token: UInt64) throws {
        guard publisher == nil, socketReady else { return }
        guard let video, let audio else {
            setStatus(token, "Waiting for encoded video/audio configuration · local recording active"); return
        }
        let config = NativeSrtConfig(destination: destination.socketAddress, streamId: destination.streamID,
            cipher: destination.encrypted ? .aes256 : nil, latencyMs: 120,
            maxBufferedPackets: 128, maxBufferedBytes: 4 * 1024 * 1024,
            maxPacketAgeMs: 1000, maxSampleResidenceMs: 1000, bandwidthBytesPerSecond: 1_250_000)
        // Foreign String/Data copies cannot promise zeroisation. The Rust key
        // owner zeroises its transferred secret; no credential is logged here.
        publisher = try NativeSrtPublisher(config: config, video: video, audio: audio,
            passphrase: destination.passphrase.map { Data($0.utf8) }, nowUs: Self.nowUs())
        connectDeadlineUs = nil
        setStatus(token, "SRT handshake · playback unconfirmed")
    }
    func offer(_ sample: MediaSample) throws {
        lock.lock(); let active = accepting; let token = generation; let budget = admission; lock.unlock()
        guard active else { return }
        let bytes = sample.len()
        guard bytes <= UInt64(Int.max), let reservation = budget.reserve(bytes: Int(bytes)) else {
            if budget.close() { queue.async { self.fail(token, "SRT ingress exceeded 4 MiB / 128 samples / 1 s · local recording continues") } }
            return
        }
        let admitted = Self.nowUs()
        queue.async {
            guard self.isCurrent(token), !self.stopping, self.connection != nil else { budget.release(reservation); return }
            guard self.connected else { budget.release(reservation); return } // No pre-handshake media backlog.
            self.pending.append(Pending(sample: sample, budget: budget, reservation: reservation, admittedUs: admitted))
            do { try self.drive(token) } catch { self.failProtocol(token, error) }
        }
    }
    private func receive(_ token: UInt64) {
        guard let connection, isCurrent(token) else { return }
        connection.receiveMessage { [weak self] data, _, _, error in
            guard let self, self.isCurrent(token), self.connection != nil, !self.stopping, !self.cancelling else { return }
            if error != nil { self.fail(token, "SRT UDP receive failed · local recording continues"); return }
            do {
                if let data, data.count <= 1500, let publisher = self.publisher {
                    // Connected UDP restricts the peer; the numeric identity is
                    // fixed at construction, with no unauthorised peer switch.
                    try publisher.feed(source: self.destination.socketAddress, bytes: data, nowUs: Self.nowUs())
                }
                // Oversized native datagrams are discarded before Rust ingress;
                // deadlines remain driven even under malformed input.
                try self.drive(token)
                self.receive(token)
            } catch { self.failProtocol(token, error) }
        }
    }
    private func drive(_ token: UInt64) throws {
        guard isCurrent(token), connection != nil, !stopping, !cancelling else { return }
        defer { armDeadline(token) }
        guard let publisher else { return }
        let now = Self.nowUs()
        try publisher.tick(nowUs: now)
        guard try observeEvents(token) else { return }
        if let first = pending.first, now - first.admittedUs >= 1_000_000 {
            fail(token, "SRT ingress media expired · local recording continues"); return
        }
        guard !writing else { return }
        // One bounded work batch, then yield to socket/timer/cancellation work.
        for _ in 0..<16 {
            if connected, !pending.isEmpty, try publisher.isReady() {
                let first = pending.removeFirst()
                defer { first.budget.release(first.reservation) }
                if waitingForKeyframe {
                    guard first.sample.isVideo(), first.sample.isKeyframe() else { continue }
                    waitingForKeyframe = false
                }
                try publisher.push(sample: first.sample, nowUs: Self.nowUs())
            }
            if let datagram = try publisher.pollDatagram(nowUs: Self.nowUs()) {
                guard !datagram.bytes.isEmpty, datagram.bytes.count <= 1500 else {
                    fail(token, "SRT returned an invalid UDP packet · local recording continues"); return
                }
                send(datagram.bytes, token)
                return
            }
            guard try observeEvents(token) else { return }
            let immediate = try publisher.hasImmediateWork()
            if !immediate {
                if pending.isEmpty { break }
                if !(try publisher.isReady()) { break }
            }
        }
        try updateStatus(token)
        let immediate = try publisher.hasImmediateWork()
        var canAccept = false
        if connected && !pending.isEmpty { canAccept = try publisher.isReady() }
        if immediate || canAccept { queueContinuation(token) }
    }
    private func queueContinuation(_ token: UInt64) {
        guard !continuationQueued else { return }
        continuationQueued = true
        queue.async {
            guard self.isCurrent(token) else { return }
            self.continuationQueued = false
            guard !self.stopping else { return }
            do { try self.drive(token) } catch { self.failProtocol(token, error) }
        }
    }
    private func send(_ bytes: Data, _ token: UInt64) {
        guard let connection else { return }
        writing = true; writeDeadlineUs = Self.nowUs() + 100_000
        connection.send(content: bytes, completion: .contentProcessed { [weak self] error in
            guard let self, self.isCurrent(token), self.connection != nil, !self.cancelling else { return }
            self.writing = false; self.writeDeadlineUs = nil
            if error != nil {
                try? self.publisher?.socketSendFailed(nowUs: Self.nowUs())
                self.fail(token, "SRT UDP submission failed · local recording continues"); return
            }
            if self.stopping { self.finishStop(token); return }
            do { try self.drive(token) } catch { self.failProtocol(token, error) }
        })
    }
    private func observeEvents(_ token: UInt64) throws -> Bool {
        guard let publisher else { return false }
        for _ in 0..<8 {
            guard let event = try publisher.pollEvent() else { break }
            switch event {
            case .connected:
                connected = true
                observe(.accepted, DispatchTime.now().uptimeNanoseconds)
                setStatus(token, "SRT connected · waiting for video keyframe · playback unconfirmed")
            case .sending: observe(.sending, DispatchTime.now().uptimeNanoseconds)
            case .peerMediaProgress: observe(.peerMediaProgress, DispatchTime.now().uptimeNanoseconds)
            case .failed, .closed:
                fail(token, "SRT attempt ended · local recording continues; prepare again to retry")
                return false
            }
        }
        return true
    }
    private func updateStatus(_ token: UInt64) throws {
        guard connected, !waitingForKeyframe, let publisher else { return }
        let now = Self.nowUs()
        guard statusAtUs == 0 || now - statusAtUs >= 1_000_000 else { return }
        statusAtUs = now
        let stats = try publisher.stats()
        let protection = destination.encrypted ? "encrypted SRT" : "explicit plaintext SRT"
        setStatus(token, "Sending \(protection) · peer ACK \(stats.acknowledgedBytes) bytes · playback unconfirmed")
    }
    private func armDeadline(_ token: UInt64) {
        guard isCurrent(token), connection != nil, !cancelling else { return }
        do {
            var native = stopping ? nil : try publisher?.nextDeadlineUs()
            // Immediate mux work waits on the ONE outstanding UDP submission.
            // Its fixed 100 ms write deadline bounds that wait without polling.
            if writing, let value = native, value <= Self.nowUs() { native = nil }
            let ingress = pending.first.map { $0.admittedUs + 1_000_000 }
            let deadline = [native, connectDeadlineUs, writeDeadlineUs, stopDeadlineUs, ingress].compactMap { $0 }.min()
            timer.schedule(atNs: deadline.map { $0 * 1000 }, generation: token) { [weak self] in self?.deadlineFired($0) }
        } catch { failProtocol(token, error) }
    }
    private func deadlineFired(_ token: UInt64) {
        guard isCurrent(token), connection != nil else { return }
        let now = Self.nowUs()
        if stopping { finishStop(token); return }
        if connectDeadlineUs.map({ now >= $0 }) == true {
            fail(token, "SRT socket/configuration startup timed out · local recording continues"); return
        }
        if writeDeadlineUs.map({ now >= $0 }) == true {
            try? publisher?.socketSendFailed(nowUs: now)
            fail(token, "SRT UDP submission timed out · local recording continues"); return
        }
        do { try drive(token) } catch { failProtocol(token, error) }
    }
    func stop() { requestStop(nil) }
    func stop(completion: @escaping () -> Void) { requestStop(completion) }
    private func requestStop(_ completion: (() -> Void)?) {
        lock.lock(); let token = generation; accepting = false; admission.close(); lock.unlock()
        queue.async {
            guard self.isCurrent(token) else { completion?(); return }
            if let completion, self.stopCompletions.count < 2 { self.stopCompletions.append(completion) }
            if self.cancelling { return }
            if self.stopping && self.connection != nil { return }
            self.stopping = true; self.clearPending()
            do {
                try self.publisher?.stop(nowUs: Self.nowUs())
                if !self.writing, let datagram = try self.publisher?.pollDatagram(nowUs: Self.nowUs()) {
                    self.stopDeadlineUs = Self.nowUs() + 100_000
                    self.send(datagram.bytes, token); self.armDeadline(token)
                    return
                }
            } catch { /* Best-effort shutdown never prevents local teardown. */ }
            self.finishStop(token)
        }
    }
    private func finishStop(_ token: UInt64) {
        guard isCurrent(token) else { return }
        releaseTransport(); setStatus(token, "SRT publisher stopped · destination end unconfirmed")
    }
    private func failProtocol(_ token: UInt64, _ error: Error) {
        if let error = error as? NativeSrtError, error == .Backpressure {
            fail(token, "SRT media backpressure · local recording continues")
        } else { fail(token, "SRT protocol/media attempt failed · local recording continues") }
    }
    private func fail(_ token: UInt64, _ message: String) {
        lock.lock()
        guard generation == token, accepting else { lock.unlock(); return }
        accepting = false; admission.close(); currentStatus = message
        lock.unlock()
        observe(.failed, DispatchTime.now().uptimeNanoseconds)
        releaseTransport()
    }
    private func clearPending() {
        for item in pending { item.budget.release(item.reservation) }
        pending.removeAll(keepingCapacity: false)
    }
    private func releaseTransport() {
        timer.cancel()
        try? publisher?.stop(nowUs: Self.nowUs()); publisher = nil
        clearPending(); video = nil; audio = nil
        socketReady = false; connected = false; writing = false; continuationQueued = false
        connectDeadlineUs = nil; writeDeadlineUs = nil; stopDeadlineUs = nil
        guard let connection else { completeRelease(); return }
        guard !cancelling else { return }
        cancelling = true
        connection.cancel()
    }
    private func completeRelease() {
        connection?.stateUpdateHandler = nil; connection = nil; cancelling = false
        lock.lock(); released = true; accepting = false; lock.unlock()
        let completions = stopCompletions; stopCompletions.removeAll()
        for completion in completions { completion() }
    }
    private func isCurrent(_ token: UInt64) -> Bool { lock.lock(); defer { lock.unlock() }; return token == generation }
    private func isAccepting() -> Bool { lock.lock(); defer { lock.unlock() }; return accepting }
    private func setStatus(_ token: UInt64, _ status: String) {
        lock.lock(); defer { lock.unlock() }
        if generation == token { currentStatus = status }
    }
    private static func nowUs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1000 }
}
