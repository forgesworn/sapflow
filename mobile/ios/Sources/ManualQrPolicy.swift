import Foundation

/// Scalar scan ownership. Permission, native work, parsing and callbacks never
/// run under this lock. Cancellation closes results, not resource ownership.
final class ManualQrPolicy {
    struct Ticket: Equatable { fileprivate let serial: UInt64 }
    struct Completion { let ticket: Ticket; let delivery: ManualQrDelivery? }
    static let maximumBytes = 2048
    static let lifetimeNs: UInt64 = 60_000_000_000
    private struct State {
        let ticket: Ticket, began: UInt64, deadline: UInt64
        var closing = false, opened = false, nativeClosed = false, modalClaimed = false, modalClosed = false
        var permissionPending = false, permissionIssued = false
        var candidate: String?
    }
    /// Called when the exact sheet binding first grants presentation. If it
    /// never granted it, cancellation can prove there was no modal owner.
    func claimModal(_ ticket: Ticket) -> Bool {
        locked {
            guard var value = state, value.ticket == ticket else { return false }
            if value.modalClaimed { return true }
            guard !value.closing else { return false }
            value.modalClaimed = true; state = value; return true
        }
    }
    private let lock = NSLock()
    private var serial: UInt64 = 0
    private var state: State?
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    func begin(now: UInt64) -> Ticket? {
        locked {
            guard state == nil, serial < UInt64.max, now <= UInt64.max - Self.lifetimeNs else { return nil }
            serial += 1
            let ticket = Ticket(serial: serial)
            state = State(ticket: ticket, began: now, deadline: now + Self.lifetimeNs)
            return ticket
        }
    }
    func beginPermission(_ ticket: Ticket, now: UInt64) -> Bool {
        locked {
            guard var value = state, value.ticket == ticket, value.modalClaimed,
                  !value.closing, !value.opened, !value.permissionIssued,
                  now >= value.began, now < value.deadline else { return false }
            value.permissionPending = true; value.permissionIssued = true; state = value; return true
        }
    }
    /// An exact delivered OS reply, including cancellation/denial. It is not
    /// camera permission to open; claimOpen still checks the current lifetime.
    func permissionReturned(_ ticket: Ticket) -> Bool {
        locked {
            guard var value = state, value.ticket == ticket, value.permissionPending else { return false }
            value.permissionPending = false; state = value; return true
        }
    }
    func claimOpen(_ ticket: Ticket, now: UInt64) -> Bool {
        locked {
            guard var value = state, value.ticket == ticket, value.modalClaimed,
                  !value.permissionPending, !value.closing, !value.opened else { return false }
            guard now < value.deadline else {
                value.closing = true; value.nativeClosed = true; state = value; return false
            }
            value.opened = true; state = value; return true
        }
    }
    func permits(_ ticket: Ticket, now: UInt64) -> Bool {
        locked { state.map { $0.ticket == ticket && !$0.closing && now < $0.deadline } ?? false }
    }
    /// Call only after bounded canonical envelope AND profile validation.
    func candidate(_ text: String, ticket: Ticket, now: UInt64) -> Bool {
        guard Self.boundedASCII(text) else { return false }
        return locked {
            guard var value = state, value.ticket == ticket, value.opened,
                  !value.closing, now < value.deadline else { return false }
            value.candidate = text; value.closing = true; state = value; return true
        }
    }
    func cancel(_ ticket: Ticket) {
        locked {
            guard var value = state, value.ticket == ticket else { return }
            value.closing = true; value.candidate = nil
            if !value.opened { value.nativeClosed = true }
            if !value.modalClaimed { value.modalClosed = true }
            state = value
        }
    }
    func expire(_ ticket: Ticket, now: UInt64) -> Bool {
        locked {
            guard var value = state, value.ticket == ticket, !value.closing, now >= value.deadline else { return false }
            value.closing = true; value.candidate = nil
            if !value.opened { value.nativeClosed = true }
            if !value.modalClaimed { value.modalClosed = true }
            state = value; return true
        }
    }
    func nativeClosed(_ ticket: Ticket, clean: Bool) {
        locked {
            guard var value = state, value.ticket == ticket, value.closing else { return }
            value.nativeClosed = clean
            if !clean { value.candidate = nil }
            state = value
        }
    }
    func modalClosed(_ ticket: Ticket) {
        locked {
            guard var value = state, value.ticket == ticket else { return }
            value.modalClosed = true; state = value
        }
    }
    func takeCompletion(_ ticket: Ticket, now: UInt64) -> Completion? {
        locked {
            guard let value = state, value.ticket == ticket, value.closing,
                  value.nativeClosed, value.modalClosed, !value.permissionPending else { return nil }
            state = nil
            let delivery = value.candidate.flatMap { text in
                now >= value.began && now < value.deadline ?
                    ManualQrDelivery(text: text, beganNs: value.began, deadlineNs: value.deadline) : nil
            }
            return Completion(ticket: ticket, delivery: delivery)
        }
    }
    static func boundedASCII(_ text: String) -> Bool {
        let bytes = text.utf8.prefix(maximumBytes + 1)
        return !bytes.isEmpty && bytes.count <= maximumBytes && bytes.allSatisfy { (33...126).contains($0) }
    }
    static func validatedCandidate(_ text: String) -> Bool {
        guard boundedASCII(text) else { return false }
        do { _ = try ImportedManualDestination(ManualDestinationEnvelope(text).document); return true }
        catch { return false }
    }
}

/// Actual request/reply ownership shared by the platform adapter and tests.
/// Even a cancelled request retains capacity through its original OS callback
/// and the receiver queue. Neither cancellation nor a timer fabricates a reply.
enum ManualQrPermissionRequest {
    static func begin(policy: ManualQrPolicy, ticket: ManualQrPolicy.Ticket, now: UInt64,
                      request: (@escaping (Bool) -> Void) -> Void,
                      deliver: @escaping (@escaping () -> Void) -> Void,
                      reply: @escaping (Bool) -> Void) -> Bool {
        guard policy.beginPermission(ticket, now: now) else { return false }
        request { granted in
            deliver {
                guard policy.permissionReturned(ticket) else { return }
                reply(granted)
            }
        }
        return true
    }
}

/// Cold native cleanup composition, shared with the camera adapter. A callback
/// barrier is mandatory even after stop succeeds; no view/timer fakes it.
enum ManualQrCleanup {
    static func close(stop: () -> Bool, detach: () -> Void,
                      afterCallbacks: (@escaping () -> Void) -> Void,
                      release: @escaping () -> Void, complete: @escaping (Bool) -> Void) {
        let clean = stop()
        detach()
        afterCallbacks {
            if clean { release() }
            complete(clean)
        }
    }
}
