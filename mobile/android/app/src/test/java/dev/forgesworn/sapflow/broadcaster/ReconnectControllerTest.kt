package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

class ReconnectControllerTest {
    private class Policy : ReconnectPolicyPort {
        override var attemptIdentity: SdkAttemptIdentity? = null
        private var sdkEpoch = 0uL
        private fun connect(): ReconnectAction { attemptIdentity = SdkAttemptIdentity(1uL, ++sdkEpoch); return ReconnectAction.Connect }
        var deadlineNs: Long? = null
        var retry: Long? = null
        override val nextDeadlineNs: Long? get() = listOfNotNull(deadlineNs, retry).minOrNull()
        val progress = mutableListOf<String>()
        var sending = false; var exhausted = false
        var starts = 0; var connections = 0; var confirmations = 0; var stops = 0
        var lastConfirmation: SdkAttemptIdentity? = null
        override fun start(nowNs: Long): ReconnectAction { starts++; return connect() }
        override fun failed(nowNs: Long): ReconnectAction {
            if (exhausted) return ReconnectAction.Expired
            sending = false
            if (deadlineNs == null) deadlineNs = nowNs + 90
            retry = minOf(nowNs + 5, deadlineNs!!)
            return if (nowNs >= deadlineNs!!) ReconnectAction.Expired else ReconnectAction.At(retry!!)
        }
        override fun connected(nowNs: Long) { connections++; retry = null; progress.add("accepted") }
        override fun sending(nowNs: Long): ReconnectAction {
            progress.add("sending")
            if (!sending && deadlineNs?.let { nowNs >= it } == true) return ReconnectAction.Expired
            sending = true
            return if (deadlineNs?.let { nowNs >= it } == true) exhaust() else ReconnectAction.None
        }
        override fun peerProgress(source: PeerProgressSource, nowNs: Long): ReconnectAction {
            check(sending)
            progress.add("peer:$source")
            if (deadlineNs?.let { nowNs >= it } == true) return exhaust()
            deadlineNs = null; return ReconnectAction.None
        }
        private fun exhaust(): ReconnectAction { exhausted = true; deadlineNs = null; return ReconnectAction.Exhausted }
        override fun intentionalReplace(nowNs: Long): ReconnectAction {
            retry = null
            if (exhausted) return ReconnectAction.Expired
            sending = false
            return if (deadlineNs?.let { nowNs >= it } == true) ReconnectAction.Expired else connect()
        }
        override fun confirmed(attempt: SdkAttemptIdentity, nowNs: Long) {
            lastConfirmation = attempt
            check(attempt == attemptIdentity) { "SDK rejected stale attempt" }
            confirmations++; deadlineNs = null; retry = null
        }
        override fun tick(nowNs: Long): ReconnectAction {
            if (deadlineNs?.let { nowNs >= it } == true) return if (sending) exhaust() else ReconnectAction.Expired
            if (retry?.let { nowNs >= it } == true) { retry = null; return connect() }
            return ReconnectAction.None
        }
        override fun stop() { stops++ }
    }
    private class Attempt(val epoch: Long, val token: Long) : PublishingAttempt {
        var starts = 0; var closes = 0; var closeFails = false; var startFails = false
        var onClose: () -> Unit = {}
        override fun start() { starts++; if (startFails) error("synthetic setup failure") }
        override fun close() { closes++; onClose(); if (closeFails) error("synthetic retained owner") }
    }
    private class Harness {
        var now = 0L
        var permitted = true
        val policy = Policy()
        val attempts = mutableListOf<Attempt>()
        val ready = mutableListOf<Pair<Long, Long>>()
        var failNextStart = false
        val controller = ReconnectController(policy, { now }, { permitted }, { epoch, token, _ ->
            assertTrue("old attempt must finish before replacement", attempts.lastOrNull()?.let { it.closes > 0 } != false)
            Attempt(epoch, token).also { it.startFails = failNextStart; failNextStart = false; attempts.add(it) }
        }, { epoch, token -> ready.add(epoch to token) })
        fun accepted() = controller.event(controller.token, PublishingAttemptEvent.Accepted)
        fun failed() = controller.event(controller.token, PublishingAttemptEvent.Failed())
    }
    @Test fun prepareIsDormantAndRetryCreatesFreshTransportAfterOldOwnerCloses() {
        val h = Harness()
        assertEquals(0, h.attempts.size)
        h.controller.attach(1)
        h.accepted(); h.accepted()
        assertEquals(listOf(1L to 1L), h.ready)
        h.failed()
        assertEquals(1, h.attempts[0].closes)
        assertEquals(5L, h.controller.nextWakeNs)
        h.now = 4; h.controller.tick()
        assertEquals(5L, h.controller.nextWakeNs) // An early/stale timer cannot lose backoff.
        assertEquals(1, h.attempts.size)
        h.now = 5; h.controller.tick(); h.accepted()
        assertEquals(2, h.attempts.size)
        assertEquals(listOf(1L to 1L, 1L to 2L), h.ready)
        assertEquals(1, h.policy.starts)
    }
    @Test fun repeatedFailureAndTransportSuccessKeepOriginalOutageDeadlineWithoutObservation() {
        val h = Harness(); h.controller.attach(1); h.accepted()
        h.now = 10; h.failed()
        assertEquals(100L, h.policy.deadlineNs)
        h.now = 15; h.controller.tick(); h.accepted()
        assertEquals(100L, h.controller.nextWakeNs)
        h.now = 20; h.failed()
        assertEquals(100L, h.policy.deadlineNs)
        h.now = 25; h.controller.tick(); h.accepted()
        assertEquals(0, h.policy.confirmations)
        h.now = 100; h.controller.tick()
        assertTrue(h.controller.status.contains("grace expired"))
        h.now = 200; h.controller.tick()
        assertEquals(3, h.attempts.size)
    }
    @Test fun stopDuringBackoffAndLateCallbacksCannotRestartDestination() {
        val h = Harness(); h.controller.attach(1); h.failed()
        val token = h.controller.token
        h.permitted = false // Immediate caller fence precedes queued cleanup.
        h.now = 5; h.controller.tick()
        h.controller.event(token, PublishingAttemptEvent.Accepted)
        h.controller.stop()
        h.now = 100; h.controller.tick()
        assertEquals(1, h.attempts.size)
        assertEquals(0, h.ready.size)
        assertThrows(IllegalStateException::class.java) { h.controller.attach(2) }
        assertEquals(1, h.policy.stops)
    }
    @Test fun stopBeforeStartNeverConstructsTransportAndDuplicateStopIsHarmless() {
        val h = Harness(); h.controller.stop(); h.controller.stop()
        assertEquals(0, h.attempts.size)
        assertEquals(1, h.policy.stops)
        assertThrows(IllegalStateException::class.java) { h.controller.attach(1) }
    }
    @Test fun staleAttemptFailureAndObservationCannotCloseOrConfirmReplacement() {
        val h = Harness(); h.controller.attach(1); h.failed()
        h.now = 5; h.controller.tick(); h.accepted()
        h.controller.event(1, PublishingAttemptEvent.Failed())
        h.controller.event(1, PublishingAttemptEvent.Accepted)
        h.controller.observedMedia(1, 1, checkNotNull(h.policy.attemptIdentity))
        h.controller.observedMedia(99, 2, checkNotNull(h.policy.attemptIdentity))
        assertEquals(0, h.policy.confirmations)
        assertEquals(0, h.attempts[1].closes)
        h.controller.observedMedia(1, 2, checkNotNull(h.policy.attemptIdentity))
        assertEquals(1, h.policy.confirmations)
        assertNull(h.controller.nextWakeNs)
        h.now = 200; h.failed()
        assertEquals(290L, h.policy.deadlineNs) // Only real observation closes prior outage.
    }
    @Test fun orientationRetiresTransportButKeepsPolicyAndOriginalDeadline() {
        val h = Harness(); h.controller.attach(1); h.accepted()
        h.now = 10; h.failed()
        h.controller.detach(1)
        h.now = 15; h.controller.tick() // No codec epoch available, so no transport.
        assertEquals(1, h.attempts.size)
        h.controller.attach(2); h.accepted()
        assertEquals(2L, h.attempts[1].epoch)
        assertEquals(100L, h.policy.deadlineNs)
        assertEquals(1, h.policy.starts)
        h.controller.detach(1) // Delayed old epoch cleanup is harmless.
        assertEquals(0, h.attempts[1].closes)
        h.controller.observedMedia(1, 2, checkNotNull(h.policy.attemptIdentity))
        assertEquals(0, h.policy.confirmations)
    }
    @Test fun orientationCannotCreateTransportAfterOriginalGraceExpires() {
        val h = Harness(); h.controller.attach(1); h.failed(); h.controller.detach(1)
        h.now = 5; h.controller.tick()
        h.now = 90; h.controller.attach(2)
        assertEquals(1, h.attempts.size)
        assertTrue(h.controller.status.contains("grace expired"))
    }
    @Test fun healthyOrientationUsesExplicitReplacementWithoutManufacturingAnOutage() {
        val h = Harness(); h.controller.attach(1); h.accepted()
        h.controller.detach(1)
        assertNull(h.policy.deadlineNs)
        assertEquals(1, h.attempts.size)
        h.now = 1000; h.controller.attach(2); h.accepted()
        assertEquals(2, h.attempts.size)
        assertNull(h.controller.nextWakeNs)
        assertEquals(1, h.policy.starts)
        assertEquals(0, h.policy.confirmations)
    }
    @Test fun cleanupFailureRetainsOwnerAndProhibitsFreshAttempt() {
        val h = Harness(); h.controller.attach(1)
        h.attempts[0].closeFails = true
        assertThrows(IllegalStateException::class.java) { h.failed() }
        assertTrue(h.controller.status.contains("cleanup incomplete"))
        h.now = 100; h.controller.tick()
        assertEquals(1, h.attempts.size)
        assertNull(h.controller.nextWakeNs)
        assertThrows(IllegalStateException::class.java) { h.controller.attach(2) }
    }
    @Test fun startupFailureReleasesPartialAttemptBeforeBoundedRetry() {
        val h = Harness(); h.failNextStart = true; h.controller.attach(1)
        assertEquals(1, h.attempts[0].closes)
        h.now = 5; h.controller.tick()
        assertEquals(2, h.attempts.size)
        assertEquals(1, h.attempts[1].starts)
    }
    @Test fun nonretryableFailureStopsAndUnacceptedTransportCannotConfirmMedia() {
        val h = Harness(); h.controller.attach(1)
        h.controller.observedMedia(1, 1, checkNotNull(h.policy.attemptIdentity))
        assertEquals(0, h.policy.confirmations)
        h.controller.event(1, PublishingAttemptEvent.Failed(retryable = false))
        h.now = 100; h.controller.tick()
        assertEquals(1, h.attempts.size)
        assertEquals(1, h.policy.stops)
    }
    @Test fun slowCleanupCannotExtendTheFirstOutageWindow() {
        val h = Harness(); h.controller.attach(1); h.now = 10
        h.attempts[0].onClose = { h.now = 50 }
        h.failed()
        assertEquals(100L, h.policy.deadlineNs)
        assertEquals(15L, h.controller.nextWakeNs)
        h.controller.tick()
        assertEquals(2, h.attempts.size)
    }
    @Test fun firstFailureIsObservableBeforeCleanupReturnsAndDuplicatesAreIgnored() {
        val closed = AtomicBoolean(false); val notices = AtomicInteger(); val cleanups = AtomicInteger()
        val enteredCleanup = CountDownLatch(1); val finishCleanup = CountDownLatch(1)
        val failure = AtomicReference<Throwable?>()
        val worker = Thread {
            try {
                failAttemptOnce(closed, notify = { notices.incrementAndGet() }, cleanup = {
                    cleanups.incrementAndGet(); enteredCleanup.countDown()
                    check(finishCleanup.await(2, TimeUnit.SECONDS))
                })
            } catch (error: Throwable) { failure.set(error) }
        }.apply { start() }
        try {
            assertTrue(enteredCleanup.await(2, TimeUnit.SECONDS))
            assertTrue(closed.get())
            assertEquals(1, notices.get())
            assertTrue(worker.isAlive)
            failAttemptOnce(closed, notify = { notices.incrementAndGet() }, cleanup = { cleanups.incrementAndGet() })
            assertEquals(1, notices.get()); assertEquals(1, cleanups.get())
        } finally { finishCleanup.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive)
        assertNull(failure.get())
    }
    @Test fun failureObserverExceptionStillRunsCleanupAndCannotReopenAttempt() {
        val closed = AtomicBoolean(false); var cleanups = 0
        assertThrows(IllegalStateException::class.java) {
            failAttemptOnce(closed, notify = { error("synthetic observer failure") }, cleanup = { cleanups++ })
        }
        assertTrue(closed.get()); assertEquals(1, cleanups)
        failAttemptOnce(closed, notify = { fail("duplicate failure") }, cleanup = { cleanups++ })
        assertEquals(1, cleanups)
    }
    @Test fun oneShotProgressBeforeAcceptedIsLatchedAndAppliedInPolicyOrderOnce() {
        val h = Harness(); h.controller.attach(1)
        h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
        h.controller.event(1, PublishingAttemptEvent.Sending)
        assertTrue(h.policy.progress.isEmpty())
        assertEquals(0, h.policy.confirmations)
        h.accepted()
        assertEquals(listOf("accepted", "sending", "peer:RTMP"), h.policy.progress)
        repeat(3) {
            h.accepted(); h.controller.event(1, PublishingAttemptEvent.Sending)
            h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
        }
        assertEquals(3, h.policy.progress.size)
        assertEquals(0, h.policy.confirmations)
        assertTrue(h.controller.status.contains("decoded media unconfirmed"))
    }
    @Test fun failedAttemptDiscardsEarlyProgressBeforeReplacementAcceptance() {
        val h = Harness(); h.controller.attach(1)
        h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        h.failed(); h.now = 5; h.controller.tick()
        h.controller.event(1, PublishingAttemptEvent.Accepted)
        h.controller.event(1, PublishingAttemptEvent.Sending)
        h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        h.accepted()
        assertEquals(listOf("accepted"), h.policy.progress)
        assertEquals(90L, h.policy.deadlineNs)
        h.controller.event(2, PublishingAttemptEvent.Sending)
        h.controller.event(2, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        assertEquals(listOf("accepted", "sending", "peer:SRT"), h.policy.progress)
        assertNull(h.policy.deadlineNs); assertNull(h.controller.nextWakeNs)
        assertEquals(0, h.policy.confirmations)
    }
    @Test fun stopAndRotationCannotCarryPendingProofIntoAnotherEpoch() {
        val h = Harness(); h.controller.attach(1)
        h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
        h.controller.detach(1); h.controller.attach(2); h.accepted()
        assertEquals(listOf("accepted"), h.policy.progress)
        h.controller.event(2, PublishingAttemptEvent.Sending)
        h.controller.stop()
        h.controller.event(2, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
        assertEquals(listOf("accepted", "sending"), h.policy.progress)
    }
    @Test fun acceptedSendingWithoutProofSurvivesDeadlineWithNoWakeOrNewRetry() {
        val h = Harness(); h.controller.attach(1); h.accepted(); h.now = 10; h.failed()
        h.now = 15; h.controller.tick(); h.accepted()
        h.controller.event(2, PublishingAttemptEvent.Sending)
        h.now = 100; h.controller.tick()
        assertFalse(h.controller.terminal); assertNull(h.controller.nextWakeNs)
        assertEquals(0, h.attempts[1].closes)
        assertTrue(h.controller.status.contains("retry budget exhausted"))
        h.controller.event(2, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        assertFalse(h.policy.progress.any { it.startsWith("peer:") })
        repeat(5) { h.controller.tick() }
        assertNull(h.controller.nextWakeNs); assertEquals(2, h.attempts.size)
        h.now = 101; h.failed()
        assertTrue(h.controller.terminal); assertEquals(2, h.attempts.size)
    }
    @Test fun exhaustedSendingOrientationClosesOldOwnerWithoutCreatingNewTransport() {
        val h = Harness(); h.controller.attach(1); h.failed()
        h.now = 5; h.controller.tick(); h.accepted(); h.controller.event(2, PublishingAttemptEvent.Sending)
        h.now = 90; h.controller.tick(); h.controller.detach(1)
        assertTrue(h.controller.terminal); assertEquals(1, h.attempts[1].closes)
        assertThrows(IllegalStateException::class.java) { h.controller.attach(2) }
        assertEquals(2, h.attempts.size)
    }
    @Test fun firstStartupSubmittedAtDeadlineCannotReviveAnUnstartedAttempt() {
        val h = Harness(); h.controller.attach(1); h.failed()
        h.now = 5; h.controller.tick(); h.accepted()
        h.now = 90; h.controller.event(2, PublishingAttemptEvent.Sending)
        assertTrue(h.controller.terminal); assertEquals(1, h.attempts[1].closes)
        assertEquals(0, h.policy.confirmations)
    }
    @Test fun peerWatermarkClearsOnlyNetworkWindowAndLaterFailureStartsNewWindow() {
        val h = Harness(); h.controller.attach(1); h.failed()
        h.now = 5; h.controller.tick(); h.accepted(); h.controller.event(2, PublishingAttemptEvent.Sending)
        h.now = 20; h.controller.event(2, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        assertNull(h.policy.deadlineNs); assertNull(h.controller.nextWakeNs)
        assertEquals(0, h.policy.confirmations)
        h.now = 200; h.failed()
        assertEquals(290L, h.policy.deadlineNs)
    }

    @Test fun oldSdkReplyCannotConfirmTheNewLocalAttemptInTheSameGeneration() {
        val h = Harness(); h.controller.attach(1); h.accepted()
        val old = checkNotNull(h.policy.attemptIdentity)
        h.now = 10; h.failed(); h.now = 15; h.controller.tick(); h.accepted()
        val current = checkNotNull(h.policy.attemptIdentity)
        assertEquals(old.generation, current.generation); assertNotEquals(old.attemptEpoch, current.attemptEpoch)
        h.controller.observedMedia(1, h.controller.token, old)
        assertEquals(0, h.policy.confirmations); assertEquals(100L, h.policy.deadlineNs)
        h.controller.observedMedia(1, h.controller.token, current)
        h.controller.observedMedia(1, h.controller.token, current)
        assertEquals(1, h.policy.confirmations); assertEquals(current, h.policy.lastConfirmation)
    }
    @Test fun finalPolicyReceivesCapturedSdkIdentityRatherThanLatestCompletionSnapshot() {
        val h = Harness(); h.controller.attach(1); h.accepted(); h.now = 10; h.failed()
        h.now = 15; h.controller.tick(); h.accepted()
        val captured = checkNotNull(h.policy.attemptIdentity)
        // Simulates the SDK's final defence if a platform forgot to retire an
        // observation after a separately advanced SDK attempt.
        h.policy.attemptIdentity = captured.copy(attemptEpoch = captured.attemptEpoch + 1uL)
        assertThrows(IllegalStateException::class.java) { h.controller.observedMedia(1, h.controller.token, captured) }
        assertEquals(captured, h.policy.lastConfirmation)
        assertEquals(0, h.policy.confirmations); assertEquals(100L, h.policy.deadlineNs)
    }
    @Test fun laterTransportFactsCannotDowngradeAnActualDecodedObservation() {
        val h = Harness(); h.controller.attach(1); h.accepted()
        h.controller.observedMedia(1, 1, checkNotNull(h.policy.attemptIdentity))
        h.controller.event(1, PublishingAttemptEvent.Sending)
        h.controller.event(1, PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
        assertEquals("Origin confirmed playable media", h.controller.status)
        assertEquals(1, h.policy.confirmations)
    }
}
