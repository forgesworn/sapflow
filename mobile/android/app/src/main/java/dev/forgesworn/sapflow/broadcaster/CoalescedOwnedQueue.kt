package dev.forgesworn.sapflow.broadcaster

import java.util.ArrayDeque

/** Immutable handle transfer, with budgets retained until the borrowed owner
 * is actually closed. One coalesced continuation drains at most eight owners.
 * No platform codec buffers or payload copies are retained here. */
internal class CoalescedOwnedQueue<T : AutoCloseable>(
    private val maxCount: Int,
    private val maxBytes: Long,
    private val maxAgeNs: Long,
    private val nowNs: () -> Long,
    private val post: (Runnable) -> Boolean,
    private val consume: (T) -> Unit,
    private val onFailure: (Exception) -> Unit,
    private val onClosedIdle: () -> Unit,
) {
    private data class Entry<T>(val owner: T, val bytes: Long, val admittedNs: Long)
    private val guard = Any()
    private val ready = ArrayDeque<Entry<T>>()
    private var open = true
    private var scheduled = false
    private var count = 0
    private var bytes = 0L
    private var idleNotified = false
    private var highCount = 0
    private var highBytes = 0L
    private var maxDelayNs = 0L
    val outstanding get() = synchronized(guard) { count }
    val diagnostics get() = synchronized(guard) { "$count/$maxCount handles $bytes/${maxBytes}B high=$highCount/${highBytes}B maxDelayUs=${maxDelayNs / 1000}" }
    private val request = Runnable { drain() }
    init { require(maxCount > 0 && maxBytes > 0 && maxAgeNs > 0) }

    /** Transfers ownership even on refusal. The caller must not close again. */
    fun offer(owner: T, size: Long) {
        var admitted = false
        var wake = false
        try {
            synchronized(guard) {
                check(open) { "Encoded audio handoff is closed" }
                require(size > 0 && size <= maxBytes)
                check(count < maxCount && size <= maxBytes - bytes) { "Encoded audio handoff saturated: $count/$maxCount handles $bytes/$maxBytes bytes" }
                ready.addLast(Entry(owner, size, nowNs()))
                count++; bytes += size; admitted = true
                highCount = maxOf(highCount, count); highBytes = maxOf(highBytes, bytes)
                if (!scheduled) { scheduled = true; wake = true }
            }
        } finally { if (!admitted) owner.close() }
        if (wake && !post(request)) { close(drain = false); error("Encoded audio handoff owner stopped") }
    }

    private fun drain() {
        try {
            repeat(8) {
                val entry = synchronized(guard) { ready.pollFirst() } ?: return@repeat
                try {
                    val age = nowNs() - entry.admittedNs
                    synchronized(guard) { maxDelayNs = maxOf(maxDelayNs, age) }
                    check(age in 0..maxAgeNs) { "Encoded audio handoff stale: ageNs=$age limitNs=$maxAgeNs" }
                    consume(entry.owner)
                    val completionAge = nowNs() - entry.admittedNs
                    synchronized(guard) { maxDelayNs = maxOf(maxDelayNs, completionAge) }
                    check(completionAge in 0..maxAgeNs) { "Encoded audio fanout call exceeded residence: ageNs=$completionAge limitNs=$maxAgeNs" }
                } finally { release(entry) }
            }
        } catch (error: Exception) {
            try { close(drain = false) } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
            onFailure(error)
        } finally {
            val again = synchronized(guard) {
                if (ready.isEmpty()) { scheduled = false; false } else true
            }
            if (again && !post(request)) {
                try { close(drain = false) } catch (error: Exception) { onFailure(error) }
                onFailure(IllegalStateException("Encoded audio handoff owner stopped"))
            }
            notifyIdle()
        }
    }

    private fun release(entry: Entry<T>) {
        try { entry.owner.close() }
        finally { synchronized(guard) { count--; bytes -= entry.bytes; check(count >= 0 && bytes >= 0) }; notifyIdle() }
    }

    fun close(drain: Boolean) {
        val discarded = mutableListOf<Entry<T>>()
        synchronized(guard) {
            open = false
            if (!drain) while (ready.isNotEmpty()) discarded.add(ready.removeFirst())
        }
        var failure: Exception? = null
        for (entry in discarded) try { release(entry) }
        catch (error: Exception) { if (failure == null) failure = error else failure.addSuppressed(error) }
        notifyIdle()
        failure?.let { throw it }
    }

    private fun notifyIdle() {
        val notify = synchronized(guard) {
            if (!open && count == 0 && !idleNotified) { idleNotified = true; true } else false
        }
        if (notify) onClosedIdle()
    }
}
