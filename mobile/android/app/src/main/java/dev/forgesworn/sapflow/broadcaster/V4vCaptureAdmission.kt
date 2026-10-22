package dev.forgesworn.sapflow.broadcaster

/** A queued control reply grants no authority after synchronous Stop/End/expiry.
 * The platform holds its owner lock across consume and destination adoption. */
internal class V4vCaptureAdmission {
    data class Prepared(val owner: Long, val identity: V4vIdentity, val generation: Long, val intent: CaptureIntent)
    private var generation = 0L
    private var prepared: Prepared? = null
    @Synchronized fun snapshot(): Long = generation
    @Synchronized fun invalidate() { generation = Math.addExact(generation, 1); prepared = null }
    @Synchronized fun current(ticket: Long) = ticket == generation
    @Synchronized fun bind(owner: Long, identity: V4vIdentity, intent: CaptureIntent = CaptureIntent.DEFAULT): Prepared = Prepared(owner, identity, generation, intent).also { prepared = it }
    @Synchronized fun canStart(binding: Prepared, approval: Long, identity: V4vIdentity, intent: CaptureIntent = CaptureIntent.DEFAULT): Boolean =
        prepared == binding && binding.intent == intent && approval == generation && binding.generation == generation && identity.sameSession(binding.identity) && identity.revision >= binding.identity.revision
    @Synchronized fun consume(ticket: Long, intent: CaptureIntent, maxPublishBps: Long) {
        intent.requireGrant(maxPublishBps)
        consume(ticket)
    }
    fun bind(owner: Long, identity: V4vIdentity, profile: CaptureProfile): Prepared = bind(owner, identity, CaptureIntent.AudioVideo(profile))
    fun canStart(binding: Prepared, approval: Long, identity: V4vIdentity, profile: CaptureProfile): Boolean = canStart(binding, approval, identity, CaptureIntent.AudioVideo(profile))
    fun consume(ticket: Long, profile: CaptureProfile, maxPublishBps: Long) = consume(ticket, CaptureIntent.AudioVideo(profile), maxPublishBps)
    @Synchronized fun consume(ticket: Long) {
        check(ticket == generation) { "V4V capture permission was retired" }
        invalidate() // A duplicate queued answer cannot create a second owner.
    }
}
internal class V4vCapturePermit(val ticket: Long, val grant: V4vGrant, val intent: CaptureIntent) {
    constructor(ticket: Long, grant: V4vGrant, profile: CaptureProfile) : this(ticket, grant, CaptureIntent.AudioVideo(profile))
    override fun toString() = "V4V capture permission (redacted)"
}

