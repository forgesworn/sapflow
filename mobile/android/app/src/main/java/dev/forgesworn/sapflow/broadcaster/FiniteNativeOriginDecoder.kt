package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaDataSource
import android.media.MediaExtractor
import android.media.MediaFormat
import java.util.concurrent.atomic.AtomicBoolean

/** Two immutable downloaded spans, with no concatenation or disk/private-file
 * access. Extractor copies requested compressed ranges into its platform buffer. */
internal class ProbeMediaSource(private val init: ByteArray, private val part: ByteArray) : MediaDataSource() {
    private val closed = AtomicBoolean()
    init { require(init.size in 1..OriginProbeLimits.INIT_BYTES && part.size in 1..OriginProbeLimits.PART_BYTES) }
    override fun getSize(): Long = init.size.toLong() + part.size
    override fun readAt(position: Long, buffer: ByteArray, offset: Int, size: Int): Int {
        check(!closed.get())
        require(position >= 0 && offset >= 0 && size >= 0 && offset <= buffer.size - size)
        if (size == 0) return 0
        if (position >= getSize()) return -1
        var at = position.toInt(); var target = offset
        val count = minOf(size.toLong(), getSize() - position).toInt()
        var remaining = count
        if (at < init.size) {
            val n = minOf(remaining, init.size - at)
            init.copyInto(buffer, target, at, at + n); remaining -= n; at += n; target += n
        }
        if (remaining > 0) part.copyInto(buffer, target, at - init.size, at - init.size + remaining)
        return count
    }
    override fun close() { closed.set(true) }
}

/** Finite buffer-mode decoding. No Surface, AudioTrack, raw-frame copy or PCM
 * playback. Hardware classification is the platform/vendor report (API29),
 * not a physical performance certification. Native AAC software is permitted. */
