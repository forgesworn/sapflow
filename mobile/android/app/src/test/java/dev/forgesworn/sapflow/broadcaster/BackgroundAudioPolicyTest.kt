package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

class BackgroundAudioPolicyTest {
    private class Show(val gate: CaptureStartAdmission = CaptureStartAdmission()) {
        val preparation = checkNotNull(gate.reserve(CaptureIntent.AudioOnly48kMono, backgroundAudio = true))
        fun prepare() {
            check(gate.claim(preparation)); check(gate.beginEpoch(preparation, 1)); check(gate.prepared(preparation, 1))
        }
        fun commit() = check(gate.commit(checkNotNull(gate.start(preparation)), 1))
        fun clean() { gate.beginCleanup(preparation, false); gate.cleanup(preparation, true) }
    }
    private val a = "01".repeat(16)
    private val b = "02".repeat(16)

    @Test fun onlyPromotedCommittedOptInAudioCanContinue() {
        val show = Show()
        assertFalse(show.gate.continuesInBackground(show.preparation)) // queued Prepare
        show.prepare()
        assertNull(show.gate.start(show.preparation)) // Queued service start is not promotion.
        assertFalse(show.gate.continuesInBackground(show.preparation)) // PREPARED
        assertTrue(show.gate.servicePromoted(show.preparation))
        val starting = checkNotNull(show.gate.start(show.preparation))
        assertFalse(show.gate.continuesInBackground(show.preparation)) // STARTING
        assertTrue(show.gate.commit(starting, 1))
        assertTrue(show.gate.continuesInBackground(show.preparation))
        assertTrue(show.gate.stop(show.preparation, 2))
        assertFalse(show.gate.continuesInBackground(show.preparation))
    }

    @Test fun defaultOffAndVideoNeverBecomeBackgroundOwners() {
        val gate = CaptureStartAdmission()
        assertNull(gate.reserve(CaptureIntent.DEFAULT, backgroundAudio = true))
        val value = checkNotNull(gate.reserve(CaptureIntent.AudioOnly48kMono))
        assertFalse(value.backgroundAudio)
        assertFalse(gate.servicePromoted(value))
        val policy = BackgroundAudioPolicy()
        assertNull(policy.reserve(value, a))
    }

    @Test fun capabilityAndSerialMustBothMatchTheActualOwner() {
        val show = Show(); val policy = BackgroundAudioPolicy()
        assertNull(policy.reserve(show.preparation, "A".repeat(32)))
        assertNull(policy.reserve(show.preparation, "01"))
        val first = checkNotNull(policy.reserve(show.preparation, a))
        assertNull(policy.matches(show.preparation.id, b))
        assertNull(policy.matches(show.preparation.id + 1, a))
        assertSame(first, policy.matches(show.preparation.id, a))
        assertFalse(policy.ready(show.preparation))
        assertTrue(policy.promote(first)); assertTrue(policy.ready(show.preparation))
    }

