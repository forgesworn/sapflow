package dev.forgesworn.sapflow.broadcaster

/** One immutable wall/BOOTTIME anchor. Production injects elapsedRealtimeNanos;
 * tests can advance suspension independently of the hardware/media clock. */
internal class AuthorityTimebase(wallMs: Long, private val elapsedNs: () -> Long) {
    private val anchorNs = elapsedNs()
    private val anchorMs = wallMs
    init { require(wallMs >= 0 && anchorNs >= 0) }
    fun nowNs(): Long = elapsedNs().also { require(it >= anchorNs) { "Authority clock regressed" } }
    fun nowMs(): Long = Math.addExact(anchorMs, (nowNs() - anchorNs) / 1_000_000)
}
