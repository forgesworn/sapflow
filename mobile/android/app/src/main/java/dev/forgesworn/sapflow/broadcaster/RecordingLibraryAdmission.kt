package dev.forgesworn.sapflow.broadcaster

/** Cold local-file work only. Cancel cannot release a queued/running IO owner;
 * only its actual completion does. Picker dismissal owns no file resources. */
internal class RecordingLibraryAdmission {
    enum class Kind { SCAN, PREFLIGHT, EXPORT }
    private enum class Phase { PICKER, QUEUED, RUNNING, RETAINED }
    class Ticket internal constructor(val id: Long, val kind: Kind)
    private var serial = 0L
    private var current: Ticket? = null
    private var phase: Phase? = null
    private var cancelled = false
    private var closed = false
    private var delivery: Ticket? = null

    val idle: Boolean @Synchronized get() = !closed && current == null
    val cleanupRetained: Boolean @Synchronized get() = phase == Phase.RETAINED

    @Synchronized fun reserve(kind: Kind, picker: Boolean = false): Ticket? {
        if (closed || current != null || serial == Long.MAX_VALUE || (picker && kind != Kind.EXPORT)) return null
        return Ticket(++serial, kind).also {
            current = it; phase = if (picker) Phase.PICKER else Phase.QUEUED
            cancelled = false; delivery = null
        }
    }
    @Synchronized fun picked(ticket: Ticket): Boolean {
        if (current !== ticket || phase != Phase.PICKER || closed || cancelled) return false
        phase = Phase.QUEUED
        return true
    }
    @Synchronized fun begin(ticket: Ticket): Boolean {
        if (current !== ticket || phase != Phase.QUEUED || closed || cancelled) return false
        phase = Phase.RUNNING
        return true
    }
    @Synchronized fun shouldCancel(ticket: Ticket): Boolean = current !== ticket || cancelled || closed
    @Synchronized fun cancel(ticket: Ticket) {
        if (current !== ticket) return
        cancelled = true; delivery = null
        if (phase == Phase.PICKER) { current = null; phase = null }
    }
    /** Invoke after actual worker IO/close, including a cancelled queued task
     * that reached its worker and constructed no owners. False cleanup retains
     * capacity; a timeout or a repeated no-op close is not a cleanup receipt. */
    @Synchronized fun finish(ticket: Ticket, cleanupComplete: Boolean): Boolean {
        if (current !== ticket || phase == Phase.PICKER) return false
        if (!cleanupComplete) { phase = Phase.RETAINED; delivery = null; return false }
        val allowed = !cancelled && !closed
        current = null; phase = null
        delivery = if (allowed) ticket else null
        return allowed
    }
    /** A delayed UI delivery cannot replace a newer scan/preparation/export. */
    @Synchronized fun canDeliver(ticket: Ticket): Boolean = !closed && delivery === ticket && current == null
    @Synchronized fun close() {
        closed = true; cancelled = true; delivery = null
        if (phase == Phase.PICKER) { current = null; phase = null }
    }
}
