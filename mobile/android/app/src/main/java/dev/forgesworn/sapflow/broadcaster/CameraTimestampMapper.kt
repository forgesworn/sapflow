package dev.forgesworn.sapflow.broadcaster

import android.os.SystemClock

/** GL-owner timestamp conversion. This is a source-clock mapping, never a
 * callback-latency estimate. Queue residence and protocol clocks stay separate. */
internal class CameraTimestampMapper private constructor(
    private val bootNs: (() -> Long)?,
    private val monotonicNs: () -> Long,
) {
    data class Calibration(val offsetNs: Long, val uncertaintyNs: Long)
    private var calibration = bootNs?.let { calibrate(it, monotonicNs) }
    private var lastCheckNs = 0L
    private var lastNowNs = 0L
    private var lastSourceNs = -1L
    private var lastMappedNs = -1L

    fun map(timestampNs: Long): Long {
        require(timestampNs >= 0 && timestampNs > lastSourceNs) { "Camera timestamp regressed or repeated" }
        val now = monotonicNs()
        require(now >= lastNowNs) { "Monotonic camera clock regressed" }
        lastNowNs = now
        if (bootNs != null && now - lastCheckNs >= 1_000_000_000L) {
            val current = calibrate(bootNs, monotonicNs)
            val old = calibration!!
            val difference = Math.subtractExact(current.offsetNs, old.offsetNs)
            require(difference in -1_000_000L..1_000_000L) { "Camera clock offset changed; start a new epoch" }
            // Freeze the epoch's original mapping: resampling validates it but
            // does not silently move media timing to a new clock.
            lastCheckNs = now
        }
        val mapped = Math.subtractExact(timestampNs, calibration?.offsetNs ?: 0L)
        val age = Math.subtractExact(now, mapped)
        require(mapped >= 0 && mapped > lastMappedNs && age in -5_000_000L..1_000_000_000L) {
            "Camera timestamp is outside its monotonic age bound"
        }
        lastSourceNs = timestampNs; lastMappedNs = mapped
        return mapped
    }

    companion object {
        fun monotonic(monotonicNs: () -> Long = { System.nanoTime() }): CameraTimestampMapper = CameraTimestampMapper(null, monotonicNs)
        fun realtime(bootNs: () -> Long = { SystemClock.elapsedRealtimeNanos() }, monotonicNs: () -> Long = { System.nanoTime() }): CameraTimestampMapper =
            CameraTimestampMapper(bootNs, monotonicNs)

        /** Three fixed-cost bracketed observations; keep the narrowest span. */
        internal fun calibrate(bootNs: () -> Long, monotonicNs: () -> Long): Calibration {
            var best: Calibration? = null
            repeat(3) {
                val before = bootNs(); val mono = monotonicNs(); val after = bootNs()
                require(before >= 0 && mono >= 0 && after >= before) { "Invalid camera clock calibration" }
                val span = after - before
                val offset = Math.subtractExact(before + span / 2, mono)
                val sample = Calibration(offset, span / 2 + span % 2)
                val previous = best
                if (previous == null || sample.uncertaintyNs < previous.uncertaintyNs) best = sample
            }
            return best!!.also { require(it.uncertaintyNs <= 250_000L) { "Camera clock calibration is too uncertain" } }
        }
    }
}
