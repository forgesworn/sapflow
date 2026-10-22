package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** Ephemeral status-only API authority, never passed to the media fetcher,
 * decoder, journal, logs or public route. Issued by the control actor. */
internal class V4vObservationPermit(val identity: V4vIdentity, private val bearer: String, val deadlineNs: Long,
    private val nowNs: () -> Long = System::nanoTime) {
    fun status(api: V4vApi): V4vSessionReceipt {
        check(nowNs() < deadlineNs)
        val result = api.request("GET", "/api/live-events/${identity.eventId}/native-session/${identity.sessionId}", bearer)
        require(with(V4vJson) { result.flag("ok") })
        val receipt = V4vSessionReceipt.read(checkNotNull(result["session"] as? kotlinx.serialization.json.JsonObject))
        check(nowNs() < deadlineNs)
        require(identity.sameSession(receipt.identity) && receipt.identity.revision >= identity.revision)
        return receipt
    }
    override fun toString() = "Ephemeral origin status authority (redacted)"
}

internal interface OriginObservationResources : AutoCloseable {
    fun status(): V4vSessionReceipt
    fun get(path: String, max: Int, cancel: OriginProbeCancellation): ByteArray
}

private class PlatformObservationResources(profile: V4vProfile, private val permit: V4vObservationPermit) : OriginObservationResources {
    private val api = V4vControlClient(profile)
    private val fetch = OriginMediaFetch(ApprovedPlaybackRoute(profile))
    override fun status() = permit.status(api)
    override fun get(path: String, max: Int, cancel: OriginProbeCancellation) = fetch.get(path, max, cancel)
    override fun close() {
        var failure: Exception? = null
        try { api.close() } catch (error: Exception) { failure = error }
        try { fetch.close() } catch (error: Exception) { if (failure == null) failure = error else failure.addSuppressed(error) }
        failure?.let { throw OriginProbeCleanupFailure(it, listOf(api, fetch)) }
    }
}

/** One finite observation per actual accepted attempt. Cancellation closes its
 * private HTTP owners immediately. The slot is released ONLY by actual native
 * worker completion; a stuck driver cannot be bypassed by another attempt. */
