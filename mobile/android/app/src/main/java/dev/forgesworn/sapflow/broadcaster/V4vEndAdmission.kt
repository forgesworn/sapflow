package dev.forgesworn.sapflow.broadcaster

/** Cold control-only ownership. Tickets carry no event, credential or guessed SID.
 * There is one offered prompt, one confirmed claim and one replacement transition.
 * No callback, journal operation or platform work runs while this monitor is held. */
internal class V4vEndAdmission(initialEligible: Boolean) {
    internal class Ticket internal constructor()
    internal class Claim internal constructor()
    internal class Replacement internal constructor()
    private var owner: Any? = if (initialEligible) Any() else null
    private var offered: Ticket? = null
    private var offeredOwner: Any? = null
    private var claim: Claim? = null
    private var claimedOwner: Any? = null
    private var replacement: Replacement? = null
    private var closed = false

    @Synchronized fun invitation(): Ticket? {
        if (closed || owner == null || claim != null || replacement != null) return null
        return Ticket().also { offered = it; offeredOwner = owner }
    }
    @Synchronized fun cancel(ticket: Ticket) {
        if (offered === ticket) { offered = null; offeredOwner = null }
    }
    @Synchronized fun confirm(ticket: Ticket): Claim? {
        if (closed || owner == null || offered !== ticket || offeredOwner !== owner || claim != null) return null
        offered = null; offeredOwner = null
        return Claim().also { claim = it; claimedOwner = owner }
    }
    @Synchronized fun current(value: Claim): Boolean =
        !closed && claim === value && owner != null && owner === claimedOwner
    @Synchronized fun ending(): Boolean = claim != null
    @Synchronized fun complete(value: Claim) {
        if (claim === value) { claim = null; claimedOwner = null }
    }
    /** Reserve BEFORE a replacement's first GET. An End claim and a successor
     * cannot both win; the actor may not send or save a successor after End wins. */
    @Synchronized fun beginReplacement(): Replacement? {
        if (closed || claim != null || replacement != null) return null
        offered = null; offeredOwner = null; owner = null
        return Replacement().also { replacement = it }
    }
    /** Called after the exact Prepare is durable, before HTTP, or after failure.
     * Failure restores eligibility with a fresh owner, never an old invitation. */
    @Synchronized fun settle(value: Replacement, eligible: Boolean) {
        if (closed || replacement !== value) return
        replacement = null; owner = if (eligible) Any() else null
    }
    @Synchronized fun close() {
        closed = true; owner = null; offered = null; offeredOwner = null
        claim = null; claimedOwner = null; replacement = null
    }
}

/** Activity-thread-only invitation scope. A session ticket alone cannot cancel
 * a newer manual or V4V local preparation in the same Activity. */
internal class V4vEndUiScope(private val client: Any, private val request: Long,
    private val pending: Any?, private val capture: Any?) {
    fun confirmIfCurrent(client: Any, request: Long, pending: Any?, capture: Any?,
        v4vSelected: Boolean, foreground: Boolean, alive: Boolean, action: () -> Boolean): Boolean {
        if (this.client !== client || this.request != request || this.pending !== pending || this.capture !== capture ||
            !v4vSelected || !foreground || !alive) return false
        return action()
    }
}
