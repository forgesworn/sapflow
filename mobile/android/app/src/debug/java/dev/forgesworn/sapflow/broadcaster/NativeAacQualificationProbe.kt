package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec
import android.media.MediaFormat
import android.media.MediaMuxer
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Generated-only real native codec observations. Fixture allocations and file
 * copies below are outside capture. No microphone, camera, Rust sample/muxer,
 * timing authority or complete-AU assumption is used. Each case owns one real
 * encoder and its serial callback/index lifetime, never a mocked callback. */
internal class NativeAacQualificationProbe(private val root: File, private val cancelled: AtomicBoolean,
                                           private val mode: Mode = Mode.ORIGINAL) {
    enum class Mode { ORIGINAL, DRAIN_EXPERIMENTS }
    private data class Case(val name: String, val raw: IntArray, val zero: IntArray,
                            val originUs: Long = 0, val omitInputEos: Boolean = false,
                            val blockPad: Boolean = false, val codecDrain: Int = 0,
                            val eosOnLastNonempty: Boolean = false) {
        val programmeFrames = raw.sum() + zero.sum()
        val paddingFrames = if (blockPad) (1024 - programmeFrames % 1024) % 1024 else 0
        val frames = programmeFrames + paddingFrames + codecDrain
        init {
            require(raw.size == zero.size && raw.all { it >= 0 } && zero.all { it >= 0 })
            require(programmeFrames > 0 && frames < 64 * 1024 && codecDrain in 0..4096)
            require(!(omitInputEos && eosOnLastNonempty))
        }
        fun isZero(frame: Int): Boolean {
            var at = 0
            for (index in raw.indices) {
                at += raw[index]
                if (frame < at) return false
                at += zero[index]
                if (frame < at) return true
            }
            error("Generated source cursor exceeded its fixed plan")
        }
    }
    private val originalCases = listOf(
        Case("raw-one-second", intArrayOf(48_000), intArrayOf(0)),
        Case("mixed-short", intArrayOf(700, 800, 17), intArrayOf(1024, 224, 0)),
        Case("generated-zero", intArrayOf(0), intArrayOf(9600)),
        Case("signed-origin", intArrayOf(4096), intArrayOf(0), -100_000),
        Case("missing-input-eos", intArrayOf(4096), intArrayOf(0), omitInputEos = true),
    )
    private val cases: List<Case> = if (mode == Mode.ORIGINAL) originalCases else buildList {
        for (short in listOf(false, true)) for (policy in 0..4) for (lastEos in listOf(false, true)) {
            val programme = if (short) "mixed-short" else "raw-one-second"
            val treatment = when (policy) { 0 -> "no-pad"; 1 -> "block-pad"; else -> "drain-${listOf(0, 0, 1024, 2048, 4096)[policy]}" }
            val placement = if (lastEos) "last-nonempty-eos" else "empty-eos"
            add(Case("$programme-$treatment-$placement",
                if (short) intArrayOf(700, 800, 17) else intArrayOf(48_000),
                if (short) intArrayOf(1024, 224, 0) else intArrayOf(0),
                blockPad = policy > 0, codecDrain = listOf(0, 0, 1024, 2048, 4096)[policy], eosOnLastNonempty = lastEos))
        }
    }
    // Retained if native shutdown or owner-thread closure is unconfirmed.
    private var retained: NativeCase? = null

    fun run(): JSONObject {
        check(Build.HARDWARE in setOf("ranchu", "goldfish")) { "This packet only permits Android Emulator hardware" }
        check(ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN)
        val results = JSONArray()
        var cleanupConfirmed = true
        for ((index, plan) in cases.withIndex()) {
            if (cancelled.get()) break
            val owner = NativeCase(plan, index + 1)
            retained = owner
            val result = owner.run()
            results.put(result)
            cleanupConfirmed = result.getBoolean("cleanupConfirmed")
            if (!cleanupConfirmed) break
            retained = null
        }
        return JSONObject().put("schema", if (mode == Mode.ORIGINAL) "generated-native-aac-observation-v1" else "generated-native-aac-drain-experiment-v1")
            .put("mode", mode.name)
            .put("generatedOnly", true).put("emulatorOnly", true).put("hardware", Build.HARDWARE)
            .put("sdk", Build.VERSION.SDK_INT).put("model", Build.MODEL).put("fingerprint", Build.FINGERPRINT)
            .put("cases", results).put("allCasesVisited", results.length() == cases.size)
            .put("cleanupConfirmed", cleanupConfirmed).put("cancelled", cancelled.get())
            .put("nativeAacQualified", false).put("sdkReceiptIssued", false)
            .put("scope", "Selected emulator/OS/codec configuration and actual generated buffers only. No capture, AU cardinality certificate, trim truth, decoder presentation, app admission or physical proof.")
    }

    private inner class NativeCase(private val plan: Case, private val generation: Int) : MediaCodec.Callback() {
        private val directory = File(root, plan.name).apply { check(mkdir()) }
        private val thread = HandlerThread("sapflow-aac-probe-$generation")
        private lateinit var handler: Handler
        private val terminal = CountDownLatch(1)
        private val sourceQueued = CountDownLatch(1)
        private val problem = AtomicReference<String?>()
        private var outputs: FileOutputStream? = null
        private var source: FileOutputStream? = null
        private var rows: FileOutputStream? = null
        private var inputRows: FileOutputStream? = null
        private var programmeSource: FileOutputStream? = null
        private val formats = JSONArray()
        private var codec: MediaCodec? = null
        private var muxer: MediaMuxer? = null
        private var controlFormat: MediaFormat? = null
        private var muxerConstructionUnconfirmed = false
        private var muxerStartAttempted = false
        private var muxerCleanupUnconfirmed = false
        private var muxerStopSkipped: String? = null
        private val controlCleanupIssue = AtomicReference<String?>()
        private var controlProblem: String? = null
        private var codecName = "UNCREATED"
        private val selectedProfile = AudioAacProfile.Observation(this, generation.toLong())
        private var codecStarted = false
        private var muxerStarted = false
        private var track = -1
        private var closed = false
        private var inputFrames = 0
        private var rawFrames = 0
        private var zeroFrames = 0
        private var paddingFrames = 0
        private var codecDrainFrames = 0
        private var inputBuffers = 0
        private var inputEos = false
        private var outputEos = false
        private var outputCallbacks = 0
        private var outputReleaseAttempts = 0
        private var outputReleaseReturned = 0
        private var outputBytes = 0L
        private var muxedBuffers = 0
        private var lateCallbacks = 0
        private var cleanupAttempted = false
        private var stopReturned = false
        private var releaseReturned = false
        private var muxerStopReturned = false
        private var muxerReleaseReturned = false
        private var streamsClosed = false
        private val deadlineNs = System.nanoTime() + 12_000_000_000L

        private fun owner() = check(Looper.myLooper() === thread.looper) { "Wrong native callback owner" }
        private fun active() { check(!cancelled.get() && System.nanoTime() < deadlineNs) { "Probe cancelled or case deadline reached" } }
        private fun fail(error: Throwable) {
            problem.compareAndSet(null, error.javaClass.simpleName + ": " + error.message?.take(512))
            terminal.countDown(); sourceQueued.countDown()
        }
        private fun shaRange(bytes: ByteArray, offset: Int, count: Int): String = MessageDigest.getInstance("SHA-256").apply {
            update(bytes, offset, count)
        }.digest().joinToString("") { "%02x".format(it) }
        private fun sha(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

        fun run(): JSONObject {
            try {
                thread.start(); handler = Handler(thread.looper)
                check(handler.post {
                    try {
                        owner(); active()
                        outputs = FileOutputStream(File(directory, "buffers.bin"))
                        source = FileOutputStream(File(directory, "source.s16le"))
                        rows = FileOutputStream(File(directory, "outputs.jsonl"))
                        inputRows = FileOutputStream(File(directory, "inputs.jsonl"))
                        if (mode == Mode.DRAIN_EXPERIMENTS) programmeSource = FileOutputStream(File(directory, "programme.s16le"))
                        val native = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                        codec = native; codecName = native.name
                        native.setCallback(this, handler)
                        selectedProfile.configured(this, generation.toLong(), AudioAacEncoderConfiguration.configure(native, 96_000))
                        formats.put(format("input-configured", native.inputFormat))
                        native.start(); codecStarted = true
                        if (mode == Mode.DRAIN_EXPERIMENTS) {
                            try { formats.put(format("input-after-start", native.inputFormat)) }
                            catch (error: Exception) { formats.put(missingFormat("input-after-start", "READ_FAILED").put("reason", error.javaClass.simpleName)) }
                            formats.put(vendorParameters(native))
                        }
                    } catch (error: Exception) { fail(error) }
                })
                val remaining = { ((deadlineNs - System.nanoTime()) / 1_000_000).coerceAtLeast(1) }
                if (plan.omitInputEos) {
                    check(sourceQueued.await(remaining(), TimeUnit.MILLISECONDS)) { "Generated input deadline" }
                    // Deliberate missing-EOS negative. No output drain/EOF claim.
                    if (problem.get() == null) Thread.sleep(200)
                } else check(terminal.await(remaining(), TimeUnit.MILLISECONDS)) { "Native output EOS deadline" }
            } catch (error: Exception) { fail(error) }
            val cleanup = retire()
            val profile = checkNotNull(selectedProfile.freeze(this, generation.toLong()))
            if (!cleanup) {
                return JSONObject().put("name", plan.name).put("generation", generation)
                    .put("cleanupConfirmed", false).put("failure", problem.get() ?: "Native owner cleanup unconfirmed")
                    .put("filesPreserved", true).put("nativeAacQualified", false)
                    .put("selectedEncoderProfile", profileJSON(profile))
                    .put("nativeMuxerCleanupIssue", controlCleanupIssue.get() ?: "OWNER_ACK_OR_DRAIN_UNCONFIRMED")
                    .also { File(directory, "receipt.json").writeText(it.toString(2) + "\n") }
            }
            val observed = JSONObject().put("name", plan.name).put("generation", generation)
                .put("codec", codecName).put("requestedProfile", "AAC-LC 48000Hz mono 96000bps max-input2048")
                .put("inputOriginUs", plan.originUs).put("plannedFrames", plan.frames).put("plannedProgrammeFrames", plan.programmeFrames)
                .put("plannedBlockPaddingFrames", plan.paddingFrames).put("plannedCodecDrainFrames", plan.codecDrain)
                .put("programmeFrames", rawFrames + zeroFrames).put("blockPaddingFrames", paddingFrames).put("codecDrainFrames", codecDrainFrames)
                .put("inputEosPlacement", if (plan.omitInputEos) "OMITTED" else if (plan.eosOnLastNonempty) "LAST_NONEMPTY" else "SEPARATE_EMPTY")
                .put("nativeQueuedFramesConserved", inputFrames == rawFrames + zeroFrames + paddingFrames + codecDrainFrames)
                .put("plannedRawFrames", plan.raw.sum()).put("plannedZeroFrames", plan.zero.sum())
                .put("sourcePlan", JSONArray(plan.raw.indices.map { JSONObject().put("rawFrames", plan.raw[it]).put("zeroFrames", plan.zero[it]) }))
                .put("inputFrames", inputFrames).put("rawFrames", rawFrames).put("zeroFrames", zeroFrames)
                .put("inputBuffers", inputBuffers).put("inputEosQueued", inputEos).put("outputEosObserved", outputEos)
                .put("deliberateMissingInputEos", plan.omitInputEos).put("outputCallbacks", outputCallbacks)
                .put("outputReleaseAttempts", outputReleaseAttempts).put("outputReleaseReturned", outputReleaseReturned)
                .put("callbackBarrier", true).put("callbackOwnerDrained", true).put("handoff", "NOT_PRESENT_DIRECT_FIXTURE_OBSERVATION").put("lateCallbacks", lateCallbacks)
                .put("nativeStopReturned", stopReturned).put("nativeReleaseReturned", releaseReturned)
                .put("nativeMuxerCreated", muxer != null).put("nativeMuxerStartAttempted", muxerStartAttempted)
                .put("nativeMuxerStarted", muxerStarted).put("nativeMuxerCleanupUnconfirmed", muxerCleanupUnconfirmed)
                .put("nativeMuxerStopSkipped", muxerStopSkipped ?: JSONObject.NULL)
                .put("nativeMuxerStopReturned", muxerStopReturned).put("nativeMuxerReleaseReturned", muxerReleaseReturned)
                .put("muxedBuffers", muxedBuffers).put("streamsClosed", streamsClosed)
                .put("containerControlFailure", controlProblem ?: JSONObject.NULL)
                .put("sourceWaveform", "per-frame integer mix v1; exact queued source.s16le")
                .put("cleanupConfirmed", true).put("failure", problem.get() ?: JSONObject.NULL)
                .put("formats", formats).put("auCardinality", "UNKNOWN")
                .put("selectedEncoderProfile", profileJSON(profile))
                .put("nativeAacQualified", false).put("sdkReceiptIssued", false)
                .put("classification", if (problem.get() != null) "FAILED_OBSERVATION" else if (plan.omitInputEos) "INCOMPLETE_NO_INPUT_EOS" else if (inputEos && outputEos) "OBSERVED_NATIVE_EOS" else "INCOMPLETE")
            File(directory, "receipt.json").writeText(observed.toString(2) + "\n")
            return observed
        }

        /** Deterministic nonperiodic fixture waveform. Intentional Int wrapping
         * mixes the absolute generated frame index; exported bytes are truth. */
        private fun rawSample(frame: Int): Short {
            var value = frame + 1
            value = (value xor (value ushr 16)) * 0x45d9f3b
            value = (value xor (value ushr 16)) * 0x45d9f3b
            value = value xor (value ushr 16)
            return (((value ushr 16) - 32768) / 4).toShort()
        }

        override fun onInputBufferAvailable(native: MediaCodec, index: Int) {
            owner()
            if (closed) { lateCallbacks++; return }
            if (inputEos || problem.get() != null) return
            try {
                active()
                check(native === codec)
                if (inputFrames == plan.frames) {
                    sourceQueued.countDown()
                    if (!plan.omitInputEos) {
                        val pts = plan.originUs + inputFrames.toLong() * 1_000_000 / 48_000
                        native.queueInputBuffer(index, 0, 0, pts, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                        inputEos = true
                        val row = JSONObject().put("sequence", inputBuffers + 1).put("type", "EOS")
                            .put("frames", 0).put("nativePtsUs", pts).put("flags", MediaCodec.BUFFER_FLAG_END_OF_STREAM).put("queueReturned", true)
                        checkNotNull(inputRows).write((row.toString() + "\n").toByteArray(Charsets.UTF_8))
                    }
                    return
                }
                check(inputBuffers < 64)
                val frames = minOf(1024, plan.frames - inputFrames)
                val bytes = ByteArray(frames * 2)
                val pcm = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
                var raw = 0; var zero = 0; var padding = 0; var drain = 0
                repeat(frames) { offset ->
                    val frame = inputFrames + offset
                    when {
                        frame >= plan.programmeFrames + plan.paddingFrames -> { pcm.putShort(0); drain++ }
                        frame >= plan.programmeFrames -> { pcm.putShort(0); padding++ }
                        plan.isZero(frame) -> { pcm.putShort(0); zero++ }
                        else -> { pcm.putShort(rawSample(frame)); raw++ }
                    }
                }
                val buffer = checkNotNull(native.getInputBuffer(index))
                check(buffer.capacity() >= 2048); buffer.clear(); buffer.put(bytes)
                val pts = plan.originUs + inputFrames.toLong() * 1_000_000 / 48_000
                val flags = if (plan.eosOnLastNonempty && inputFrames + frames == plan.frames) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0
                native.queueInputBuffer(index, 0, bytes.size, pts, flags)
                // Programme, alignment padding and codec drain are distinct
                // successful native inputs. Drain never becomes show audio.
                inputFrames += frames; rawFrames += raw; zeroFrames += zero
                paddingFrames += padding; codecDrainFrames += drain; inputBuffers++
                if (flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) inputEos = true
                checkNotNull(source).write(bytes)
                programmeSource?.write(bytes, 0, (raw + zero) * 2)
                val row = JSONObject().put("sequence", inputBuffers).put("type", "SOURCE")
                    .put("sourceByteOffset", (inputFrames - frames) * 2).put("frames", frames)
                    .put("rawFrames", raw).put("zeroFrames", zero).put("nativePtsUs", pts)
                    .put("flags", flags).put("queueReturned", true).put("sourceSha256", sha(bytes))
                    .put("programmeByteOffset", (rawFrames + zeroFrames - raw - zero) * 2)
                    .put("programmeFrames", raw + zero).put("blockPaddingFrames", padding).put("codecDrainFrames", drain)
                    .put("programmeSha256", shaRange(bytes, 0, (raw + zero) * 2))
                    .put("blockPaddingSha256", shaRange(bytes, (raw + zero) * 2, padding * 2))
                    .put("codecDrainSha256", shaRange(bytes, (raw + zero + padding) * 2, drain * 2))
                checkNotNull(inputRows).write((row.toString() + "\n").toByteArray(Charsets.UTF_8))
                if (inputFrames == plan.frames) sourceQueued.countDown()
            } catch (error: Exception) { fail(error) }
        }

        override fun onOutputFormatChanged(native: MediaCodec, value: MediaFormat) {
            owner()
            if (closed) { lateCallbacks++; return }
            try {
                active(); check(native === codec && controlFormat == null)
                selectedProfile.format(this, generation.toLong(), AudioAacProfile.Phase.OUTPUT_FORMAT, AudioAacEncoderConfiguration.format(value))
                formats.put(format("output-format", value))
                check(value.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48_000 && value.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
                val csd = checkNotNull(value.getByteBuffer("csd-0")).duplicate()
                check(csd.remaining() == 2 && (csd.get().toInt() and 255) == 0x11 && (csd.get().toInt() and 255) == 0x88)
                controlFormat = value
                // Signed-origin observations must not construct an MP4 control.
                // Preserve every original buffer/EOS; do not rebase or drop it.
                if (plan.originUs < 0) controlProblem = "SKIPPED_SIGNED_ORIGIN"
            } catch (error: Exception) { fail(error) }
        }

        override fun onOutputBufferAvailable(native: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            owner()
            if (closed) { lateCallbacks++; return } // stop invalidated native indices.
            outputCallbacks++
            val offset = info.offset; val size = info.size; val flags = info.flags; val pts = info.presentationTimeUs
            val eos = flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
            val config = flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
            if (eos) {
                outputEos = true
                if (formats.length() < (if (mode == Mode.DRAIN_EXPERIMENTS) 8 else 4)) {
                    if (Build.VERSION.SDK_INT >= 30) {
                        try {
                            val terminalFormat = native.getOutputFormat(index)
                            if (native === codec) selectedProfile.format(this, generation.toLong(), AudioAacProfile.Phase.OUTPUT_EOS, AudioAacEncoderConfiguration.format(terminalFormat))
                            formats.put(format("output-eos", terminalFormat))
                        }
                        catch (error: Exception) {
                            if (native === codec) selectedProfile.format(this, generation.toLong(), AudioAacProfile.Phase.OUTPUT_EOS, AudioAacEncoderConfiguration.format(null))
                            formats.put(missingFormat("output-eos", "READ_FAILED").put("reason", error.javaClass.simpleName))
                        }
                    } else {
                        if (native === codec) selectedProfile.format(this, generation.toLong(), AudioAacProfile.Phase.OUTPUT_EOS,
                            AudioAacEncoderConfiguration.format(null, AudioAacProfile.State.API_UNAVAILABLE))
                        formats.put(missingFormat("output-eos", "API_UNAVAILABLE"))
                    }
                }
                if (mode == Mode.DRAIN_EXPERIMENTS && formats.length() < 8) {
                    try { formats.put(format("input-at-output-eos", native.inputFormat)) }
                    catch (error: Exception) { formats.put(missingFormat("input-at-output-eos", "READ_FAILED").put("reason", error.javaClass.simpleName)) }
                }
            }
            val row = JSONObject().put("index", index).put("sequence", outputCallbacks)
                .put("offset", offset).put("size", size).put("flags", flags).put("nativePtsUs", pts)
                .put("config", config).put("eos", eos).put("payloadOffset", outputBytes)
                .put("auCardinality", "UNKNOWN").put("muxed", false)
            try {
                active(); check(native === codec && outputCallbacks <= 256)
                if (size > 0) {
                    val buffer = checkNotNull(native.getOutputBuffer(index))
                    row.put("capacity", buffer.capacity())
                    row.put("singleAuEligibility", AudioAacProfile.output(selectedProfile.eligibility(this, generation.toLong()),
                        offset, size, flags, true, buffer.capacity()).name)
                    row.put("disposition", AudioAacOutputDisposition.classify(offset, size, flags, true, buffer.capacity()))
                    check(offset >= 0 && size <= 65_536 && offset.toLong() + size <= buffer.capacity())
                    check(outputBytes + size <= 1024 * 1024)
                    val view = buffer.duplicate().apply { position(offset); limit(offset + size) }
                    val bytes = ByteArray(size); view.get(bytes)
                    checkNotNull(outputs).write(bytes); outputBytes += size
                    row.put("payloadSha256", sha(bytes))
                    if (!config) {
                        if (flags and MediaCodec.BUFFER_FLAG_PARTIAL_FRAME != 0) controlProblem = "Partial native packing unsupported by the container control"
                        if (controlFormat == null && controlProblem == null) controlProblem = "Native output preceded container format"
                        if (pts < 0 && controlProblem == null) controlProblem = "SKIPPED_NEGATIVE_NATIVE_PTS"
                        if (controlProblem == null) try {
                            if (muxer == null) {
                                val destination = File(directory, "native.mp4")
                                check(!destination.exists())
                                // A constructor throwing before returning an owner
                                // leaves this control unknown; never retry it.
                                muxerConstructionUnconfirmed = true
                                val control = MediaMuxer(destination.path, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
                                muxer = control; muxerConstructionUnconfirmed = false
                                track = control.addTrack(checkNotNull(controlFormat))
                                muxerStartAttempted = true
                                control.start(); muxerStarted = true
                            }
                            // Original bytes, flags and signed PTS, without rebasing.
                            checkNotNull(muxer).writeSampleData(track, buffer.duplicate(), info)
                            row.put("muxed", true); muxedBuffers++
                        } catch (error: Exception) { controlProblem = error.javaClass.simpleName + ": " + error.message?.take(512) }
                        if (controlProblem != null) row.put("containerControlFailure", controlProblem)
                    }
                } else {
                    check(size == 0 && offset >= 0)
                    row.put("disposition", AudioAacOutputDisposition.classify(offset, size, flags, true))
                    row.put("singleAuEligibility", AudioAacProfile.output(selectedProfile.eligibility(this, generation.toLong()),
                        offset, size, flags, true, 0).name)
                }
            } catch (error: Exception) { row.put("failure", error.javaClass.simpleName + ": " + error.message?.take(512)); fail(error) }
            finally {
                outputReleaseAttempts++
                try { native.releaseOutputBuffer(index, false); outputReleaseReturned++; row.put("releaseReturned", true) }
                catch (error: Exception) { row.put("releaseReturned", false); fail(error) }
                try { if (outputCallbacks <= 256) checkNotNull(rows).write((row.toString() + "\n").toByteArray(Charsets.UTF_8)) }
                catch (error: Exception) { fail(error) }
            }
            if (eos) terminal.countDown() // after this actual index release and row write.
        }

        override fun onError(native: MediaCodec, error: MediaCodec.CodecException) { owner(); if (!closed) fail(error) else lateCallbacks++ }

        /** One attempt on the same real callback owner. Never release native
         * indices from the worker after a timeout. Uncertainty retains owners. */
        private fun retire(): Boolean {
            check(!cleanupAttempted); cleanupAttempted = true
            if (!::handler.isInitialized) {
                if (thread.isAlive) { thread.quitSafely(); thread.join(2000) }
                return !thread.isAlive // Native and file operations were never posted.
            }
            val done = CountDownLatch(1)
            val posted = handler.post {
                owner(); closed = true
                try {
                    if (codecStarted) try { checkNotNull(codec).stop(); stopReturned = true } catch (error: Exception) { fail(error) }
                    else stopReturned = codec == null
                    val control = muxer
                    if (muxerConstructionUnconfirmed) {
                        muxerCleanupUnconfirmed = true; muxerStopSkipped = "CONSTRUCTION_UNCONFIRMED"
                        controlCleanupIssue.set(muxerStopSkipped)
                    } else if (control == null) {
                        muxerStopSkipped = controlProblem ?: "NO_CONTAINER_CONTROL_CREATED"
                    } else if (muxerStartAttempted && (!muxerStarted || muxedBuffers == 0)) {
                        // MPEG4Writer stop/release of an empty or uncertain
                        // started writer is unsafe on the observed profile.
                        // Keep the exact object rooted; root stops the emulator
                        // app after preserving this unknown-cleanup receipt.
                        muxerCleanupUnconfirmed = true; muxerStopSkipped = "EMPTY_OR_UNKNOWN_STARTED_WRITER_RETAINED"
                        controlCleanupIssue.set(muxerStopSkipped)
                    } else {
                        if (muxerStarted) try { control.stop(); muxerStopReturned = true }
                        catch (error: Exception) {
                            muxerCleanupUnconfirmed = true; controlCleanupIssue.set("CONTAINER_STOP_UNCONFIRMED")
                            controlProblem = error.javaClass.simpleName + ": " + error.message?.take(512)
                        }
                        else muxerStopSkipped = "CONTROL_NEVER_STARTED"
                    }
                    try { codec?.release(); releaseReturned = true } catch (error: Exception) { fail(error) }
                    // Preserve known-owner close order. A throwing stop is
                    // unknown, so release must not implicitly retry that writer.
                    if (control != null && !muxerCleanupUnconfirmed) try { control.release(); muxerReleaseReturned = true }
                    catch (error: Exception) { muxerCleanupUnconfirmed = true; controlCleanupIssue.set("CONTAINER_RELEASE_UNCONFIRMED"); fail(error) }
                    var closedStreams = true
                    for (stream in listOfNotNull(outputs, source, rows, inputRows, programmeSource)) {
                        try { stream.flush(); stream.fd.sync() } catch (error: Exception) { fail(error) }
                        try { stream.close() } catch (error: Exception) { closedStreams = false; fail(error) }
                    }
                    streamsClosed = closedStreams
                } finally { done.countDown() }
            }
            if (!posted) return false
            try { if (!done.await(6, TimeUnit.SECONDS)) return false } catch (_: InterruptedException) { return false }
            // quitSafely drains already queued callbacks; all see the closed fence.
            thread.quitSafely()
            try { thread.join(2000) } catch (_: InterruptedException) { return false }
            return !thread.isAlive && releaseReturned && !muxerCleanupUnconfirmed && (muxer == null || muxerReleaseReturned) && streamsClosed
        }

        /** Cold public API inventory only. No subscriptions/configuration or
         * inferred latency authority is introduced into this experiment. */
        private fun vendorParameters(native: MediaCodec): JSONObject {
            val result = JSONObject().put("phase", "vendor-parameters-after-start").put("subscriptionsAttempted", false)
            if (Build.VERSION.SDK_INT < 31) return result.put("state", "API_UNAVAILABLE")
            return try {
                val names = native.supportedVendorParameters
                val descriptors = JSONArray()
                for (name in names.take(16)) {
                    val row = JSONObject().put("name", name.take(256)).put("nameTruncated", name.length > 256)
                    if (name.length <= 256) try {
                        val description = native.getParameterDescriptor(name)
                        row.put("descriptorName", description?.name?.take(256) ?: JSONObject.NULL).put("type", description?.type ?: JSONObject.NULL)
                    } catch (error: Exception) { row.put("state", "READ_FAILED").put("reason", error.javaClass.simpleName) }
                    descriptors.put(row)
                }
                result.put("state", "OBSERVED").put("totalNames", names.size).put("truncated", names.size > 16).put("descriptors", descriptors)
            } catch (error: Exception) { result.put("state", "READ_FAILED").put("reason", error.javaClass.simpleName) }
        }

        private fun missingFormat(phase: String, state: String): JSONObject = JSONObject()
            .put("phase", phase).put("state", state)
            .put(MediaFormat.KEY_ENCODER_DELAY, JSONObject().put("state", state))
            .put(MediaFormat.KEY_ENCODER_PADDING, JSONObject().put("state", state))

        private fun profileJSON(snapshot: AudioAacProfile.Snapshot): JSONObject {
            fun fact(value: AudioAacProfile.Fact<*>) = JSONObject().put("state", value.state.name).put("value", value.value ?: JSONObject.NULL)
            fun format(value: AudioAacProfile.Format?): Any = value?.let { JSONObject()
                .put("mime", fact(it.mime)).put("rate", fact(it.rate)).put("channels", fact(it.channels))
                .put("csd0", fact(it.csd0)).put("batchMaximum", fact(it.batchMaximum)).put("batchThreshold", fact(it.batchThreshold)) } ?: JSONObject.NULL
            val setup = snapshot.setup; val selection = setup?.selection
            return JSONObject().put("generation", snapshot.generation).put("eligibility", snapshot.eligibility.name)
                .put("conflict", snapshot.conflict).put("api", selection?.api ?: JSONObject.NULL)
                .put("name", selection?.let { fact(it.name) } ?: JSONObject.NULL)
                .put("canonicalName", selection?.let { fact(it.canonicalName) } ?: JSONObject.NULL)
                .put("encoder", selection?.let { fact(it.encoder) } ?: JSONObject.NULL)
                .put("multipleSupported", selection?.let { fact(it.multipleSupported) } ?: JSONObject.NULL)
                .put("multipleRequired", selection?.let { fact(it.multipleRequired) } ?: JSONObject.NULL)
                .put("request", selection?.request?.let { JSONObject().put("rate", it.rate).put("channels", it.channels)
                    .put("aacProfile", it.aacProfile).put("bitrate", it.bitrate).put("maxInputBytes", it.maxInputBytes)
                    .put("batchMaximum", fact(it.batchMaximum)).put("batchThreshold", fact(it.batchThreshold)) } ?: JSONObject.NULL)
                .put("configuredInput", format(setup?.input)).put("configuredOutput", format(setup?.output))
                .put("outputFormat", format(snapshot.output)).put("terminalOutput", format(snapshot.terminalOutput))
                .put("nativeSourceQualified", false).put("sdkReceiptIssued", false).put("appAdmission", false)
        }

        private fun format(phase: String, value: MediaFormat): JSONObject {
            val result = JSONObject().put("phase", phase).put("reported", value.toString().take(4096))
            for (key in listOf(MediaFormat.KEY_SAMPLE_RATE, MediaFormat.KEY_CHANNEL_COUNT, MediaFormat.KEY_AAC_PROFILE)) {
                try { result.put(key, if (value.containsKey(key)) value.getInteger(key) else JSONObject.NULL) }
                catch (error: Exception) { result.put(key, "READ_FAILED:" + error.javaClass.simpleName) }
            }
            for (key in listOf("csd-0", "csd-1")) {
                try {
                    val buffer = value.getByteBuffer(key)?.duplicate()
                    if (buffer != null) {
                        check(buffer.remaining() <= 1024)
                        val bytes = ByteArray(buffer.remaining()); buffer.get(bytes)
                        result.put(key + "Hex", bytes.joinToString("") { "%02x".format(it) })
                    }
                } catch (error: Exception) { result.put(key, "READ_FAILED:" + error.javaClass.simpleName) }
            }
            for (key in listOf(MediaFormat.KEY_ENCODER_DELAY, MediaFormat.KEY_ENCODER_PADDING)) {
                val trim = JSONObject()
                if (Build.VERSION.SDK_INT < 30) trim.put("state", "API_UNAVAILABLE")
                else try {
                    if (value.containsKey(key)) {
                        val reported = value.getInteger(key)
                        val maximum = if (key == MediaFormat.KEY_ENCODER_DELAY) 8191 else 1023
                        trim.put("state", if (reported < 0 || reported > maximum) "UNSUPPORTED" else "VALUE").put("reported", reported)
                    } else trim.put("state", "ABSENT")
                } catch (_: ClassCastException) { trim.put("state", "WRONG_TYPE") }
                catch (error: Exception) { trim.put("state", "READ_FAILED") }
                result.put(key, trim)
            }
            return result
        }
    }
}
