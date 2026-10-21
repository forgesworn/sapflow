package dev.forgesworn.sapflow.broadcaster

/** Encoder surfaces use MONOTONIC by default, independently of the camera
 * sensor's REALTIME timestamp source. Never mix elapsedRealtime (BOOTTIME)
 * into this media epoch. Queue residence/deadline clocks remain separate.
 * The first valid AAC timestamp anchors its exact 1024-sample cadence. */
internal class CaptureClock(private val monotonicUs: () -> Long = { System.nanoTime() / 1000 }) {
    private var epochUs = 0L
    private var lastVideoSlot = -1L
    private var firstKeyframe = false
    private var firstAudioTick: Long? = null
    private var audioUnits = 0L

    fun start(epochUs: Long = monotonicUs()) {
        require(epochUs >= 0)
        this.epochUs = epochUs
        lastVideoSlot = -1
        firstKeyframe = false
        firstAudioTick = null
        audioUnits = 0
    }

    fun mediaEpochNs(): Long = Math.multiplyExact(epochUs, 1000)

    /** Microphone input PTS shares the encoder's session-relative epoch. */
    fun audioStartUs(): Long = (monotonicUs() - epochUs).also {
        require(it >= 0) { "Monotonic capture clock regressed" }
    }

    /** Signed callback age in the encoder surface's clock, not sensor time. */
    fun videoAgeMs(encoderUs: Long): Long = (monotonicUs() - encoderUs) / 1000

    /** 90 kHz DTS/PTS, or null for pre-epoch/non-IDR/duplicate capture slots. */
    fun video(encoderUs: Long, keyframe: Boolean): Long? {
        if (encoderUs < epochUs || (!firstKeyframe && !keyframe)) return null
        val relative = encoderUs - epochUs
        require(relative <= (Long.MAX_VALUE - 500_000) / 30) { "Video timestamp overflow" }
        val slot = (relative * 30 + 500_000) / 1_000_000
        if (slot <= lastVideoSlot) return null
        require(slot <= Long.MAX_VALUE / 3000) { "Video timestamp overflow" }
        lastVideoSlot = slot
        firstKeyframe = true
        return slot * 3000
    }

    /** Exact 48 kHz AAC-LC cadence; negative encoder priming units are dropped. */
    fun audio(encoderUs: Long): Long? {
        if (encoderUs < 0) return null
        require(encoderUs <= (Long.MAX_VALUE - 500) / 48) { "Audio timestamp overflow" }
        val observed = (encoderUs * 48 + 500) / 1000
        val first = firstAudioTick ?: observed.also { firstAudioTick = it }
        require(audioUnits <= (Long.MAX_VALUE - first) / 1024) { "Audio timestamp overflow" }
        val expected = first + audioUnits * 1024
        require(observed in maxOf(0, expected - 2048)..minOf(Long.MAX_VALUE - 2048, expected) + 2048) {
            "Audio clock discontinuity; prepare a new session"
        }
        audioUnits++
        return expected
    }
}
