import Foundation

/// Scalar-only admission accounting. Work may complete in a different order
/// from admission; opaque reservations release the correct oldest-time entry.
final class EncodedWorkBudget {
    fileprivate final class Identity {}
    struct Reservation {
        fileprivate let owner: Identity
        fileprivate let id: UInt64
        fileprivate let bytes: Int
        fileprivate let time: Double
    }
    private let lock = NSLock()
    private let identity = Identity()
    private let maxBytes: Int
    private let maxCount: Int
    private let maxSpan: Double
    private var bytes = 0
    private var reservations: [Reservation] = []
    private var nextID: UInt64 = 0
    private var closed = false
    init(bytes: Int = 8 * 1024 * 1024, count: Int = 128, span: Double = 2) {
        maxBytes = bytes; maxCount = count; maxSpan = span
    }
    func reserve(bytes amount: Int, time supplied: Double? = nil) -> Reservation? {
        lock.lock(); defer { lock.unlock() }
        return reserveLocked(bytes: amount, time: supplied)
    }
    private func reserveLocked(bytes amount: Int, time supplied: Double?) -> Reservation? {
        let time = supplied ?? ProcessInfo.processInfo.systemUptime
        guard !closed, amount > 0, amount <= maxBytes - bytes, reservations.count < maxCount,
              time.isFinite, nextID < UInt64.max,
              reservations.first.map({ time >= $0.time && time - $0.time <= maxSpan }) ?? true else { return nil }
        let reservation = Reservation(owner: identity, id: nextID, bytes: amount, time: time)
        nextID += 1; bytes += amount; reservations.append(reservation)
        return reservation
    }
    func release(_ reservation: Reservation) {
        lock.lock(); defer { lock.unlock() }
        guard reservation.owner === identity,
              let index = reservations.firstIndex(where: { $0.id == reservation.id }) else { return }
        bytes -= reservations.remove(at: index).bytes
    }
    /// Admission and actual dispatch share the close lock. A producer cannot
    /// reserve, pause, then submit behind the owner's finish barrier.
    @discardableResult func enqueue(bytes: Int, time: Double? = nil, on queue: DispatchQueue,
                                    execute work: @escaping (Reservation) -> Void) -> Reservation? {
        lock.lock(); defer { lock.unlock() }
        guard let reservation = reserveLocked(bytes: bytes, time: time) else { return nil }
        queue.async { work(reservation) }
        return reservation
    }
    /// The terminal barrier follows every admitted dispatch. Repeated finish
    /// callers still enqueue their joins; a fault uses firstOnly to avoid
    /// turning work rejected after ordinary Stop into a new failure.
    @discardableResult func close(enqueueOn queue: DispatchQueue, firstOnly: Bool = false,
                                  execute work: @escaping () -> Void) -> Bool {
        lock.lock(); defer { lock.unlock() }
        let first = !closed
        closed = true
        if first || !firstOnly { queue.async { work() } }
        return first
    }
    @discardableResult func close() -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return false }
        closed = true
        return true
    }
    var pendingCount: Int { lock.lock(); defer { lock.unlock() }; return reservations.count }
}
