import Foundation

enum ReconnectAction: Equatable {
    case none, connect, retryAt(UInt64), awaitDeadline(UInt64), exhausted, expired, end
}
enum PublishingAttemptEvent { case accepted, sending, peerMediaProgress, failed }
/// Captured on the policy owner before constructing any socket. Never replace
/// these fields with a later SDK snapshot when a decoder finishes.
struct ReconnectAttemptContext: Equatable {
    let codecEpoch: UInt64
    let token: UInt64
    let sdkGeneration: UInt64
    let sdkAttempt: UInt64
}
protocol ReconnectPolicyPort: AnyObject {
    var generation: UInt64 { get }
    var attemptEpoch: UInt64 { get }
    var nextDeadlineNs: UInt64? { get }
    var exhausted: Bool { get }
    func start(_ now: UInt64) throws -> ReconnectAction
    func replace(_ now: UInt64) throws -> ReconnectAction
    func connected(_ now: UInt64) throws
    func sending(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction
    func recovered(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction
    func confirmed(_ context: ReconnectAttemptContext, _ now: UInt64) throws
    func failed(_ now: UInt64) throws -> ReconnectAction
    func tick(_ now: UInt64) throws -> ReconnectAction
    func stop() throws
}
protocol ReconnectAttemptPort: AnyObject {
    func start() throws
    /// Completion means actual platform cancellation and native owner release.
    /// It must not mean cancel() was merely requested.
    func close(completion: @escaping () -> Void)
}

/// Caller-driven serial control owner. No media, socket, file, timer or SDK
/// runtime lives here. A single old attempt is retained until actual cleanup.
final class ReconnectController {
    typealias Factory = (ReconnectAttemptContext, @escaping (PublishingAttemptEvent, UInt64) -> Void) throws -> ReconnectAttemptPort
    private let policy: ReconnectPolicyPort
    private let now: () -> UInt64
    private let changed: () -> Void
    private let attemptReady: (ReconnectAttemptContext, UInt64) -> Void
    private let activeChanged: (ReconnectAttemptPort?) -> Void
    private var factory: Factory?
    private var owner: ReconnectAttemptPort?
    private var token: UInt64 = 0
    private var policyAttempt: UInt64 = 0
    private var epoch: UInt64?
    private var started = false
    private var terminal = false
    private var stopped = false
    private var closing = false
    private var pendingConnect = false
    private var cleanupDeadline: UInt64?
    private var detachCompletion: ((Result<Void, Error>) -> Void)?
    private var accepted = false
    private var startup = false
    private var peerProgress = false
    private var sendingApplied = false
    private var recoveryApplied = false
    private var readyApplied = false
    private var decodedApplied = false
    private var context: ReconnectAttemptContext?
    private(set) var status = "Destination prepared · explicit Start required"
    var cleanupComplete: Bool { owner == nil && !closing }
    var nextDeadlineNs: UInt64? {
        [terminal || stopped ? nil : policy.nextDeadlineNs, cleanupDeadline].compactMap { $0 }.min()
    }
    init(policy: ReconnectPolicyPort, now: @escaping () -> UInt64,
         activeChanged: @escaping (ReconnectAttemptPort?) -> Void, changed: @escaping () -> Void,
         attemptReady: @escaping (ReconnectAttemptContext, UInt64) -> Void = { _, _ in }) {
        self.policy = policy; self.now = now; self.activeChanged = activeChanged; self.changed = changed; self.attemptReady = attemptReady
    }
    /// Compatibility entry for generic destinations that do not observe output.
    func attach(epoch: UInt64, factory: @escaping (@escaping (PublishingAttemptEvent, UInt64) -> Void) throws -> ReconnectAttemptPort) {
        attach(epoch: epoch, contextualFactory: { _, observe in try factory(observe) })
    }
    func attach(epoch: UInt64, contextualFactory factory: @escaping Factory) {
        guard !stopped, self.epoch == nil, cleanupComplete else { return }
        self.epoch = epoch
        if terminal { changed(); return } // recording-only epoch after publishing ended
        self.factory = factory
        do {
            if !started { started = true; try apply(policy.start(now())) }
            else if pendingConnect { try open() }
        } catch { failClosed("Publisher policy failed · local recording continues") }
        changed()
    }
    func detach(epoch: UInt64, completion: @escaping (Result<Void, Error>) -> Void) {
        guard self.epoch == epoch else {
            completion(cleanupComplete && (stopped || terminal) ? .success(()) : .failure(ReconnectFailure.invalidEpoch)); return
        }
        guard detachCompletion == nil else { completion(.failure(ReconnectFailure.cleanupRetained)); return }
        self.epoch = nil; factory = nil; decodedApplied = false
        status = "Publisher epoch closing · playback unconfirmed"
        detachCompletion = completion
        activeChanged(nil)
        if owner != nil { beginClose() }
        else { detached() }
        changed()
    }
    func stop() {
        guard !stopped else { return }
        stopped = true; terminal = true; pendingConnect = false; factory = nil
        activeChanged(nil)
        try? policy.stop()
        status = "Publisher stopping · playback unconfirmed"
        if owner != nil { beginClose() } else { detached() }
        changed()
    }
    func tick() {
        let instant = now()
        if let deadline = cleanupDeadline, instant >= deadline {
            cleanupDeadline = nil
            failClosed("Publisher cleanup unconfirmed; old owner retained; Prepare is blocked")
            finishDetach(.failure(ReconnectFailure.cleanupRetained))
            changed(); return
        }
        guard !terminal, !stopped else { return }
        do { try apply(policy.tick(instant)) }
        catch { failClosed("Publisher policy clock/state failed · local recording continues") }
        changed()
    }
    private func open() throws {
        guard !terminal, !stopped, owner == nil, !closing, epoch != nil, let factory else { pendingConnect = true; return }
        // An actual cancellation callback can arrive after the native deadline
        // but before its queued timer. Revalidate before constructing any owner.
        let instant = now()
        if let deadline = policy.nextDeadlineNs, instant >= deadline {
            pendingConnect = false
            try apply(policy.tick(instant))
            return
        }
        guard token < UInt64.max else { throw ReconnectFailure.invalidEpoch }
        token += 1; let current = token
        policyAttempt = policy.attemptEpoch
        accepted = false; startup = false; peerProgress = false; sendingApplied = false; recoveryApplied = false
        readyApplied = false; decodedApplied = false
        guard let epoch, policy.generation > 0, policyAttempt > 0 else { throw ReconnectFailure.invalidEpoch }
        let captured = ReconnectAttemptContext(codecEpoch: epoch, token: current,
            sdkGeneration: policy.generation, sdkAttempt: policyAttempt)
        context = captured
        pendingConnect = false
        let attempt = try factory(captured, { [weak self] event, time in self?.event(current, event, time) })
        owner = attempt; activeChanged(attempt)
        status = "Connecting · decoded playback unconfirmed"
        do { try attempt.start() }
        catch { event(current, .failed, now()) }
    }
    private func event(_ current: UInt64, _ event: PublishingAttemptEvent, _ observedAt: UInt64) {
        guard current == token, owner != nil, !closing, !terminal, !stopped else { return }
        // Timers and observations use this serial owner's monotonic clock.
        // A queued callback's older stamp cannot roll the SDK clock backwards.
        let instant = now()
        do {
            switch event {
            case .accepted:
                if !accepted { try policy.connected(instant); accepted = true }
            case .sending: startup = true
            case .peerMediaProgress: startup = true; peerProgress = true
            case .failed:
                // Start/retain the real failure window BEFORE asynchronous close.
                let action = try policy.failed(instant)
                activeChanged(nil)
                beginClose()
                try apply(action)
                changed(); return
            }
            if accepted && startup && !sendingApplied {
                sendingApplied = true
                try apply(policy.sending(policyAttempt, instant))
            }
            if accepted && sendingApplied && !readyApplied && !terminal, let context {
                readyApplied = true
                attemptReady(context, instant)
            }
            if accepted && sendingApplied && peerProgress && !recoveryApplied && !policy.exhausted && !terminal {
                recoveryApplied = true
                try apply(policy.recovered(policyAttempt, instant))
            }
            if !terminal && !decodedApplied {
                status = !accepted ? "Connecting · decoded playback unconfirmed" : policy.exhausted ? "Sending · peer progress unverified · automatic retries exhausted" :
                    (recoveryApplied ? "Sending · peer transport progress observed · decoded playback unconfirmed" :
                        (sendingApplied ? "Sending · peer progress and decoded playback unconfirmed" : "Destination accepted · waiting for complete startup media"))
            }
        } catch { failClosed("Publisher policy failed · local recording continues") }
        changed()
    }
    /// Must run on the same serial owner as policy events. The app's cold
    /// confirmation gate must linearise this call with synchronous invalidation.
    func confirmObservedMedia(_ captured: ReconnectAttemptContext, nowNs: UInt64) throws {
        guard context == captured, owner != nil, epoch == captured.codecEpoch,
              !closing, !terminal, !stopped, accepted, sendingApplied, readyApplied,
              !decodedApplied else { throw ReconnectFailure.invalidEpoch }
        try policy.confirmed(captured, nowNs)
        decodedApplied = true
        status = "Live · native decoded playback verified"
        changed()
    }
    private func apply(_ action: ReconnectAction) throws {
        switch action {
        case .none: break
        case .connect: pendingConnect = true; try open()
        case .retryAt: status = "Uplink interrupted · retry scheduled · local recording continues"
        case .awaitDeadline: status = "Uplink interrupted · awaiting original retry deadline"
        case .exhausted: status = "Sending · peer progress unverified · automatic retries exhausted"
        case .expired, .end: failClosed("Publishing stopped · automatic retry budget ended · local recording continues")
        }
    }
    private func beginClose() {
        guard !closing, let owner else { return }
        closing = true
        let deadline = now().addingReportingOverflow(4_000_000_000)
        cleanupDeadline = deadline.overflow ? UInt64.max : deadline.partialValue
        let current = token
        owner.close { [weak self] in self?.closed(current) }
    }
    private func closed(_ current: UInt64) {
        guard current == token, closing else { return }
        owner = nil; closing = false; cleanupDeadline = nil
        if epoch == nil { detached() }
        else if !terminal && !stopped && pendingConnect {
            do { try open() } catch { failClosed("Publisher restart failed · local recording continues") }
        }
        changed()
    }
    private func detached() {
        if !terminal && !stopped && started {
            do { try apply(policy.replace(now())) }
            catch { failClosed("Publisher epoch replacement failed · local recording continues") }
        }
        finishDetach(.success(()))
    }
    private func finishDetach(_ result: Result<Void, Error>) {
        let completion = detachCompletion; detachCompletion = nil; completion?(result)
    }
    private func failClosed(_ message: String) {
        terminal = true; pendingConnect = false; factory = nil
        activeChanged(nil); try? policy.stop(); status = message
        if owner != nil { beginClose() }
    }
}
enum ReconnectFailure: Error { case invalidEpoch, cleanupRetained }

/// Serialises only a cold bounded Start enqueue with a synchronous Stop fence.
/// Socket/file/FFI work must remain outside the supplied admission closure.
final class AttemptStartFence {
    private let lock = NSLock()
    private var closed = false
    func start(_ admit: () throws -> Void) throws {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { throw ReconnectFailure.invalidEpoch }
        try admit()
    }
    func close(_ cancelAdmission: () -> Void) {
        lock.lock(); closed = true; lock.unlock()
        cancelAdmission()
    }
}

/// Scalar one-shot facts shared by fused media results and bounded cold drains.
/// No callback/packet history or payload is retained.
final class PublishingProgressLatch {
    private var sending = false
    private var peer = false
    func observe(sending: Bool, peer: Bool, notify: (PublishingAttemptEvent) -> Void) {
        if (sending || peer) && !self.sending { self.sending = true; notify(.sending) }
        if peer && !self.peer { self.peer = true; notify(.peerMediaProgress) }
    }
    func drain(next: () throws -> PublishingAttemptEvent?, notify: (PublishingAttemptEvent) -> Void) rethrows {
        for _ in 0..<4 {
            guard let event = try next() else { return }
            switch event {
            case .sending: observe(sending: true, peer: false, notify: notify)
            case .peerMediaProgress: observe(sending: true, peer: true, notify: notify)
            default: break
            }
        }
    }
}
