package dev.forgesworn.sapflow.broadcaster

/** One show audio cursor, separate from a replaceable native encoder's input
 * clock. Fresh zero-only encoders cannot normalise positive priming into an
 * overlap or silently rebase their first kept unit across a gap. */
internal class AudioGenerationClock {
    class Generation internal constructor(val serial: Long, val inputStartUs: Long, val strict: Boolean)
    private var generation: Generation? = null
    private var next: Long? = null
    private var finished = true
    private var serial = 0L
    var primingUnits = 0L; private set
    val nextOutputTick: Long? get() = next
    fun begin(value: Long, inputStartUs: Long, zeroOnly: Boolean): Generation {
        check(finished && value > serial && inputStartUs >= 0) { "AAC generation overlaps its native predecessor" }
        serial = value; finished = false
        return Generation(value,inputStartUs,zeroOnly).also { generation = it }
    }
    fun closed(value: Generation) { if (generation === value) finished = true }
    fun output(value: Generation, encoderUs: Long): Long {
        if (generation !== value || finished) return STALE
        if (encoderUs < 0 || value.strict && encoderUs < value.inputStartUs) { primingUnits = Math.addExact(primingUnits,1); return PRIMING }
        val observed = tick(encoderUs)
        val expected = next ?: observed
        if (value.strict) {
            check(observed == expected && (next != null || observed == tick(value.inputStartUs))) { "Fresh AAC output has an unproved gap or overlap" }
        } else {
            // Preserve the pre-Standby live encoder contract; strict native
            // boundaries apply after cold replacement, not a guessed delay.
            check(observed in maxOf(0,expected-2048)..minOf(Long.MAX_VALUE-2048,expected)+2048) { "Audio clock discontinuity" }
        }
        next = Math.addExact(expected,1024)
        return expected
    }
    fun replacementStartUs(fallbackUs: Long): Long = next?.let(::microseconds) ?: fallbackUs.also { require(it >= 0) }
    companion object {
        const val PRIMING = -1L
        const val STALE = -2L
        fun tick(microseconds: Long): Long { require(microseconds >= 0); return Math.addExact(Math.multiplyExact(microseconds,48),500) / 1000 }
        fun microseconds(tick: Long): Long { require(tick >= 0); return Math.addExact(Math.multiplyExact(tick,1000),24) / 48 }
    }
}

/** Fixed scalar plan for placing actual timestamped Resume PCM on the already
 * running zero-input cursor. It trims overlap and inserts only the measured
 * gap; it never assigns old microphone samples a later timestamp. */
internal class ResumePcmAlignment(private val maxFrames: Long = 9600) {
    var offsetBytes = 0; private set
    var zeroFrames = 0; private set
    var copyBytes = 0; private set
    val outputBytes get() = zeroFrames * 2 + copyBytes
    fun plan(nextTick: Long, capturedStartUs: Long, offset: Int, bytes: Int) {
        require(nextTick >= 0 && offset in 0..bytes && bytes in 2..2048 && bytes%2==0 && offset%2==0)
        val start = Math.addExact(AudioGenerationClock.tick(capturedStartUs),offset/2L)
        val delta = Math.subtractExact(start,nextTick)
        check(delta in -maxFrames..maxFrames) { "Resume PCM does not meet the existing show clock" }
        offsetBytes = offset + minOf((-delta).coerceAtLeast(0),(bytes-offset)/2L).toInt()*2
        if (offsetBytes==bytes) { zeroFrames=0;copyBytes=0;return }
        val selectedStart = Math.addExact(AudioGenerationClock.tick(capturedStartUs),offsetBytes/2L)
        zeroFrames = (selectedStart-nextTick).coerceIn(0,1024).toInt()
        copyBytes = minOf(bytes-offsetBytes,2048-zeroFrames*2)
    }
}
