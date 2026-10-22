import Foundation
import Darwin
#if os(iOS)
import AVFAudio
#endif

/// One instance per logical Prepare. Default resolves once, not once per codec epoch.
final class AudioInputPin {
    private let lock = NSLock()
    private var id: String?
    init(requestedID: String?) { id = requestedID }
    var resolvedID: String? { lock.lock(); defer { lock.unlock() }; return id }
    func accept(_ actual: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard id == nil || id == actual else { return false }; id = actual; return true
    }
}

struct AudioInputOwner: Equatable {
    let preparation: CaptureAdmission.Preparation
    let epoch: UInt64
}

/// Route getters run only at activation and route events. The media
/// path subsequently reads the policy's scalar gate, never the audio session.
final class AudioInputRouting {
    struct Failure: LocalizedError { let errorDescription: String? }
    let owner: AudioInputOwner
    private let policy: AudioInputPolicy<AudioInputOwner>
    private let pin: AudioInputPin
    private let read: () -> String?
    private let afterWait: () -> Void
    private var activeWaiters = 0
    private let beforeRead: () -> Void
    private let now: () -> UInt64
    private var oldestWaitStart: UInt64?
    private var settledAt: UInt64?
    private struct Verification { let revision: UInt64; let initial: Bool }
    private let failure: () -> Void
    private let changed: (String) -> Void
    private let lock = AudioRouteCondition()
    private var checking = false
    private var closed = false
    private var verification: UInt64 = 0
    private var armed = false
    private var activating = false
    private var pendingRoute = false
    private var verified = false
    private var failed = false
    private var failureText = "Microphone route changed, disconnected or is unsupported"
    private var notification: NSObjectProtocol?

