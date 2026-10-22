import Foundation

struct OriginStatusSnapshot {
    let receipt: V4vSessionReceipt
    let expiresAtNs: UInt64
}

protocol OriginSessionStatusFetching: AnyObject {
    var cleanupComplete: Bool { get }
    func whenCleanupComplete(_ completion: @escaping () -> Void)
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void)
    func close()
}

protocol OriginStatusControlExecuting: AnyObject {
    var cleanupComplete: Bool { get }
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void)
    func close()
}
extension V4vControlClient: OriginStatusControlExecuting {}

/// Uses the existing bounded control implementation, on a separate connection
/// owner so cancellation cannot close the creator's End/refresh client. Only
/// the session engine can issue the exact-session bearer request.
final class V4vOriginStatusHTTP: OriginSessionStatusFetching, @unchecked Sendable {
    private let engine: V4vSessionEngine
    private let client: OriginStatusControlExecuting
    private let cleanup: OriginCleanupNotification
    private let owner: DispatchQueue
    private let clock: () -> V4vClock
    private let lock = NSLock()
    private var busy = false, closed = false
    init(profile: V4vConnectorProfile, engine: V4vSessionEngine, owner: DispatchQueue, clock: @escaping () -> V4vClock,
         makeClient: ((@escaping () -> Void) -> OriginStatusControlExecuting)? = nil) throws {
        try engine.requireProfile(profile)
        let cleanup = OriginCleanupNotification(); self.cleanup = cleanup
        self.engine = engine; self.owner = owner; self.clock = clock
        client = makeClient?({ cleanup.complete() }) ?? V4vControlClient(profile: profile, onClosed: { cleanup.complete() })
    }
    var cleanupComplete: Bool { client.cleanupComplete }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) {
        lock.lock()
        guard !closed, !busy else { let error: OriginObservationFailure = closed ? .cancelled : .busy; lock.unlock(); completion(.failure(error)); return }
        busy = true; lock.unlock()
        owner.async { [self] in
            guard !lock.withLock({ closed }) else { finish(.failure(.cancelled), completion); return }
            do {
                let permit = try engine.observationStatusRequest(clock: clock()), request = permit.call
                let exactPath = "/api/live-events/\(attempt.identity.eventId)/native-session/\(attempt.identity.sessionId)"
                guard request.path == exactPath else { throw OriginObservationFailure.scope }
                client.execute(request) { [self] answer in
                    owner.async { [self] in
                        guard !lock.withLock({ closed }) else { finish(.failure(.cancelled), completion); return }
                        do {
                            guard clock().monotonicNs < permit.expiresAtNs else { throw OriginObservationFailure.scope }
                            let raw = try answer.get()
                            let fields = try raw.object(required: ["ok", "session"])
                            guard try fields.flag("ok") else { throw OriginObservationFailure.scope }
                            let receipt = try V4vSessionReceipt(fields.need("session"))
                            guard receipt.identity.sameSession(attempt.identity) else { throw OriginObservationFailure.scope }
                            // Observation is read-only. A parallel creator
                            // Refresh may have advanced revision while this GET
                            // was in flight; that old reply must not mutate or
                            // fence the publisher's healthy refreshed grant.
                            let current = try engine.observationStatusRequest(clock: clock())
                            guard current.identity == receipt.identity else { throw OriginObservationFailure.unavailable }
                            let expires = min(permit.expiresAtNs, current.expiresAtNs)
                            guard clock().monotonicNs < expires else { throw OriginObservationFailure.scope }
                            finish(.success(OriginStatusSnapshot(receipt: receipt, expiresAtNs: expires)), completion)
                        } catch { finish(.failure((error as? OriginObservationFailure) ?? .unavailable), completion) }
                    }
                }
            } catch { finish(.failure((error as? OriginObservationFailure) ?? .unavailable), completion) }
        }
    }
    private func finish(_ result: Result<OriginStatusSnapshot, OriginObservationFailure>, _ completion: (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) {
        lock.withLock { busy = false }; completion(result)
    }
    func close() { lock.withLock { closed = true }; client.close() }
}

