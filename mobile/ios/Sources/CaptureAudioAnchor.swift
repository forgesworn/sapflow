import Foundation

/// Actual native I/O receipts belong to the existing control owner. This scalar
/// lifecycle never grants background eligibility or synthesises a close receipt.
struct CaptureAudioAnchor<Owner: Equatable> {
    enum Phase { case ready, starting, active, stopping, closed, retained }
    enum Deactivation { case confirmed, inactiveWithBusy, unknown }
    let owner: Owner
    private(set) var phase = Phase.ready
    private(set) var discarding = false
    private(set) var misuseFault = false
    private var inputStopped = false
    private var callbacksClosed = false
    private var routeClosed = false
    private var sessionInactive = false

    init?(owner: Owner, hasVideo: Bool, optedIn: Bool) {
        guard !hasVideo, optedIn else { return nil }
        self.owner = owner
    }
    mutating func begin(_ value: Owner, exactStart: Bool, permissionGranted: Bool) -> Bool {
        guard value == owner, exactStart, permissionGranted, phase == .ready else { return false }
        phase = .starting; return true
    }
    /// Re-read admission AFTER any held delegate/callback handoff, immediately
    /// before the native start call. The earlier reservation is not a lease to
    /// reopen input after synchronous Stop.
    func permitsNativeStart(_ value: Owner, exactStart: Bool, permissionGranted: Bool) -> Bool {
        value == owner && phase == .starting && exactStart && permissionGranted
    }
    /// Called after actual startRunning/isRunning. A Stop while native start
    /// was withheld retains the newly created I/O for real cleanup.
    mutating func started(_ value: Owner, nativeRunning: Bool, stillCurrent: Bool) -> Bool {
        guard value == owner, phase == .starting || phase == .stopping else { return false }
        guard nativeRunning else { phase = .stopping; return false }
        guard phase == .starting, stillCurrent else { phase = .stopping; return false }
        phase = .active; return true
    }
    mutating func retainForStandby(_ value: Owner, committed: Bool, permissionGranted: Bool) -> Bool {
        guard value == owner, committed, permissionGranted, phase == .active else { return false }
        discarding = true; return true
    }
    mutating func resume(_ value: Owner) -> Bool {
        guard value == owner, phase == .active else { return false }
        discarding = false; return true
    }
    @discardableResult mutating func stop(_ value: Owner) -> Bool {
        guard value == owner, phase != .closed, phase != .retained, phase != .stopping else { return false }
        phase = .stopping; discarding = false; return true
    }
    mutating func stoppedInput(_ value: Owner, confirmed: Bool) {
        guard value == owner, phase == .stopping else { return }
        guard confirmed else { phase = .retained; return }
        inputStopped = true; settle()
    }
    mutating func closedCallbacks(_ value: Owner) {
        guard value == owner, phase == .stopping else { return }
        callbacksClosed = true; settle()
    }
    mutating func closedRoute(_ value: Owner) {
        guard value == owner, phase == .stopping else { return }
        routeClosed = true; settle()
    }
    mutating func deactivated(_ value: Owner, outcome: Deactivation) {
        guard value == owner, phase == .stopping else { return }
        switch outcome {
        case .confirmed: sessionInactive = true
        case .inactiveWithBusy: sessionInactive = true; misuseFault = true
        case .unknown: phase = .retained
        }
        settle()
    }
    private mutating func settle() {
        if phase == .stopping, inputStopped, callbacksClosed, routeClosed, sessionInactive { phase = .closed }
    }
    var cleanupComplete: Bool { phase == .closed }
    var standbyInputActive: Bool { phase == .active && discarding }
}
