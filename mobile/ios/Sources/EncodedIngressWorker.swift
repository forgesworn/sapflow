import Foundation

final class IngressWorker {
    private let queue = DispatchQueue(label: "sapflow.encoded-ingress", qos: .userInitiated)
    private let budget = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
    let ingress: RustEncodedIngress
    private let failure: (Error) -> Void
    private let statistics: (UInt64, String) -> Void
    private var lastStatistics = 0.0
    private var failed = false
    init(ingress: RustEncodedIngress, failure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void) { self.ingress = ingress; self.failure = failure; self.statistics = statistics }
    func offer(_ unit: CompressedUnit) {
        guard let reservation = budget.reserve(bytes: unit.byteCount) else {
            if budget.close() { queue.async { self.fail(CaptureFailure(message: "Encoded SDK ingress exceeded its bounded backlog")) } }
            return
        }
        queue.async {
            defer { self.budget.release(reservation) }
            guard !self.failed else { return }
            do {
                try self.ingress.accept(unit)
                let now = ProcessInfo.processInfo.systemUptime
                if now - self.lastStatistics >= 0.5 {
                    self.lastStatistics = now
                    self.statistics(self.ingress.accepted, self.ingress.status)
                }
            } catch { if self.budget.close() { self.fail(error) } }
        }
    }
    /// A publishing failure closes this worker, leaving the separately owned
    /// recorder running. This also covers a recording epoch after explicit End.
    /// Only expose the native run after start, so its earliest encoded samples
    /// cannot configure a destination before its logical epoch exists.
    @discardableResult func startPublishing(activate: () -> Void = {}) -> Bool {
        defer { activate() }
        do { try ingress.destination?.start(); return true }
        catch {
            if budget.close() { queue.async { self.fail(error) } }
            return false
        }
    }
    private func fail(_ error: Error) {
        guard !failed else { return }
        failed = true
        ingress.destination?.endDestination()
        failure(error)
    }
    func finish(_ completion: @escaping (Result<Void, Error>) -> Void) {
        budget.close()
        queue.async {
            if let destination = self.ingress.destination { destination.finishEpoch(completion: completion) }
            else { completion(.success(())) }
        }
    }
}
