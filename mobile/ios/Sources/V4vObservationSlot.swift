import Foundation

/// A pending scope contains no HTTP client or decoder. Its factory is admitted
/// only after the previous observer reports actual cleanup, including late
/// cleanup after a semantic failure. One app-owned slot spans destinations.
struct V4vObservationRequest {
    let attempt: V4vObservedAttempt
    let gate: OriginConfirmationGate
    let deadlineNs: UInt64
    let make: () throws -> V4vOriginObserver
    let completed: (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void
}

final class V4vObservationSlot {
    private let owner: DispatchQueue
    private let clock: () -> V4vClock
    private var active: (attempt: V4vObservedAttempt, observer: V4vOriginObserver)?
    private var pending: V4vObservationRequest?
    private var pendingTimer: DispatchSourceTimer?
    private var isClosed = false
    private var closeCompletion: (() -> Void)?
    private var retainedSelf: V4vObservationSlot?
    init(owner: DispatchQueue, clock: @escaping () -> V4vClock) { self.owner = owner; self.clock = clock }

    func submit(_ request: V4vObservationRequest) {
        dispatchPrecondition(condition: .onQueue(owner))
        guard !isClosed else { discard(request, reason: .cancelled); return }
        let previous = pending
        pending = request
        if let previous { discard(previous, reason: .cancelled) }
        armPendingDeadline()
        if let active { active.observer.cancel() }
        else { advance() }
    }
    /// Owner-queue barrier for actual observer cleanup, not its semantic
    /// timeout. One pending close listener is retained; duplicate closes
    /// coalesce. After actual closure, callbacks run immediately.
    func close(onClosed: @escaping () -> Void) {
        dispatchPrecondition(condition: .onQueue(owner))
        guard !isClosed else {
            if active == nil { onClosed() }
            return
        }
        isClosed = true
        closeCompletion = onClosed
        if active != nil { retainedSelf = self }
        let previous = pending
        pending = nil; cancelPendingTimer()
        if let previous { discard(previous, reason: .cancelled) }
        active?.observer.cancel()
        finishCloseIfClean()
    }
    func cancel(_ local: OriginAttemptIdentity) {
        dispatchPrecondition(condition: .onQueue(owner))
        cancelMatching { $0.local == local }
    }
    func cancelEpoch(destination: UInt64, epoch: UInt64) {
        dispatchPrecondition(condition: .onQueue(owner))
        cancelMatching { $0.local.destination == destination && $0.local.codecEpoch <= epoch }
    }
    func cancelDestination(_ destination: UInt64) {
        dispatchPrecondition(condition: .onQueue(owner))
        cancelMatching { $0.local.destination == destination }
    }
    private func cancelMatching(_ matches: (V4vObservedAttempt) -> Bool) {
        if let previous = pending, matches(previous.attempt) { pending = nil; cancelPendingTimer(); discard(previous, reason: .cancelled) }
        if let active, matches(active.attempt) { active.observer.cancel() }
    }
    private func discard(_ request: V4vObservationRequest, reason: OriginObservationFailure) {
        request.gate.invalidate(attempt: request.attempt)
        request.completed(.failure(reason))
    }
    private func advance() {
        guard !isClosed, active == nil, let request = pending else { return }; pending = nil; cancelPendingTimer()
        let now = clock().monotonicNs
        guard request.deadlineNs > now, request.deadlineNs - now <= 8_000_000_000 else { discard(request, reason: .deadline); return }
        guard request.gate.permits(request.attempt, nowNs: now) else { discard(request, reason: .scope); return }
        do {
            let observer = try request.make()
            active = (request.attempt, observer)
            observer.whenCleanupComplete { [weak self] in
                guard let self else { return }
                self.owner.async { [weak self] in self?.closed(request.attempt) }
            }
            observer.start(deadlineNs: request.deadlineNs, completion: request.completed)
        } catch { discard(request, reason: (error as? OriginObservationFailure) ?? .unavailable) }
    }
    private func closed(_ attempt: V4vObservedAttempt) {
        guard active?.attempt == attempt else { return }
        active = nil
        if isClosed { finishCloseIfClean() } else { advance() }
    }
    private func finishCloseIfClean() {
        guard isClosed, active == nil else { return }
        let callback = closeCompletion; closeCompletion = nil
        retainedSelf = nil
        callback?()
    }
    private func cancelPendingTimer() { pendingTimer?.setEventHandler {}; pendingTimer?.cancel(); pendingTimer = nil }
    private func armPendingDeadline() {
        cancelPendingTimer()
        guard let captured = pending else { return }
        let timer = DispatchSource.makeTimerSource(queue: owner)
        timer.schedule(deadline: DispatchTime(uptimeNanoseconds: captured.deadlineNs))
        timer.setEventHandler { [weak self] in
            guard let self, self.pending?.attempt == captured.attempt else { return }
            self.pending = nil; self.cancelPendingTimer(); self.discard(captured, reason: .deadline)
        }
        pendingTimer = timer; timer.resume()
    }
}
