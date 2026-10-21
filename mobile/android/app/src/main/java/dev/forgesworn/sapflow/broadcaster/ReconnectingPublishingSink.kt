package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeSession
import dev.forgesworn.sapflow.NativeSessionAction
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

internal data class OriginMediaConfirmation(val destination: Long, val epoch: Long, val attempt: Long)

/** One logical destination, fresh bounded transport per attempt, no media queue.
 * Epoch.close joins transport cleanup before CaptureController releases codecs.
 * Explicit Stop fences synchronously, then cleans up on this control owner. */
internal class ReconnectingPublishingSink(
    private val policyFactory: () -> ReconnectPolicyPort = { NativePolicy() },
    private val factory: ((PublishingAttemptEvent) -> Unit) -> EncodedSink,
) {
    private data class Codecs(val video: RustCodec?, val audio: RustCodec?)
    private data class Current(val epoch: Epoch, val token: Long, val sink: EncodedSink, val report: (PublishingAttemptEvent) -> Unit)
    private val fenced = AtomicBoolean(false)
    private val destinationId = destinations.incrementAndGet()
    private val sequence = AtomicLong()
    private val current = AtomicReference<Current?>()
    private val message = AtomicReference("Destination prepared · press Start")
    private val executor = ScheduledThreadPoolExecutor(1) { Thread(it, "sapflow-reconnect") }.apply { removeOnCancelPolicy = true }
    private val stopped = CompletableFuture<Unit>()
    private val observation = AtomicReference<OriginMediaConfirmation?>()
    private val observationPosted = AtomicBoolean(false)
    private var alarm: ScheduledFuture<*>? = null // Control owner only.
    private val epochs = java.util.concurrent.ConcurrentHashMap<Long, Epoch>()
    private val controller: ReconnectController by lazy { ReconnectController(policyFactory(), System::nanoTime, { !fenced.get() },
        factory = attempt@{ epoch, token ->
            val owner = epochs[epoch]
            if (owner == null || owner.closed.get() || fenced.get()) return@attempt object : PublishingAttempt {
                override fun start() {}
                override fun close() {}
            }
            val accepted = AtomicBoolean(false); val failed = AtomicBoolean(false)
            val report: (PublishingAttemptEvent) -> Unit = { event ->
                val first = when (event) {
                    PublishingAttemptEvent.Accepted -> accepted.compareAndSet(false, true)
                    is PublishingAttemptEvent.Failed -> failed.compareAndSet(false, true)
                }
                if (first) post { controller.event(token, event) }
            }
            val sink = factory(report)
            val record = Current(owner, token, sink, report)
            current.set(record)
            object : PublishingAttempt {
                private var closeError: Exception? = null
                private var closed = false
                override fun start() {
                    if (owner.closed.get() || fenced.get()) return
                    try {
                        val codecs = owner.codecs.get()
                        sink.configure(codecs.video, codecs.audio)
                        sink.start()
                    } catch (error: Exception) {
                        // ExplicitAttempt attaches failed partial-start cleanup
                        // as suppressed. Its second close is a no-op, so never
                        // mistake that no-op for proven resource release.
                        if (error.suppressed.isNotEmpty()) closeError = error
                        throw error
                    }
                }
                override fun close() {
                    current.compareAndSet(record, null)
                    closeError?.let { throw it }
                    if (closed) return
                    closed = true
                    try { sink.close() } catch (error: Exception) { closeError = error; throw error }
                }
            }
        }, ready = { epoch, _ -> epochs[epoch]?.requestIdr?.get()?.invoke() }) }

    fun newEpoch(): EncodedSink {
        val epoch = Epoch(sequence.incrementAndGet())
        if (!fenced.get()) epochs[epoch.id] = epoch
        return epoch
    }
    /** Future V4V adapter seam. Manual mode deliberately never invokes it. */
    fun originMediaConfirmed(value: OriginMediaConfirmation) {
        if (fenced.get() || value.destination != destinationId) return
        observation.set(value)
        postObservation()
    }
    private fun postObservation() {
        if (observationPosted.compareAndSet(false, true)) post {
            try {
                observation.getAndSet(null)?.let { controller.observedMedia(it.epoch, it.attempt) }
            } finally {
                observationPosted.set(false)
                if (!fenced.get() && observation.get() != null) postObservation()
            }
        }
    }
    val observationScope: OriginMediaConfirmation? get() = current.get()?.let { OriginMediaConfirmation(destinationId, it.epoch.id, it.token) }
    private fun post(action: () -> Unit) {
        if (fenced.get()) return
        runCatching { executor.execute { runControl(action) } }.onFailure { stopDestination() }
    }
    private fun runControl(action: () -> Unit) {
        if (fenced.get()) return
        try {
            action(); message.set(controller.status)
            if (controller.terminal) {
                fenced.set(true); alarm?.cancel(false)
                releaseEpochDescriptors(); stopped.complete(Unit); executor.shutdown()
            } else arm()
        }
        catch (_: Exception) { stopDestination() }
    }
    private fun releaseEpochDescriptors() {
        observation.set(null)
        epochs.values.forEach { it.codecs.set(Codecs(null, null)); it.requestIdr.set(null) }
        epochs.clear()
    }
    private fun arm() {
        alarm?.cancel(false); alarm = null
        controller.nextWakeNs?.let { at ->
            alarm = executor.schedule({ runControl { controller.tick() } }, (at - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
        }
    }
    fun stopDestination() {
        if (!fenced.compareAndSet(false, true)) return
        message.set("Stopping publisher")
        try { executor.execute {
            alarm?.cancel(false)
            try {
                controller.stop()
                releaseEpochDescriptors()
                message.set(controller.status); stopped.complete(Unit)
            }
            catch (error: Exception) { message.set("Publisher cleanup incomplete"); stopped.completeExceptionally(error) }
            finally { executor.shutdown() }
        } } catch (error: Exception) { stopped.completeExceptionally(error) }
    }
    private inner class Epoch(val id: Long) : EncodedSink {
        val closed = AtomicBoolean(false)
        val codecs = AtomicReference(Codecs(null, null))
        val requestIdr = AtomicReference<(() -> Unit)?>(null)
        private val started = AtomicBoolean(false)
        private val completion = AtomicReference<CompletableFuture<Unit>?>(null)
        override val status: String get() = message.get()
        override val diagnostics: String get() = current.get()?.takeIf { it.epoch === this }?.sink?.diagnostics ?: "No active publishing attempt"
        override fun setKeyframeRequester(request: () -> Unit) { requestIdr.set(request) }
        override fun configure(video: RustCodec?, audio: RustCodec?) {
            if (closed.get() || fenced.get()) return
            codecs.set(Codecs(video, audio))
            current.get()?.takeIf { it.epoch === this }?.sink?.configure(video, audio)
        }
        override fun start() {
            check(!closed.get() && started.compareAndSet(false, true))
            if (fenced.get()) return // A terminal publisher never stops capture/recording.
            post { if (!closed.get()) controller.attach(id) }
        }
        override fun consume(sample: MediaSample) {
            if (!closed.get() && !fenced.get()) current.get()?.takeIf { it.epoch === this }?.let {
                try { it.sink.consume(sample) } catch (_: Exception) { it.report(PublishingAttemptEvent.Failed()) }
            }
        }
        override fun stopDestination() = this@ReconnectingPublishingSink.stopDestination()
        override fun close() {
            closed.set(true)
            val future = CompletableFuture<Unit>()
            val existing = if (completion.compareAndSet(null, future)) null else completion.get()
            val result = existing ?: future.also {
                if (fenced.get()) stopped.whenComplete { _, error -> if (error == null) it.complete(Unit) else it.completeExceptionally(error) }
                else try { executor.execute {
                    try { controller.detach(id); epochs.remove(id); codecs.set(Codecs(null, null)); requestIdr.set(null); message.set(controller.status); arm(); it.complete(Unit) }
                    catch (error: Exception) { it.completeExceptionally(error); stopDestination() }
                } } catch (_: Exception) { stopped.whenComplete { _, error -> if (error == null) it.complete(Unit) else it.completeExceptionally(error) } }
            }
            result.get(2, TimeUnit.SECONDS)
        }
    }

    private class NativePolicy : ReconnectPolicyPort {
        private val native = NativeSession(90_000uL)
        private var generation: ULong? = null
        override fun start(nowNs: Long): ReconnectAction {
            val value = native.start(nowNs.toULong()); generation = value
            return action(native.prepared(value))
        }
        override fun failed(nowNs: Long) = action(native.transportFailed(checkNotNull(generation), nowNs.toULong()))
        override fun intentionalReplace(nowNs: Long) = action(native.replaceTransport(checkNotNull(generation), nowNs.toULong()))
        override fun connected(nowNs: Long) = native.connected(checkNotNull(generation), nowNs.toULong())
        override fun confirmed(nowNs: Long) = native.mediaConfirmed(checkNotNull(generation), nowNs.toULong())
        override fun tick(nowNs: Long) = action(native.tick(checkNotNull(generation), nowNs.toULong()))
        override val deadlineNs: Long? get() = native.snapshot().reconnectDeadlineNs?.toLong()
        override fun stop() {
            try { generation?.let { native.finishStop(native.stop(it).generation) } }
            finally { native.close() }
        }
        private fun action(value: NativeSessionAction): ReconnectAction = when (value) {
            NativeSessionAction.Connect -> ReconnectAction.Connect
            is NativeSessionAction.RetryAt -> ReconnectAction.At(value.atNs.toLong())
            is NativeSessionAction.AwaitGraceDeadline -> ReconnectAction.At(value.atNs.toLong())
            NativeSessionAction.GraceExpired, NativeSessionAction.EndDestination -> ReconnectAction.Expired
            NativeSessionAction.None -> ReconnectAction.None
        }
    }
    private companion object { val destinations = AtomicLong() }
}
