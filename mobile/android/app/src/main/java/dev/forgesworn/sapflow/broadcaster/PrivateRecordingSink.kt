package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.BindingException
import dev.forgesworn.sapflow.NativeRecordingException
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
    private val nativeCleanupUnknown = AtomicBoolean(false)
    private val lock = ReentrantLock()
    private var muxer: NativeRecordingMuxer? = null // worker, or bounded cleanup lock
    // Legacy callers did not supply quality/show ownership. Never invent their
    // profile; the application passes one explicit Show across rotation epochs.
    private val identity = show?.nextIdentity() ?: RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "UNSPECIFIED")
    private val intent = show?.intent ?: CaptureIntent.DEFAULT
    private val filename = identity.filename
    private val receipt = AtomicReference<RecordingCatalogue.Writer?>()
    @Volatile private var source: RecordingSourceGate.Recorder? = null
    /** Bound once by the real capture owner, before any media is consumed. */
    internal fun bindCaptureSource(gate: RecordingSourceGate): RecordingSourceGate.Recorder = lock.withLock {
        check(!closed.get() && receipt.get() == null && muxer == null && source == null)
        gate.bindRecorder(this).also { source = it }
    }


    override fun start() {
        try { check(!closed.get() && started.compareAndSet(false, true)) { "Recording already started or closed" } }
        catch (error: Throwable) { source?.failed(); throw error }
    }

    override fun consume(sample: MediaSample) {
        try {
            check(started.get() && !closed.get()) { "Recording not accepting samples" }
            lock.withLock {
                check(!closed.get()) { "Recording stopped" }
                if (receipt.get() == null) {
                    val writer = (catalogue ?: RecordingCatalogue(directory)).begin(identity)
                    receipt.set(writer)
                    source?.let(writer::bindSource)
                    check(!closed.get()) { "Recording stopped during storage admission" }
                }
                check(!nativeCleanupUnknown.get()) { "Native recording ownership is unknown" }
                val native = muxer ?: acquireMuxer()
                native.push(sample)
                if (drain(native)) {
                    message.set("Recording private MP4 · ${native.stats().durationMs / 1000uL}s")
                }
            }
        } catch (error: Throwable) {
            source?.failed()
            throw error
        } finally { if (closed.get()) runCatching { close() } }
    }

    /** Typed Rust validation failures return no handle. Unexpected interop or
     * wrapper acquisition failure cannot prove the acquired handle was closed. */
    private fun acquireMuxer(): NativeRecordingMuxer {
        nativeCleanupUnknown.set(true)
        try {
            val native = NativeRecordingMuxer(intent.hasVideo, intent.hasAudio, 0uL, 8uL * 1024uL * 1024uL, 1000u)
            muxer = native
            nativeCleanupUnknown.set(false)
            return native
        } catch (error: BindingException) { nativeCleanupUnknown.set(false); throw error }
        catch (error: NativeRecordingException) { nativeCleanupUnknown.set(false); throw error }
        catch (error: Throwable) { receipt.get()?.retainUncertainCleanup(this); throw error }
    }

    private fun closeMuxer(native: NativeRecordingMuxer) {
        // One checked close only. A throwing close retains the exact owner;
        // cleanup cannot retry a possibly released native handle.
        nativeCleanupUnknown.set(true)
        try { native.close() }
        catch (error: Throwable) { receipt.get()?.retainUncertainCleanup(this); throw error }
        muxer = null
        nativeCleanupUnknown.set(false)
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
    override fun finish() = finishOwned(null)
    internal fun finish(permit: RecordingSourceGate.FinishPermit) = finishOwned(permit)
    /** Preserve the legacy readable accepted prefix, with no FINALISED/source
     * admission. Called only on the existing recording worker. */
    internal fun finishIncompletePrefix() {
        source?.failed()
        finishOwned(null, incompletePrefix = true)
    }
    private fun finishOwned(permit: RecordingSourceGate.FinishPermit?, incompletePrefix: Boolean = false) {
        try {
            lock.withLock {
                if (closed.get()) return
                val boundSource = source
                if (incompletePrefix) check(permit == null)
                else if (boundSource != null) {
                    check(boundSource.owns(this) && permit != null && permit.recorder === boundSource) { "Exact source finish permit required" }
                } else check(permit == null) { "Unbound recorder cannot accept a foreign source permit" }
                check(!nativeCleanupUnknown.get()) { "Native recording ownership is unknown" }
                val native = muxer
                if (native == null) {
                    check(file.get() == null && !fileCleanupUnknown.get()) { "No-file closure requires actual file absence" }
                    receipt.get()?.let { writer ->
                        // A typed constructor refusal may leave a known lease,
                        // but cannot authorise the source no-input success path.
                        check(incompletePrefix) { "Storage admission occurred without a native recorder" }
                        writer.cancel(); writer.ownersClosed()
                    }
                    if (boundSource != null && !incompletePrefix) boundSource.commitEmpty(checkNotNull(permit))
                    message.set("No recording samples · no file written")
                } else {
                    native.finish()
                    drain(native)
                    val duration = native.stats().durationMs
                    check(duration <= Long.MAX_VALUE.toULong())
                    val output = checkNotNull(file.get()) { "Recording file is unavailable" }
                    try { output.finish() } // Remains reachable by bounded abort during force/close.
                    catch (error: Throwable) { fileCleanupUnknown.set(true); throw error }
                    file.compareAndSet(output, null)
                    closeMuxer(native)
                    // The terminal CAS rejects cancellation that won before
                    // the actual mux/file completion. Receipt IO stays here on
                    // the recorder worker, after every accepted media write.
                    val writer = checkNotNull(receipt.get())
                    if (incompletePrefix) {
                        writer.cancel(); writer.ownersClosed()
                        message.set("Closed incomplete private MP4 prefix · $filename · playback unchecked")
                    } else {
                        if (boundSource == null) writer.finalised(duration.toLong())
                        else writer.finalised(duration.toLong(), checkNotNull(permit))
                        saved.set(true)
                        message.set("Finalised private MP4 · ${duration / 1000uL}s · $filename · playback unchecked")
                    }
                }
                closed.set(true)
            }
        } catch (error: Throwable) {
            source?.failed()
            try { close() } catch (cleanup: Throwable) { error.addSuppressed(cleanup) }
            throw error
        }
    }

    /** Bounded abort, including from the stop deadline while worker IO stalls. */
    override fun close() {
        source?.failed() // Scalar loss/cancellation cut precedes cleanup IO.
        closed.set(true)
        receipt.get()?.cancel()
        val fileClosed = if (fileCleanupUnknown.get() || !fileClosing.compareAndSet(false, true)) false else try {
            val output = file.get()
            // RecordingFile.abort clears its channel before checked close. An
            // unconfirmed return must stay sticky even when a retry would be
            // a no-op. Retain this exact sink/file and never retry that owner.
            fileCleanupUnknown.set(true)
            try {
                output?.abort()
                file.compareAndSet(output, null)
                fileCleanupUnknown.set(false)
                true
            } catch (_: Throwable) { false }
        } finally { fileClosing.set(false) }
        val released = try {
            lock.cleanupWithin(100) {
                check(!nativeCleanupUnknown.get()) { "Native recording cleanup is retained" }
                muxer?.let { native -> runCatching { native.cancel() }; closeMuxer(native) }
            }
        } catch (_: Throwable) { false }
        if (!saved.get()) message.set("Recording stopped · any written prefix retained · $filename")
        if (!released || !fileClosed) receipt.get()?.retainUncertainCleanup(this)
        check(released && fileClosed) { "Recording worker cleanup exceeded its deadline or file close failed" }
        // An unfinished writing receipt becomes INCOMPLETE only after the
        // actual owners release this lease. Unknown cleanup keeps capacity.
        receipt.get()?.ownersClosed()
    }
}