    @Test fun oldNotificationAndOldPromotionCannotStopOrPromoteSuccessor() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation)); show.commit()
        val policy = BackgroundAudioPolicy(); val first = checkNotNull(policy.reserve(show.preparation, a))
        assertTrue(policy.promote(first))
        assertTrue(show.gate.stop(show.preparation)); show.clean(); assertTrue(policy.retire(first))
        val next = checkNotNull(show.gate.reserve(CaptureIntent.AudioOnly48kMono, backgroundAudio = true))
        val second = checkNotNull(policy.reserve(next, b))
        assertNull(policy.matches(show.preparation.id, a))
        assertFalse(show.gate.stop(show.preparation))
        assertFalse(show.gate.servicePromoted(show.preparation))
        assertFalse(policy.promote(first)); assertFalse(policy.retire(first))
        assertSame(second, policy.current()); assertTrue(show.gate.owns(next))
        assertFalse(show.gate.backgroundReady(next))
    }

    @Test fun serviceFailureWhileStartIsQueuedCannotResurrectAndKeepsCleanupCapacity() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation))
        val start = checkNotNull(show.gate.start(show.preparation))
        assertTrue(show.gate.stop(show.preparation)) // onDestroy or promotion/permission failure
        assertFalse(show.gate.commit(start))
        assertFalse(show.gate.servicePromoted(show.preparation))
        show.gate.beginCleanup(show.preparation, false); show.gate.cleanup(show.preparation, false)
        assertNull(show.gate.reserve())
        show.gate.cleanup(show.preparation, true)
        assertTrue(show.gate.idle)
        assertFalse(show.gate.servicePromoted(show.preparation)) // No sticky Intent re-admission.
    }

    @Test fun endBroadcastPreservesCommittedRecordingButNeverStartsPendingCapture() {
        val active = Show(); active.prepare(); check(active.gate.servicePromoted(active.preparation)); active.commit()
        assertFalse(active.gate.cancelInitial(active.preparation, 2))
        assertTrue(active.gate.continuesInBackground(active.preparation))
        val pending = Show(); pending.prepare(); check(pending.gate.servicePromoted(pending.preparation))
        val start = checkNotNull(pending.gate.start(pending.preparation))
        assertTrue(pending.gate.cancelInitial(pending.preparation, 2))
        assertFalse(pending.gate.commit(start, 3))
        assertFalse(pending.gate.continuesInBackground(pending.preparation))
    }

    @Test fun cleanupAndRotationGapNeverAdvertiseBackgroundAdmission() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation)); show.commit()
        show.gate.beginCleanup(show.preparation, true)
        assertFalse(show.gate.continuesInBackground(show.preparation))
        assertNull(show.gate.reserve())
    }

    @Test fun unexpectedServiceLossIsStickyButNormalStopIsNotReclassified() {
        val interrupted = Show(); interrupted.prepare(); check(interrupted.gate.servicePromoted(interrupted.preparation)); interrupted.commit()
        assertTrue(interrupted.gate.interrupt(interrupted.preparation))
        assertFalse(interrupted.gate.interrupt(interrupted.preparation))
        assertFalse(interrupted.gate.continuesInBackground(interrupted.preparation))
        val normal = Show(); normal.prepare(); check(normal.gate.servicePromoted(normal.preparation)); normal.commit()
        assertTrue(normal.gate.stop(normal.preparation))
        assertFalse(normal.gate.interrupt(normal.preparation)) // Service destruction cannot turn user's save into a new fault.
    }

    @Test fun oldCapabilityCannotMatchANewProcessEvenWhenSerialStartsAgain() {
        val old = Show(); val policy = BackgroundAudioPolicy()
        val prior = checkNotNull(policy.reserve(old.preparation, a)); assertTrue(policy.retire(prior))
        val newProcess = Show() // A fresh admission starts at the same local serial.
        assertEquals(old.preparation.id, newProcess.preparation.id)
        val next = checkNotNull(policy.reserve(newProcess.preparation, b))
        assertNull(policy.matches(old.preparation.id, a))
        assertSame(next, policy.matches(newProcess.preparation.id, b))
    }

    @Test fun staleActivityStopCannotStopTheSuccessorPublisherEvenWithNoForegroundClient() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation)); show.commit()
        show.gate.stop(show.preparation); show.clean()
        val next = checkNotNull(show.gate.reserve(CaptureIntent.AudioOnly48kMono, backgroundAudio = true))
        check(show.gate.claim(next)); check(show.gate.beginEpoch(next, 2)); check(show.gate.prepared(next, 2))
        check(show.gate.servicePromoted(next)); check(show.gate.commit(checkNotNull(show.gate.start(next))))
        val publisherStops = AtomicInteger()
        // The actual Activity calls stopPublisher ONLY when its exact Stop wins,
        // even when a stale foreground-client callback says not to continue.
        if (show.gate.stop(show.preparation)) publisherStops.incrementAndGet()
        assertEquals(0, publisherStops.get())
        assertTrue(show.gate.continuesInBackground(next))
    }

    @Test fun repeatedServiceTicksWhileHandlerAndCleanupAreBlockedDispatchOnlyOneStop() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation)); show.commit()
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val dispatches = AtomicInteger()
        val handler = Thread {
            entered.countDown(); check(release.await(2, TimeUnit.SECONDS))
            show.clean()
        }
        try {
            repeat(1_000) {
                if (show.gate.stop(show.preparation)) {
                    dispatches.incrementAndGet(); handler.start()
                    check(entered.await(2, TimeUnit.SECONDS))
                }
            }
            assertEquals(1, dispatches.get())
            assertTrue(show.gate.owns(show.preparation)) // Real cleanup still retains capacity/notification.
            assertNull(show.gate.reserve())
        } finally { release.countDown(); handler.join(2_000) }
        assertFalse(handler.isAlive); assertTrue(show.gate.idle)
    }

    @Test fun ordinaryPromotionWaitNeverRetiresThePreparedShow() {
        val show = Show(); show.prepare(); val policy = BackgroundAudioPolicy()
        val owner = checkNotNull(policy.reserve(show.preparation, a))
        repeat(20) {
            assertFalse(policy.ready(show.preparation)); assertNull(show.gate.start(show.preparation))
            assertTrue(show.gate.owns(show.preparation))
        }
        check(show.gate.servicePromoted(show.preparation)); check(policy.promote(owner))
        assertTrue(policy.ready(show.preparation)); show.commit()
        assertTrue(show.gate.continuesInBackground(show.preparation))
    }

    @Test fun delayedOldActivityDepartureIsAlwaysStaleAfterSameShowReattachment() {
        val show = Show(); show.prepare(); check(show.gate.servicePromoted(show.preparation)); show.commit()
        val clients = CaptureForegroundClients(); val a = Any(); val b = Any()
        clients.enter(a)
        clients.enter(b) // B attaches the SAME committed preparation, not a successor show.
        assertEquals(CaptureForegroundClients.Departure.STALE, clients.leave(a, mayContinue = false))
        assertTrue(clients.present())
        assertEquals(CaptureForegroundClients.Departure.CONTINUE, clients.leave(b, mayContinue = true))
        assertFalse(clients.present())
        assertEquals(CaptureForegroundClients.Departure.STALE, clients.leave(a, mayContinue = false))
        assertEquals(CaptureForegroundClients.Departure.STALE, clients.leave(b, mayContinue = false))
        assertTrue(show.gate.continuesInBackground(show.preparation))
        clients.enter(b)
        assertEquals(CaptureForegroundClients.Departure.STOP, clients.leave(b, mayContinue = false))
    }
}
