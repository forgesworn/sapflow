package dev.forgesworn.sapflow.broadcaster

/** Handler-owned control state. No sample retention, timer or encoder IO. */
internal class KeyframeRecovery {
    private var generation = -1L
    private var waiting = true // Initial wait is not runtime overflow recovery.
    private var pending = false
    private var lastRequestNs: Long? = null

    fun begin(generation: Long) {
        this.generation = generation
        waiting = true
        pending = false
        lastRequestNs = null
    }

    fun observe(generation: Long, waiting: Boolean, overflows: ULong, nowNs: Long): Boolean {
        if (generation != this.generation) return false
        val transition = waiting && !this.waiting
        this.waiting = waiting
        if (!transition || overflows == 0uL || pending) return false
        val previous = lastRequestNs
        if (previous != null && nowNs - previous < 500_000_000L) return false
        lastRequestNs = nowNs
        pending = true
        return true
    }

    fun take(generation: Long): Boolean {
        if (generation != this.generation || !pending) return false
        pending = false
        return true
    }
}
