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
    private var codecs: (MediaCodec?, MediaCodec)?
    private let admission = AttemptStartFence()
    private let deliver: (@escaping () -> Void) -> Void
    private let closed: (ReconnectAttemptContext, SocketOutboundSample?) -> Void
    init(sink: EncodedPublishingSink, context: ReconnectAttemptContext, video: MediaCodec?, audio: MediaCodec,
         deliver: @escaping (@escaping () -> Void) -> Void, closed: @escaping (ReconnectAttemptContext, SocketOutboundSample?) -> Void) {
        self.sink = sink; self.context = context; codecs = (video, audio); self.deliver = deliver; self.closed = closed
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
            deliver { [self] in
                closed(context, sink.finalSocketOutbound)
                codecs = nil; completion() // final accounting precedes any successor
            }
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
    private let authorityClock: () -> UInt64
    private let hasVideo: Bool
    private lazy var timer = NativeDeadlineTimer(queue: queue, now: authorityClock)
    private lazy var controller = ReconnectController(policy: policy, now: authorityClock,
        activeChanged: { [weak self] value in self?.setActive(value as? NativePublishingAttempt) },
        changed: { [weak self] in self?.publishState() },
        attemptReady: { [weak self] context, instant in self?.attemptReady(context, instant) })
    private var attachedEpoch: UInt64? // serial owner only
    private var policyStarted = false // serial owner only; no wake may prepare/start a session
    private var currentEpoch: UInt64?
    private var epochCounter: UInt64 = 0
    private var configured = false
    private var codecClosing = false
    private var failedContext: ReconnectAttemptContext?
    private var fenced = false
    private var released = true
    private var active: NativePublishingAttempt?
    private var authorityDeadlineNs: UInt64? // scalar mirror, lock only
    private var authorityCheckQueued = false // one cold revalidation, lock only
    private var currentStatus = "Destination prepared · explicit Start required"
    private var telemetry = PublishingTelemetryAggregate<ReconnectAttemptContext>() // queue only
    private var telemetryContext: ReconnectAttemptContext? // queue only
    private var telemetryReadPending = false // queue only
    private var telemetryReadSerial: UInt64 = 0 // queue only
    private var broadcastEndedAtNs: UInt64? // lock only
    private var broadcastEndObserver: ((UInt64) -> Void)? // lock only, one cold listener

    convenience init(graceMs: UInt64 = 90_000, source: NativeTransportRecoverySource, hasVideo: Bool = true, factory: @escaping AttemptFactory) throws {
        try self.init(graceMs: graceMs, source: source, hasVideo: hasVideo, contextualFactory: { _, observe in factory(observe) })
    }
    /// All observer/controller SDK work shares this serial owner. Cold fence
    /// callbacks can arrive from any thread and must never perform IO or joins.
    init(graceMs: UInt64 = 90_000, source: NativeTransportRecoverySource,
         hasVideo: Bool = true,
         authorityClock: @escaping () -> UInt64 = AudioShowAuthorityClock.nowNs,
         ownerQueue: DispatchQueue = DispatchQueue(label: "sapflow.logical-publisher", qos: .userInitiated),
         attemptReady: @escaping (ReconnectAttemptContext, UInt64) -> Void = { _, _ in },
         attemptInvalidated: @escaping (ReconnectAttemptContext) -> Void = { _ in },
         epochClosed: @escaping (UInt64) -> Void = { _ in },
         destinationEnded: @escaping () -> Void = {},
         contextualFactory: @escaping ContextualAttemptFactory) throws {
        policy = try NativeReconnectPolicy(graceMs: graceMs, source: source)
        self.authorityClock = authorityClock
        self.hasVideo = hasVideo
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
    func observeBroadcastEnd(_ completion: @escaping (UInt64) -> Void) {
        lock.lock(); broadcastEndObserver = completion; let ended = broadcastEndedAtNs; lock.unlock()
        if let ended { completion(ended) }
    }
    private func noteTerminal(_ instant: UInt64) {
        lock.lock()
        guard broadcastEndedAtNs == nil else { lock.unlock(); return }
        broadcastEndedAtNs = instant; let callback = broadcastEndObserver
        lock.unlock(); callback?(instant)
    }
    func readSocketOutbound(completion: @escaping (SocketOutboundSample?) -> Void) {
        queue.async {
            self.lock.lock()
            let attempt = !self.fenced && !self.codecClosing && self.failedContext == nil ? self.active : nil
            self.lock.unlock()
            guard !self.telemetryReadPending, let attempt, self.telemetryReadSerial < UInt64.max else { completion(nil); return }
            self.telemetryReadSerial += 1; let query = self.telemetryReadSerial
            self.telemetryReadPending = true
            attempt.sink.readSocketOutbound { [weak self] sample in
                guard let self else { return }
                self.queue.async {
                    guard self.telemetryReadPending, self.telemetryReadSerial == query else { return }
                    self.telemetryReadPending = false
                    self.lock.lock()
                    let current = !self.fenced && !self.codecClosing && self.failedContext == nil && self.active?.context == attempt.context
                    self.lock.unlock()
                    completion(current ? self.telemetry.read(attempt.context, sample: sample) : nil)
                }
            }
        }
    }
    func start() throws {
        lock.lock(); defer { lock.unlock() }
        guard !fenced, released, currentEpoch == nil, epochCounter < UInt64.max else { throw ReconnectFailure.cleanupRetained }
        epochCounter += 1; currentEpoch = epochCounter; configured = false; codecClosing = false; failedContext = nil
        currentStatus = hasVideo ? "Waiting for this epoch's immutable video/audio configuration" : "Waiting for this epoch's immutable audio configuration"
    }
    func configure(video: MediaCodec?, audio: MediaCodec) throws {
        guard hasVideo == (video != nil) else { throw CaptureFailure(message: "Reconnect codecs do not match the prepared capture") }
        lock.lock()
        guard let epoch = currentEpoch, !fenced, !codecClosing, !configured else { lock.unlock(); return }
        configured = true; released = false
        lock.unlock()
        queue.async {
            guard self.isCurrentEpoch(epoch) else { self.publishState(); return }
            self.attachedEpoch = epoch
            self.policyStarted = true
            self.controller.attach(epoch: epoch, contextualFactory: { [weak self] context, observe in
                guard let self, self.isCurrentEpoch(epoch) else { throw ReconnectFailure.invalidEpoch }
                let sink = try self.factory(context, { [weak self] event, instant in
                    // Fence synchronously before serial control can be delayed.
                    // Stale contexts can invalidate only their own gate scope.
                    if case .failed = event { self?.invalidateAttempt(context) }
                    self?.queue.async { observe(event, instant) }
                })
                self.telemetry.opened(context); self.telemetryContext = context
                return NativePublishingAttempt(sink: sink, context: context, video: video, audio: audio,
                    deliver: { [weak self] work in self?.queue.async(execute: work) },
                    closed: { [weak self] context, sample in self?.telemetry.finished(context, sample: sample) })
            })
            self.publishState()
        }
    }
    func offer(_ sample: MediaSample) throws {
        guard hasVideo || !sample.isVideo() else { throw CaptureFailure(message: "Video is forbidden in this audio-only session") }
        lock.lock(); let deadline = authorityDeadlineNs; lock.unlock()
        // Usually no clock read is needed. At an outstanding retry/cleanup
        // boundary, close this scalar gate before a delayed timer can race PCM.
        if let deadline, authorityClock() >= deadline { revalidateAuthority(); return }
        lock.lock()
        let attempt = !fenced && !codecClosing && currentEpoch != nil && failedContext == nil && !authorityCheckQueued ? active : nil
        lock.unlock()
        try attempt?.sink.offer(sample)
    }
    func revalidateAuthority() {
        lock.lock()
        guard !fenced, !authorityCheckQueued else { lock.unlock(); return }
        authorityCheckQueued = true
        lock.unlock()
        queue.async { [self] in
            if policyStarted { controller.tick(); publishState() }
            lock.lock(); authorityCheckQueued = false; lock.unlock()
        }
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
        if value == nil, let context = telemetryContext { telemetry.closing(context); telemetryContext = nil }
        lock.lock(); let closed = fenced || codecClosing || (value != nil && currentEpoch != value?.codecEpoch)
        active = closed ? nil : value
        if !closed, let value, value.context != failedContext { failedContext = nil }
        lock.unlock()
        if closed { value?.fence() }
    }
    private func publishState() {
        let clean = controller.cleanupComplete; let value = controller.status
        let deadline = controller.nextDeadlineNs
        lock.lock(); released = clean
        authorityDeadlineNs = deadline
        // A queued old controller publication must never restore Live after a
        // synchronous cold fence, including the gap before detach is processed.
        if value.hasPrefix("Live"), fenced || codecClosing || currentEpoch == nil || failedContext != nil {
            currentStatus = fenced ? "Publisher stopping · playback unconfirmed" : "Publisher epoch closing · playback unconfirmed"
        } else { currentStatus = value }
        lock.unlock()
        if controller.terminalAtNs != nil { noteTerminal(DispatchTime.now().uptimeNanoseconds) }
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
        let instant = DispatchTime.now().uptimeNanoseconds
        let notify = broadcastEndedAtNs == nil
        if notify { broadcastEndedAtNs = instant }
        let endObserver = notify ? broadcastEndObserver : nil
        currentStatus = "Publisher stopping · playback unconfirmed"
        lock.unlock()
        destinationEnded() // observation fence precedes any policy/socket queue
        endObserver?(instant)
        attempt?.fence() // close admission before queued policy/socket cleanup
        queue.async { self.controller.stop() }
    }
    func stop() { endDestination() }
    func stop(completion: @escaping () -> Void) {
        endDestination()
        finishEpoch { result in if case .success = result { completion() } }
    }
}
