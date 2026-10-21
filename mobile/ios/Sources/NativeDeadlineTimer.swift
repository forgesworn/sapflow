import Foundation

/// One one-shot timer owned by its serial network queue. A nil deadline releases
/// the source; source revisions and caller generations fence queued callbacks.
final class NativeDeadlineTimer {
    private let queue: DispatchQueue
    private var source: DispatchSourceTimer?
    private var revision: UInt64 = 0
    init(queue: DispatchQueue) { self.queue = queue }
    deinit { source?.cancel() }

    func schedule(atNs deadline: UInt64?, generation: UInt64, fire: @escaping (UInt64) -> Void) {
        guard let deadline else { cancel(); return }
        if source == nil {
            revision &+= 1
            let timer = DispatchSource.makeTimerSource(queue: queue)
            source = timer
            timer.schedule(deadline: .distantFuture)
            timer.resume()
        }
        let scheduledRevision = revision
        source?.setEventHandler { [weak self] in
            guard let self, self.source != nil, self.revision == scheduledRevision else { return }
            fire(generation)
        }
        source?.schedule(deadline: DispatchTime(uptimeNanoseconds: deadline), leeway: .microseconds(100))
    }
    func cancel() {
        revision &+= 1
        source?.cancel(); source = nil
    }
}