/// One finite observation of one captured transport. The destination owner
/// retains this object until cleanupComplete, including after a timeout. It
/// must use the same confirmation gate for synchronous Stop/End/expiry,
/// transport failure and codec-close fencing. No camera or recording is owned.
final class V4vOriginObserver: @unchecked Sendable {
    private let attempt: V4vObservedAttempt
    private let profile: V4vConnectorProfile
    private let gate: OriginConfirmationGate
    private let status: OriginSessionStatusFetching
    private let media: OriginMediaFetching
    private let decoder: OriginMediaDecoding
    private let clock: () -> V4vClock
    private let confirm: (OriginAttemptIdentity, UInt64) throws -> Void
    private let queue: DispatchQueue
    private let lock = NSLock()
    private var used = false, closed = false
    private var phase = 0
    private var deadlineNs: UInt64 = 0
    private var authorityDeadlineNs: UInt64 = UInt64.max
    private var timer: DispatchSourceTimer?
    private var cleanupTimer: DispatchSourceTimer?
    private let cleanup = OriginCleanupNotification()
    private var outcome: Result<OriginDecodedMedia, OriginObservationFailure>?
    private var completion: ((Result<OriginDecodedMedia, OriginObservationFailure>) -> Void)?
    private var selection: OriginMediaSelection?
    private var beforeRequests = 0
    var cleanupComplete: Bool { status.cleanupComplete && media.cleanupComplete && decoder.cleanupComplete }

