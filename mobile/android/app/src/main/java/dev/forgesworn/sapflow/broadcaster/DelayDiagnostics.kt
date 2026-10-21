package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicLong

/** Fixed-size counters only; strings are rendered by the 5Hz UI snapshot. */
internal class DelayDiagnostics {
    private val maximumNs = AtomicLong()
    private val slow = AtomicLong()
    private val count = AtomicLong()
    private val totalNs = AtomicLong()
    fun record(durationNs: Long) {
        totalNs.addAndGet(durationNs)
        count.incrementAndGet()
        if (durationNs >= 100_000_000L) slow.incrementAndGet()
        var previous = maximumNs.get()
        while (durationNs > previous && !maximumNs.compareAndSet(previous, durationNs)) previous = maximumNs.get()
    }
    fun text(): String {
        val samples = count.get()
        return "n $samples avg ${totalNs.get() / maxOf(1, samples) / 1000}us max ${maximumNs.get() / 1_000_000}ms/slow ${slow.get()}"
    }
}
