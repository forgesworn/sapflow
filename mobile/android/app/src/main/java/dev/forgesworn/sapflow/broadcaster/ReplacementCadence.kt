package dev.forgesworn.sapflow.broadcaster

/** Owner-confined rational media cadence. A negative claim means not due.
 * No allocation, timer dispatch or clock query occurs inside the policy.
 * Video may skip obsolete slots; audio never skips sample extents. */
internal class ReplacementCadence(private val epochNs: Long, units: Long, rate: Long,
    private val maxLateNs: Long = 200_000_000L, private val skipObsolete: Boolean = false) {
    private val numerator = Math.multiplyExact(units,1_000_000_000L)
    private val denominator = rate
    private var index = 0L
    private var lastObservedNs = -1L
    var nextNs: Long = epochNs; private set
    init { require(epochNs >= 0 && units > 0 && rate > 0 && maxLateNs > 0) }
    fun claim(nowNs: Long): Long {
        check(nowNs >= 0 && nowNs >= lastObservedNs) { "Replacement media clock regressed" }
        lastObservedNs=nowNs
        if (nowNs < nextNs) return -1
        check(nowNs - nextNs <= maxLateNs) { "Replacement media driver exceeded its freshness bound" }
        if (skipObsolete) {
            // At most 200 ms / native frame period, never an unbounded burst.
            while (time(Math.addExact(index,1)) <= nowNs) index = Math.addExact(index,1)
        }
        val claimed = time(index)
        index = Math.addExact(index,1); nextNs = time(index)
        return claimed
    }
    private fun time(value: Long): Long = Math.addExact(epochNs,Math.multiplyExact(value,numerator) / denominator)
}
