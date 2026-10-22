package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** One existing publishing epoch, no new worker, queue, timer or media owner.
 * Publishing failure discards subsequent publishing borrows; recording remains
 * independent. The controller owns asynchronous actual close on its usual path. */
internal class IsolatedPublishingSink(private val delegate: EncodedSink, private val failed: (Exception) -> Unit) : EncodedSink, SocketOutputProvider {
    override val socketOutput: SocketOutputSource? get() = runCatching { (delegate as? SocketOutputProvider)?.socketOutput }.getOrNull()
    private val unavailable = AtomicBoolean(false)
    private val cleanupFailure = AtomicReference<Exception?>()
    private val closure = AtomicReference<CompletableFuture<Unit>?>()
    override val status: String get() = if (unavailable.get()) "Publisher unavailable · local recording continues" else delegate.status
    override val diagnostics: String get() = if (unavailable.get()) "Publishing epoch fenced; cleanup ${if (cleanupFailure.get() == null) "pending/closed" else "unconfirmed"}" else delegate.diagnostics
    private inline fun publish(action: () -> Unit) {
        if (unavailable.get()) return
        try { action() } catch (error: Exception) { fail(error) }
    }
    private fun fail(error: Exception, fence: Boolean = true) {
        if (error.suppressed.isNotEmpty()) cleanupFailure.compareAndSet(null, error)
        if (!unavailable.compareAndSet(false, true)) return
        if (fence) runCatching { delegate.stopDestination() }.onFailure { cleanupFailure.compareAndSet(null, it as? Exception ?: error) }
        runCatching { failed(error) } // Diagnostic delivery cannot fail local recording.
    }
    override fun configure(video: RustCodec?, audio: RustCodec?) = publish { delegate.configure(video, audio) }
    override fun start() = publish { delegate.start() }
    override fun consume(sample: MediaSample) = publish { delegate.consume(sample) }
    override fun setKeyframeRequester(request: () -> Unit) = publish { delegate.setKeyframeRequester(request) }
    override fun stopDestination() { runCatching { delegate.stopDestination() }.onFailure {
        val error = it as? Exception ?: IllegalStateException("Publisher fence failed")
        cleanupFailure.compareAndSet(null, error)
        fail(error, fence = false)
    } }
    override fun close() {
        unavailable.set(true)
        val created = CompletableFuture<Unit>()
        val existing = if (closure.compareAndSet(null, created)) null else closure.get()
        val result = existing ?: created.also {
            try {
                delegate.close()
                cleanupFailure.get()?.let { error -> throw error }
                it.complete(Unit)
            } catch (error: Exception) { cleanupFailure.compareAndSet(null, error); it.completeExceptionally(error) }
        }
        try { result.get(1, TimeUnit.SECONDS) }
        catch (error: ExecutionException) { throw (error.cause as? Exception ?: error) }
    }
}
