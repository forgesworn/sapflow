package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean

/** These limits apply only to the finite origin check, never capture queues. */
internal object OriginProbeLimits {
    const val INIT_BYTES = 256 * 1024
    const val PART_BYTES = 2 * 1024 * 1024
    const val TOTAL_NS = 20_000_000_000L
    const val DECODE_NS = 4_000_000_000L
    const val MAX_SAMPLES = 128
    const val MAX_SAMPLE_BYTES = 512 * 1024
    const val MAX_VIDEO_FRAMES = 32
    const val MAX_PCM_BYTES = 256 * 1024
}

/** Reject truncated/oversized top-level boxes before platform parsing. This is
 * deliberately not a second MP4 demuxer; track/sample checks remain native. */
internal fun validateProbeMp4(bytes: ByteArray, initialization: Boolean) {
    require(bytes.size in 1..if (initialization) OriginProbeLimits.INIT_BYTES else OriginProbeLimits.PART_BYTES)
    val expected = if (initialization) listOf("ftyp", "moov") else listOf("moof", "mdat")
    val found = mutableListOf<String>()
    var offset = 0
    while (offset < bytes.size) {
        require(bytes.size - offset >= 8 && found.size < 8)
        var length = 0L
        repeat(4) { length = (length shl 8) or (bytes[offset + it].toLong() and 255) }
        // Sapflow emits ordinary finite boxes, not extended, zero-to-EOF or
        // sidecar boxes. Refuse ambiguous layouts rather than guess.
        require(length in 8..(bytes.size - offset).toLong())
        val kind = String(bytes, offset + 4, 4, Charsets.US_ASCII)
        found.add(kind); offset += length.toInt()
    }
    require(found == expected)
}

/** The prefix replaces /live/, not the URL authority or its root directory. */
internal class ApprovedPlaybackRoute(private val profile: V4vProfile) {
    fun resource(canonical: String): String {
        require(canonical.matches(Regex("/live/event-[a-z0-9-]{1,122}/[0-9a-f]{1,12}_(?:init(?:[1-9][0-9]*)?|part(?:0|[1-9][0-9]*))\\.mp4")))
        return profile.playbackOrigin + profile.playbackPrefix + canonical.removePrefix("/live/")
    }
}

/** Admission cancellation is synchronous. Native calls may finish later; the
 * worker retains its sole owner until their real finally blocks return. */
internal class OriginProbeCancellation(private val now: () -> Long = System::nanoTime,
    val deadlineNs: Long = now() + OriginProbeLimits.TOTAL_NS) {
    private val cancelled = AtomicBoolean()
    fun cancel() { cancelled.set(true) }
    fun check() { check(!cancelled.get() && now() < deadlineNs) { "Origin check cancelled or timed out" } }
    fun remainingNs(): Long { check(); return deadlineNs - now() }
}

internal data class NativeTrackEvidence(val codec: String, val software: Boolean, val outputs: Int,
    val firstPtsUs: Long, val lastPtsUs: Long, val decodedBytes: Long)
internal data class NativeOriginEvidence(val video: NativeTrackEvidence, val audio: NativeTrackEvidence)
/** Retain the actual platform wrappers when release cannot be proved. No new
 * native probe may be admitted in this process after this terminal failure. */
internal class OriginProbeCleanupFailure(cause: Exception, private val retainedOwners: List<Any>) :
    IllegalStateException("Native origin cleanup incomplete", cause) {
    val retainedCount: Int get() = retainedOwners.size
}
internal class DecodedProbeProgress(private val video: Boolean, private val maxVideoBytes: Int) {
    var count = 0; private set
    var bytes = 0L; private set
    var firstPtsUs = -1L; private set
    var lastPtsUs = -1L; private set
    private var previousBytes = 0
    fun output(size: Int, ptsUs: Long) {
        require(size > 0 && ptsUs >= 0 && count < OriginProbeLimits.MAX_VIDEO_FRAMES)
        require(size <= if (video) maxVideoBytes else OriginProbeLimits.MAX_PCM_BYTES)
        if (!video) require(size % 2 == 0 && bytes + size <= OriginProbeLimits.MAX_PCM_BYTES)
        if (count > 0) {
            val step = ptsUs - lastPtsUs
            require(step in 1..100_000)
            // At most one 48kHz sample of timestamp quantisation; never hide
            // missing/repeated PCM with a millisecond-scale tolerance.
            if (!video) require(kotlin.math.abs(step - previousBytes.toLong() * 1_000_000 / 96_000) <= 21) { "Decoded PCM timing is discontinuous" }
        }
        if (count == 0) firstPtsUs = ptsUs
        count++; bytes += size; lastPtsUs = ptsUs; previousBytes = size
    }
    fun evidence(codec: String, software: Boolean): NativeTrackEvidence {
        val endUs = if (video) lastPtsUs else lastPtsUs + previousBytes.toLong() * 1_000_000 / 96_000
        if (video) require(count >= 2 && lastPtsUs > firstPtsUs)
        else require(count >= 1 && bytes >= 4096 && endUs - firstPtsUs >= 42_666) { "Native AAC needs 2048 decoded PCM frames and their actual time extent" }
        return NativeTrackEvidence(codec, software, count, firstPtsUs, endUs, bytes)
    }
}
internal fun interface OriginDecoder {
    /** Success is returned only after actual extractor/codec cleanup. */
    fun decode(init: ByteArray, part: ByteArray, profile: CaptureProfile, cancel: OriginProbeCancellation): NativeOriginEvidence
}

/** A post-decode status may advance revision/part, but never scope, connection
 * or initialization. Availability still is not a decoder result. */
internal fun sameDecodedBoundary(before: V4vMatchedMedia, after: V4vMatchedMedia): Boolean =
    before.scope === after.scope && before.hasAudio && after.hasAudio &&
        before.connectionEpoch == after.connectionEpoch && before.initializationId == after.initializationId &&
        before.initPath == after.initPath && after.partId >= before.partId
