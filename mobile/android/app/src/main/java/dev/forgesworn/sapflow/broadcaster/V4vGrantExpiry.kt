package dev.forgesworn.sapflow.broadcaster

/** A metadata/HTTP stall cannot delay this independent publisher admission fence.
 * The platform owns the one alarm; this pure policy fences stale alarm deliveries. */
internal class V4vGrantExpiry(private val nowNs: () -> Long) {
    data class Ticket(val token: Long, val owner: Long, val deadlineNs: Long)
    private data class Active(val identity: V4vIdentity, val ticket: Ticket)
    private var current: Active? = null
    private var sequence = 0L
    @Synchronized fun begin(owner: Long, identity: V4vIdentity, deadlineNs: Long): Ticket {
        require(deadlineNs > nowNs()) { "V4V grant expired before capture admission" }
        return Ticket(++sequence, owner, deadlineNs).also { current = Active(identity, it) }
    }
    @Synchronized fun refresh(identity: V4vIdentity, deadlineNs: Long): Ticket? {
        val old = current ?: return null
        // Late commit cannot revive a destination whose old admission already expired.
        if (nowNs() >= old.ticket.deadlineNs || !identity.sameSession(old.identity) || identity.revision < old.identity.revision || deadlineNs < old.ticket.deadlineNs) return null
        return Ticket(++sequence, old.ticket.owner, deadlineNs).also { current = Active(identity, it) }
    }
    @Synchronized fun expired(ticket: Ticket): Long? {
        val active = current ?: return null
        if (active.ticket != ticket || nowNs() < ticket.deadlineNs) return null
        current = null
        return ticket.owner
    }
    /** Synchronous start admission never depends on alarm scheduling latency. */
    @Synchronized fun admits(owner: Long): Boolean = current?.let {
        it.ticket.owner == owner && nowNs() < it.ticket.deadlineNs
    } == true
    /** The owner lock uses this before accepting refreshed approvals; stale
     * scheduled alarms still use expired(ticket) and cannot affect a new owner. */
    @Synchronized fun expiredCurrent(): Long? {
        val active = current ?: return null
        return expired(active.ticket)
    }
    @Synchronized fun clear() { current = null }
}