internal class FiniteNativeOriginDecoder : OriginDecoder {
    override fun decode(init: ByteArray, part: ByteArray, intent: CaptureIntent, cancel: OriginProbeCancellation): NativeOriginEvidence {
        validateProbeMp4(init, true); validateProbeMp4(part, false)
        val deadline = minOf(cancel.deadlineNs, cancel.nowNs() + OriginProbeLimits.DECODE_NS)
        fun checkDeadline() { cancel.check(); check(cancel.nowNs() < deadline) { "Native origin decode timed out" } }
        fun track(video: Boolean): NativeTrackEvidence {
            val source = ProbeMediaSource(init, part)
            val extractor = MediaExtractor()
            var codec: MediaCodec? = null
            var started = false
            try {
                checkDeadline(); extractor.setDataSource(source); checkDeadline()
                val trackCount = extractor.trackCount
                require(trackCount == if (intent.hasVideo) 2 else 1) { "Origin track count differs from prepared capture" }
                val tracks = (0 until trackCount).map { extractor.getTrackFormat(it) }
                intent.requireTrackMimes(tracks.map { it.getString(MediaFormat.KEY_MIME) })
                val index = tracks.indexOfFirst { it.getString(MediaFormat.KEY_MIME) == if (video) "video/avc" else "audio/mp4a-latm" }
                val format = tracks[index]
                if (video) {
                    val profile = checkNotNull(intent.videoProfile)
                    val w = format.getInteger(MediaFormat.KEY_WIDTH); val h = format.getInteger(MediaFormat.KEY_HEIGHT)
                    require((w == profile.landscapeWidth && h == profile.landscapeHeight) ||
                        (h == profile.landscapeWidth && w == profile.landscapeHeight)) { "Origin dimensions differ from prepared profile" }
                } else {
                    require(format.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && format.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
                    val asc = checkNotNull(format.getByteBuffer("csd-0")).duplicate()
                    require(asc.remaining() == 2 && (asc.get(asc.position()).toInt() and 255) == 0x11 &&
                        (asc.get(asc.position() + 1).toInt() and 255) == 0x88) { "Origin 48kHz mono AAC-LC 1024-frame configuration required" }
                }
                val mime = checkNotNull(format.getString(MediaFormat.KEY_MIME))
                val info = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.firstOrNull { candidate ->
                    !candidate.isEncoder && (!video || candidate.isHardwareAccelerated && !candidate.isSoftwareOnly) &&
                        candidate.supportedTypes.any { it.equals(mime, true) } && runCatching {
                            val capabilities = candidate.getCapabilitiesForType(mime)
                            capabilities.isFormatSupported(format) && (!video || capabilities.colorFormats.contains(MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible))
                        }.getOrDefault(false)
                } ?: error("Required native origin decoder unavailable")
                if (video) format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible)
                checkDeadline(); val decoder = MediaCodec.createByCodecName(info.name); codec = decoder
                val actualCodec = decoder.codecInfo
                if (video) require(actualCodec.isHardwareAccelerated && !actualCodec.isSoftwareOnly)
                decoder.configure(format, null, null, 0); checkDeadline(); decoder.start(); started = true
                extractor.selectTrack(index)
                var inputEnded = false; var outputEnded = false; var inputs = 0
                var previousInput = -1L
                val progress = DecodedProbeProgress(video, intent.videoProfile?.let { it.landscapeWidth * it.landscapeHeight * 4 } ?: 0)
                val output = MediaCodec.BufferInfo()
                while (!outputEnded && if (video) progress.count < 2 else progress.bytes < 4096) {
                    checkDeadline()
                    if (!inputEnded) {
                        val slot = decoder.dequeueInputBuffer(1000)
                        if (slot >= 0) {
                            val buffer = checkNotNull(decoder.getInputBuffer(slot)); buffer.clear()
                            val time = extractor.sampleTime
                            if (time < 0) {
                                decoder.queueInputBuffer(slot, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM); inputEnded = true
                            } else {
                                require(++inputs <= OriginProbeLimits.MAX_SAMPLES && extractor.sampleTrackIndex == index && time >= previousInput)
                                if (video && inputs == 1) require(extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_SYNC != 0) { "Independent origin video required" }
                                require(extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_ENCRYPTED == 0)
                                require(extractor.sampleSize in 1..minOf(buffer.capacity(), OriginProbeLimits.MAX_SAMPLE_BYTES).toLong())
                                val n = extractor.readSampleData(buffer, 0); require(n > 0 && n <= OriginProbeLimits.MAX_SAMPLE_BYTES)
                                decoder.queueInputBuffer(slot, 0, n, time, 0); previousInput = time; extractor.advance()
                            }
                        }
                    }
                    val slot = decoder.dequeueOutputBuffer(output, 1000)
                    if (slot == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                        val actual = decoder.outputFormat
                        if (video) require(actual.getInteger(MediaFormat.KEY_WIDTH) == format.getInteger(MediaFormat.KEY_WIDTH) && actual.getInteger(MediaFormat.KEY_HEIGHT) == format.getInteger(MediaFormat.KEY_HEIGHT))
                        else require(actual.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && actual.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1 &&
                            (!actual.containsKey(MediaFormat.KEY_PCM_ENCODING) || actual.getInteger(MediaFormat.KEY_PCM_ENCODING) == android.media.AudioFormat.ENCODING_PCM_16BIT))
                    } else if (slot >= 0) {
                        try {
                            if (output.size > 0 && output.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                                val buffer = checkNotNull(decoder.getOutputBuffer(slot))
                                require(output.offset >= 0 && output.size <= buffer.capacity() - output.offset)
                                progress.output(output.size, output.presentationTimeUs)
                            }
                            outputEnded = output.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                        } finally { decoder.releaseOutputBuffer(slot, false) }
                    }
                }
                checkDeadline()
                return progress.evidence(decoder.name, actualCodec.isSoftwareOnly)
            } finally {
                // Always try every owner. Any cleanup exception prevents success;
                // a blocked native cleanup retains this worker and its capacity.
                var failure: Exception? = null
                fun release(action: () -> Unit) { try { action() } catch (error: Exception) { if (failure == null) failure = error else failure?.addSuppressed(error) } }
                if (started) release { codec?.stop() }
                release { codec?.release() }; release { extractor.release() }; release { source.close() }
                failure?.let { throw OriginProbeCleanupFailure(it, listOfNotNull(codec, extractor, source)) }
            }
        }
        val video = if (intent.hasVideo) track(true) else null; val audio = track(false)
        // Same completed part must yield overlapping A/V media, not merely two
        // unrelated valid tracks. Allow one 30fps frame plus AAC framing.
        if (video != null) require(maxOf(video.firstPtsUs, audio.firstPtsUs) <= minOf(video.lastPtsUs, audio.lastPtsUs) + 60_000)
        checkDeadline()
        return NativeOriginEvidence(video, audio)
    }
}
