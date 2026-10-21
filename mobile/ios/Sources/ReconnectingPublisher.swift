import Foundation
import Sapflow

private final class NativeReconnectPolicy: ReconnectPolicyPort {
    private let native: NativeSession
    private let source: NativeTransportRecoverySource
    private var generation: UInt64 = 0
    private var snapshot: NativeSessionSnapshot?
    var attemptEpoch: UInt64 { snapshot?.attemptEpoch ?? 0 }
    var nextDeadlineNs: UInt64? { snapshot?.nextDeadlineNs }
    var exhausted: Bool { snapshot?.retryBudget == .exhausted }
    init(graceMs: UInt64, source: NativeTransportRecoverySource) throws {
        native = try NativeSession(graceMs: graceMs); self.source = source
    }
    private func mapped(_ action: NativeSessionAction) throws -> ReconnectAction {
        snapshot = try native.snapshot()
        switch action {
        case .none: return .none
        case .connect: return .connect
        case let .retryAt(atNs): return .retryAt(atNs)
        case let .awaitGraceDeadline(atNs): return .awaitDeadline(atNs)
        case .retryBudgetExhausted: return .exhausted
        case .graceExpired: return .expired
        case .endDestination: return .end
        }
    }
    func start(_ now: UInt64) throws -> ReconnectAction {
        generation = try native.start(nowNs: now)
        return try mapped(native.prepared(generation: generation))
    }
    func replace(_ now: UInt64) throws -> ReconnectAction { try mapped(native.replaceTransport(generation: generation, nowNs: now)) }
    func connected(_ now: UInt64) throws { try native.connected(generation: generation, nowNs: now); snapshot = try native.snapshot() }
    func sending(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { try mapped(native.sending(generation: generation, attemptEpoch: epoch, nowNs: now)) }
    func recovered(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { try mapped(native.transportRecovered(generation: generation, attemptEpoch: epoch, nowNs: now, source: source)) }
    func failed(_ now: UInt64) throws -> ReconnectAction { try mapped(native.transportFailed(generation: generation, nowNs: now)) }
    func tick(_ now: UInt64) throws -> ReconnectAction { try mapped(native.tick(generation: generation, nowNs: now)) }
    func stop() throws { if generation != 0 { generation = try native.cancel(generation: generation); snapshot = try native.snapshot() } }
}

private final class NativePublishingAttempt: ReconnectAttemptPort {
    let sink: EncodedPublishingSink
    private var codecs: (MediaCodec, MediaCodec)?
    private let admission = AttemptStartFence()
    private let deliver: (@escaping () -> Void) -> Void
    init(sink: EncodedPublishingSink, video: MediaCodec, audio: MediaCodec, deliver: @escaping (@escaping () -> Void) -> Void) {
        self.sink = sink; codecs = (video, audio); self.deliver = deliver
    }
    func start() throws {
        try admission.start {
            guard let codecs else { throw ReconnectFailure.invalidEpoch }
            try sink.start(); try sink.configure(video: codecs.0, audio: codecs.1)
        }
    }
    func fence() {
        admission.close { sink.endDestination() }
    }
    func close(completion: @escaping () -> Void) {
        fence()
        sink.stop { [self] in
            deliver { [self] in codecs = nil; completion() }
        }
    }
}

/// One user-started logical policy, reused across capture/codec epochs. The
/// serial owner does no per-sample scheduling: media takes a locked pointer to
/// the current bounded attempt and retains its existing immutable handle.
final class ReconnectingPublisher: EncodedPublishingSink {
    typealias AttemptFactory = (@escaping (PublishingAttemptEvent, UInt64) -> Void) -> EncodedPublishingSink
    private let queue = DispatchQueue(label: "sapflow.logical-publisher", qos: .userInitiated)
    private let lock = NSLock()
    private let factory: AttemptFactory
    private let policy: ReconnectPolicyPort
    private lazy var timer = NativeDeadlineTimer(queue: queue)
    private lazy var controller = ReconnectController(policy: policy, now: { DispatchTime.now().uptimeNanoseconds },
        activeChanged: { [weak self] value in self?.setActive(value as? NativePublishingAttempt) },
        changed: { [weak self] in self?.publishState() })
    private var currentEpoch: UInt64?
    private var epochCounter: UInt64 = 0
    private var configured = false
    private var fenced = false
    private var released = true
    private var active: NativePublishingAttempt?
    private var currentStatus = "Destination prepared · explicit Start required"

    init(graceMs: UInt64 = 90_000, source: NativeTransportRecoverySource, factory: @escaping AttemptFactory) throws {
        policy = try NativeReconnectPolicy(graceMs: graceMs, source: source); self.factory = factory
    }
    var status: String { lock.lock(); defer { lock.unlock() }; return currentStatus }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return released }
    func start() throws {
        lock.lock(); defer { lock.unlock() }
        guard !fenced, released, currentEpoch == nil, epochCounter < UInt64.max else { throw ReconnectFailure.cleanupRetained }
        epochCounter += 1; currentEpoch = epochCounter; configured = false
        currentStatus = "Waiting for this epoch's immutable video/audio configuration"
    }
    func configure(video: MediaCodec, audio: MediaCodec) throws {
        lock.lock()
        guard let epoch = currentEpoch, !fenced, !configured else { lock.unlock(); return }
        configured = true; released = false
        lock.unlock()
        queue.async {
            self.controller.attach(epoch: epoch) { [weak self] observe in
                guard let self, !self.isFenced else { throw ReconnectFailure.invalidEpoch }
                let sink = self.factory { [weak self] event, instant in
                    self?.queue.async { observe(event, instant) }
                }
                return NativePublishingAttempt(sink: sink, video: video, audio: audio,
                    deliver: { [weak self] work in self?.queue.async(execute: work) })
            }
            self.publishState()
        }
    }
    func offer(_ sample: MediaSample) throws {
        lock.lock(); let attempt = !fenced && currentEpoch != nil ? active : nil; lock.unlock()
        try attempt?.sink.offer(sample)
    }
    private var isFenced: Bool { lock.lock(); defer { lock.unlock() }; return fenced }
    private func setActive(_ value: NativePublishingAttempt?) {
        lock.lock(); let closed = fenced; active = closed ? nil : value; lock.unlock()
        if closed { value?.fence() }
    }
    private func publishState() {
        let clean = controller.cleanupComplete; let value = controller.status
        lock.lock(); released = clean; currentStatus = value; lock.unlock()
        timer.schedule(atNs: controller.nextDeadlineNs, generation: 1) { [weak self] _ in self?.controller.tick() }
    }
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void) {
        lock.lock(); let epoch = currentEpoch; let hadConfiguration = configured
        currentEpoch = nil; active = nil; configured = false
        lock.unlock()
        guard let epoch, hadConfiguration else { completion(.success(())); return }
        queue.async { self.controller.detach(epoch: epoch, completion: completion) }
    }
    func endDestination() {
        lock.lock(); fenced = true; let attempt = active; active = nil; lock.unlock()
        attempt?.fence() // close admission before queued policy/socket cleanup
        queue.async { self.controller.stop() }
    }
    func stop() { endDestination() }
    func stop(completion: @escaping () -> Void) {
        endDestination()
        finishEpoch { result in if case .success = result { completion() } }
    }
}
