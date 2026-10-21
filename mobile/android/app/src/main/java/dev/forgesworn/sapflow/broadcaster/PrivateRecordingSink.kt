package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeRecordingMuxer
import java.nio.file.Path
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/** Same encoded handles as the publisher, with file/mux work confined to the
 * existing recording worker. No network, second encoder or media staging queue. */
internal class PrivateRecordingSink(private val directory: Path) : EncodedSink {
    private val message = AtomicReference("Private recording prepared · press Start")
    override val status: String get() = message.get()
    private val started = AtomicBoolean(false)
    private val closed = AtomicBoolean(false)
    private val saved = AtomicBoolean(false)
    private val file = AtomicReference<RecordingFile?>()
    private val lock = ReentrantLock()
    private var muxer: NativeRecordingMuxer? = null // worker, or bounded cleanup lock
    private val filename = "session-${UUID.randomUUID()}.mp4"

    override fun start() { check(!closed.get() && started.compareAndSet(false, true)) { "Recording already started or closed" } }

    override fun consume(sample: MediaSample) {
        check(started.get() && !closed.get()) { "Recording not accepting samples" }
        try {
            lock.withLock {
                check(!closed.get()) { "Recording stopped" }
                val native = muxer ?: NativeRecordingMuxer(true, true, 0uL, 8uL * 1024uL * 1024uL, 1000u).also { muxer = it }
                native.push(sample)
                if (drain(native)) {
                    message.set("Recording private MP4 · ${native.stats().durationMs / 1000uL}s")
                }
            }
        } finally { if (closed.get()) runCatching { close() } }
    }

    private fun drain(native: NativeRecordingMuxer): Boolean {
        var wrote = false
        while (true) {
            val output = native.takeOutput() ?: return wrote
            val destination = file.get() ?: RecordingFile(directory, filename).also {
                if (closed.get()) { it.abort(); error("Recording cancelled") }
                file.set(it)
            }
            destination.write(output.bytes, output.rewriteHeader)
            wrote = true
        }
    }

    /** Only the recorder worker invokes graceful finalisation. */
    override fun finish() {
        try {
            lock.withLock {
                if (closed.get()) return
                val native = muxer
                if (native == null) {
                    message.set("No recording samples · no file written")
                } else {
                    native.finish()
                    drain(native)
                    val duration = native.stats().durationMs / 1000uL
                    file.get()?.finish()
                    file.set(null)
                    saved.set(true)
                    message.set("Saved private MP4 · ${duration}s · $filename")
                    native.close()
                    muxer = null
                }
                closed.set(true)
            }
        } catch (error: Exception) {
            close()
            throw error
        }
    }

    /** Bounded abort, including from the stop deadline while worker IO stalls. */
    override fun close() {
        closed.set(true)
        val fileClosed = runCatching { file.getAndSet(null)?.abort() }.isSuccess
        val released = lock.cleanupWithin(100) {
            muxer?.let { runCatching { it.cancel() }; it.close() }
            muxer = null
        }
        if (!saved.get()) message.set("Recording stopped · any written prefix retained · $filename")
        check(released && fileClosed) { "Recording worker cleanup exceeded its deadline or file close failed" }
    }
}
