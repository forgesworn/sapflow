package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean

/** One popped sample owner and one coalesced control wake-up. Completion is
 * published by the worker, so ingress need not wait for a queued handler wake. */
internal class DrainAdmission {
    private val inFlight = AtomicBoolean(false)
    private val continuation = AtomicBoolean(false)
    private val closed = AtomicBoolean(false)

    fun claim(): Boolean {
        if (closed.get() || !inFlight.compareAndSet(false, true)) return false
        if (!closed.get()) return true
        inFlight.set(false)
        return false
    }

    /** Caller has released its sample. True grants one handler wake-up. */
    fun complete(): Boolean {
        inFlight.set(false)
        return !closed.get() && continuation.compareAndSet(false, true)
    }

    /** Empty queue: release admission without generating an idle wake loop. */
    fun empty() { inFlight.set(false) }
    fun resumed() { continuation.set(false) }
    fun close() { closed.set(true) }
}
