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
internal class PrivateRecordingSink(private val directory: Path, private val catalogue: RecordingCatalogue? = null,
    show: RecordingCatalogue.Show? = null) : EncodedSink {
    private val message = AtomicReference("Private recording prepared · press Start")
    override val status: String get() = message.get()
    private val started = AtomicBoolean(false)
    private val closed = AtomicBoolean(false)
    private val saved = AtomicBoolean(false)
    private val file = AtomicReference<RecordingFile?>()
    private val fileClosing = AtomicBoolean(false)
    private val fileCleanupUnknown = AtomicBoolean(false)
    private val lock = ReentrantLock()
    private var muxer: NativeRecordingMuxer? = null // worker, or bounded cleanup lock
    // Legacy callers did not supply quality/show ownership. Never invent their
    // profile; the application passes one explicit Show across rotation epochs.
    private val identity = show?.nextIdentity() ?: RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "UNSPECIFIED")
    private val filename = identity.filename
    private val receipt = AtomicReference<RecordingCatalogue.Writer?>()

    override fun start() { check(!closed.get() && started.compareAndSet(false, true)) { "Recording already started or closed" } }

    override fun consume(sample: MediaSample) {
        check(started.get() && !closed.get()) { "Recording not accepting samples" }
        try {
            lock.withLock {
                check(!closed.get()) { "Recording stopped" }
                if (receipt.get() == null) {
                    receipt.set((catalogue ?: RecordingCatalogue(directory)).begin(identity))
                    check(!closed.get()) { "Recording stopped during storage admission" }
                }
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
                    val duration = native.stats().durationMs
                    check(duration <= Long.MAX_VALUE.toULong())
                    val output = checkNotNull(file.get()) { "Recording file is unavailable" }
                    try { output.finish() } // Remains reachable by bounded abort during force/close.
                    catch (error: Exception) { fileCleanupUnknown.set(true); throw error }
                    file.compareAndSet(output, null)
                    native.close()
                    muxer = null
                    // The terminal CAS rejects cancellation that won before
                    // the actual mux/file completion. Receipt IO stays here on
                    // the recorder worker, after every accepted media write.
                    checkNotNull(receipt.get()).finalised(duration.toLong())
                    saved.set(true)
                    message.set("Finalised private MP4 · ${duration / 1000uL}s · $filename · playback unchecked")
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
        receipt.get()?.cancel()
        val fileClosed = if (fileCleanupUnknown.get() || !fileClosing.compareAndSet(false, true)) false else try {
            val output = file.get()
            try { output?.abort(); file.compareAndSet(output, null); true }
            catch (_: Exception) { fileCleanupUnknown.set(true); false }
        } finally { fileClosing.set(false) }
        val released = lock.cleanupWithin(100) {
            muxer?.let { runCatching { it.cancel() }; it.close() }
            muxer = null
        }
        if (!saved.get()) message.set("Recording stopped · any written prefix retained · $filename")
        if (!released || !fileClosed) receipt.get()?.retainUncertainCleanup()
        check(released && fileClosed) { "Recording worker cleanup exceeded its deadline or file close failed" }
        // An unfinished writing receipt becomes INCOMPLETE only after the
        // actual owners release this lease. Unknown cleanup keeps capacity.
        receipt.get()?.ownersClosed()
    }
}
