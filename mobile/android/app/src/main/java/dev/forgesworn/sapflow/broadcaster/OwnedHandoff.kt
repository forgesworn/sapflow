package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicReference

/** The sole admitted handle awaiting its worker, with explicit stop cleanup.
 * DrainAdmission prevents another offer until the worker closes its owner. */
internal class OwnedHandoff<T : AutoCloseable> {
    private val value = AtomicReference<T?>()
    fun offer(value: T) {
        if (!this.value.compareAndSet(null, value)) {
            value.close()
            error("Handoff already occupied")
        }
    }
    fun take(): T? = value.getAndSet(null)
    fun peek(): T? = value.get()
    fun cancel() { take()?.close() }
}
