import Foundation
import Sapflow

/// An acknowledged lease has an independent cold expiry owner. Refresh and
/// expiry race on this lock, so a delayed old alarm cannot end a renewed lease
/// and a refresh cannot revive one whose expiry has already won.
final class V4vPublishingLifetime: @unchecked Sendable {
    private let lock = NSLock()
    private var value: V4vPublishingGrantPermit
    private var ended = false
    private var highWaterNs: UInt64
    init(_ permit: V4vPublishingGrantPermit, nowNs: UInt64) throws {
        guard permit.expiresAtNs > nowNs else { throw OriginObservationFailure.deadline }
        value = permit; highWaterNs = nowNs
    }
    func current(nowNs: UInt64) throws -> V4vPublishingGrantPermit {
        try lock.withLock {
            highWaterNs = max(highWaterNs, nowNs)
            guard !ended, highWaterNs < value.expiresAtNs else { ended = true; throw OriginObservationFailure.deadline }
            return value
        }
    }
    func committed(_ next: V4vPublishingGrantPermit, nowNs: UInt64) throws {
        try lock.withLock {
            highWaterNs = max(highWaterNs, nowNs)
            guard !ended, highWaterNs < value.expiresAtNs, next.expiresAtNs > highWaterNs,
                  next.expiresAtNs >= value.expiresAtNs, value.grant.immutableMatches(next.grant),
                  next.grant.identity.revision >= value.grant.identity.revision else { throw OriginObservationFailure.scope }
            value = next
        }
    }
    func expire(nowNs: UInt64) -> Bool {
        lock.withLock {
            highWaterNs = max(highWaterNs, nowNs)
            guard !ended, highWaterNs >= value.expiresAtNs else { return false }
            ended = true; return true
        }
    }
    func close() { lock.withLock { ended = true } }
}

/// The actual scoped destination adapter for CaptureEngine. It reuses the
/// existing reconnect, SRT and compressed-media path. Observer failures affect
/// decoded confirmation only; independent capture/recording keep their owners.
final class V4vObservedPublisher: EncodedPublishingSink, @unchecked Sendable {
    typealias TransportFactory = (ManualSrtDestination, SrtPublishingLimits, Bool, AudioShowOutputAuthority,
                                 @escaping (PublishingAttemptEvent, UInt64) -> Void) throws -> EncodedPublishingSink
    typealias ObserverFactory = (V4vObservedAttempt, OriginConfirmationGate, @escaping (OriginAttemptIdentity, UInt64) throws -> Void) throws -> V4vOriginObserver
    private let destination: UInt64
    private let captureProfile: CaptureIntent
    private let expectedMedia: OriginExpectedMedia
    private let owner: DispatchQueue
    private let clock: () -> V4vClock
    private let lifetime: V4vPublishingLifetime
    private let outputAuthority: AudioShowOutputAuthority
    private let gate = OriginConfirmationGate()
    private let slot: V4vObservationSlot
    private let transport: TransportFactory
    private let observation: ObserverFactory
    private let expiryQueue = DispatchQueue(label: "sapflow.v4v.publishing-expiry")
    private let expiryTimer: NativeDeadlineTimer
    private var logical: ReconnectingPublisher!
    private var currentAttempt: V4vObservedAttempt? // owner queue only

