import Foundation

/// One cold recording operation/owner for the admitted capture intent. A Stop
/// fences work immediately, but only the actual worker's cleanup releases it.
/// No callback, filesystem or native operation runs under this scalar lock.
final class RecordingCaptureAdmission {
    struct RotationDeadline {
        let preparation: CaptureAdmission.Preparation
        let deadlineNs: UInt64
        private let beganAtNs: UInt64
        init?(preparation: CaptureAdmission.Preparation, nowNs: UInt64) {
            let (deadline, overflow) = nowNs.addingReportingOverflow(6_000_000_000)
            guard !overflow else { return nil }
            self.preparation = preparation; deadlineNs = deadline; beganAtNs = nowNs
        }
        func permits(_ preparation: CaptureAdmission.Preparation, nowNs: UInt64) -> Bool {
            self.preparation == preparation && nowNs >= beganAtNs && nowNs < deadlineNs
        }
    }
    enum Request: Equatable {
        case preflight(CaptureAdmission.Preparation)
        case segment(CaptureAdmission.Start)
        var preparation: CaptureAdmission.Preparation {
            switch self { case let .preflight(value): return value; case let .segment(value): return value.preparation }
        }
    }
    struct Ticket: Equatable { fileprivate let serial: UInt64; let request: Request }
    private enum Phase { case pending, active, closing, retained }
    private let lock = NSLock()
    private var serial: UInt64 = 0
    private var owner: (ticket: Ticket, phase: Phase, cancelled: Bool, recorder: ObjectIdentifier?)?
    private func locked<T>(_ action: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return action() }
    static func storageAvailable(profile: CaptureIntent, bytes: UInt64?) -> Bool {
        guard let bytes, let budget = try? RecordingStorage.budget(profile: profile) else { return false }
        return bytes > 200 * 1024 * 1024 && bytes >= budget.requiredBytes
    }
    var cleanupRetained: Bool { locked { owner?.phase == .retained } }
    func contains(_ ticket: Ticket) -> Bool { locked { owner?.ticket == ticket } }
    var hasOwner: Bool { locked { owner != nil } }
    var isWorking: Bool { locked { owner.map { $0.phase != .active } ?? false } }
    func begin(_ request: Request) -> Ticket? {
        locked {
            guard owner == nil, serial < UInt64.max else { return nil }
            serial += 1
            let ticket = Ticket(serial: serial, request: request)
            owner = (ticket, .pending, false, nil)
            return ticket
        }
    }
    func bindRecorder(_ ticket: Ticket, recorder: AnyObject) -> Bool {
        locked {
            guard owner?.ticket == ticket, owner?.recorder == nil, owner?.cancelled == false,
                  case .segment = ticket.request else { return false }
            owner?.recorder = ObjectIdentifier(recorder); return true
        }
    }
    func ownsRecorder(_ ticket: Ticket, recorder: AnyObject) -> Bool {
        locked { owner?.ticket == ticket && owner?.recorder == ObjectIdentifier(recorder) }
    }
    func isActive(_ ticket: Ticket) -> Bool {
        locked { owner?.ticket == ticket && owner?.cancelled == false && owner?.phase == .active }
    }
    func permits(_ ticket: Ticket) -> Bool {
        locked { owner?.ticket == ticket && owner?.cancelled == false && owner?.phase == .pending }
    }
    func activate(_ ticket: Ticket) -> Bool {
        locked {
            guard owner?.ticket == ticket, owner?.cancelled == false, owner?.phase == .pending,
                  case .segment = ticket.request else { return false }
            owner?.phase = .active
            return true
        }
    }
    func cancel(_ preparation: CaptureAdmission.Preparation?) {
        locked {
            guard let preparation, owner?.ticket.request.preparation == preparation else { return }
            owner?.cancelled = true; owner?.phase = .closing
        }
    }
    func closing(_ ticket: Ticket) {
        locked { if owner?.ticket == ticket { owner?.phase = .closing } }
    }
    /// True acknowledges actual cleanup, not a timeout/cancel request. A later
    /// real receipt can release this same retained owner, never its successor.
    @discardableResult func closed(_ ticket: Ticket, cleanupComplete: Bool) -> Bool {
        locked {
            guard owner?.ticket == ticket else { return false }
            guard cleanupComplete else { owner?.cancelled = true; owner?.phase = .retained; return false }
            owner = nil
            return true
        }
    }
}

/// Recording-only failure is distinct from an ordinary admission close. Commit
/// holds this scalar lock while the exact run admission makes its own decision.
final class CaptureRecordingLoss {
    private let lock = NSLock()
    private var failed = false, committed = false
    func mark() { lock.lock(); failed = true; lock.unlock() }
    func commit(admission: CaptureAdmission, run: CaptureAdmission.MediaRun, start: CaptureAdmission.Start? = nil) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !failed, !committed, admission.commitRecordingClosure(run, start: start) else { return false }
        committed = true; return true
    }
}

