package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicReference

/** One shared immutable grant state. Cold refresh and hot admission contend
 * with the SAME expiry retirement CAS; a stale read cannot retire a refresh.
 * Valid admission reads only a scalar state and clock, with no monitor or IO. */
internal class V4vGrantExpiry(private val nowNs: () -> Long) {
    data class Ticket(val token: Long, val owner: Long, val deadlineNs: Long)
    private sealed interface State
    private class Active(val identity: V4vIdentity, val ticket: Ticket) : State
    private class Retired(val active: Active, val reported: Boolean = false) : State
    private val current = AtomicReference<State?>()
    private var sequence = 0L // Cold begin/refresh monitor only.
    @Synchronized fun begin(owner: Long, identity: V4vIdentity, deadlineNs: Long): Ticket {
        require(deadlineNs > nowNs()) { "V4V grant expired before capture admission" }
        val ticket = Ticket(Math.addExact(sequence, 1).also { sequence = it }, owner, deadlineNs)
        current.set(Active(identity, ticket))
        return ticket
    }
    @Synchronized fun refresh(identity: V4vIdentity, deadlineNs: Long): Ticket? {
        while (true) {
            val old = current.get() as? Active ?: return null
            if (nowNs() >= old.ticket.deadlineNs || !identity.sameSession(old.identity) ||
                identity.revision < old.identity.revision || deadlineNs < old.ticket.deadlineNs) return null
            val ticket = Ticket(Math.addExact(sequence, 1).also { sequence = it }, old.ticket.owner, deadlineNs)
            if (current.compareAndSet(old, Active(identity, ticket))) return ticket
            // Sample/write expiry may have won. Never revive its retired state.
        }
    }
    /** Synchronous admission is also the timer-independent expiry fence.
     * On failed CAS retry the latest refresh instead of reporting stale expiry. */
    fun admits(owner: Long): Boolean {
        while (true) {
            val active = current.get() as? Active ?: return false
            if (active.ticket.owner != owner) return false
            if (nowNs() < active.ticket.deadlineNs) return true
            if (current.compareAndSet(active, Retired(active))) return false
        }
    }
    fun expired(ticket: Ticket): Long? = takeExpired(ticket)
    fun expiredCurrent(): Long? = takeExpired(null)
    private fun takeExpired(expected: Ticket?): Long? {
        while (true) {
            when (val state = current.get()) {
                null -> return null
                is Active -> {
                    if (expected != null && state.ticket != expected || nowNs() < state.ticket.deadlineNs) return null
                    if (current.compareAndSet(state, Retired(state))) continue
                }
                is Retired -> {
                    if (expected != null && state.active.ticket != expected || state.reported) return null
                    if (current.compareAndSet(state, Retired(state.active, reported = true))) return state.active.ticket.owner
                }
            }
        }
    }
    @Synchronized fun clear() { current.set(null) }
}
