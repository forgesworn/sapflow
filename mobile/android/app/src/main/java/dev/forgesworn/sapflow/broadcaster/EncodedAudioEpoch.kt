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
    private inner class Transfer : AudioTimingTransfer<MediaSample, NativeAacTimingViews, ByteBuffer>(
        originalFactory, viewsFactory, noHandleRejection, originalReturned) {
        override fun close() {
            super.close()
            synchronized(slotGuard) { free.addLast(this) }
        }
    }
    // Keep quarantined owners reachable even when they cannot rejoin free.
    private val slots = Array(16) { Transfer() }
    private val free = ArrayDeque<Transfer>().apply { slots.forEach { addLast(it) } }
    private fun discard(slot: Transfer, problem: Exception) {
        if (slot.ownershipUnknown) return // Failure fences this generation, never guess a native retry.
        try {
            if (slot.reusable) synchronized(slotGuard) { free.addLast(slot) } else slot.close()
        } catch (cleanup: Throwable) { problem.addSuppressed(cleanup) }
    }
    private val handoff = CoalescedOwnedQueue<Transfer>(16, 64 * 1024, 250_000_000,
        SystemClock::elapsedRealtimeNanos, callbacks::post,
        {
            observation.handoffEntered(nativeGeneration)
            if (sampleMode == AudioSampleMode.PRESERVE_SOURCE_UNATTESTED) onTimingViews(it.timingViews, it.tick, nativeGeneration)
            else onSample(it.sample, it.tick, nativeGeneration)
            observation.handoffReturned(nativeGeneration)
        },
        { onFailure("Encoded audio handoff failed: ${it.message}") },
        { callbacks.post { onHandoffIdle() } })
    val outstanding get() = AudioSourceRetirement.outstanding(handoff.outstanding, slots)
    val diagnostics get() = "AAC generation $nativeGeneration conversion ${conversion.text()} · handoff ${handoff.diagnostics}"

    fun begin() { check(owner.post { if (!closed) started = true }) { "AAC owner stopped" } }
    override fun onInputBufferAvailable(codec: PlatformCodec, index: Int) { if (!closed) onInput(index) }
    override fun onOutputFormatChanged(codec: PlatformCodec, format: MediaFormat) {
        if (closed) return
        observeTrims(format,AudioGenerationObservation.Phase.FORMAT)
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
        observation.output(nativeGeneration,info.size,info.presentationTimeUs,info.flags,
            info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG != 0,isEos,started,info.offset)
        if (isEos) {
            // One terminal format observation while this exact native output
            // index is still owned. No per-buffer metadata query or inferred EOF.
            val terminal = if (Build.VERSION.SDK_INT >= 30) try { codec.getOutputFormat(index) } catch (_: Exception) { null } else null
            observeTrims(terminal,AudioGenerationObservation.Phase.OUTPUT_EOS)
        }
        var transfer: Transfer? = null
        try {
            if (!closed && started && info.size > 0 && info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                check(formatConfirmed) { "AAC output preceded its validated native format" }
                onRawOutput(info.presentationTimeUs)
                require(info.size in 1..4096) { "Mono AAC access unit exceeded 4096 bytes" }
                val tick = sampleClock.output(clockGeneration,info.presentationTimeUs)
                if (sampleMode == AudioSampleMode.PRESERVE_SOURCE_UNATTESTED && tick != AudioGenerationClock.STALE) {
                    val buffer = codec.getOutputBuffer(index) ?: error("AAC output buffer unavailable")
                    observation.borrowedOutputRange(nativeGeneration, info.offset, info.size, buffer.capacity())
                    buffer.position(info.offset); buffer.limit(info.offset + info.size)
                    val slot = synchronized(slotGuard) { free.pollFirst() } ?: error("AAC scalar handoff capacity exhausted")
                    transfer = slot
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
            if (released.isFailure && !closed) {
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
        if (info.flags and PlatformCodec.BUFFER_FLAG_END_OF_STREAM != 0 && !closed) callbacks.post { onEos() }
    }
    /** Optional scalar diagnostics only: every failure stays unknown and
     * cannot enter the existing capture failure path. Called at format/EOS. */
    private fun observeTrims(format: MediaFormat?, phase: AudioGenerationObservation.Phase) {
        if (Build.VERSION.SDK_INT < 30) {
            observation.trim(nativeGeneration,phase,AudioGenerationObservation.Trim.LEADING,Build.VERSION.SDK_INT,AudioGenerationObservation.State.API_UNAVAILABLE)
            observation.trim(nativeGeneration,phase,AudioGenerationObservation.Trim.PADDING,Build.VERSION.SDK_INT,AudioGenerationObservation.State.API_UNAVAILABLE)
            return
        }
        observeTrim(format,phase,AudioGenerationObservation.Trim.LEADING,MediaFormat.KEY_ENCODER_DELAY)
        observeTrim(format,phase,AudioGenerationObservation.Trim.PADDING,MediaFormat.KEY_ENCODER_PADDING)
    }
    private fun observeTrim(format: MediaFormat?, phase: AudioGenerationObservation.Phase,
                            trim: AudioGenerationObservation.Trim, key: String) {
        val api = Build.VERSION.SDK_INT
        if (api < 30) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.API_UNAVAILABLE)
            return
        }
        if (format == null) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
            return
        }
        val present = try { format.containsKey(key) } catch (_: Exception) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
            return
        }
        if (!present) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.ABSENT)
            return
        }
        try {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.VALUE,format.getInteger(key))
        } catch (_: ClassCastException) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.WRONG_TYPE)
        } catch (_: Exception) {
            observation.trim(nativeGeneration,phase,trim,api,AudioGenerationObservation.State.READ_FAILED)
        }
    }
    override fun onError(codec: PlatformCodec, error: PlatformCodec.CodecException) { if (!closed) report("AAC encoder failed",error) }
    private fun report(prefix: String,error: Exception) {
        // The exact generation callback synchronously fences its run. Only
        // diagnostic delivery/teardown may wait for the controller handler.
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
