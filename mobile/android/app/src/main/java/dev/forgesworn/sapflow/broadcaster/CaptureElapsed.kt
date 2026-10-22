package dev.forgesworn.sapflow.broadcaster

internal data class CaptureElapsedSnapshot(val captureMs: Long?, val broadcastMs: Long?, val uncertain: Boolean, val broadcastEnded: Boolean = false)

/** Owned exclusively by CaptureStartAdmission's existing monitor. Times are
 * caller-supplied monotonic nanoseconds, separate from encoder/file duration. */
internal class CaptureElapsed {
    private var startedNs: Long? = null
    private var captureEndedNs: Long? = null
    private var broadcastEndedNs: Long? = null
    private var floorNs = 0L
    private var uncertain = false
    fun reset() { startedNs = null; captureEndedNs = null; broadcastEndedNs = null; floorNs = 0; uncertain = false }
    fun commit(nowNs: Long) { val now = advance(nowNs); if (startedNs == null) startedNs = now }
    fun end(nowNs: Long) { val now = advance(nowNs); if (broadcastEndedNs == null) broadcastEndedNs = now }
    fun stop(nowNs: Long) { val now = advance(nowNs); if (captureEndedNs == null) captureEndedNs = now; if (broadcastEndedNs == null) broadcastEndedNs = now }
    fun snapshot(nowNs: Long, publisherAttached: Boolean, destinationEndNs: Long?): CaptureElapsedSnapshot {
        val now = advance(nowNs)
        // A captured terminal event may be observed after later UI reads. Its
        // immutable original time is historical evidence, not clock rollback.
        if (destinationEndNs != null) {
            if (destinationEndNs < 0 || destinationEndNs > now) uncertain = true
            else broadcastEndedNs = minOf(broadcastEndedNs ?: destinationEndNs, destinationEndNs)
        }
        val start = startedNs
        if (start == null || uncertain) return CaptureElapsedSnapshot(null, null, uncertain, broadcastEndedNs != null)
        return CaptureElapsedSnapshot((maxOf(start, captureEndedNs ?: now) - start) / 1_000_000,
            if (publisherAttached) (maxOf(start, broadcastEndedNs ?: now) - start) / 1_000_000 else null, false, broadcastEndedNs != null)
    }
    private fun advance(nowNs: Long): Long {
        if (nowNs < floorNs) uncertain = true else floorNs = nowNs
        return floorNs
    }
}
