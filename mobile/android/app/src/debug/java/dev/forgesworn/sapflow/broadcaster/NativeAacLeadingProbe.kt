package dev.forgesworn.sapflow.broadcaster

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean

/** Generated-only cold experiment. This owns one encoder, never resets it at
 * Start, and uses one temporary decoder only for preparation. There is no
 * capture/SDK receipt path, production media queue, or programme hot decode.
 * The Activity roots this actual owner until every checked close returns. */
internal class NativeAacLeadingProbe(private val root: File, private val cancelled: AtomicBoolean) {
    private var retained: Case? = null
    private var worker: Thread? = null
    private fun owner() = check(Thread.currentThread() === worker)

    fun run(): JSONObject {
        check(worker == null) { "One run per retained owner" }
        worker = Thread.currentThread()
        owner()
        check(Build.HARDWARE in setOf("ranchu", "goldfish"))
        val cases = JSONArray()
        var cleanup = true
        for (seed in listOf(0x13579, 0x24680)) for (programme in listOf(48_000, 2765)) {
            if (!cleanup || cancelled.get()) break
            val item = Case(seed, programme)
            retained = item
            val result = item.run()
            cases.put(result)
            cleanup = result.getBoolean("cleanupConfirmed")
            if (cleanup) retained = null
        }
        return JSONObject().put("schema", "generated-native-aac-cold-leading-v1")
            .put("generatedOnly", true).put("emulatorOnly", true).put("hardware", Build.HARDWARE)
            .put("sdk", Build.VERSION.SDK_INT).put("fingerprint", Build.FINGERPRINT)
            .put("cases", cases).put("allCasesVisited", cases.length() == 4)
            .put("cleanupConfirmed", cleanup).put("cancelled", cancelled.get())
            .put("nativeAacQualified", false).put("sdkReceiptIssued", false)
            .put("productionCalibration", false).put("programmeDecodedByNativeProbe", false)
            .put("scope", "Generated same-encoder cold experiment only. Raw byte consumption, exact AU/PCM extent, native decoder semantics and paired presentation remain independent required qualifications.")
    }

    private inner class Handle {
        var codec: MediaCodec? = null
        var createAttempted = false
        var startAttempted = false
        var stopAttempted = false
        var stopReturned = false
        var releaseAttempted = false
        var releaseReturned = false
        fun close(): Boolean {
            owner()
            val native = codec ?: return !createAttempted
            if (startAttempted && !stopAttempted) {
                stopAttempted = true
                try { native.stop(); stopReturned = true } catch (_: Exception) { }
            }
            if (!releaseAttempted) {
                releaseAttempted = true
                try { native.release(); releaseReturned = true } catch (_: Exception) { }
            }
            // An uncertain stop/close is never retried, even after release.
            return releaseReturned && (!startAttempted || stopReturned)
        }
        fun receipt() = JSONObject().put("createAttempted", createAttempted)
            .put("startAttempted", startAttempted).put("stopAttempted", stopAttempted)
            .put("stopReturned", stopReturned).put("releaseAttempted", releaseAttempted)
            .put("releaseReturned", releaseReturned)
    }

