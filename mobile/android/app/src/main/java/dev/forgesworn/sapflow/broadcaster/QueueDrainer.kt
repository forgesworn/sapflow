package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeSampleFanout
import android.os.Handler
import android.os.SystemClock
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/** Rust owns both bounded queues and the one shared immutable payload. Each
 * worker owns one fixed batch of at most eight shared handles, without copies.
 * All fanout calls run on the codec handler to preserve its monotonic clock. */
internal class QueueDrainer(
    private val handler: Handler,
    private val queues: NativeSampleFanout,
    private val publishing: Boolean,
    private val sink: EncodedSink?,
    private val failure: (String) -> Unit,
    private val recordingSource: RecordingSourceGate.Recorder? = null,
    private val nativeRun: AudioNativeAacRun? = null,
    private val nativeWorker: AudioNativeAacWorker? = null,
) {
    init {
        // Refuse invalid wiring before creating a worker or any other owner.
        require(!publishing || nativeRun == null && nativeWorker == null)
        require((nativeRun == null) == (nativeWorker == null))
        require(nativeWorker == null || nativeWorker === sink && recordingSource == null)
    }
    private val executor = if (sink == null) null else ThreadPoolExecutor(
        // One fixed batch task plus a reserved control/finalisation task.
        // Admission prevents another batch until all its owners have closed.
        1, 1, 0, TimeUnit.MILLISECONDS, ArrayBlockingQueue(2),
        { task -> Thread(task, if (publishing) "sapflow-publish-sink" else "sapflow-record-sink") },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { prestartAllCoreThreads() }
    private val closed = AtomicBoolean(false)
    private val aborted = AtomicBoolean(false)
    private val closing = AtomicBoolean(false)
    private val nativeClose = NativeCloseOnce()
    private val admission = DrainAdmission()
    private val handoff = OwnedHandoff<OwnedBatch<MediaSample>>()
    private val activeBatch = AtomicReference<OwnedBatch<MediaSample>?>()
    private val batches = AtomicLong()
    private val batchedSamples = AtomicLong()
    private val maxBatchSize = AtomicLong()
    private val consumeDelay = DelayDiagnostics()
    private val dispatchDelay = DelayDiagnostics()
    private val handlerDelay = DelayDiagnostics()
    private val batchDelay = DelayDiagnostics()
    private val completionNs = AtomicLong()
    val diagnostics: String get() {
        val batch = activeBatch.get() ?: handoff.peek()
        val count = batches.get()
        return "consume ${consumeDelay.text()} · dispatch ${dispatchDelay.text()} · handler ${handlerDelay.text()} · batches $count/avg ${batchedSamples.get().toDouble() / maxOf(1, count)}/max ${maxBatchSize.get()} · retained ${batch?.remainingSamples ?: 0}/${batch?.remainingBytes ?: 0}B · batch ${batchDelay.text()}"
    }
    private val request = Runnable {
        val completed = completionNs.getAndSet(0)
        if (completed != 0L) handlerDelay.record(System.nanoTime() - completed)
        admission.resumed()
        available()
    }

    /** Source FIFO acceptance is observed on its actual handler immediately
     * after queues.push reports recordingQueued. No media lookup or copy. */
    fun recordingAccepted(nativeGeneration: Any? = null, nativeAudio: Boolean = false) {
        check(!publishing && handler.looper.isCurrentThread)
        recordingSource?.accepted()
        if (nativeRun != null) {
            val accounted = if (nativeAudio) nativeGeneration != null && nativeRun.accepted(nativeGeneration)
                else nativeRun.acceptedVideo()
            if (!accounted) {
                nativeRun.fail()
                recordingSource?.failed()
            }
        }
    }

    /** Known loss must fence both recording protocols before asynchronous
     * prefix cleanup. Rejected video is absent from accepted-AU counters. */
    fun recordingLost() {
        check(!publishing)
        nativeRun?.fail()
        recordingSource?.failed()
    }

    private fun consumeOwned(sample: MediaSample) {
        val run = nativeRun
        if (run == null) {
            checkNotNull(sink).consume(sample)
            recordingSource?.consumed()
            return
        }
        val worker = checkNotNull(nativeWorker)
        run.beforeSample(worker)
        checkNotNull(sink).consume(sample)
        run.afterSample(worker)
        recordingSource?.consumed()
    }

    private fun closeSink() {
        if (nativeRun == null) sink?.close()
        else nativeClose.close { checkNotNull(sink).close() }
    }

    private fun finishSink() {
        if (nativeRun == null) sink?.finish()
        else nativeClose.close { checkNotNull(sink).finish() }
    }

    fun available() {
        check(handler.looper.isCurrentThread)
        if (!closed.get() && admission.claim()) drainOne()
    }

    private fun drainOne() {
        if (closed.get()) { admission.empty(); return }
        // Retain early video in the existing bounded fanout until the first
        // accepted original audio establishes H. Its ingress wakes us again.
        if (nativeRun?.awaitingFirstAudio() == true) { admission.empty(); return }
        try {
            val batch = OwnedBatch.collect(if (publishing) 4L * 1024 * 1024 else 8L * 1024 * 1024,
                pop = {
                    val now = SystemClock.elapsedRealtimeNanos().toULong()
                    if (publishing) queues.popPublishing(now) else queues.popRecording(now)
                }, sizeOf = { it.len().toLong() })
            if (batch.size == 0) { batch.close(); admission.empty(); return }
            // The disconnected baseline validates and discards here. It never
            // claims that an unattached sink wrote a recording or broadcast.
            if (sink == null) {
                batch.close()
                admission.empty()
                // At most one per ingress call; the next callback wakes us.
                return
            }
            try {
                val submittedNs = System.nanoTime()
                handoff.offer(batch)
                batches.incrementAndGet()
                batchedSamples.addAndGet(batch.size.toLong())
                maxBatchSize.accumulateAndGet(batch.size.toLong(), ::maxOf)
                executor!!.execute {
                    val startedNs = System.nanoTime()
                    dispatchDelay.record(startedNs - submittedNs)
                    val owned = handoff.take()
                    activeBatch.set(owned)
                    try {
                        recordingSource.fenceFailure {
                            owned?.use {
                            while (!aborted.get() && it.consumeNext { sample ->
                                val consumeStartedNs = System.nanoTime()
                                try { if (!aborted.get()) consumeOwned(sample) }
                                finally { consumeDelay.record(System.nanoTime() - consumeStartedNs) }
                            }) { /* At most eight synchronous borrows, in FIFO order. */ }
                            }
                        }
                    }
                    catch (error: Throwable) {
                        nativeRun?.fail()
                        recordingSource?.failed() // Before deferred controller work.
                        admission.close()
                        if (!closed.getAndSet(true)) handler.post { failure("Sink failed: ${error.javaClass.simpleName}") }
                    }
                    finally {
                        activeBatch.compareAndSet(owned, null)
                        batchDelay.record(System.nanoTime() - startedNs)
                        // The batch has closed every borrowed owner. Make
                        // admission visible immediately: a fresh codec ingress
                        // may pump the next sample before this control message.
                        if (admission.complete()) {
                            completionNs.set(System.nanoTime())
                            if (!handler.post(request)) admission.resumed()
                        }
                    }
                }
            } catch (error: Throwable) {
                nativeRun?.fail()
                recordingSource?.failed()
                try { handoff.cancel() } catch (cleanup: Throwable) { error.addSuppressed(cleanup) }
                throw error
            }
        } catch (error: Throwable) {
            nativeRun?.fail()
            recordingSource?.failed()
            admission.empty()
            admission.close()
            closed.set(true)
            failure("Shared queue failed: ${error.javaClass.simpleName}")
        }
    }

    private fun cancelBatch() {
        try { handoff.cancel() } finally { activeBatch.get()?.close() }
    }

    /** Used only while capture continues after a recording failure. Clocked
     * fanout pops stay serialised on the codec handler; IO remains on worker. */
    private fun popRecordingOnHandler(deadlineNs: Long): MediaSample? {
        val guard = Any()
        val done = CountDownLatch(1)
        var cancelled = false
        var sample: MediaSample? = null
        var failure: Throwable? = null
        val request = Runnable {
            synchronized(guard) {
                if (!cancelled) {
                    try { sample = queues.popRecording(SystemClock.elapsedRealtimeNanos().toULong()) }
                    catch (error: Throwable) { nativeRun?.fail(); recordingSource?.failed(); failure = error }
                }
                done.countDown()
            }
        }
        check(handler.post(request)) { "Recording queue owner stopped" }
        val remaining = (deadlineNs - SystemClock.elapsedRealtimeNanos()).coerceAtLeast(0)
        if (!done.await(remaining, TimeUnit.NANOSECONDS)) {
            synchronized(guard) { cancelled = true; sample?.close(); sample = null }
            handler.removeCallbacks(request)
            error("Recording queue drain deadline exceeded")
        }
        return synchronized(guard) {
            failure?.let { throw it }
            sample.also { sample = null }
        }
    }

    fun close(graceful: Boolean = false, drainQueued: Boolean = false, drainViaHandler: Boolean = false): Boolean =
        closeOwned(graceful, drainQueued, drainViaHandler, null)

    fun closeWithSource(graceful: Boolean, drainQueued: Boolean, drainViaHandler: Boolean,
        source: RecordingSourceGate.Terminal?): Boolean = closeOwned(graceful, drainQueued, drainViaHandler, source)

    private fun closeOwned(graceful: Boolean, drainQueued: Boolean, drainViaHandler: Boolean,
        source: RecordingSourceGate.Terminal?): Boolean {
        closed.set(true)
        admission.close()
        handler.removeCallbacks(request)
        if (!closing.compareAndSet(false, true)) {
            return if (nativeClose.closeUnknown) false else executor?.isTerminated ?: true
        }
        val worker = executor ?: return true
        if (graceful && sink != null) {
            val deadlineNs = SystemClock.elapsedRealtimeNanos() + 3_000_000_000L
            return worker.finishWithin(3000, finish = {
                recordingSource.fenceFailure {
                    if (drainQueued) {
                        // Capture/publishing have stopped. This is the sole
                        // fanout owner until finalisation, not a second queue.
                        while (true) {
                            check(SystemClock.elapsedRealtimeNanos() < deadlineNs) { "Recording drain deadline exceeded" }
                            val sample = if (drainViaHandler) popRecordingOnHandler(deadlineNs)
                                else queues.popRecording(SystemClock.elapsedRealtimeNanos().toULong())
                            if (sample == null) break
                            sample.use { consumeOwned(it) }
                        }
                    }
                    val sourceRecorder = recordingSource
                    if (sourceRecorder == null) {
                        when (nativeRun?.finish(checkNotNull(nativeWorker))) {
                            null, AudioNativeAacFinish.FILE_RECEIPT -> finishSink()
                            AudioNativeAacFinish.EMPTY -> closeSink()
                        }
                    } else {
                        check(sink is PrivateRecordingSink) { "Source-bound recorder changed" }
                        val permit = sourceRecorder.drained(source)
                        if (permit == null) {
                            sink.finishIncompletePrefix()
                            error("Source admission is incomplete; accepted prefix closed without FINALISED receipt")
                        }
                        sink.finish(permit)
                    }
                }
            }, abort = {
                nativeRun?.fail()
                recordingSource?.failed()
                aborted.set(true)
                cancelBatch()
                closeSink()
            })
        }
        nativeRun?.fail()
        recordingSource?.failed()
        aborted.set(true)
        cancelBatch()
        // Invoked on a control worker by CaptureController, outside callbacks.
        val sinkClosed = runCatching { closeSink() }.isSuccess
        worker.shutdownNow()
        return worker.awaitTermination(1, TimeUnit.SECONDS) && sinkClosed && !graceful
    }
}
