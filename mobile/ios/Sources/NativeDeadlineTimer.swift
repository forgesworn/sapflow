import Foundation

/// One one-shot timer owned by its serial network queue. A nil deadline releases
/// the source; source revisions and caller generations fence queued callbacks.
final class NativeDeadlineTimer {
    private let queue: DispatchQueue
    private let now: () -> UInt64
    private var source: DispatchSourceTimer?
    private var revision: UInt64 = 0
    init(queue: DispatchQueue, now: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }) { self.queue = queue; self.now = now }
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
            guard self.now() >= deadline else {
                self.schedule(atNs: deadline, generation: generation, fire: fire); return
            }
            fire(generation)
        }
        let instant = now()
        let remaining = AudioShowLifecycle.remaining(deadline: deadline, now: instant)
        source?.schedule(deadline: .now() + .nanoseconds(Int(min(remaining, UInt64(Int.max)))), leeway: .microseconds(100))
    }
    func cancel() {
        revision &+= 1
        source?.cancel(); source = nil
    }
}
