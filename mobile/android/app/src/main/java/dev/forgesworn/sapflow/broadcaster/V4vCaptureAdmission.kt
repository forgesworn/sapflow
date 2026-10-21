package dev.forgesworn.sapflow.broadcaster

/** A queued control reply grants no authority after synchronous Stop/End/expiry.
 * The platform holds its owner lock across consume and destination adoption. */
internal class V4vCaptureAdmission {
    data class Prepared(val owner: Long, val identity: V4vIdentity, val generation: Long)
    private var generation = 0L
    private var prepared: Prepared? = null
    @Synchronized fun snapshot(): Long = generation
    @Synchronized fun invalidate() { generation = Math.addExact(generation, 1); prepared = null }
    @Synchronized fun current(ticket: Long) = ticket == generation
    @Synchronized fun bind(owner: Long, identity: V4vIdentity): Prepared = Prepared(owner, identity, generation).also { prepared = it }
    @Synchronized fun canStart(binding: Prepared, approval: Long, identity: V4vIdentity): Boolean =
        prepared == binding && approval == generation && binding.generation == generation && identity.sameSession(binding.identity) && identity.revision >= binding.identity.revision
    @Synchronized fun consume(ticket: Long) {
        check(ticket == generation) { "V4V capture permission was retired" }
        invalidate() // A duplicate queued answer cannot create a second owner.
    }
}
internal class V4vCapturePermit(val ticket: Long, val grant: V4vGrant) {
    override fun toString() = "V4V capture permission (redacted)"
}