internal class V4vOriginObserver(private val profile: V4vProfile,
    private val permit: (V4vCapturedAttempt, (V4vObservationPermit?) -> Unit) -> Unit,
    private val current: (V4vCapturedAttempt) -> Boolean,
    private val match: (V4vCapturedAttempt, V4vSessionReceipt) -> V4vMatchedMedia?,
    private val confirmed: (V4vCapturedAttempt, NativeOriginEvidence, Long) -> Unit,
    private val decoder: OriginDecoder = FiniteNativeOriginDecoder(),
    private val resources: (V4vObservationPermit) -> OriginObservationResources = { PlatformObservationResources(profile, it) }) : AutoCloseable {
    private class Job(val scope: V4vCapturedAttempt, val intent: CaptureIntent) {
        val cancel = OriginProbeCancellation()
        val resources = AtomicReference<OriginObservationResources?>()
        val wake = CountDownLatch(1)
        val permitWaiter = AtomicReference<CountDownLatch?>()
        val invalidated = java.util.concurrent.atomic.AtomicBoolean()
        val workerDone = java.util.concurrent.atomic.AtomicBoolean()
        val closeSubmitted = java.util.concurrent.atomic.AtomicBoolean()
        val closeDone = java.util.concurrent.atomic.AtomicBoolean()
        @Volatile var retainedFailure: OriginProbeCleanupFailure? = null
        @Synchronized fun retain(failure: OriginProbeCleanupFailure) {
            val old = retainedFailure
            if (old == null) retainedFailure = failure else old.addSuppressed(failure)
        }
        @Volatile var result: NativeOriginEvidence? = null
        @Volatile var authorityDeadline = Long.MAX_VALUE
        var alarm: java.util.concurrent.ScheduledFuture<*>? = null
    }
    private val owner = AtomicReference<Job?>()
    private val pending = AtomicReference<Job?>() // One latest scalar scope, never media/native ownership.
    private val worker = Executors.newSingleThreadExecutor { Thread(it, "sapflow-origin-native-check").apply { isDaemon = true } }
    private val cancellation = Executors.newSingleThreadExecutor { Thread(it, "sapflow-origin-check-cancel").apply { isDaemon = true } }
    private val timer = Executors.newSingleThreadScheduledExecutor { Thread(it, "sapflow-origin-check-deadline").apply { isDaemon = true } }
    private val state = AtomicReference("Playback has not been verified")
    private val reported = AtomicReference<PublishingAttemptContext?>()
    private val closed = java.util.concurrent.atomic.AtomicBoolean()
    val diagnostics: String get() = state.get()
    @Synchronized fun accepted(scope: V4vCapturedAttempt, intent: CaptureIntent) {
        if (closed.get() || owner.get()?.scope === scope || pending.get()?.scope === scope) return
        reported.set(scope.context); state.set("Playback has not been verified")
        val job = Job(scope, intent) // Deadline starts at Accepted, not deferred cleanup completion.
        val old = owner.get()
        if (old != null) {
            pending.getAndSet(job)?.let(::cancelJob)
            cancelJob(old)
            state.set("Playback check waiting for previous cleanup")
        } else start(job)
    }
    fun accepted(scope: V4vCapturedAttempt, quality: CaptureProfile) = accepted(scope, CaptureIntent.AudioVideo(quality))
    /** Called under this monitor. No IO or native operation runs here. */
    private fun start(job: Job) {
        if (closed.get() || !current(job.scope)) return
        val remaining = runCatching { job.cancel.remainingNs() }.getOrNull() ?: return
        check(owner.compareAndSet(null, job))
        job.alarm = timer.schedule({ if (owner.get() === job) {
            state.set("Playback check timed out; cleanup pending"); cancelJob(job)
        } }, remaining, TimeUnit.NANOSECONDS)
        worker.execute {
            try { job.result = run(job) }
            catch (failure: OriginProbeCleanupFailure) { job.retain(failure); state.set("Playback check cleanup incomplete; further checks disabled") }
            catch (_: Exception) { state.set("Playback has not been verified") }
            finally { job.workerDone.set(true); requestClose(job); finish(job) }
        }
    }
    /** Synchronous fences only. Platform disconnect may block and is owned by
     * one separate cancellation worker, never UI/expiry/transport callbacks. */
    private fun cancelJob(job: Job) {
        job.invalidated.set(true); job.cancel.cancel(); job.wake.countDown(); job.permitWaiter.get()?.countDown()
        requestClose(job)
    }
    private fun requestClose(job: Job) {
        val io = job.resources.get() ?: return
        if (!job.closeSubmitted.compareAndSet(false, true)) return
        cancellation.execute {
            try { io.close() }
            catch (error: Exception) { job.retain(OriginProbeCleanupFailure(error, listOf(io))); state.set("Playback check cleanup incomplete; further checks disabled") }
            finally { job.closeDone.set(true); finish(job) }
        }
    }
    @Synchronized private fun finish(job: Job) {
        if (!job.workerDone.get() || job.resources.get() != null && !job.closeDone.get() || owner.get() !== job) return
        job.alarm?.cancel(false)
        if (job.retainedFailure != null) return // Keep actual uncertain wrappers alive; never admit a successor.
        val proof = job.result
        if (proof != null && !job.invalidated.get() && !closed.get() && current(job.scope) &&
            System.nanoTime() < minOf(job.cancel.deadlineNs, job.authorityDeadline)) {
            runCatching { confirmed(job.scope, proof, minOf(job.cancel.deadlineNs, job.authorityDeadline)) }
                .onSuccess { state.set("Origin ${if (proof.video == null) "audio" else "video and audio"} decoded · ${proof.audio.codec} software=${proof.audio.software} native AAC · PCM ${proof.audio.decodedBytes}B pts=${proof.audio.firstPtsUs}..${proof.audio.lastPtsUs}us") }
                .onFailure { state.set("Playback has not been verified") }
        }
        owner.compareAndSet(job, null)
        pending.getAndSet(null)?.let { if (!closed.get()) start(it) }
    }
    fun cancel(context: PublishingAttemptContext? = null) {
        if (context == null || reported.get() == context) { reported.set(null); state.set("Playback has not been verified") }
        pending.get()?.takeIf { context == null || it.scope.context == context }?.let { if (pending.compareAndSet(it, null)) cancelJob(it) }
        owner.get()?.takeIf { context == null || it.scope.context == context }?.let(::cancelJob)
    }
    @Synchronized override fun close() { closed.set(true); cancel(); timer.shutdownNow(); worker.shutdown() }
    fun awaitStopped(timeoutMs: Long): Boolean {
        val end = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        if (!worker.awaitTermination(timeoutMs, TimeUnit.MILLISECONDS)) return false
        cancellation.shutdown()
        return cancellation.awaitTermination((end - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
    }
    private fun run(job: Job): NativeOriginEvidence {
        fun checkCurrent() { job.cancel.check(); check(owner.get() === job && current(job.scope)) }
        checkCurrent(); state.set("Checking origin playback")
        var authority: V4vObservationPermit? = null
        repeat(3) { index -> if (authority == null) {
            checkCurrent()
            val answer = AtomicReference<V4vObservationPermit?>(); val ready = CountDownLatch(1)
            job.permitWaiter.set(ready)
            permit(job.scope) { value -> answer.set(value); ready.countDown() }
            check(ready.await(job.cancel.remainingNs(), TimeUnit.NANOSECONDS))
            job.permitWaiter.compareAndSet(ready, null); checkCurrent(); authority = answer.get()
            if (authority == null && index < 2) { job.wake.await(750, TimeUnit.MILLISECONDS); checkCurrent() }
        } }
        // Busy actor admission gets bounded retries; End/refresh retain their
        // own HTTP owner and reserved admissions. Original deadline never moves.
        val credentials = checkNotNull(authority); job.authorityDeadline = credentials.deadlineNs
        val io = resources(credentials); job.resources.set(io)
        if (job.invalidated.get()) requestClose(job)
        checkCurrent()
        var before: V4vMatchedMedia? = null
        repeat(3) { index ->
            if (before == null) {
                checkCurrent(); before = match(job.scope, io.status())
                if (before == null && index < 2) { job.wake.await(750, TimeUnit.MILLISECONDS); checkCurrent() }
            }
        }
        val boundary = checkNotNull(before) { "No fresh independent origin part" }
        require(job.intent == job.scope.intent && job.intent.matchesTracks(boundary.hasVideo, boundary.hasAudio)) { "Origin tracks differ from prepared capture" }
        val init = io.get(boundary.initPath, OriginProbeLimits.INIT_BYTES, job.cancel)
        val part = io.get(boundary.partPath, OriginProbeLimits.PART_BYTES, job.cancel)
        checkCurrent()
        val evidence = decoder.decode(init, part, job.intent, job.cancel)
        checkCurrent()
        require((evidence.video != null) == job.intent.hasVideo) { "Decoded tracks differ from capture intent" }
        val after = checkNotNull(match(job.scope, io.status()))
        require(sameDecodedBoundary(boundary, after))
        checkCurrent()
        // Confirmation waits for this worker AND the independent IO closer.
        return evidence
    }
}
