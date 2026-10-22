package dev.forgesworn.sapflow.broadcaster

/** Cold service/main-owner driver. It never grants background admission: the caller
 * supplies the already committed, opt-in audio-only owner's current eligibility.
 * Renewal uses BOOTTIME and the existing service check, never media callbacks.
 * A stalled check cannot retrospectively renew an expired native lease. */
internal class AudioBroadcastWakeLease<O : Any>(private val platform: Platform,
    private val afterAcquireNs: (() -> Long)? = null) {
    interface Platform {
        fun held(): Boolean
        fun acquire(timeoutMs: Long)
        /** Must throw if actual release cannot be established. */
        fun release()
    }
    private var owner: O? = null
    private var closed = false
    private var retired = false
    private var deadlineNs: Long? = null
    private var renewAtNs = 0L
    private var lastNowNs = -1L

    fun bind(value: O): Boolean {
        if (owner === value) return !closed
        if (owner != null) {
            if (!retired || !closed || !release()) return false
            owner = null
        }
        owner = value; closed = false; retired = false; deadlineNs = null; lastNowNs = -1
        return true
    }

    /** False is an exact-owner fault/retirement, requiring Stop, never a new grant. */
    fun maintain(value: O, eligible: Boolean, nowNs: Long, renew: Boolean = true): Boolean {
        if (owner !== value || closed) return false
        if (nowNs < 0 || nowNs < lastNowNs || nowNs > Long.MAX_VALUE - LEASE_NS) {
            stop(value); return false
        }
        lastNowNs = nowNs
        val deadline = deadlineNs
        if (deadline != null && (nowNs >= deadline || !isHeld())) {
            stop(value); return false
        }
        if (!eligible) {
            // A merely Prepared owner never acquires. Losing eligibility after
            // acquiring is terminal, including Stop/fault while cleanup blocks.
            if (deadline != null) { stop(value); return false }
            return true
        }
        if (deadline == null || renew && nowNs >= renewAtNs) {
            try {
                platform.acquire(LEASE_MS)
                check(platform.held())
                // The system call is cold but may block. A late successful
                // return cannot renew an owner whose original lease elapsed.
                val completedNs = afterAcquireNs?.invoke() ?: nowNs
                check(completedNs >= nowNs && completedNs < (deadline ?: (nowNs + LEASE_NS)))
                lastNowNs = completedNs
                deadlineNs = nowNs + LEASE_NS
                renewAtNs = nowNs + RENEW_NS
            } catch (_: Exception) { stop(value); return false }
        }
        return true
    }

    /** Logical retirement is sticky even if native release throws. Keep the
     * exact owner/capacity until release is proven; the native timeout is backup. */
    fun stop(value: O): Boolean {
        if (owner !== value) return false
        closed = true
        return release()
    }
    fun retire(value: O): Boolean {
        if (owner !== value) return false
        retired = true
        if (!stop(value)) return false
        owner = null
        return true
    }
    fun destroy() { owner?.let(::stop) }
    private fun isHeld(): Boolean = try { platform.held() } catch (_: Exception) { false }
    private fun release(): Boolean = try {
        platform.release()
        !platform.held()
    } catch (_: Exception) { false }

    companion object {
        const val LEASE_MS = 30_000L
        private const val LEASE_NS = LEASE_MS * 1_000_000L
        private const val RENEW_NS = 10_000_000_000L
    }
}
