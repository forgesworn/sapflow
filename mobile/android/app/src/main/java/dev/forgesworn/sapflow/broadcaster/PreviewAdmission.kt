package dev.forgesworn.sapflow.broadcaster

/** GL-owner admission for the optional debug-only live preview cap.
 * The caller still latches and submits every eligible encoder frame first. */
internal class PreviewAdmission(private val fps: Int) {
    init { require(fps == 0 || fps == 15) }
    private var origin: Long? = null
    private var nextNs = 0L

    fun reset() { origin = null }

    fun claim(nowNs: Long): Boolean {
        if (fps == 0) return true
        val start = origin
        if (start == null || nowNs < start) {
            origin = nowNs
            nextNs = nowNs
        }
        if (nowNs < nextNs) return false
        val elapsed = try { Math.subtractExact(nowNs, checkNotNull(origin)) }
            catch (_: ArithmeticException) { reset(); return claim(nowNs) }
        // Floor each absolute rational slot independently. This accepts the
        // 66,666,666 ns boundary of a 30 fps source without drifting to 10 fps.
        val whole = elapsed / 1_000_000_000L
        val remainder = elapsed % 1_000_000_000L
        val slot = whole * fps + ((remainder + 1) * fps - 1) / 1_000_000_000L
        val next = slot + 1
        try {
            val offset = Math.addExact(Math.multiplyExact(next / fps, 1_000_000_000L),
                (next % fps) * 1_000_000_000L / fps)
            nextNs = Math.addExact(checkNotNull(origin), offset)
        } catch (_: ArithmeticException) {
            // Do not queue catch-up work or wrap the deadline on a clock edge.
            reset()
        }
        return true
    }
}
