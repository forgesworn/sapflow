import Foundation
import Sapflow

private final class NativeReconnectPolicy: ReconnectPolicyPort {
    private let native: NativeSession
    private let source: NativeTransportRecoverySource
    private(set) var generation: UInt64 = 0
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
    func confirmed(_ context: ReconnectAttemptContext, _ now: UInt64) throws {
        try OriginSDKConfirmation(session: native).apply(generation: context.sdkGeneration,
            attemptEpoch: context.sdkAttempt, nowNs: now)
        snapshot = try native.snapshot()
    }
    func failed(_ now: UInt64) throws -> ReconnectAction { try mapped(native.transportFailed(generation: generation, nowNs: now)) }
    func tick(_ now: UInt64) throws -> ReconnectAction { try mapped(native.tick(generation: generation, nowNs: now)) }
    func stop() throws { if generation != 0 { generation = try native.cancel(generation: generation); snapshot = try native.snapshot() } }
}

private final class NativePublishingAttempt: ReconnectAttemptPort {
    let sink: EncodedPublishingSink
    let context: ReconnectAttemptContext
    var codecEpoch: UInt64 { context.codecEpoch }
    private var codecs: (MediaCodec, MediaCodec)?
    private let admission = AttemptStartFence()
    private let deliver: (@escaping () -> Void) -> Void
    init(sink: EncodedPublishingSink, context: ReconnectAttemptContext, video: MediaCodec, audio: MediaCodec, deliver: @escaping (@escaping () -> Void) -> Void) {
        self.sink = sink; self.context = context; codecs = (video, audio); self.deliver = deliver
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
    typealias ContextualAttemptFactory = (ReconnectAttemptContext, @escaping (PublishingAttemptEvent, UInt64) -> Void) throws -> EncodedPublishingSink
    private let queue: DispatchQueue
    private let lock = NSLock()
    private let factory: ContextualAttemptFactory
    private let attemptReady: (ReconnectAttemptContext, UInt64) -> Void
    private let attemptInvalidated: (ReconnectAttemptContext) -> Void
    private let epochClosed: (UInt64) -> Void
    private let destinationEnded: () -> Void
    private let policy: ReconnectPolicyPort
    private lazy var timer = NativeDeadlineTimer(queue: queue)
    private lazy var controller = ReconnectController(policy: policy, now: { DispatchTime.now().uptimeNanoseconds },
        activeChanged: { [weak self] value in self?.setActive(value as? NativePublishingAttempt) },
        changed: { [weak self] in self?.publishState() },
        attemptReady: { [weak self] context, instant in self?.attemptReady(context, instant) })
    private var attachedEpoch: UInt64? // serial owner only
    private var currentEpoch: UInt64?
    private var epochCounter: UInt64 = 0
    private var configured = false
    private var codecClosing = false
    private var failedContext: ReconnectAttemptContext?
    private var fenced = false
    private var released = true
    private var active: NativePublishingAttempt?
    private var currentStatus = "Destination prepared · explicit Start required"

    convenience init(graceMs: UInt64 = 90_000, source: NativeTransportRecoverySource, factory: @escaping AttemptFactory) throws {
        try self.init(graceMs: graceMs, source: source, contextualFactory: { _, observe in factory(observe) })
    }
    /// All observer/controller SDK work shares this serial owner. Cold fence
    /// callbacks can arrive from any thread and must never perform IO or joins.
    init(graceMs: UInt64 = 90_000, source: NativeTransportRecoverySource,
         ownerQueue: DispatchQueue = DispatchQueue(label: "sapflow.logical-publisher", qos: .userInitiated),
         attemptReady: @escaping (ReconnectAttemptContext, UInt64) -> Void = { _, _ in },
         attemptInvalidated: @escaping (ReconnectAttemptContext) -> Void = { _ in },
         epochClosed: @escaping (UInt64) -> Void = { _ in },
         destinationEnded: @escaping () -> Void = {},
         contextualFactory: @escaping ContextualAttemptFactory) throws {
        policy = try NativeReconnectPolicy(graceMs: graceMs, source: source)
        queue = ownerQueue; factory = contextualFactory
        self.attemptReady = attemptReady; self.attemptInvalidated = attemptInvalidated
        self.epochClosed = epochClosed; self.destinationEnded = destinationEnded
    }
    /// Call only from the injected serial owner, inside the app's final cold
    /// confirmation gate. No synchronous queue hop or latest-snapshot lookup.
    func confirmObservedMedia(_ captured: ReconnectAttemptContext, nowNs: UInt64) throws {
        dispatchPrecondition(condition: .onQueue(queue))
        guard isCurrentEpoch(captured.codecEpoch) else { throw ReconnectFailure.invalidEpoch }
        try controller.confirmObservedMedia(captured, nowNs: nowNs)
    }
    var status: String { lock.lock(); defer { lock.unlock() }; return currentStatus }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return released }
    func start() throws {
        lock.lock(); defer { lock.unlock() }
        guard !fenced, released, currentEpoch == nil, epochCounter < UInt64.max else { throw ReconnectFailure.cleanupRetained }
        epochCounter += 1; currentEpoch = epochCounter; configured = false; codecClosing = false; failedContext = nil
        currentStatus = "Waiting for this epoch's immutable video/audio configuration"
    }
    func configure(video: MediaCodec, audio: MediaCodec) throws {
        lock.lock()
        guard let epoch = currentEpoch, !fenced, !codecClosing, !configured else { lock.unlock(); return }
        configured = true; released = false
        lock.unlock()
        queue.async {
            guard self.isCurrentEpoch(epoch) else { self.publishState(); return }
            self.attachedEpoch = epoch
            self.controller.attach(epoch: epoch, contextualFactory: { [weak self] context, observe in
                guard let self, self.isCurrentEpoch(epoch) else { throw ReconnectFailure.invalidEpoch }
                let sink = try self.factory(context, { [weak self] event, instant in
                    // Fence synchronously before serial control can be delayed.
                    // Stale contexts can invalidate only their own gate scope.
                    if case .failed = event { self?.invalidateAttempt(context) }
                    self?.queue.async { observe(event, instant) }
                })
                return NativePublishingAttempt(sink: sink, context: context, video: video, audio: audio,
                    deliver: { [weak self] work in self?.queue.async(execute: work) })
            })
            self.publishState()
        }
    }
    func offer(_ sample: MediaSample) throws {
        lock.lock(); let attempt = !fenced && !codecClosing && currentEpoch != nil ? active : nil; lock.unlock()
        try attempt?.sink.offer(sample)
    }
    private func isCurrentEpoch(_ epoch: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return !fenced && !codecClosing && currentEpoch == epoch && configured
    }
    private func invalidateAttempt(_ context: ReconnectAttemptContext) {
        lock.lock()
        if !fenced, !codecClosing, currentEpoch == context.codecEpoch, active?.context == context {
            failedContext = context
            currentStatus = "Uplink interrupted · playback unconfirmed"
        }
        lock.unlock()
        attemptInvalidated(context)
    }
    private func setActive(_ value: NativePublishingAttempt?) {
        lock.lock(); let closed = fenced || codecClosing || (value != nil && currentEpoch != value?.codecEpoch)
        active = closed ? nil : value
        if !closed, let value, value.context != failedContext { failedContext = nil }
        lock.unlock()
        if closed { value?.fence() }
    }
    private func publishState() {
        let clean = controller.cleanupComplete; let value = controller.status
        lock.lock(); released = clean
        // A queued old controller publication must never restore Live after a
        // synchronous cold fence, including the gap before detach is processed.
        if value.hasPrefix("Live"), fenced || codecClosing || currentEpoch == nil || failedContext != nil {
            currentStatus = fenced ? "Publisher stopping · playback unconfirmed" : "Publisher epoch closing · playback unconfirmed"
        } else { currentStatus = value }
        lock.unlock()
        timer.schedule(atNs: controller.nextDeadlineNs, generation: 1) { [weak self] _ in self?.controller.tick() }
    }
    /// Early codec-teardown fence. Keep the epoch identity for the later actual
    /// encoder completion, but stop admission/confirmation before its flush.
    func closeCodecEpoch() {
        lock.lock()
        guard let epoch = currentEpoch, !codecClosing else { lock.unlock(); return }
        codecClosing = true
        let attempt = active; active = nil
        currentStatus = "Publisher epoch closing · playback unconfirmed"
        lock.unlock()
        epochClosed(epoch)
        attempt?.fence()
    }
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void) {
        closeCodecEpoch()
        lock.lock(); let epoch = currentEpoch; let hadConfiguration = configured
        currentEpoch = nil; active = nil; configured = false
        lock.unlock()
        guard let epoch, hadConfiguration else { completion(.success(())); return }
        queue.async {
            guard self.attachedEpoch == epoch else { completion(.success(())); self.publishState(); return }
            self.attachedEpoch = nil
            self.controller.detach(epoch: epoch, completion: completion)
        }
    }
    func endDestination() {
        lock.lock(); fenced = true; let attempt = active; active = nil
        currentStatus = "Publisher stopping · playback unconfirmed"
        lock.unlock()
        destinationEnded() // observation fence precedes any policy/socket queue
        attempt?.fence() // close admission before queued policy/socket cleanup
        queue.async { self.controller.stop() }
    }
    func stop() { endDestination() }
    func stop(completion: @escaping () -> Void) {
        endDestination()
        finishEpoch { result in if case .success = result { completion() } }
    }
}