    private inner class Case(private val seed: Int, private val programmeFrames: Int) {
        private val directory = File(root, "seed-$seed-programme-$programmeFrames").apply { check(mkdir()) }
        private val encoder = Handle()
        private val decoder = Handle()
        private val streams = mutableListOf<FileOutputStream>()
        private var streamCloseAttempted = false
        private var streamsClosed = false
        private var encoded: FileOutputStream? = null
        private var source: FileOutputStream? = null
        private var programme: FileOutputStream? = null
        private var inputLog: FileOutputStream? = null
        private var outputLog: FileOutputStream? = null
        private var prepPcmFile: FileOutputStream? = null
        private var decoderLog: FileOutputStream? = null
        private val formats = JSONArray()
        private var codecName = "UNCREATED"
        private var decoderName = "UNCREATED"
        private val preamble = ShortArray(8192) { NativeAacLeadingAlignment.sample(it, seed) }
        private val decoded = ShortArray(8192)
        private var decodedFrames = 0
        private var decoderInputBuffers = 0
        private var decoderInputBytes = 0
        private var decoderOutputBuffers = 0
        private var decoderInputEos = false
        private var decoderOutputEos = false
        private var decoderOutputFormatSeen = false
        private var inputCursor = 0
        private var inputSequence = 0
        private var encodedBytes = 0L
        private var positiveBuffers = 0
        private var outputSequence = 0
        private var inputEos = false
        private var outputEos = false
        private var outputReleaseAttempts = 0
        private var outputReleaseReturned = 0
        private var decoderReleaseAttempts = 0
        private var decoderReleaseReturned = 0
        private var formatSeen = false
        private var startIssued = false
        private var candidate: NativeAacLeadingAlignment.Candidate? = null
        private var firstPrepQueueNs = 0L
        private var measuredNs = 0L
        private var startNs = 0L
        private var prepDiscardBytes = 0L
        private val pad = (1024 - programmeFrames % 1024) % 1024
        private val drain = 4096
        private val totalInput = 8192 + programmeFrames + pad + drain
        private val deadlineNs = System.nanoTime() + 12_000_000_000L
        private fun live() {
            owner()
            check(!cancelled.get() && !Thread.currentThread().isInterrupted && System.nanoTime() < deadlineNs) { "Cancelled/case deadline" }
        }
        private fun prepDeadline() {
            live()
            if (firstPrepQueueNs != 0L && !startIssued) {
                check(System.nanoTime() - firstPrepQueueNs <= 250_000_000L) { "Cold preparation exceeded preregistered 250 ms wall bound" }
            }
        }
        private fun open(name: String) = FileOutputStream(File(directory, name)).also { streams.add(it) }
        private fun sha(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
        private fun line(file: FileOutputStream?, value: JSONObject) = checkNotNull(file).write((value.toString() + "\n").toByteArray(Charsets.UTF_8))

        fun run(): JSONObject {
            var failure: String? = null
            try {
                live()
                encoded = open("buffers.bin"); source = open("source.s16le"); programme = open("programme.s16le")
                inputLog = open("inputs.jsonl"); outputLog = open("outputs.jsonl")
                prepPcmFile = open("prep-native.s16le"); decoderLog = open("prep-decoder.jsonl")
                encoder.createAttempted = true
                val native = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                encoder.codec = native; codecName = native.name
                AudioAacEncoderConfiguration.configure(native, 96_000)
                formats.put(format("encoder-input-configured", native.inputFormat))
                encoder.startAttempted = true; native.start()
                formats.put(format("encoder-input-after-start", native.inputFormat))
                while (!outputEos) {
                    live(); prepDeadline()
                    if (!inputEos && (inputCursor < 8192 || startIssued)) queueEncoder(native)
                    drainEncoder(native)
                    if (!startIssued && inputCursor == 8192 && decoderInputBuffers == 6) finishPreparation()
                }
                check(inputEos && startIssued && inputCursor == totalInput)
                formats.put(format("encoder-input-at-output-eos", native.inputFormat))
                check(outputReleaseAttempts == outputReleaseReturned)
            } catch (error: Exception) {
                failure = error.javaClass.simpleName + ": " + error.message?.take(512)
            }
            val decoderClosed = decoder.close()
            val encoderClosed = encoder.close()
            if (!streamCloseAttempted) {
                streamCloseAttempted = true
                var all = true
                for (stream in streams) try { stream.close() } catch (_: Exception) { all = false }
                streamsClosed = all
            }
            val cleanup = decoderClosed && encoderClosed && streamsClosed
            val result = JSONObject().put("name", directory.name).put("seed", seed)
                .put("generatedOnly", true).put("cleanupConfirmed", cleanup)
                .put("failure", failure ?: JSONObject.NULL).put("nativeAacQualified", false).put("sdkReceiptIssued", false)
                .put("encoderName", codecName).put("decoderName", decoderName).put("formats", formats)
                .put("sameEncoderAcrossStart", startIssued).put("encoderFlushOrRestartAttempted", false)
                .put("preambleFrames", 8192).put("programmeFrames", programmeFrames).put("blockPaddingFrames", pad)
                .put("codecDrainFrames", drain).put("successfulQueuedFrames", inputCursor).put("successfulInputBuffers", inputSequence)
                .put("inputEos", inputEos).put("outputEos", outputEos)
                .put("generatedProgrammeRawFrames", (inputCursor - 8192).coerceIn(0, programmeFrames))
                .put("successfulPreambleFrames", inputCursor.coerceAtMost(8192))
                .put("successfulBlockPaddingFrames", (inputCursor - 8192 - programmeFrames).coerceIn(0, pad))
                .put("successfulCodecDrainFrames", (inputCursor - 8192 - programmeFrames - pad).coerceIn(0, drain))
                .put("intentionalProgrammeZeroFrames", 0)
                .put("originalPositiveOutputBuffers", positiveBuffers).put("originalOutputBytes", encodedBytes)
                .put("originalOutputBytesMeaning", "FULLY_EXPORTED_ORIGINAL_RANGES_ONLY")
                .put("preStartDiscardedOriginalBuffers", decoderInputBuffers).put("preStartDiscardedOriginalBytes", prepDiscardBytes)
                .put("discardedCodedFrames", "UNKNOWN_UNTIL_INDEPENDENT_RAW_PARSE")
                .put("nativePrepDecodedFrames", decodedFrames).put("nativePrepDecodedFramesMeaning", "SUCCESSFULLY_COPIED_NATIVE_PCM_FRAMES").put("decoderInputEos", decoderInputEos).put("decoderOutputEos", decoderOutputEos)
                .put("decoderOutputFormatSeen", decoderOutputFormatSeen).put("decoderOutputBuffers", decoderOutputBuffers)
                .put("decoderInputBytes", decoderInputBytes).put("startIssued", startIssued)
                .put("firstPrepQueueNs", firstPrepQueueNs).put("measuredNs", measuredNs).put("startNs", startNs)
                .put("sameEncoderMeasurementBeforeStart", startIssued && measuredNs != 0L && startNs >= measuredNs)
                .put("measurementWallNs", if (measuredNs == 0L) JSONObject.NULL else measuredNs - firstPrepQueueNs)
                .put("startWallNs", if (startNs == 0L) JSONObject.NULL else startNs - firstPrepQueueNs)
                .put("encoderOutputReleaseAttempts", outputReleaseAttempts).put("encoderOutputReleaseReturned", outputReleaseReturned)
                .put("decoderOutputReleaseAttempts", decoderReleaseAttempts).put("decoderOutputReleaseReturned", decoderReleaseReturned)
                .put("encoderClosure", encoder.receipt()).put("decoderClosure", decoder.receipt()).put("streamsClosed", streamsClosed)
                .put("candidateLeadingFrames", candidate?.lag ?: JSONObject.NULL)
                .put("candidate", candidate?.let { JSONObject().put("lag", it.lag).put("first", anchor(it.first)).put("second", anchor(it.second)) } ?: JSONObject.NULL)
                .put("classification", if (failure == null && cleanup && outputEos) "COLD_CANDIDATE_OBSERVED_NOT_QUALIFIED" else "FAILED_OR_INCOMPLETE_EXPERIMENT")
                .put("thresholds", JSONObject().put("minCorrelation", 0.92).put("minRunnerUpMargin", 0.02)
                    .put("gainMin", 0.50).put("gainMax", 1.50).put("exactDisjointIntegerLagAgreement", true)
                    .put("maxLag", 4095).put("anchorFrames", 1024).put("coldWallBoundNs", 250_000_000L))
            File(directory, "receipt.json").writeText(result.toString(2) + "\n")
            return result
        }

        private fun queueEncoder(native: MediaCodec) {
            val index = native.dequeueInputBuffer(0)
            if (index < 0) return
            val limit = if (startIssued) totalInput else 8192
            if (inputCursor == limit && !startIssued) return // Not reached: caller only queues while below prep limit.
            val count = minOf(1024, limit - inputCursor)
            val phase = when {
                inputCursor < 8192 -> "PREAMBLE"
                inputCursor < 8192 + programmeFrames -> "PROGRAMME"
                inputCursor < 8192 + programmeFrames + pad -> "BLOCK_PADDING"
                else -> "CODEC_DRAIN"
            }
            // Never mix source phases in one native input receipt.
            val phaseEnd = when (phase) {
                "PREAMBLE" -> 8192
                "PROGRAMME" -> 8192 + programmeFrames
                "BLOCK_PADDING" -> 8192 + programmeFrames + pad
                else -> totalInput
            }
            val frames = minOf(count, phaseEnd - inputCursor)
            val bytes = ByteArray(frames * 2)
            val pcm = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
            repeat(frames) { i ->
                val at = inputCursor + i
                pcm.putShort(when (phase) {
                    "PREAMBLE" -> preamble[at]
                    "PROGRAMME" -> NativeAacLeadingAlignment.sample(at - 8192, seed xor 0x5a5a5a5a)
                    else -> 0.toShort()
                })
            }
            val buffer = checkNotNull(native.getInputBuffer(index)); buffer.clear(); check(buffer.remaining() >= bytes.size); buffer.put(bytes)
            val pts = inputCursor.toLong() * 1_000_000 / 48_000
            val flags = if (inputCursor == totalInput) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0
            val proposedQueueNs = System.nanoTime()
            native.queueInputBuffer(index, 0, bytes.size, pts, flags)
            if (firstPrepQueueNs == 0L) firstPrepQueueNs = proposedQueueNs
            val previous = inputCursor; inputCursor += frames; inputSequence++
            if (flags != 0) inputEos = true
            checkNotNull(source).write(bytes)
            if (phase == "PROGRAMME") checkNotNull(programme).write(bytes)
            line(inputLog, JSONObject().put("sequence", inputSequence).put("phase", if (flags != 0) "EOS" else phase)
                .put("sourceByteOffset", previous * 2).put("frames", frames).put("nativePtsUs", pts)
                .put("flags", flags).put("queueReturned", true).put("sha256", sha(bytes)))
        }

        private fun drainEncoder(native: MediaCodec) {
            val info = MediaCodec.BufferInfo()
            val index = native.dequeueOutputBuffer(info, 1000)
            if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                check(!formatSeen); formatSeen = true
                val value = native.outputFormat
                check(value.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && value.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
                val csd = checkNotNull(value.getByteBuffer("csd-0")).duplicate()
                check(csd.remaining() == 2 && (csd.get().toInt() and 255) == 0x11 && (csd.get().toInt() and 255) == 0x88)
                formats.put(format("encoder-output", value)); startDecoder()
                return
            }
            if (index < 0) return
            var returned = false
            val sequence = ++outputSequence
            val offset = encodedBytes
            val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
            val eos = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
            if (eos) outputEos = true
            if (!config && info.size > 0) positiveBuffers++
            try {
                check(sequence <= 128 && info.size in 0..65536 && info.offset >= 0)
                val bytes = ByteArray(info.size)
                if (info.size > 0) {
                    val buffer = checkNotNull(native.getOutputBuffer(index)).duplicate()
                    check(info.offset.toLong() + info.size <= buffer.capacity().toLong())
                    buffer.position(info.offset); buffer.limit(info.offset + info.size); buffer.get(bytes)
                    check(encodedBytes + bytes.size <= 1_048_576)
                    checkNotNull(encoded).write(bytes); encodedBytes += bytes.size
                }
                val feed = !config && bytes.isNotEmpty() && !startIssued && decoderInputBuffers < 6
                if (feed) {
                    check(formatSeen && !eos && info.flags and MediaCodec.BUFFER_FLAG_PARTIAL_FRAME == 0)
                    check(decoderInputBytes + bytes.size <= 65536)
                    queueDecoder(bytes, info.presentationTimeUs, false)
                } else if (!startIssued && !config && bytes.isNotEmpty()) error("Unexpected extra prep output before Start")
                line(outputLog, JSONObject().put("sequence", sequence).put("offset", offset).put("size", bytes.size)
                    .put("nativeOffset", info.offset).put("nativePtsUs", info.presentationTimeUs).put("flags", info.flags)
                    .put("config", config).put("eos", eos).put("sha256", sha(bytes)).put("sentToPrepDecoder", feed)
                    .put("preStartDiscard", feed).put("startAlreadyIssued", startIssued))
            } finally {
                outputReleaseAttempts++
                try { native.releaseOutputBuffer(index, false); outputReleaseReturned++; returned = true }
                finally { check(returned) { "Encoder output release unconfirmed" } }
            }
        }

        private fun startDecoder() {
            check(decoder.codec == null)
            decoder.createAttempted = true
            val native = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
            decoder.codec = native; decoderName = native.name
            val value = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, 48000, 1)
            value.setByteBuffer("csd-0", ByteBuffer.wrap(byteArrayOf(0x11, 0x88.toByte())))
            value.setInteger(MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
            // Decoder policy requests no *external* gapless trim. These zeroes
            // are not a claim about encoder delay or decoder intrinsic delay.
            value.setInteger("encoder-delay", 0); value.setInteger("encoder-padding", 0)
            native.configure(value, null, null, 0)
            val accepted = native.inputFormat
            formats.put(format("decoder-input-configured", accepted))
            requireNoContradictoryExternalTrim(accepted)
            check(accepted.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && accepted.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
            if (accepted.containsKey("csd-0")) {
                val csd = checkNotNull(accepted.getByteBuffer("csd-0")).duplicate()
                check(csd.remaining() == 2 && (csd.get().toInt() and 255) == 0x11 && (csd.get().toInt() and 255) == 0x88)
            }
            decoder.startAttempted = true; native.start()
        }

        private fun queueDecoder(bytes: ByteArray, pts: Long, eos: Boolean) {
            val native = checkNotNull(decoder.codec)
            while (true) {
                prepDeadline()
                val index = native.dequeueInputBuffer(0)
                if (index >= 0) {
                    val buffer = checkNotNull(native.getInputBuffer(index)); buffer.clear()
                    check(buffer.remaining() >= bytes.size); buffer.put(bytes)
                    native.queueInputBuffer(index, 0, bytes.size, pts, if (eos) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0)
                    if (eos) decoderInputEos = true
                    else {
                        decoderInputBuffers++; decoderInputBytes += bytes.size; prepDiscardBytes += bytes.size
                    }
                    line(decoderLog, JSONObject().put("kind", "input").put("originalOutputSequence", if (eos) JSONObject.NULL else outputSequence)
                        .put("size", bytes.size).put("sha256", sha(bytes)).put("eos", eos).put("nativePtsUs", pts).put("queueReturned", true))
                    break
                }
                drainDecoder(native)
            }
            drainDecoder(native)
        }

        private fun drainDecoder(native: MediaCodec) {
            val info = MediaCodec.BufferInfo()
            val index = native.dequeueOutputBuffer(info, 1000)
            if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                check(!decoderOutputFormatSeen); decoderOutputFormatSeen = true
                val value = native.outputFormat
                formats.put(format("decoder-output", value))
                requireNoContradictoryExternalTrim(value)
                check(value.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && value.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
                check(value.containsKey(MediaFormat.KEY_PCM_ENCODING) && value.getInteger(MediaFormat.KEY_PCM_ENCODING) == AudioFormat.ENCODING_PCM_16BIT)
                return
            }
            if (index < 0) return
            decoderOutputBuffers++
            val eos = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
            if (eos) decoderOutputEos = true
            try {
                check(decoderOutputBuffers <= 16)
                check(info.flags and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                check(decoderOutputFormatSeen && info.offset >= 0 && info.size >= 0 && info.size % 2 == 0)
                check(decodedFrames + info.size / 2 <= decoded.size)
                val bytes = ByteArray(info.size)
                if (bytes.isNotEmpty()) {
                    val buffer = checkNotNull(native.getOutputBuffer(index)).duplicate()
                    check(info.offset.toLong() + info.size <= buffer.capacity().toLong())
                    buffer.position(info.offset); buffer.limit(info.offset + info.size); buffer.get(bytes)
                    val pcm = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
                    repeat(info.size / 2) { decoded[decodedFrames++] = pcm.short }
                    checkNotNull(prepPcmFile).write(bytes)
                }
                line(decoderLog, JSONObject().put("kind", "output").put("size", info.size)
                    .put("nativePtsUs", info.presentationTimeUs).put("flags", info.flags).put("eos", eos)
                    .put("sha256", sha(bytes)).put("cumulativeFrames", decodedFrames))
            } finally {
                decoderReleaseAttempts++
                native.releaseOutputBuffer(index, false); decoderReleaseReturned++
            }
        }

        private fun finishPreparation() {
            prepDeadline()
            check(!decoderInputEos && decoderInputBuffers == 6)
            queueDecoder(ByteArray(0), 8192L * 1_000_000 / 48000, true)
            val native = checkNotNull(decoder.codec)
            while (!decoderOutputEos) { prepDeadline(); drainDecoder(native) }
            check(decoderReleaseAttempts == decoderReleaseReturned)
            check(decoder.close()) { "Temporary decoder close unconfirmed; Start forbidden" }
            prepDeadline()
            candidate = NativeAacLeadingAlignment.measure(preamble, decoded, decodedFrames)
            measuredNs = System.nanoTime()
            prepDeadline()
            // This scalar precedes Start, while the same encoder is still live.
            // Independent qualification must first establish its meaning.
            startNs = System.nanoTime()
            check(startNs - firstPrepQueueNs <= 250_000_000L) { "Recorded Start exceeded cold wall bound" }
            check(!cancelled.get() && !Thread.currentThread().isInterrupted)
            startIssued = true
        }

        private fun requireNoContradictoryExternalTrim(value: MediaFormat) {
            for (key in listOf("encoder-delay", "encoder-padding")) {
                if (value.containsKey(key)) check(value.getInteger(key) == 0) { "Decoder returned nonzero external trim" }
            }
            // Absent keys remain unknown until independent PCM checks; they
            // are never converted into encoder-leading or decoder guarantees.
        }

        private fun anchor(value: NativeAacLeadingAlignment.Anchor) = JSONObject().put("sourceStart", value.sourceStart)
            .put("lag", value.lag).put("correlation", value.correlation).put("runnerUp", value.runnerUp).put("gain", value.gain)
        private fun format(phase: String, value: MediaFormat): JSONObject {
            val result = JSONObject().put("phase", phase)
            for (key in listOf(MediaFormat.KEY_SAMPLE_RATE, MediaFormat.KEY_CHANNEL_COUNT, MediaFormat.KEY_PCM_ENCODING, "encoder-delay", "encoder-padding")) {
                val row = JSONObject()
                try {
                    if (!value.containsKey(key)) row.put("state", "ABSENT")
                    else row.put("state", "PRESENT_INT").put("value", value.getInteger(key))
                } catch (error: Exception) { row.put("state", "READ_FAILED_OR_WRONG_TYPE").put("reason", error.javaClass.simpleName) }
                result.put(key, row)
            }
            val csd = JSONObject()
            try {
                if (!value.containsKey("csd-0")) csd.put("state", "ABSENT")
                else {
                    val bytes = checkNotNull(value.getByteBuffer("csd-0")).duplicate()
                    check(bytes.remaining() <= 64)
                    val hex = buildString { while (bytes.hasRemaining()) append("%02x".format(bytes.get().toInt() and 255)) }
                    csd.put("state", "PRESENT_BYTES").put("hex", hex)
                }
            } catch (error: Exception) { csd.put("state", "READ_FAILED_OR_UNSUPPORTED").put("reason", error.javaClass.simpleName) }
            result.put("csd-0", csd)
            return result
        }
    }
}
