package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeSession
import dev.forgesworn.sapflow.NativeSessionAction
import dev.forgesworn.sapflow.NativeTransportRecoverySource
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/** One logical destination, fresh bounded transport per attempt, no media queue.
 * Epoch.close joins transport cleanup before CaptureController releases codecs.
 * Explicit Stop fences synchronously, then cleans up on this control owner. */
internal class ReconnectingPublishingSink(
    private val policyFactory: (() -> ReconnectPolicyPort)? = null,
    private val graceMs: Long = 90_000,
    private val observationGate: ObservationLinearisation = ObservationLinearisation(),
    private val observationChanged: (PublishingAttemptContext, Boolean) -> Unit = { _, _ -> },
    // Production supplies BOOTTIME; pure host tests retain their injected domain.
    private val authorityNowNs: () -> Long = System::nanoTime,
    /** Optional exact logical-grant fence. One atomic deadline read plus clock;
     * no callback dispatch, FFI, IO, sample copy or allocation on a valid sample. */
    private val sampleAuthority: PublishingAuthority? = null,
    private val factory: PublishingAttemptContext.((PublishingAttemptEvent) -> Unit) -> EncodedSink,
) {
    init { require(graceMs in 0..3_600_000) }
    private data class Codecs(val video: RustCodec?, val audio: RustCodec?)
    private data class Current(val epoch: Epoch, val token: Long, val context: PublishingAttemptContext, val admitted: AtomicBoolean, val failed: AtomicBoolean, val sink: EncodedSink, val report: (PublishingAttemptEvent) -> Unit)
    private val fenced = AtomicBoolean(false)
    private val destinationId = destinations.incrementAndGet()
    private val sequence = AtomicLong()
    private val current = AtomicReference<Current?>()
    private val output = LogicalSocketOutput()
    private val message = AtomicReference("Destination prepared · press Start")
    private val executor = ScheduledThreadPoolExecutor(1) { Thread(it, "sapflow-reconnect") }.apply { removeOnCancelPolicy = true }
    private val stopped = CompletableFuture<Unit>()
    private data class Observation(val context: PublishingAttemptContext, val valid: () -> Boolean)
    private val observation = AtomicReference<Observation?>()
    private val observationPosted = AtomicBoolean(false)
    private val authorityPosted = AtomicBoolean(false)
    private var alarm: ScheduledFuture<*>? = null // Control owner only.
    private val epochs = java.util.concurrent.ConcurrentHashMap<Long, Epoch>()
    private val controller: ReconnectController by lazy { ReconnectController(policyFactory?.invoke() ?: NativePolicy(graceMs), authorityNowNs, { !fenced.get() },
        factory = attempt@{ epoch, token, sdk ->
            val owner = epochs[epoch]
            if (owner == null || owner.closed.get() || fenced.get()) return@attempt object : PublishingAttempt {
                override fun start() {}
                override fun close() {}
            }
            val accepted = AtomicBoolean(false); val failed = AtomicBoolean(false)
            val sending = AtomicBoolean(false); val progress = AtomicBoolean(false)
            val report: (PublishingAttemptEvent) -> Unit = { event ->
                val first = when (event) {
                    PublishingAttemptEvent.Accepted -> accepted.compareAndSet(false, true)
                    PublishingAttemptEvent.Sending -> sending.compareAndSet(false, true)
                    is PublishingAttemptEvent.PeerMediaProgress -> progress.compareAndSet(false, true)
                    is PublishingAttemptEvent.Failed -> observationGate.withGate { failed.compareAndSet(false, true) }
                }
                if (first) {
                    if (event is PublishingAttemptEvent.Failed) current.get()?.takeIf { it.token == token }?.let { notifyObservation(it.context, false) }
                    post { controller.event(token, event) }
                }
            }
            val context = PublishingAttemptContext(destinationId, epoch, token, sdk)
            val sink = factory(context, report)
            val record = Current(owner, token, context, AtomicBoolean(false), failed, sink, report)
            current.set(record)
            output.attach(context, runCatching { (sink as? SocketOutputProvider)?.socketOutput }.getOrNull())
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
                    output.closing(record.context)
                    if (current.compareAndSet(record, null)) notifyObservation(record.context, false)
                    closeError?.let { output.unconfirmed(record.context); throw it }
                    if (closed) return
                    closed = true
                    try { sink.close(); output.retired(record.context, System.nanoTime()) }
                    catch (error: Exception) { output.unconfirmed(record.context); closeError = error; throw error }
                }
            }
        }, ready = { epoch, token ->
            current.get()?.takeIf { it.epoch.id == epoch && it.token == token && !it.failed.get() }?.let {
                it.admitted.set(true); notifyObservation(it.context, true)
            }
            epochs[epoch]?.requestIdr?.get()?.invoke()
        }) }

    fun newEpoch(): EncodedSink {
        val epoch = Epoch(sequence.incrementAndGet())
        if (!fenced.get()) epochs[epoch.id] = epoch
        return epoch
    }
    /** Only actual finite decoded evidence may call this, with a nonblocking
     * final grant fence. No network/codec operation runs under this cold gate. */
    fun originMediaConfirmed(value: PublishingAttemptContext, valid: () -> Boolean = { true }) {
        if (!isCurrentObservation(value)) return
        observation.updateAndGet { pending -> if (isCurrentObservation(value)) Observation(value, valid) else pending }
        postObservation()
    }
    private fun postObservation() {
        if (observationPosted.compareAndSet(false, true)) post {
            try {
                observation.getAndSet(null)?.let { captured ->
                    observationGate.withGate {
                        val value = captured.context
                        if (isCurrentObservation(value) && runCatching(captured.valid).getOrDefault(false)) controller.observedMedia(value.epoch, value.attempt, value.sdk)
                    }
                }
            } finally {
                observationPosted.set(false)
                if (!fenced.get() && observation.get() != null) postObservation()
            }
        }
    }
    private fun isCurrentObservation(value: PublishingAttemptContext): Boolean = observationScope == value
    val observationScope: PublishingAttemptContext? get() {
        if (fenced.get()) return null
        val record = current.get() ?: return null
        if (record.epoch.closed.get() || record.failed.get() || !record.admitted.get()) return null
        return record.context
    }
    private fun notifyObservation(context: PublishingAttemptContext, active: Boolean) {
        // Observation failure must never fail the publisher/capture owner.
        runCatching { observationChanged(context, active) }
    }
    private fun post(action: () -> Unit) {
        if (fenced.get()) return
        runCatching { executor.execute { runControl(action) } }.onFailure { stopDestination() }
    }
    private fun runControl(action: () -> Unit) {
        if (fenced.get()) return
        try {
            action(); message.set(controller.status)
            if (controller.terminal) {
                output.ended(System.nanoTime())
                fenced.set(true); current.get()?.let { notifyObservation(it.context, false) }; alarm?.cancel(false)
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
            alarm = executor.schedule({ runControl { controller.tick() } }, (at - authorityNowNs()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
        }
    }
    fun stopDestination() {
        if (!observationGate.withGate { fenced.compareAndSet(false, true) }) return
        output.ended(System.nanoTime())
        current.get()?.let { notifyObservation(it.context, false) }
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
    private inner class Epoch(val id: Long) : EncodedSink, SocketOutputProvider {
        override val socketOutput: SocketOutputSource get() = output
        val closed = AtomicBoolean(false)
        val codecs = AtomicReference(Codecs(null, null))
        val requestIdr = AtomicReference<(() -> Unit)?>(null)
        private val started = AtomicBoolean(false)
        private val completion = AtomicReference<CompletableFuture<Unit>?>(null)
        override val status: String get() = message.get()
        override val diagnostics: String get() {
            val active = current.get()?.takeIf { it.epoch === this }
            return "Destination $destinationId epoch=$id attempt=${active?.token ?: 0} stopped=${fenced.get()} · ${active?.sink?.diagnostics ?: "No active publishing attempt"}"
        }
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
                // Timers may not execute during suspend. Validate BOOTTIME here
                // before the first resumed AAC can reach any transport owner.
                try { sampleAuthority?.requireValid() }
                catch (error: Exception) { stopDestination(); throw error }
                try { it.sink.consume(sample) } catch (_: Exception) { it.report(PublishingAttemptEvent.Failed()) }
            }
        }
        override fun stopDestination() = this@ReconnectingPublishingSink.stopDestination()
        override fun revalidateAuthority() {
            if (authorityPosted.compareAndSet(false, true)) post {
                try { controller.tick() } finally { authorityPosted.set(false) }
            }
        }
        override fun close() {
            observationGate.withGate { closed.set(true) }
            current.get()?.takeIf { it.epoch === this }?.let { notifyObservation(it.context, false) }
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

    private class NativePolicy(graceMs: Long) : ReconnectPolicyPort {
        private val native = NativeSession(graceMs.toULong())
        private var generation: ULong? = null
        private var attemptEpoch = 0uL
        override var attemptIdentity: SdkAttemptIdentity? = null
            private set
        override fun start(nowNs: Long): ReconnectAction {
            val value = native.start(nowNs.toULong()); generation = value
            return action(native.prepared(value))
        }
        override fun failed(nowNs: Long) = action(native.transportFailed(checkNotNull(generation), nowNs.toULong()))
        override fun intentionalReplace(nowNs: Long) = action(native.replaceTransport(checkNotNull(generation), nowNs.toULong()))
        override fun connected(nowNs: Long) = native.connected(checkNotNull(generation), nowNs.toULong())
        override fun sending(nowNs: Long) = action(native.sending(checkNotNull(generation), attemptEpoch, nowNs.toULong()))
        override fun peerProgress(source: PeerProgressSource, nowNs: Long) = action(native.transportRecovered(checkNotNull(generation), attemptEpoch, nowNs.toULong(), when (source) {
            PeerProgressSource.SRT -> NativeTransportRecoverySource.SRT_PEER_ACKNOWLEDGED_WATERMARK
            PeerProgressSource.RTMP -> NativeTransportRecoverySource.RTMP_PEER_ACKNOWLEDGED_WATERMARK
        }))
        override fun confirmed(attempt: SdkAttemptIdentity, nowNs: Long) = native.mediaConfirmedForAttempt(attempt.generation, attempt.attemptEpoch, nowNs.toULong())
        override fun tick(nowNs: Long) = action(native.tick(checkNotNull(generation), nowNs.toULong()))
        override val nextDeadlineNs: Long? get() = native.snapshot().nextDeadlineNs?.toLong()
        override fun stop() {
            try { generation?.let { native.finishStop(native.stop(it).generation) } }
            finally { native.close() }
        }
        private fun action(value: NativeSessionAction): ReconnectAction = when (value) {
            NativeSessionAction.Connect -> {
                attemptEpoch = native.snapshot().attemptEpoch
                attemptIdentity = SdkAttemptIdentity(checkNotNull(generation), attemptEpoch)
                ReconnectAction.Connect
            }
            is NativeSessionAction.RetryAt -> ReconnectAction.At(value.atNs.toLong())
            is NativeSessionAction.AwaitGraceDeadline -> ReconnectAction.At(value.atNs.toLong())
            NativeSessionAction.GraceExpired, NativeSessionAction.EndDestination -> ReconnectAction.Expired
            NativeSessionAction.RetryBudgetExhausted -> ReconnectAction.Exhausted
            NativeSessionAction.None -> ReconnectAction.None
        }
    }
    private companion object { val destinations = AtomicLong() }
}