    init(attempt: V4vObservedAttempt, profile: V4vConnectorProfile, gate: OriginConfirmationGate,
         status: OriginSessionStatusFetching, media: OriginMediaFetching = OriginMediaHTTP(), decoder: OriginMediaDecoding = OriginMediaDecoder(),
         owner: DispatchQueue = DispatchQueue(label: "sapflow.origin.observer"),
         clock: @escaping () -> V4vClock, confirm: @escaping (OriginAttemptIdentity, UInt64) throws -> Void) {
        self.attempt = attempt; self.profile = profile; self.gate = gate; self.status = status; self.media = media; self.decoder = decoder
        queue = owner; self.clock = clock; self.confirm = confirm
        let changed = { [weak self] in
            guard let self else { return }
            self.queue.async { [weak self] in self?.cleanupChanged() }
        }
        status.whenCleanupComplete(changed); media.whenCleanupComplete(changed); decoder.whenCleanupComplete(changed)
    }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func start(deadlineNs originalDeadlineNs: UInt64? = nil, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) {
        lock.lock()
        guard !closed, !used else { let error: OriginObservationFailure = closed ? .cancelled : .busy; lock.unlock(); completion(.failure(error)); return }
        used = true; lock.unlock()
        queue.async { [self] in
            self.completion = completion
            let now = clock().monotonicNs
            guard now <= UInt64.max - 8_000_000_000 else { finish(.failure(.deadline)); return }
            deadlineNs = originalDeadlineNs ?? now + 8_000_000_000
            guard deadlineNs > now, deadlineNs - now <= 8_000_000_000 else { finish(.failure(.deadline)); return }
            let deadline = DispatchSource.makeTimerSource(queue: queue)
            deadline.schedule(deadline: DispatchTime(uptimeNanoseconds: deadlineNs))
            deadline.setEventHandler { [weak self] in self?.cancel(.deadline) }
            timer = deadline; deadline.resume()
            guard permitted() else { finish(.failure(.scope)); return }
            phase = 1
            requestBefore()
        }
    }
    /// Cancellation orders with SDK confirmation immediately, before queueing
    /// HTTP/native cleanup. It fences observation, not the independent recorder.
    func cancel() { cancel(.cancelled) }
    private func cancel(_ reason: OriginObservationFailure) {
        gate.invalidate(attempt: attempt)
        let first = lock.withLock { () -> Bool in
            guard !closed else { return false }; closed = true; return true
        }
        guard first else { return }
        status.close(); media.close(); decoder.close()
        queue.async { [self] in finish(.failure(reason)) }
    }
    private func permitted() -> Bool {
        let instant = clock().monotonicNs
        return !lock.withLock({ closed }) && instant < min(deadlineNs, authorityDeadlineNs) && gate.permits(attempt, nowNs: instant)
    }
    private func requestBefore() {
        guard phase == 1, permitted(), beforeRequests < 12 else { finish(.failure(.unavailable)); return }
        beforeRequests += 1
        status.fresh(attempt) { [self] answer in queue.async { [self] in before(answer) } }
    }
    private func before(_ answer: Result<OriginStatusSnapshot, OriginObservationFailure>) {
        guard phase == 1 else { return }
        do {
            guard permitted() else { throw OriginObservationFailure.scope }
            let snapshot = try answer.get()
            authorityDeadlineNs = min(authorityDeadlineNs, snapshot.expiresAtNs)
            guard permitted() else { throw OriginObservationFailure.scope }
            let chosen = try OriginMediaSelection.match(snapshot.receipt, attempt: attempt, nowMs: clock().wallMs)
            let resources = try OriginApprovedResources(profile: profile, media: chosen)
            selection = chosen; phase = 2
            media.fetch(resources, deadlineNs: min(deadlineNs, authorityDeadlineNs)) { [self] result in queue.async { [self] in fetched(result) } }
        } catch {
            let reason = (error as? OriginObservationFailure) ?? .unavailable
            if [.busy, .unavailable, .network].contains(reason), permitted(), beforeRequests < 12 {
                // Finite availability/control-contention retries retain the
                // original observation and authority deadlines. No second
                // decoder or session-policy retry budget is created.
                queue.asyncAfter(deadline: .now() + .milliseconds(500)) { [self] in requestBefore() }
            } else { finish(.failure(reason)) }
        }
    }
    private func fetched(_ answer: Result<Data, OriginObservationFailure>) {
        guard phase == 2 else { return }
        do {
            guard permitted() else { throw OriginObservationFailure.scope }
            let data = try answer.get(), now = clock().monotonicNs
            guard now <= UInt64.max - 3_000_000_000 else { throw OriginObservationFailure.deadline }
            phase = 3
            decoder.decode(data, expectedMedia: attempt.expectedMedia, deadlineNs: min(deadlineNs, authorityDeadlineNs, now + 3_000_000_000)) { [self] result in queue.async { [self] in decoded(result) } }
        } catch { finish(.failure((error as? OriginObservationFailure) ?? .unavailable)) }
    }
    private func decoded(_ answer: Result<OriginDecodedMedia, OriginObservationFailure>) {
        guard phase == 3 else { return }
        do {
            guard permitted() else { throw OriginObservationFailure.scope }
            let proof = try answer.get()
            guard proof.sufficient(for: attempt.expectedMedia), decoder.cleanupComplete else { throw OriginObservationFailure.progress }
            phase = 4
            status.fresh(attempt) { [self] result in queue.async { [self] in after(result, proof: proof) } }
        } catch { finish(.failure((error as? OriginObservationFailure) ?? .unavailable)) }
    }
    private func after(_ answer: Result<OriginStatusSnapshot, OriginObservationFailure>, proof: OriginDecodedMedia) {
        guard phase == 4 else { return }
        do {
            guard permitted(), let selection else { throw OriginObservationFailure.scope }
            let snapshot = try answer.get()
            authorityDeadlineNs = min(authorityDeadlineNs, snapshot.expiresAtNs)
            guard permitted() else { throw OriginObservationFailure.scope }
            let current = try OriginMediaSelection.match(snapshot.receipt, attempt: attempt, nowMs: clock().wallMs)
            guard selection.stillCurrent(current) else { throw OriginObservationFailure.scope }
            finish(.success(proof))
        } catch { finish(.failure((error as? OriginObservationFailure) ?? .unavailable)) }
    }
    private func finish(_ result: Result<OriginDecodedMedia, OriginObservationFailure>) {
        guard phase < 5, completion != nil else { return }; phase = 5
        if case .failure = result { gate.invalidate(attempt: attempt) }
        outcome = result
        status.close(); media.close(); decoder.close()
        if cleanupComplete { cleanupChanged(); return }
        let deadline = DispatchSource.makeTimerSource(queue: queue)
        deadline.schedule(deadline: .now() + .seconds(2))
        deadline.setEventHandler { [weak self] in
            guard let self, self.phase == 5 else { return }
            if self.cleanupComplete { self.cleanupChanged(); return }
            self.phase = 6; self.stopTimers(); self.outcome = nil
            self.gate.invalidate(attempt: self.attempt)
            let callback = self.completion; self.completion = nil; callback?(.failure(.cleanup))
            // Late actual cleanup can still notify the retained app slot,
            // but can never adopt a proof after this failure was reported.
        }
        cleanupTimer = deadline; deadline.resume()
    }
    private func stopTimers() {
        timer?.setEventHandler {}; timer?.cancel(); timer = nil
        cleanupTimer?.setEventHandler {}; cleanupTimer?.cancel(); cleanupTimer = nil
    }
    private func cleanupChanged() {
        guard cleanupComplete else { return }
        if phase == 5, let outcome {
            phase = 6; stopTimers(); self.outcome = nil
            let final: Result<OriginDecodedMedia, OriginObservationFailure>
            do {
                let proof = try outcome.get()
                guard !lock.withLock({ closed }) else { throw OriginObservationFailure.cancelled }
                try gate.confirm(attempt, beforeNs: min(deadlineNs, authorityDeadlineNs), clock: { self.clock().monotonicNs }, apply: confirm)
                final = .success(proof)
            } catch { final = .failure((error as? OriginObservationFailure) ?? .scope) }
            let callback = completion; completion = nil; callback?(final)
            cleanup.complete()
        } else if phase == 6 { cleanup.complete() }
    }
}
