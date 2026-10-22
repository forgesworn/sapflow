package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/** Exercise real Java executor/epoch/cleanup ownership with no native calls. */
class ReconnectingPublishingSinkTest {
    private class Policy : ReconnectPolicyPort {
        override var attemptIdentity: SdkAttemptIdentity? = null
        private var sdkEpoch = 0uL
        private fun connect(): ReconnectAction { attemptIdentity = SdkAttemptIdentity(1uL, ++sdkEpoch); return ReconnectAction.Connect }
        var deadlineNs: Long? = null
        override val nextDeadlineNs: Long? get() = deadlineNs
        val progress = CopyOnWriteArrayList<String>()
        val proofDelivered = CountDownLatch(1)
        override fun sending(nowNs: Long): ReconnectAction { progress.add("sending"); return ReconnectAction.None }
        override fun peerProgress(source: PeerProgressSource, nowNs: Long): ReconnectAction {
            progress.add("peer:$source"); proofDelivered.countDown(); return ReconnectAction.None
        }
        var graceNs = 90_000_000_000L
        val starts = AtomicInteger(); val failures = AtomicInteger(); val replacements = AtomicInteger()
        val confirmations = AtomicInteger(); val stops = AtomicInteger()
        override fun start(nowNs: Long): ReconnectAction { starts.incrementAndGet(); return connect() }
        override fun failed(nowNs: Long): ReconnectAction {
            failures.incrementAndGet(); if (deadlineNs == null) deadlineNs = nowNs + graceNs
            return connect()
        }
        override fun intentionalReplace(nowNs: Long): ReconnectAction { replacements.incrementAndGet(); return connect() }
        override fun connected(nowNs: Long) { progress.add("accepted") }
        override fun confirmed(attempt: SdkAttemptIdentity, nowNs: Long) { confirmations.incrementAndGet(); deadlineNs = null }
        override fun tick(nowNs: Long) = if (deadlineNs?.let { nowNs >= it } == true) ReconnectAction.Expired else ReconnectAction.None
        override fun stop() { stops.incrementAndGet() }
    }
    private class Sink(val events: (PublishingAttemptEvent) -> Unit) : EncodedSink, SocketOutputProvider {
        val output = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        override val socketOutput: SocketOutputSource get() = output
        val started = CountDownLatch(1)
        val controlThread = java.util.concurrent.atomic.AtomicReference<Thread?>()
        val closing = CountDownLatch(1)
        val closed = CountDownLatch(1)
        var releaseClose: CountDownLatch? = null
        var failClose = false
        var failedStartupCleanup = false
        override val status = "synthetic transport"
        override fun start() {
            controlThread.set(Thread.currentThread())
            started.countDown()
            if (failedStartupCleanup) throw IllegalStateException("synthetic setup failure").apply {
                addSuppressed(IllegalStateException("synthetic partial cleanup failure"))
            }
        }
        override fun consume(sample: MediaSample) { error("No media expected in control tests") }
        override fun close() {
            closing.countDown()
            releaseClose?.let { check(it.await(2, TimeUnit.SECONDS)) { "bounded synthetic close" } }
            if (failClose) error("synthetic retained owner")
            output.seal()
            closed.countDown()
        }
    }
    private class Harness(val gate: ObservationLinearisation = ObservationLinearisation(),
        changed: (PublishingAttemptContext, Boolean) -> Unit = { _, _ -> }) {
        val policy = Policy()
        val sinks = CopyOnWriteArrayList<Sink>()
        var configure: (Sink) -> Unit = {}
        val owner = ReconnectingPublishingSink(policyFactory = { policy }, observationGate = gate, observationChanged = changed, factory = { events ->
            assertTrue("old ownership must be released", sinks.lastOrNull()?.closed?.count != 1L)
            Sink(events).also { configure(it); sinks.add(it) }
        })
        fun sink(index: Int): Sink {
            val deadline = System.nanoTime() + 2_000_000_000L
            while (sinks.size <= index && System.nanoTime() < deadline) Thread.sleep(1)
            check(sinks.size > index) { "attempt was not created" }
            return sinks[index].also { check(it.started.await(2, TimeUnit.SECONDS)) }
        }
    }
    @Test fun explicitStopFinishesOwnedTransportAndLateEventsNeverReconnect() {
        val h = Harness(); val epoch = h.owner.newEpoch()
        epoch.start(); val sink = h.sink(0)
        epoch.stopDestination(); epoch.close()
        assertEquals(0L, sink.closed.count)
        sink.events(PublishingAttemptEvent.Accepted)
        sink.events(PublishingAttemptEvent.Failed())
        assertEquals(1, h.sinks.size)
        assertEquals(1, h.policy.stops.get())
        assertNull(h.owner.observationScope)
    }
    @Test fun logicalSocketBytesSurviveActualEpochCloseAndOldCounterCannotCountTwice() {
        val h = Harness(); val a = h.owner.newEpoch(); a.start(); val first = h.sink(0)
        val source = checkNotNull((a as SocketOutputProvider).socketOutput)
        first.output.completed(100); a.close()
        assertNull(source.socketSnapshot(System.nanoTime()).endedAtNs) // Codec rotation is not logical End.
        val b = h.owner.newEpoch(); b.start(); val second = h.sink(1)
        assertSame(source, (b as SocketOutputProvider).socketOutput)
        second.output.completed(200); first.output.completed(100) // First real writer is sealed.
        assertEquals(300L, source.socketSnapshot(System.nanoTime()).completedBytes)
        b.stopDestination(); b.close()
        val final = source.socketSnapshot(System.nanoTime())
        assertEquals(300L, final.completedBytes); assertEquals(SocketOutputPhase.CLOSED, final.phase)
        assertNotNull(final.endedAtNs); assertFalse(final.incomplete)
    }
    @Test fun blockedActualCloseRetainsClosingCounterAndUnknownCleanupNeverRetiresItAsComplete() {
        val h = Harness(); val release = CountDownLatch(1)
        h.configure = { it.releaseClose = release; it.failClose = true }
        val epoch = h.owner.newEpoch(); epoch.start(); val sink = h.sink(0)
        val source = checkNotNull((epoch as SocketOutputProvider).socketOutput)
        sink.output.completed(100); epoch.stopDestination()
        assertTrue(sink.closing.await(2, TimeUnit.SECONDS)); sink.output.completed(16)
        assertEquals(116L, source.socketSnapshot(System.nanoTime()).completedBytes)
        assertEquals(SocketOutputPhase.CLOSING, source.socketSnapshot(System.nanoTime()).phase)
        release.countDown(); assertThrows(ExecutionException::class.java) { epoch.close() }
        val final = source.socketSnapshot(System.nanoTime())
        assertEquals(116L, final.completedBytes); assertTrue(final.incomplete)
        assertEquals(1, h.sinks.size)
    }
    @Test fun failedAttemptMustFinishCleanupBeforeFreshFactoryAndEventsAreCoalesced() {
        val h = Harness(); val release = CountDownLatch(1)
        h.configure = { if (h.sinks.isEmpty()) it.releaseClose = release }
        val epoch = h.owner.newEpoch(); epoch.start(); val first = h.sink(0)
        first.events(PublishingAttemptEvent.Failed())
        assertTrue(first.closing.await(2, TimeUnit.SECONDS))
        repeat(100) { first.events(PublishingAttemptEvent.Failed()) }
        assertEquals(1, h.sinks.size)
        release.countDown(); h.sink(1)
        assertEquals(0L, first.closed.count)
        assertEquals(1, h.policy.failures.get())
        epoch.stopDestination(); epoch.close()
    }
    @Test fun epochClosePreservesCoordinatorAndOldObservationCannotConfirmNewTransport() {
        val h = Harness(); val firstEpoch = h.owner.newEpoch(); val ready = CountDownLatch(1)
        firstEpoch.setKeyframeRequester { ready.countDown() }
        firstEpoch.start(); val first = h.sink(0)
        first.events(PublishingAttemptEvent.Accepted)
        assertTrue(ready.await(2, TimeUnit.SECONDS))
        val old = checkNotNull(h.owner.observationScope)
        firstEpoch.close()
        assertEquals(0, h.policy.stops.get())
        val secondEpoch = h.owner.newEpoch(); secondEpoch.start(); val second = h.sink(1)
        second.events(PublishingAttemptEvent.Accepted)
        first.events(PublishingAttemptEvent.Failed())
        h.owner.originMediaConfirmed(old)
        secondEpoch.close() // Serial barrier after the observation.
        assertEquals(0, h.policy.confirmations.get())
        assertEquals(1, h.policy.starts.get())
        assertEquals(2, h.policy.replacements.get())
        assertEquals(0, h.policy.failures.get())
        val finalEpoch = h.owner.newEpoch(); finalEpoch.stopDestination(); finalEpoch.close()
        assertEquals(1, h.policy.stops.get())
    }
    @Test fun failedCleanupIsHonestAndPreventsFurtherTransportWhileCaptureCanContinue() {
        val h = Harness(); h.configure = { it.failClose = true }
        val epoch = h.owner.newEpoch(); epoch.start(); h.sink(0)
        epoch.stopDestination()
        assertThrows(ExecutionException::class.java) { epoch.close() }
        val inert = h.owner.newEpoch()
        inert.start() // Terminal publishing does not fail a later capture epoch.
        assertThrows(ExecutionException::class.java) { inert.close() }
        assertEquals(1, h.sinks.size)
        assertTrue(epoch.status.contains("cleanup incomplete"))
    }
    @Test fun stoppingPreparedEpochPreventsItsQueuedStartFromCreatingTransport() {
        val h = Harness(); val epoch = h.owner.newEpoch()
        epoch.stopDestination(); epoch.start(); epoch.close()
        assertEquals(0, h.sinks.size)
        assertEquals(0, h.policy.starts.get())
        assertEquals(1, h.policy.stops.get())
    }
    @Test fun stopDuringFailureCleanupFencesRetryBeforeOldOwnerReturns() {
        val h = Harness(); val release = CountDownLatch(1)
        h.configure = { it.releaseClose = release }
        val epoch = h.owner.newEpoch(); epoch.start(); val first = h.sink(0)
        first.events(PublishingAttemptEvent.Failed())
        assertTrue(first.closing.await(2, TimeUnit.SECONDS))
        epoch.stopDestination()
        release.countDown(); epoch.close()
        assertEquals(1, h.sinks.size)
        assertEquals(1, h.policy.stops.get())
    }
    @Test fun originalDeadlineExpiresDuringUnacceptedRetryAndLaterEpochIsInert() {
        val h = Harness(); h.policy.graceNs = 20_000_000L
        val epoch = h.owner.newEpoch(); epoch.start(); val first = h.sink(0)
        first.events(PublishingAttemptEvent.Failed()); h.sink(1)
        val until = System.nanoTime() + 2_000_000_000L
        while (!epoch.status.contains("grace expired") && System.nanoTime() < until) Thread.sleep(1)
        assertTrue(epoch.status.contains("grace expired"))
        epoch.close()
        val next = h.owner.newEpoch(); next.start(); next.close()
        assertEquals(2, h.sinks.size)
        assertEquals(0, h.policy.confirmations.get())
        assertEquals(1, h.policy.stops.get())
    }
    @Test fun failedPartialStartupCleanupCannotBeMistakenForSuccessfulDuplicateClose() {
        val h = Harness(); h.configure = { it.failedStartupCleanup = true }
        val epoch = h.owner.newEpoch(); epoch.start(); h.sink(0)
        val until = System.nanoTime() + 2_000_000_000L
        while (!epoch.status.contains("cleanup incomplete") && System.nanoTime() < until) Thread.sleep(1)
        assertTrue(epoch.status.contains("cleanup incomplete"))
        assertThrows(ExecutionException::class.java) { epoch.close() }
        assertEquals(1, h.sinks.size)
        assertEquals(1L, h.sinks[0].closed.count)
    }
    @Test fun anObservationFromDifferentDestinationCannotConfirmReusedEpochAndAttemptNumbers() {
        val first = Harness(); val oldEpoch = first.owner.newEpoch(); val oldReady = CountDownLatch(1)
        oldEpoch.setKeyframeRequester { oldReady.countDown() }; oldEpoch.start()
        first.sink(0).events(PublishingAttemptEvent.Accepted)
        assertTrue(oldReady.await(2, TimeUnit.SECONDS))
        val oldScope = checkNotNull(first.owner.observationScope)
        oldEpoch.stopDestination(); oldEpoch.close()
        val second = Harness(); val epoch = second.owner.newEpoch(); val ready = CountDownLatch(1)
        epoch.setKeyframeRequester { ready.countDown() }; epoch.start()
        second.sink(0).events(PublishingAttemptEvent.Accepted)
        assertTrue(ready.await(2, TimeUnit.SECONDS))
        val current = checkNotNull(second.owner.observationScope)
        assertEquals(oldScope.epoch, current.epoch)
        assertEquals(oldScope.attempt, current.attempt)
        assertNotEquals(oldScope.destination, current.destination)
        second.owner.originMediaConfirmed(oldScope)
        epoch.stopDestination(); epoch.close()
        assertEquals(0, second.policy.confirmations.get())
    }
    @Test fun crossOwnerProgressBeforeAcceptedIsAppliedOnceOnTheCurrentControlOwner() {
        val h = Harness(); val epoch = h.owner.newEpoch(); epoch.start(); val sink = h.sink(0)
        sink.events(PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
        repeat(100) { sink.events(PublishingAttemptEvent.Sending) }
        sink.events(PublishingAttemptEvent.Accepted)
        assertTrue(h.policy.proofDelivered.await(2, TimeUnit.SECONDS))
        epoch.stopDestination(); epoch.close()
        assertEquals(listOf("accepted", "sending", "peer:RTMP"), h.policy.progress)
        assertEquals(0, h.policy.confirmations.get())
    }

    @Test fun immediateFailureFencesScopeBeforeItsQueuedControlEventCanRun() {
        val h = Harness(); val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val epoch = h.owner.newEpoch()
        epoch.setKeyframeRequester { entered.countDown(); check(release.await(2, TimeUnit.SECONDS)) }
        epoch.start(); val sink = h.sink(0)
        assertNull(h.owner.observationScope) // Captured but not control-owner accepted.
        sink.events(PublishingAttemptEvent.Accepted)
        try {
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            val scope = checkNotNull(h.owner.observationScope)
            assertEquals(SdkAttemptIdentity(1uL, 1uL), scope.sdk)
            sink.events(PublishingAttemptEvent.Failed()) // Queued behind ready callback.
            assertNull(h.owner.observationScope)
            h.owner.originMediaConfirmed(scope)
        } finally { release.countDown(); epoch.stopDestination(); epoch.close() }
        assertEquals(0, h.policy.confirmations.get())
    }
    @Test fun closingCodecEpochImmediatelyFencesACapturedObservationBeforeCleanupCompletes() {
        val h = Harness(); val release = CountDownLatch(1)
        h.configure = { it.releaseClose = release }
        val epoch = h.owner.newEpoch(); val ready = CountDownLatch(1)
        epoch.setKeyframeRequester { ready.countDown() }; epoch.start(); val sink = h.sink(0)
        sink.events(PublishingAttemptEvent.Accepted); assertTrue(ready.await(2, TimeUnit.SECONDS))
        val scope = checkNotNull(h.owner.observationScope)
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val closing = Thread { try { epoch.close() } catch (error: Throwable) { failure.set(error) } }.apply { start() }
        try {
            assertTrue(sink.closing.await(2, TimeUnit.SECONDS)); assertNull(h.owner.observationScope)
            h.owner.originMediaConfirmed(scope)
        } finally { release.countDown(); closing.join(2000); epoch.stopDestination(); epoch.close() }
        assertFalse(closing.isAlive); assertNull(failure.get()); assertEquals(0, h.policy.confirmations.get())
    }
    @Test fun v4vRetryAndRotationConstructFreshWireIdsFromCapturedSdkAttempts() {
        val policy = Policy(); val sinks = CopyOnWriteArrayList<Sink>(); val wires = CopyOnWriteArrayList<V4vWireAttempt>()
        val correlation = V4vAttemptCorrelation()
        val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        val original = "publish:live/event-fixture:${identity.sessionId}:${"b".repeat(43)}"
        val grant = V4vGrant(identity, "srt://192.0.2.1:8890", original, "c".repeat(43), 1000, 500, 90000, 4_000_000, 524288, 1000000)
        val owner = ReconnectingPublishingSink(policyFactory = { policy }) { events ->
            wires.add(correlation.begin(this, grant))
            Sink(events).also { sinks.add(it) }
        }
        fun next(index: Int): Sink {
            val until = System.nanoTime() + 2_000_000_000
            while (sinks.size <= index && System.nanoTime() < until) Thread.sleep(1)
            check(sinks.size > index); return sinks[index].also { check(it.started.await(2, TimeUnit.SECONDS)) }
        }
        val firstEpoch = owner.newEpoch(); var secondEpoch: EncodedSink? = null
        try {
            firstEpoch.start(); val a = next(0); a.events(PublishingAttemptEvent.Accepted)
            a.events(PublishingAttemptEvent.Failed()); val b = next(1)
            b.events(PublishingAttemptEvent.Accepted); firstEpoch.close()
            secondEpoch = owner.newEpoch(); secondEpoch.start(); next(2)
            assertEquals(3, wires.map { it.scope.nonce }.toSet().size)
            assertEquals(listOf(1uL, 2uL, 3uL), wires.map { it.scope.context.sdk.attemptEpoch })
            assertEquals(listOf(1L, 1L, 2L), wires.map { it.scope.context.epoch })
            assertEquals(1, wires.map { it.scope.context.destination }.toSet().size)
            assertTrue(wires.all { it.streamId.endsWith("forgesworn_attempt=${it.scope.nonce}") })
            assertEquals(original, grant.streamId); assertEquals(0, policy.confirmations.get())
            assertEquals(1, policy.failures.get()) // Rotation is not a new outage.
        } finally { owner.stopDestination(); secondEpoch?.close(); firstEpoch.close() }
    }
    private fun awaitFinalGate(sink: Sink) {
        val owner = checkNotNull(sink.controlThread.get())
        val until = System.nanoTime() + 2_000_000_000
        while (owner.state != Thread.State.BLOCKED && System.nanoTime() < until) Thread.sleep(1)
        assertEquals("observation reached its final cold gate", Thread.State.BLOCKED, owner.state)
    }
    @Test fun failureWinningAtTheFinalPolicyGateCannotResetTheOriginalOutage() {
        val h = Harness(); val epoch = h.owner.newEpoch(); val ready = CountDownLatch(1)
        epoch.setKeyframeRequester { ready.countDown() }
        try {
            epoch.start(); h.sink(0).events(PublishingAttemptEvent.Failed())
            val second = h.sink(1); second.events(PublishingAttemptEvent.Accepted)
            assertTrue(ready.await(2, TimeUnit.SECONDS)); val originalDeadline = h.policy.deadlineNs
            val captured = checkNotNull(h.owner.observationScope)
            h.gate.withGate {
                h.owner.originMediaConfirmed(captured)
                awaitFinalGate(second) // After queue admission, just before final SDK validation/call.
                second.events(PublishingAttemptEvent.Failed()) // Reentrant fence wins while confirmation waits.
                assertNull(h.owner.observationScope)
            }
            h.sink(2) // Serial barrier after rejected completion and failure/retry.
            assertEquals(0, h.policy.confirmations.get()); assertEquals(originalDeadline, h.policy.deadlineNs)
            assertEquals(2, h.policy.failures.get())
        } finally { epoch.stopDestination(); epoch.close() }
    }
    @Test fun explicitStopWinningAtTheFinalPolicyGateCannotConfirmOrRestart() {
        val h = Harness(); val epoch = h.owner.newEpoch(); val ready = CountDownLatch(1)
        epoch.setKeyframeRequester { ready.countDown() }
        epoch.start(); val sink = h.sink(0); sink.events(PublishingAttemptEvent.Accepted)
        assertTrue(ready.await(2, TimeUnit.SECONDS)); val captured = checkNotNull(h.owner.observationScope)
        try {
            h.gate.withGate {
                h.owner.originMediaConfirmed(captured)
                awaitFinalGate(sink)
                epoch.stopDestination() // Same synchronous fence used by End and grant expiry.
                assertNull(h.owner.observationScope)
            }
        } finally { epoch.stopDestination(); epoch.close() }
        assertEquals(0, h.policy.confirmations.get()); assertEquals(1, h.policy.stops.get()); assertEquals(1, h.sinks.size)
    }
    @Test fun delayedGrantExpiryFenceIsRecheckedAtTheActualSdkCall() {
        val h = Harness(); val epoch = h.owner.newEpoch(); val ready = CountDownLatch(1)
        val valid = java.util.concurrent.atomic.AtomicBoolean(true)
        epoch.setKeyframeRequester { ready.countDown() }
        try {
            epoch.start(); h.sink(0).events(PublishingAttemptEvent.Failed())
            val sink = h.sink(1); sink.events(PublishingAttemptEvent.Accepted)
            assertTrue(ready.await(2, TimeUnit.SECONDS)); val oldDeadline = h.policy.deadlineNs
            val scope = checkNotNull(h.owner.observationScope)
            h.gate.withGate {
                h.owner.originMediaConfirmed(scope) { valid.get() }
                awaitFinalGate(sink)
                valid.set(false) // Expiry/End can invalidate independently of the queued Stop task.
            }
            epoch.close() // Control-owner barrier after pending confirmation.
            assertEquals(0, h.policy.confirmations.get()); assertEquals(oldDeadline, h.policy.deadlineNs)
        } finally { epoch.stopDestination(); epoch.close() }
    }
    @Test fun blockingObserverDisconnectCannotDelayTheOriginalTransportFailure() {
        val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        val profile = checkNotNull(V4vProfile.approved("https://api.example.invalid", "https://play.example.invalid", "/live/"))
        val entered = CountDownLatch(1); val releaseDecode = CountDownLatch(1)
        val closing = CountDownLatch(1); val releaseClose = CountDownLatch(1)
        lateinit var observer: V4vOriginObserver
        val h = Harness(changed = { context, active ->
            if (active) observer.accepted(V4vCapturedAttempt(context, identity, "0".repeat(32)), CaptureProfile.HD_720)
            else observer.cancel(context)
        })
        val receipt = V4vSessionReceipt.read(kotlinx.serialization.json.buildJsonObject {
            put("contractVersion", kotlinx.serialization.json.JsonPrimitive(1)); put("eventId", kotlinx.serialization.json.JsonPrimitive(identity.eventId))
            put("eventGeneration", kotlinx.serialization.json.JsonPrimitive(1)); put("sessionId", kotlinx.serialization.json.JsonPrimitive(identity.sessionId))
            put("revision", kotlinx.serialization.json.JsonPrimitive(1)); put("phase", kotlinx.serialization.json.JsonPrimitive("active"))
            put("expiresAtUnixMs", kotlinx.serialization.json.JsonPrimitive(1000)); put("endComplete", kotlinx.serialization.json.JsonPrimitive(false))
        })
        observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) },
            { h.owner.observationScope == it.context }, { scope, _ -> V4vMatchedMedia(scope, 1, 1, 0, "/live/event-fixture/a_init.mp4", "/live/event-fixture/a_part1.mp4", true) },
            { _, _, _ -> error("cancelled decode must not confirm") }, OriginDecoder { _, _, _, _ -> entered.countDown(); check(releaseDecode.await(2, TimeUnit.SECONDS)); error("cancelled") },
            { object : OriginObservationResources {
                override fun status() = receipt
                override fun get(path: String, max: Int, cancel: OriginProbeCancellation) = byteArrayOf(1)
                override fun close() { closing.countDown(); check(releaseClose.await(2, TimeUnit.SECONDS)) }
            } })
        val epoch = h.owner.newEpoch()
        try {
            epoch.start(); val first = h.sink(0); first.events(PublishingAttemptEvent.Accepted)
            assertTrue(entered.await(2, TimeUnit.SECONDS)); first.events(PublishingAttemptEvent.Failed())
            h.sink(1) // Failure reached the policy and retry factory before observer IO cleanup returns.
            assertTrue(closing.await(1, TimeUnit.SECONDS)); assertEquals(1L, releaseClose.count)
            assertEquals(1, h.policy.failures.get()); assertNotNull(h.policy.deadlineNs); assertEquals(0, h.policy.confirmations.get())
        } finally {
            releaseClose.countDown(); releaseDecode.countDown(); epoch.stopDestination(); epoch.close()
            observer.close(); assertTrue(observer.awaitStopped(2000))
        }
    }
}
