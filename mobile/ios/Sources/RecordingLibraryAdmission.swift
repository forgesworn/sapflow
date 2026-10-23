import Foundation

/// Cold library commands and capture reservation share this small scalar lock.
/// The only closures run under it read or reserve existing capture admission;
/// they must perform no IO, dispatch, UI work or arbitrary completion callback.
final class RecordingLibraryAdmission {
    enum Kind { case setup, scan, export, cameraScan }
    struct Ticket: Equatable {
        fileprivate let serial: UInt64
        let kind: Kind
    }
    private enum Phase { case picker, queued, running, retained }
    private let lock = NSLock()
    private var serial: UInt64
    private var current: Ticket?
    private var phase: Phase?
    private var cancelled = false
    private var closed = false
    private var delivery: Ticket?

    init(initialSerial: UInt64 = 0) { serial = initialSerial }
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var idle: Bool { locked { !closed && current == nil } }
    var cleanupRetained: Bool { locked { phase == .retained } }
    func owns(_ ticket: Ticket) -> Bool { locked { current == ticket } }

    func withCaptureReservation<T>(_ action: () -> T?) -> T? {
        locked {
            guard !closed, current == nil || (current?.kind == .scan && phase != .retained) else { return nil }
            return action()
        }
    }
    func reserve(_ kind: Kind) -> Ticket? {
        locked {
            guard kind != .export, kind != .cameraScan else { return nil }
            return reserveLocked(kind, phase: .queued)
        }
    }
    func beginExport(canExport: () -> Bool) -> Ticket? {
        locked {
            guard !closed, current == nil, canExport() else { return nil }
            return reserveLocked(.export, phase: .picker)
        }
    }
    /// Unlike catalogue enumeration, a camera scan excludes every capture
    /// reservation. The predicate reads scalar capture/recording ownership only.
    func reserveCameraScan(canReserve: () -> Bool) -> Ticket? {
        locked {
            guard !closed, current == nil, canReserve() else { return nil }
            return reserveLocked(.cameraScan, phase: .queued)
        }
    }
    private func reserveLocked(_ kind: Kind, phase: Phase) -> Ticket? {
        guard !closed, current == nil, serial < UInt64.max else { return nil }
        serial += 1
        let ticket = Ticket(serial: serial, kind: kind)
        current = ticket; self.phase = phase; cancelled = false; delivery = nil
        return ticket
    }
    /// Only actual modal dismissal consumes this reservation. A picker reply or
    /// cancellation request alone does not prove that its UI has disappeared.
    func dismissPicker(_ ticket: Ticket, selected: Bool) -> Bool {
        locked {
            guard current == ticket, phase == .picker else { return false }
            guard !closed, !cancelled, selected else {
                current = nil; phase = nil; delivery = nil; return false
            }
            phase = .queued; return true
        }
    }
    func begin(_ ticket: Ticket) -> Bool {
        locked {
            guard current == ticket, phase == .queued, !cancelled, !closed else { return false }
            phase = .running; return true
        }
    }
    func shouldCancel(_ ticket: Ticket) -> Bool { locked { current != ticket || cancelled || closed } }
    func cancel(_ ticket: Ticket) {
        locked {
            guard current == ticket else { return }
            cancelled = true; delivery = nil
        }
    }
    /// Call only after actual worker IO and checked resource cleanup. A queued
    /// job that acknowledged cancellation without creating owners is clean.
    func finish(_ ticket: Ticket, cleanupComplete: Bool) -> Bool {
        locked {
            guard current == ticket, phase != .picker else { return false }
            guard cleanupComplete else { phase = .retained; delivery = nil; return false }
            let allowed = !closed && !cancelled
            current = nil; phase = nil; delivery = allowed ? ticket : nil
            return allowed
        }
    }
    func canDeliver(_ ticket: Ticket) -> Bool { locked { !closed && current == nil && delivery == ticket } }
    func close() {
        locked {
            closed = true; cancelled = true; delivery = nil
            // Picker dismissal and queued/running/retained owners still need
            // their actual completion. Closing the UI never releases them.
        }
    }
}
