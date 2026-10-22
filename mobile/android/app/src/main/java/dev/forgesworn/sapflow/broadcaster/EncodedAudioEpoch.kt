package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec as PlatformCodec
import android.media.MediaFormat
import android.os.Handler
import android.os.SystemClock
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import dev.forgesworn.sapflow.MediaSample
import java.nio.ByteBuffer

/** Configuration, AAC clock and compressed sample construction belong solely
 * to the AUDIO callback owner. Native output is released before a fanout turn.
 * quiesce transfers the descriptor only after this serial owner has stopped
 * constructing samples; the controller closes it after all sink owners exit. */
internal class EncodedAudioEpoch(
    private val owner: Handler,
    private val callbacks: Handler,
    private val onInput: (Int) -> Unit,
    private val onFormat: (RustCodec) -> Unit,
    private val onSample: (MediaSample) -> Unit,
    private val onValidated: () -> Unit,
    private val onPriming: () -> Unit,
    private val onRawOutput: (Long) -> Unit,
    private val onEos: () -> Unit,
    private val onFailure: (String) -> Unit,
    private val onHandoffIdle: () -> Unit,
) : PlatformCodec.Callback() {
    private var descriptor: RustCodec? = null // owner only
    private val clock = CaptureClock()
    private var started = false // owner only
    private var closed = false // owner only
    private val conversion = DelayDiagnostics()
    private val handoff = CoalescedOwnedQueue<MediaSample>(16, 64 * 1024, 250_000_000,
        SystemClock::elapsedRealtimeNanos, callbacks::post, onSample,
        { onFailure("Encoded audio handoff failed: ${it.message}") },
        { callbacks.post { onHandoffIdle() } })
    val outstanding get() = handoff.outstanding
    val diagnostics get() = "AAC conversion ${conversion.text()} · handoff ${handoff.diagnostics}"

    fun begin() { check(owner.post { if (!closed) { clock.start(0); started = true } }) { "AAC owner stopped" } }

    override fun onInputBufferAvailable(codec: PlatformCodec, index: Int) { if (!closed) onInput(index) }

    override fun onOutputFormatChanged(codec: PlatformCodec, format: MediaFormat) {
        if (closed) return
        try {
            check(descriptor == null) { "AAC format changed; prepare a new epoch" }
            val asc = format.getByteBuffer("csd-0") ?: error("Encoder supplied no AAC configuration")
            require(format.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48_000 && format.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
            require(asc.remaining() == 2 && (asc.get(asc.position()).toInt() and 255) == 0x11 && (asc.get(asc.position() + 1).toInt() and 255) == 0x88) { "Only indexed AAC-LC configuration is supported" }
            val direct = ByteBuffer.allocateDirect(2).apply { put(asc.duplicate()); flip() }
            val config = RustCodec.aac(direct)
            descriptor = config
            callbacks.post { onFormat(config) }
        } catch (error: Exception) { report("AAC configuration rejected", error) }
    }

    override fun onOutputBufferAvailable(codec: PlatformCodec, index: Int, info: PlatformCodec.BufferInfo) {
        val startNs = System.nanoTime()
        val eos = info.flags and PlatformCodec.BUFFER_FLAG_END_OF_STREAM != 0
        var encoded: MediaSample? = null
        try {
            if (!closed && started && info.size > 0 && info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                onRawOutput(info.presentationTimeUs)
                require(info.size in 1..4096) { "Mono AAC access unit exceeded 4096 bytes" }
                val tick = clock.audio(info.presentationTimeUs)
                if (tick == null) onPriming()
                else {
                    val buffer = codec.getOutputBuffer(index) ?: error("AAC output buffer unavailable")
                    buffer.position(info.offset); buffer.limit(info.offset + info.size)
                    encoded = (descriptor ?: error("AAC configuration not ready")).sample(buffer.slice(), tick, tick, 1024u, 48_000u, false)
                    onValidated()
                }
            }
        } catch (error: Exception) {
            encoded?.close(); encoded = null
            report("AAC sample rejected", error)
        } finally {
            val released = runCatching { codec.releaseOutputBuffer(index, false) }
            if (released.isFailure && !closed) {
                encoded?.close(); encoded = null
                report("AAC output release failed", released.exceptionOrNull() as? Exception ?: IllegalStateException("AAC release failed"))
            }
            conversion.record(System.nanoTime() - startNs)
        }
        // A fanout continuation is posted only AFTER actual platform release.
        // offer takes the immutable owner even when its bounds refuse it.
        encoded?.let { sample ->
            try { handoff.offer(sample, info.size.toLong()) }
            catch (error: Exception) { report("AAC handoff rejected", error) }
        }
        if (eos && !closed) callbacks.post { onEos() }
    }

    override fun onError(codec: PlatformCodec, error: PlatformCodec.CodecException) { if (!closed) report("AAC encoder failed", error) }

    private fun report(prefix: String, error: Exception) { callbacks.post { onFailure("$prefix: ${error.message ?: error.javaClass.simpleName}") } }

    fun quiesce(onQuiesced: (RustCodec?) -> Unit) {
        check(owner.post {
            closed = true; started = false
            val transferred = descriptor; descriptor = null
            callbacks.post { onQuiesced(transferred) }
        }) { "AAC owner stopped before quiescence" }
    }

    fun finishHandoff(drain: Boolean) { handoff.close(drain) }
}
