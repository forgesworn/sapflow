package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicReference

/** Preparing a destination has no network side effects. Only the explicit
 * start transition invokes IO setup. These control actions are called on the
 * capture control handler; IO itself runs on owned workers. */
internal class ExplicitAttempt(private val begin: () -> Unit, private val end: () -> Unit) {
    private enum class Phase { PREPARED, STARTED, CLOSED }
    private val phase = AtomicReference(Phase.PREPARED)
    fun start() {
        check(phase.compareAndSet(Phase.PREPARED, Phase.STARTED)) { "Attempt has already started or closed" }
        try { begin() }
        catch (error: Exception) {
            try { close() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
            throw error
        }
    }
    fun close() {
        if (phase.getAndSet(Phase.CLOSED) != Phase.CLOSED) end()
    }
}
