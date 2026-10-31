package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec as PlatformCodec
import android.media.MediaFormat
import android.os.Handler
import android.os.Build
import android.os.SystemClock
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeAacTimingViews
import dev.forgesworn.sapflow.BindingException
import java.nio.ByteBuffer
import java.util.ArrayDeque

/** One actual platform AAC generation. The run owns the Rust descriptor and
 * sample clock; neither is recreated when this platform encoder is retired.
 * Fixed transfer slots carry scalar provenance without another payload copy,
 * per-unit metadata allocation or a MediaSample timestamp FFI query. */
internal class EncodedAudioEpoch(
    private val owner: Handler,
    private val callbacks: Handler,
    private val codecDescriptor: RustCodec,
    private val sampleClock: AudioGenerationClock,
    private val clockGeneration: AudioGenerationClock.Generation,
    private val nativeGeneration: Long,
    private val observation: AudioGenerationObservation,
    private val onInput: (Int) -> Unit,
    private val onFormat: (RustCodec) -> Unit,
    private val onSample: (MediaSample, Long, Long) -> Unit,
    private val onValidated: () -> Unit,
    private val onPriming: () -> Unit,
    private val onRawOutput: (Long) -> Unit,
    private val onEos: () -> Unit,
    private val onFailure: (String) -> Unit,
    private val onHandoffIdle: () -> Unit,
    private val sampleMode: AudioSampleMode = AudioSampleMode.LEGACY_PUBLISHING,
    private val sourceCursor: AudioSourceCursor? = null,
    private val onTimingViews: (NativeAacTimingViews, Long, Long) -> Unit = { _, _, _ -> error("Source-preserving AAC consumer is absent") },
    private val nativeAac: AudioNativeAacGeneration? = null,
    private val nativeRun: AudioNativeAacRun? = null,
    private val generationOwner: Any? = null,
    private val nativeRunOwner: Any? = null,
    private val actualCodec: PlatformCodec? = null,
) : PlatformCodec.Callback() {
    private var formatConfirmed = false
    private var started = false
    private var closed = false
    private val conversion = DelayDiagnostics()
    private val slotGuard = Any()
    private val originalFactory: (ByteBuffer, Long) -> MediaSample = { bytes, sourceTick ->
        codecDescriptor.sample(bytes, sourceTick, sourceTick, 1024u, 48_000u, false)
    }
    private val viewsFactory: (MediaSample, Long, Long?) -> NativeAacTimingViews = { original, nativePtsUs, publishingTick ->
        original.aacTimingViews(nativeGeneration.toULong(), nativePtsUs, publishingTick)
    }
    private val noHandleRejection: (Throwable) -> Boolean = { it is BindingException }
    private val originalReturned: () -> Unit = { observation.constructed(nativeGeneration) }
    private inner class Transfer(val slotIndex: Int, var receiptSerial: Long = 0L) : AudioTimingTransfer<MediaSample, NativeAacTimingViews, ByteBuffer>(
        originalFactory, viewsFactory, noHandleRejection, originalReturned) {
        override fun acquisitionsReturned() {
            if (receiptSerial != 0L) nativeAac?.acquired(checkNotNull(generationOwner), checkNotNull(actualCodec),
                this@EncodedAudioEpoch, receiptSerial, slotIndex)
        }
        override fun close() {
            super.close()
            if (receiptSerial != 0L) nativeAac?.transferClosed(checkNotNull(generationOwner), checkNotNull(actualCodec),
                this@EncodedAudioEpoch, receiptSerial, slotIndex)
            receiptSerial = 0L
            synchronized(slotGuard) { free.addLast(this) }
        }
    }
    // Keep quarantined owners reachable even when they cannot rejoin free.
    private val slots = Array(AudioHandoffBounds.SLOTS) { Transfer(it) }
    private val free = ArrayDeque<Transfer>().apply { slots.forEach { addLast(it) } }
    private fun discard(slot: Transfer, problem: Exception) {
        if (slot.ownershipUnknown) return // Failure fences this generation, never guess a native retry.
        try {
            if (slot.reusable) {
                slot.receiptSerial = 0L
                synchronized(slotGuard) { free.addLast(slot) }
            } else slot.close()
        } catch (cleanup: Throwable) { problem.addSuppressed(cleanup) }
    }
    private val handoff = CoalescedOwnedQueue<Transfer>(AudioHandoffBounds.SLOTS, AudioHandoffBounds.BYTES, AudioHandoffBounds.MAX_AGE_NS,
        SystemClock::elapsedRealtimeNanos, callbacks::post,
        {
            if (it.receiptSerial != 0L) nativeAac?.handoffEntered(checkNotNull(generationOwner), checkNotNull(actualCodec),
                this, it.receiptSerial, it.slotIndex)
            observation.handoffEntered(nativeGeneration)
            if (sampleMode == AudioSampleMode.PRESERVE_SOURCE_UNATTESTED) onTimingViews(it.timingViews, it.tick, nativeGeneration)
            else onSample(it.sample, it.tick, nativeGeneration)
            if (it.receiptSerial != 0L) nativeAac?.handoffReturned(checkNotNull(generationOwner), checkNotNull(actualCodec),
                this, it.receiptSerial, it.slotIndex)
            observation.handoffReturned(nativeGeneration)
        },
        { report("Encoded audio handoff failed", it) },
        { callbacks.post { onHandoffIdle() } })
    val outstanding get() = AudioSourceRetirement.outstanding(handoff.outstanding, slots)
    val diagnostics get() = "AAC generation $nativeGeneration conversion ${conversion.text()} · handoff ${handoff.diagnostics}"

    init {
        if (nativeAac != null) require(sampleMode == AudioSampleMode.PRESERVE_SOURCE_UNATTESTED &&
            generationOwner != null && nativeRunOwner != null && actualCodec != null)
        if (nativeRun != null) require(nativeAac != null)
    }
    fun begin() { check(owner.post { if (!closed) started = true }) { "AAC owner stopped" } }
    override fun onInputBufferAvailable(codec: PlatformCodec, index: Int) { if (!closed) onInput(index) }
    override fun onOutputFormatChanged(codec: PlatformCodec, format: MediaFormat) {
        if (closed) return
        observeTrims(codec,format,AudioGenerationObservation.Phase.FORMAT)
        try {
            check(!formatConfirmed) { "AAC format changed within a native generation" }
            val asc = format.getByteBuffer("csd-0") ?: error("Encoder supplied no AAC configuration")
            require(format.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48_000 && format.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
            require(asc.remaining() == 2 && (asc.get(asc.position()).toInt() and 255) == 0x11 &&
                (asc.get(asc.position() + 1).toInt() and 255) == 0x88) { "Only indexed AAC-LC configuration is supported" }
            formatConfirmed = true
            callbacks.post { onFormat(codecDescriptor) }
        } catch (error: Exception) { report("AAC configuration rejected", error) }
    }
    override fun onOutputBufferAvailable(codec: PlatformCodec, index: Int, info: PlatformCodec.BufferInfo) {
        if(closed)return // stop on this callback owner invalidated all native buffer indices.
        val startNs = System.nanoTime()
        val isEos = info.flags and PlatformCodec.BUFFER_FLAG_END_OF_STREAM != 0
        var transfer: Transfer? = null
        var receiptSerial = 0L
        try {
            observation.output(nativeGeneration,info.size,info.presentationTimeUs,info.flags,
                info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG != 0,isEos,started,info.offset)
            val borrowed = if (nativeAac != null && started && info.size > 0 &&
                info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG == 0)
                codec.getOutputBuffer(index) ?: error("AAC output buffer unavailable") else null
            nativeAac?.let { collector ->
                receiptSerial = collector.output(checkNotNull(generationOwner), codec, this, info.offset, info.size,
                    info.flags, started, borrowed?.capacity() ?: -1)
                if (receiptSerial == 0L) collector.fault(checkNotNull(generationOwner), checkNotNull(actualCodec))
            }
            if (isEos) {
                // The exact native output index remains borrowed for this one
                // terminal format read. No decoded length supplies missing trim.
                val terminal = if (Build.VERSION.SDK_INT >= 30) try { codec.getOutputFormat(index) } catch (_: Exception) { null } else null
                observeTrims(codec,terminal,AudioGenerationObservation.Phase.OUTPUT_EOS)
            }
            if (!closed && started && info.size > 0 && info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                check(formatConfirmed) { "AAC output preceded its validated native format" }
                onRawOutput(info.presentationTimeUs)
                require(info.size in 1..4096) { "Mono AAC access unit exceeded 4096 bytes" }
                val tick = sampleClock.output(clockGeneration,info.presentationTimeUs)
                if (sampleMode == AudioSampleMode.PRESERVE_SOURCE_UNATTESTED && tick != AudioGenerationClock.STALE) {
                    val buffer = borrowed ?: codec.getOutputBuffer(index) ?: error("AAC output buffer unavailable")
                    observation.borrowedOutputRange(nativeGeneration, info.offset, info.size, buffer.capacity())
                    buffer.position(info.offset); buffer.limit(info.offset + info.size)
                    val slot = synchronized(slotGuard) { free.pollFirst() } ?: error("AAC scalar handoff capacity exhausted")
                    transfer = slot
                    slot.receiptSerial = receiptSerial
                    slot.preserve(buffer.slice(), checkNotNull(sourceCursor).reserve(tick), info.presentationTimeUs, tick)
                    onValidated()
                    // Negative/early native output was retained for recording.
                    // It is deliberately absent from publishing and readiness.
                }
                else if (tick == AudioGenerationClock.PRIMING) {
                    observation.primingFiltered(nativeGeneration)
                    onPriming()
                }
                else if (tick != AudioGenerationClock.STALE) {
                    val buffer = codec.getOutputBuffer(index) ?: error("AAC output buffer unavailable")
                    observation.borrowedOutputRange(nativeGeneration,info.offset,info.size,buffer.capacity())
                    buffer.position(info.offset); buffer.limit(info.offset + info.size)
                    val encoded = codecDescriptor.sample(buffer.slice(),tick,tick,1024u,48_000u,false)
                    observation.constructed(nativeGeneration)
                    try {
                        val slot = synchronized(slotGuard) { free.pollFirst() } ?: error("AAC scalar handoff capacity exhausted")
                        slot.attachLegacy(encoded, tick); transfer = slot
                    } catch (error: Exception) { encoded.close(); throw error }
                    onValidated()
                } else observation.staleFiltered(nativeGeneration)
            }
        } catch (error: Exception) {
            transfer?.let { discard(it,error) }; transfer = null; report("AAC sample rejected",error)
        } finally {
            val released = runCatching { codec.releaseOutputBuffer(index,false) }
            if (released.isSuccess) {
                if (receiptSerial != 0L) nativeAac?.outputReleased(checkNotNull(generationOwner), codec, this, receiptSerial)
            } else if (!closed) {
                val problem = released.exceptionOrNull() as? Exception ?: IllegalStateException("AAC release failed",released.exceptionOrNull())
                transfer?.let { discard(it,problem) }; transfer = null
                report("AAC output release failed",problem)
            }
            conversion.record(System.nanoTime()-startNs)
        }
        transfer?.let { slot -> try {
            observation.handoffAttempt(nativeGeneration)
            handoff.offer(slot,info.size.toLong())
            observation.handoffOfferReturned(nativeGeneration)
        } catch (error: Exception) { report("AAC handoff rejected",error) } }
        if (isEos && !closed) callbacks.post { onEos() }
    }
    /** The same cold facts feed diagnostics and qualification exactly once. */
    private fun observeTrims(codec: PlatformCodec, format: MediaFormat?, phase: AudioGenerationObservation.Phase) {
        val facts = AudioAacEncoderConfiguration.format(format,
            if (phase == AudioGenerationObservation.Phase.OUTPUT_EOS && Build.VERSION.SDK_INT < 30) AudioAacProfile.State.API_UNAVAILABLE else AudioAacProfile.State.READ_FAILED)
        observation.profileFormat(nativeGeneration, phase, facts)
        val leading = observeTrim(format,phase,AudioGenerationObservation.Trim.LEADING,MediaFormat.KEY_ENCODER_DELAY)
        val padding = observeTrim(format,phase,AudioGenerationObservation.Trim.PADDING,MediaFormat.KEY_ENCODER_PADDING)
        val receiptPhase = if (phase == AudioGenerationObservation.Phase.FORMAT)
            AudioAacProfile.Phase.OUTPUT_FORMAT else AudioAacProfile.Phase.OUTPUT_EOS
        val accepted = nativeAac?.format(checkNotNull(generationOwner), codec, this, receiptPhase, facts, leading, padding)
        if (accepted == true && phase == AudioGenerationObservation.Phase.FORMAT) {
            val head = leading.value
            if (leading.state == AudioAacProfile.State.VALUE && head != null && head in 0..8191 && nativeRun != null &&
                nativeRun.leading(checkNotNull(nativeRunOwner), checkNotNull(generationOwner), head) != true) nativeRun.fail()
        }
    }
    private fun observeTrim(format: MediaFormat?, phase: AudioGenerationObservation.Phase,
        trim: AudioGenerationObservation.Trim, key: String): AudioAacProfile.Fact<Int> {
        val api = Build.VERSION.SDK_INT
        if (api < 30) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.API_UNAVAILABLE)
            return AudioAacProfile.missing(AudioAacProfile.State.API_UNAVAILABLE)
        }
        if (format == null) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
            return AudioAacProfile.missing(AudioAacProfile.State.READ_FAILED)
        }
        val present = try { format.containsKey(key) } catch (_: Exception) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
            return AudioAacProfile.missing(AudioAacProfile.State.READ_FAILED)
        }
        if (!present) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.ABSENT)
            return AudioAacProfile.missing(AudioAacProfile.State.ABSENT)
        }
        return try {
            val value = format.getInteger(key)
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.VALUE,value)
            AudioAacProfile.known(value)
        } catch (_: ClassCastException) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.WRONG_TYPE)
            AudioAacProfile.missing(AudioAacProfile.State.WRONG_TYPE)
        } catch (_: Exception) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
            AudioAacProfile.missing(AudioAacProfile.State.READ_FAILED)
        }
    }
    override fun onError(codec: PlatformCodec, error: PlatformCodec.CodecException) { if (!closed) report("AAC encoder failed",error) }
    private fun report(prefix: String,error: Exception) {
        // Qualification failure is synchronous; no new publishing exception or
        // media retry is introduced by this scalar collector.
        nativeAac?.fault(checkNotNull(generationOwner), checkNotNull(actualCodec))
        onFailure("$prefix: ${error.message ?: error.javaClass.simpleName}")
    }
    /** Serial callback barrier. Descriptor remains borrowed from the run. */
    fun quiesce(stopNative:()->Unit,onQuiesced: (Boolean) -> Unit) {
        check(owner.post {
            // Producers have actually joined. The callback owner performs the
            // stop between callbacks, so no later callback can release an index
            // invalidated by native stop/release. This is not a flush.
            closed=true;started=false
            val stopped=runCatching(stopNative).isSuccess
            callbacks.post { onQuiesced(stopped) }
        }) { "AAC owner stopped before quiescence" }
    }
    fun finishHandoff(drain: Boolean) { handoff.close(drain) }
}
