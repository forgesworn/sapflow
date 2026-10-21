package dev.forgesworn.sapflow.broadcaster

/** Pure control-owner policy. Platform cleanup is asynchronous; only a real
 * completion before the original deadline may authorize a new media epoch. */
internal class OrientationRestartPlan(initialPose: CardinalPose = CardinalPose.NATURAL, private val budgetNs: Long = 6_000_000_000L) {
    data class Ticket(val token: Long, val deadlineNs: Long)
    var desiredPose = initialPose
        private set
    var pending: Ticket? = null
        private set
    private var generation = 0L
    private var lastNowNs = 0L
    private var inhibited = false

    init { require(budgetNs > 0) }

    fun request(pose: CardinalPose, running: Boolean, nowNs: Long): Ticket? {
        checkTime(nowNs)
        // A pose notification must not consume the timer's expired token.
        // Its control owner still needs to observe timeout and cancel auto Start.
        // complete()/expired() explicitly consume that result; requests only
        // coalesce the latest pose until then, preserving the original fence.
        if (pose == desiredPose) return null
        desiredPose = pose
        if (!running || pending != null || inhibited) return null
        generation = Math.addExact(generation, 1)
        return Ticket(generation, Math.addExact(nowNs, budgetNs)).also { pending = it }
    }

    fun complete(token: Long, nowNs: Long): CardinalPose? {
        checkTime(nowNs)
        if (pending?.token != token || expired(nowNs)) return null
        pending = null
        inhibited = true
        return desiredPose
    }

    fun expired(nowNs: Long): Boolean {
        checkTime(nowNs)
        if (pending?.let { nowNs >= it.deadlineNs } != true) return false
        cancel()
        return true
    }

    fun cancel() { pending = null; inhibited = true }

    /** Call only after a real new camera/encoder epoch is successfully prepared.
     * This rearms rotation policy; it never authorizes network/media Start. */
    fun beginSession() { check(pending == null); inhibited = false }

    private fun checkTime(nowNs: Long) {
        require(nowNs >= lastNowNs) { "Orientation deadline clock regressed" }
        lastNowNs = nowNs
    }
}