    init(owner: AudioInputOwner, pin: AudioInputPin, available: [AudioInputOption],
         read: @escaping () -> String?, failure: @escaping () -> Void, changed: @escaping (String) -> Void,
         beforeRead: @escaping () -> Void = {}, afterWait: @escaping () -> Void = {}, now: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }) throws {
        guard let policy = AudioInputPolicy(owner: owner, requestedID: pin.resolvedID, available: available) else {
            throw Failure(errorDescription: "Selected microphone is unavailable. Stop and choose an input before Prepare.")
        }
        self.owner = owner; self.pin = pin; self.policy = policy
        self.read = read; self.failure = failure; self.changed = changed; self.beforeRead = beforeRead; self.afterWait = afterWait; self.now = now
    }
    /// Capture configuration may stop/change a video session. Finish it before
    /// constructing the new route owner, then verify only after capture starts.
    static func activate(configure: () throws -> Void, attach: () throws -> AudioInputRouting,
                         start: () throws -> Void) throws -> AudioInputRouting {
        try configure()
        let route = try attach()
        try start()
        guard route.verify() else { throw Failure(errorDescription: "Actual microphone route is unavailable, changed or unsupported; use built-in, wired or USB input") }
        return route
    }
    #if os(iOS)
    static func inventory(_ session: AVAudioSession = .sharedInstance()) -> [AudioInputOption] {
        AudioInputOption.inventory(Array((session.availableInputs ?? []).prefix(64)).compactMap { port in
            let kind: AudioInputOption.Kind
            switch port.portType {
            case .builtInMic: kind = .builtIn
            case .headsetMic, .lineIn: kind = .wired
            case .usbAudio: kind = .usb
            default: return nil
            }
            return AudioInputOption(id: port.uid, name: port.portName, kind: kind)
        })
    }
    static func attach(owner: AudioInputOwner, pin: AudioInputPin,
                       failure: @escaping () -> Void, changed: @escaping (String) -> Void) throws -> AudioInputRouting {
        let session = AVAudioSession.sharedInstance()
        let inventory = inventory(session)
        let preferred = pin.resolvedID
        let port = preferred.flatMap { id in session.availableInputs?.prefix(64).first { $0.uid == id } }
        if preferred != nil && port == nil { throw Failure(errorDescription: "Selected microphone disconnected before Prepare.") }
        try session.setPreferredInput(port) // nil explicitly restores System Default.
        let route = try AudioInputRouting(owner: owner, pin: pin, available: inventory, read: {
            let inputs = session.currentRoute.inputs
            return inputs.count == 1 ? inputs.first?.uid : nil
        }, failure: failure, changed: changed)
        route.notification = NotificationCenter.default.addObserver(forName: AVAudioSession.routeChangeNotification,
            object: session, queue: nil) { [weak route] _ in route?.routeChanged() }
        return route
    }
    #endif
    /// A notification is a reason to check the actual input, not proof it changed.
    private func reserveLocked() -> Verification? {
        guard !closed, !failed, armed || !activating else { return nil }
        let initial = !armed
        if initial { activating = true }
        guard verification < UInt64.max else { failLocked(); return nil }
        verification += 1
        checking = true
        return Verification(revision: verification, initial: initial)
    }
    func routeChanged() {
        lock.lock()
        guard !closed, !failed else { lock.unlock(); return }
        guard armed else { pendingRoute = true; lock.unlock(); return }
        // Reservation closes PCM admission before the first event-lock release.
        let reservation = reserveLocked(); let notifyFailure = failed
        lock.unlock()
        if notifyFailure { failure() }
        if let reservation { _ = perform(reservation) }
    }
    @discardableResult func verify() -> Bool {
        lock.lock()
        let wasFailed = failed
        let reservation = reserveLocked(); let notifyFailure = failed && !wasFailed
        lock.unlock()
        if notifyFailure { failure() }
        guard let reservation else { return false }
        return perform(reservation)
    }
    private func perform(_ reservation: Verification) -> Bool {
        let revision = reservation.revision, initial = reservation.initial
        // Activation may coalesce one pending native event into a second read.
        // Repeated instability refuses admission rather than polling or looping.
        for pass in 0..<2 {
            beforeRead() // Cold dependency handoff, outside the state lock.
            let actual = read()
            lock.lock()
            guard !closed, !failed, revision == verification else { lock.unlock(); return false }
            if initial && pendingRoute {
                pendingRoute = false
                if pass == 0 { lock.unlock(); continue }
                failLocked(); lock.unlock(); failure(); return false
            }
            let settlement = now()
            if let started = oldestWaitStart, settlement < started || settlement - started >= 250_000_000 {
                failLocked("Microphone route verification exceeded 250 ms"); lock.unlock(); failure(); return false
            }
            guard let actual, policy.observe(owner: owner, id: actual) == .accepted, pin.accept(actual) else {
                failLocked(); lock.unlock(); failure(); return false
            }
            verified = true; armed = true; activating = false; checking = false
            settledAt = settlement
            lock.broadcast(); lock.unlock()
            changed(actual)
            return true
        }
        return false
    }
    var failureDescription: String { lock.lock(); defer { lock.unlock() }; return failureText }
    private func failLocked(_ reason: String? = nil) {
        if let reason { failureText = reason }
        failed = true; checking = false; policy.close(owner: owner); lock.broadcast()
    }
    func permitsPCM() -> Bool {
        lock.lock(); defer { lock.unlock() }
        return verified && !checking && !closed && !failed && policy.permitsMedia(owner: owner)
    }
    /// Normal path is scalar-only. A route transition alone may wait on the
    /// existing sample owner, at most 250 ms measured by the monotonic clock.
    func awaitPCM() -> Bool {
        lock.lock()
        if !checking {
            let allowed = verified && !closed && !failed && policy.permitsMedia(owner: owner)
            lock.unlock(); return allowed
        }
        guard activeWaiters < 1 else {
            failLocked("Microphone route wait capacity exceeded"); lock.unlock(); failure(); return false
        }
        activeWaiters += 1
        let started = now()
        if oldestWaitStart == nil { oldestWaitStart = started }
        let deadline = started.addingReportingOverflow(250_000_000)
        var timeoutFailure = false
        while checking && !closed && !failed {
            let observed = now()
            if deadline.overflow || observed < started || observed >= deadline.partialValue {
                failLocked("Microphone route verification exceeded 250 ms"); timeoutFailure = true; break
            }
            lock.wait(nanoseconds: deadline.partialValue - observed)
            lock.unlock(); afterWait(); lock.lock()
        }
        if !closed && !failed, let settledAt, settledAt < started || deadline.overflow || settledAt >= deadline.partialValue {
            failLocked("Microphone route verification exceeded 250 ms"); timeoutFailure = true
        }
        let allowed = verified && !closed && !failed && policy.permitsMedia(owner: owner)
        activeWaiters -= 1
        if activeWaiters == 0 { oldestWaitStart = nil }
        lock.unlock()
        if timeoutFailure { failure() }
        return allowed
    }
    /// Stop consumes a fault already observed by the native callback even when
    /// that callback's queued control notification has not run yet.
    @discardableResult func close() -> Bool {
        lock.lock(); closed = true; checking = false; policy.close(owner: owner); lock.broadcast()
        let fault = failed
        let token = notification; notification = nil; lock.unlock()
        if let token { NotificationCenter.default.removeObserver(token) }
        return fault
    }
    deinit { if let notification { NotificationCenter.default.removeObserver(notification) } }
}

/// Darwin's relative condition wait keeps the 250 ms budget independent of wall
/// clock changes. One mutex/condition belongs to the cold native route owner.
private final class AudioRouteCondition {
    private var mutex = pthread_mutex_t()
    private var condition = pthread_cond_t()
    init() {
        precondition(pthread_mutex_init(&mutex, nil) == 0)
        precondition(pthread_cond_init(&condition, nil) == 0)
    }
    func lock() { pthread_mutex_lock(&mutex) }
    func unlock() { pthread_mutex_unlock(&mutex) }
    func broadcast() { pthread_cond_broadcast(&condition) }
    func wait(nanoseconds: UInt64) {
        var relative = timespec(tv_sec: Int(nanoseconds / 1_000_000_000), tv_nsec: Int(nanoseconds % 1_000_000_000))
        _ = pthread_cond_timedwait_relative_np(&condition, &mutex, &relative)
    }
    deinit { pthread_cond_destroy(&condition); pthread_mutex_destroy(&mutex) }
}