    init(destination: UInt64, permit: V4vPublishingGrantPermit, profile: V4vConnectorProfile,
         engine: V4vSessionEngine, owner: DispatchQueue, slot: V4vObservationSlot, clock: @escaping () -> V4vClock,
         captureProfile: CaptureIntent = .defaultProfile,
         transport: TransportFactory? = nil,
         observation: ObserverFactory? = nil) throws {
        try engine.requireProfile(profile)
        guard destination > 0, captureProfile.hasAudio else { throw OriginObservationFailure.scope }
        _ = try SrtPublishingLimits.scoped(maxQueueBytes: permit.grant.maxQueueBytes, maxPublishBps: permit.grant.maxPublishBps, captureProfile: captureProfile)
        self.captureProfile = captureProfile
        expectedMedia = captureProfile.hasVideo ? .audioVideo : .audioOnly48kMono
        self.destination = destination; self.owner = owner; self.slot = slot; self.clock = clock
        let lifetime = try V4vPublishingLifetime(permit, nowNs: clock().monotonicNs)
        self.lifetime = lifetime
        let authority = AudioShowOutputAuthority(check: { (try? lifetime.current(nowNs: clock().monotonicNs)) != nil })
        outputAuthority = authority
        self.transport = transport ?? { ManualSrtPublisher(destination: $0, limits: $1, hasVideo: $2, outputAuthority: $3, observe: $4) }
        expiryTimer = NativeDeadlineTimer(queue: expiryQueue, now: { clock().monotonicNs })
        self.observation = observation ?? { attempt, gate, confirm in
            try V4vOriginObserver(attempt: attempt, profile: profile, gate: gate,
                status: V4vOriginStatusHTTP(profile: profile, engine: engine, owner: owner, clock: clock),
                owner: owner, clock: clock, confirm: confirm)
        }
        logical = try ReconnectingPublisher(graceMs: permit.grant.graceMs, source: .srtPeerAcknowledgedWatermark, hasVideo: captureProfile.hasVideo,
            authorityClock: { clock().monotonicNs }, ownerQueue: owner,
            attemptReady: { [weak self] in self?.ready($0, nowNs: $1) },
            attemptInvalidated: { [weak self] in self?.invalidated($0) },
            epochClosed: { [weak self] in self?.epochClosed($0) },
            destinationEnded: { [weak self] in self?.ended() },
            contextualFactory: { [weak self] context, observe in
                guard let self else { throw OriginObservationFailure.cancelled }
                return try self.make(context, observe: observe)
            })
        armExpiry(permit)
        outputAuthority.denied = { [weak self] in self?.endDestination() }
    }
    private func local(_ context: ReconnectAttemptContext) throws -> OriginAttemptIdentity {
        try OriginAttemptIdentity(destination: destination, codecEpoch: context.codecEpoch, token: context.token,
                                  sdkGeneration: context.sdkGeneration, sdkAttempt: context.sdkAttempt)
    }
    private func make(_ context: ReconnectAttemptContext, observe: @escaping (PublishingAttemptEvent, UInt64) -> Void) throws -> EncodedPublishingSink {
        dispatchPrecondition(condition: .onQueue(owner))
        let permit = try lifetime.current(nowNs: clock().monotonicNs)
        let attempt = try V4vObservedAttempt(local: local(context), grant: permit.grant, nonce: V4vObservedAttempt.randomNonce(), expectedMedia: expectedMedia)
        try gate.stage(OriginCurrentAuthority(attempt: attempt, grant: permit.grant, expiresAtNs: permit.expiresAtNs), clock: { self.clock().monotonicNs })
        do {
            let destination = try ManualSrtDestination(endpoint: permit.grant.endpoint, streamID: attempt.wireStreamID(grant: permit.grant), passphrase: permit.grant.passphrase, allowPlaintext: false)
            let limits = try SrtPublishingLimits.scoped(maxQueueBytes: permit.grant.maxQueueBytes, maxPublishBps: permit.grant.maxPublishBps, captureProfile: captureProfile)
            currentAttempt = attempt
            return try transport(destination, limits, captureProfile.hasVideo, outputAuthority, observe)
        } catch { gate.invalidate(attempt: attempt); throw error }
    }
    private func ready(_ context: ReconnectAttemptContext, nowNs: UInt64) {
        dispatchPrecondition(condition: .onQueue(owner))
        guard let attempt = currentAttempt, (try? local(context)) == attempt.local,
              nowNs <= UInt64.max - 8_000_000_000, gate.activate(attempt, clock: { self.clock().monotonicNs }) else { return }
        slot.submit(V4vObservationRequest(attempt: attempt, gate: gate, deadlineNs: nowNs + 8_000_000_000, make: { [weak self] in
            guard let self else { throw OriginObservationFailure.cancelled }
            return try self.observation(attempt, self.gate, { [weak self] captured, instant in
                guard let self, captured == attempt.local else { throw OriginObservationFailure.scope }
                try self.logical.confirmObservedMedia(context, nowNs: instant)
            })
        }, completed: { _ in })) // SDK/controller owns the visible Live state.
    }
    private func invalidated(_ context: ReconnectAttemptContext) {
        guard let local = try? local(context) else { return }
        gate.invalidate(local: local)
        owner.async { [weak self] in
            guard let self else { return }
            if self.currentAttempt?.local == local { self.currentAttempt = nil }
            self.slot.cancel(local)
        }
    }
    private func epochClosed(_ epoch: UInt64) {
        gate.closeEpoch(epoch)
        owner.async { [weak self] in
            guard let self else { return }
            if let attempt = self.currentAttempt, attempt.local.codecEpoch <= epoch { self.currentAttempt = nil }
            self.slot.cancelEpoch(destination: self.destination, epoch: epoch)
        }
    }
    private func ended() {
        gate.close(); lifetime.close()
        expiryQueue.async { [weak self] in self?.expiryTimer.cancel() }
        owner.async { [weak self] in
            guard let self else { return }; self.currentAttempt = nil; self.slot.cancelDestination(self.destination)
        }
    }
    private func armExpiry(_ permit: V4vPublishingGrantPermit) {
        expiryQueue.async { [weak self] in
            guard let self else { return }
            self.expiryTimer.schedule(atNs: permit.expiresAtNs, generation: permit.grant.identity.revision) { [weak self] _ in
                guard let self, self.lifetime.expire(nowNs: self.clock().monotonicNs) else { return }
                self.gate.close(); self.logical.endDestination()
            }
        }
    }
    /// Call only after the session engine durably commits the acknowledged
    /// refresh, on the same owner queue as the publisher and observer.
    func committed(_ permit: V4vPublishingGrantPermit) throws {
        dispatchPrecondition(condition: .onQueue(owner))
        do {
            try lifetime.committed(permit, nowNs: clock().monotonicNs)
            if let attempt = currentAttempt {
                do { try gate.refresh(OriginCurrentAuthority(attempt: attempt, grant: permit.grant, expiresAtNs: permit.expiresAtNs), clock: { self.clock().monotonicNs }) }
                catch { gate.invalidate(attempt: attempt); slot.cancel(attempt.local) }
            }
            armExpiry(permit)
        } catch { endDestination(); throw error }
    }
    var status: String { logical.status }
    var cleanupComplete: Bool { logical.cleanupComplete }
    func observeBroadcastEnd(_ completion: @escaping (UInt64) -> Void) { logical.observeBroadcastEnd(completion) }
    func readSocketOutbound(completion: @escaping (SocketOutboundSample?) -> Void) { logical.readSocketOutbound(completion: completion) }
    func start() throws { _ = try lifetime.current(nowNs: clock().monotonicNs); try logical.start() }
    func configure(video: MediaCodec?, audio: MediaCodec) throws {
        guard captureProfile.hasVideo == (video != nil) else { throw OriginObservationFailure.scope }
        try logical.configure(video: video, audio: audio)
    }
    func offer(_ sample: MediaSample) throws {
        // An alarm may be withheld while suspended. Never let its queue order
        // admit encoded media past the unchanged authoritative grant deadline.
        do { _ = try lifetime.current(nowNs: clock().monotonicNs) }
        catch { endDestination(); throw error }
        try logical.offer(sample)
    }
    func revalidateAuthority() {
        do { _ = try lifetime.current(nowNs: clock().monotonicNs); logical.revalidateAuthority() }
        catch { endDestination() }
    }
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void) { logical.finishEpoch(completion: completion) }
    func closeCodecEpoch() { logical.closeCodecEpoch() }
    func endDestination() { logical.endDestination() }
    func stop() { logical.stop() }
    func stop(completion: @escaping () -> Void) { logical.stop(completion: completion) }
}
