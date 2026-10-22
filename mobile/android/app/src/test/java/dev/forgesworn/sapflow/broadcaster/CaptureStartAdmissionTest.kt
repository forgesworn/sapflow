package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** The same cold tickets used before Handler enqueue, at renderer completion,
 * and after recorder creation. Native cleanup is a distinct, required fence. */
class CaptureStartAdmissionTest {
    private class Run {
        val gate = CaptureStartAdmission()
        val preparation = checkNotNull(gate.reserve())
        init { check(gate.claim(preparation)); check(gate.beginEpoch(preparation, 1)); check(gate.prepared(preparation, 1)) }
        fun start() = checkNotNull(gate.start(preparation))
    }

    @Test fun endBeforeQueuedStartDrainsPreventsRendererAndKeepsOwnersUntilCleanup() {
        val r = Run(); val start = r.start()
        assertTrue(r.gate.cancelInitial(r.preparation))
        assertFalse(r.gate.permits(start)) // The queued Handler task cannot start the renderer.
        assertFalse(r.gate.idle)
        assertNull(r.gate.reserve())
        r.gate.beginCleanup(r.preparation, false)
        r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.idle)
    }

    @Test fun endDuringRendererStartRejectsItsLateCompletion() {
        val r = Run(); val start = r.start()
        assertTrue(r.gate.permits(start)) // Renderer request admitted.
        assertTrue(r.gate.cancelInitial(r.preparation))
        assertFalse(r.gate.permits(start)) // Actual renderer completion rechecks this ticket.
        assertFalse(r.gate.commit(start))
        r.gate.beginCleanup(r.preparation, false)
        r.gate.cleanup(r.preparation, false)
        assertNull(r.gate.reserve()) // No scalar release on failed/unknown native detach.
        r.gate.cleanup(r.preparation, true)
        assertNotNull(r.gate.reserve())
    }

    @Test fun endWhileRecorderCreatesOwnersRejectsCommitButRequiresTheirActualCleanup() {
        val r = Run(); val start = r.start()
        val creating = CountDownLatch(1); val created = CountDownLatch(1)
        val committed = AtomicBoolean(true); val recorderOwned = AtomicBoolean(false)
        val worker = Thread {
            check(r.gate.permits(start))
            creating.countDown(); check(created.await(2, TimeUnit.SECONDS))
            recorderOwned.set(true) // Recorder.start has returned with real resources.
            committed.set(r.gate.commit(start))
        }
        worker.start()
        try {
            assertTrue(creating.await(2, TimeUnit.SECONDS))
            assertTrue(r.gate.cancelInitial(r.preparation))
        } finally { created.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive)
        assertTrue(recorderOwned.get()); assertFalse(committed.get())
        assertNull(r.gate.reserve())
        r.gate.beginCleanup(r.preparation, false)
        r.gate.cleanup(r.preparation, false)
        assertNull(r.gate.reserve())
        recorderOwned.set(false); r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.idle)
    }

    @Test fun committedRunEndPreservesRecordingAndSubsequentOrientationEpoch() {
        val r = Run(); val first = r.start()
        assertTrue(r.gate.commit(first))
        assertFalse(r.gate.cancelInitial(r.preparation))
        r.gate.beginCleanup(r.preparation, true)
        assertNull(r.gate.reserve())
        r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.beginEpoch(r.preparation, 2))
        assertTrue(r.gate.prepared(r.preparation, 2))
        val rotated = r.start()
        assertFalse(r.gate.cancelInitial(r.preparation))
        assertTrue(r.gate.commit(rotated))
        assertFalse(r.gate.commit(first))
    }

    @Test fun explicitStopAlsoCancelsCommittedRotationAndCannotBeUndoneByLateCleanup() {
        val r = Run(); assertTrue(r.gate.commit(r.start()))
        r.gate.beginCleanup(r.preparation, true)
        r.gate.stop()
        r.gate.cleanup(r.preparation, true)
        assertFalse(r.gate.beginEpoch(r.preparation, 2))
        assertTrue(r.gate.idle)
    }

    @Test fun staleEndRendererAndCleanupCannotTouchSuccessor() {
        val r = Run(); val old = r.start()
        r.gate.stop(); r.gate.beginCleanup(r.preparation, false); r.gate.cleanup(r.preparation, true)
        val successor = checkNotNull(r.gate.reserve())
        assertTrue(r.gate.claim(successor)); assertTrue(r.gate.beginEpoch(successor, 2)); assertTrue(r.gate.prepared(successor, 2))
        val next = checkNotNull(r.gate.start(successor))
        assertFalse(r.gate.cancelInitial(r.preparation)); assertFalse(r.gate.commit(old))
        r.gate.cleanup(r.preparation, true); r.gate.abandon(r.preparation)
        assertTrue(r.gate.permits(next)); assertTrue(r.gate.commit(next))
    }

    @Test fun cancelledQueuedPrepareRetainsSlotUntilItsTaskAbandons() {
        val gate = CaptureStartAdmission(); val queued = checkNotNull(gate.reserve())
        assertTrue(gate.cancelInitial(queued)); assertFalse(gate.claim(queued))
        gate.beginCleanup(queued, false); gate.cleanup(queued, true)
        assertNull(gate.reserve()) // Stop's no-resource branch cannot release queued ownership.
        gate.abandon(queued)
        assertNotNull(gate.reserve())
    }

    @Test fun rotationBeforeFirstRunStillRequiresInitialEndApproval() {
        val r = Run()
        r.gate.beginCleanup(r.preparation, true); r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.beginEpoch(r.preparation, 2)); assertTrue(r.gate.prepared(r.preparation, 2))
        val start = r.start()
        assertTrue(r.gate.cancelInitial(r.preparation)); assertFalse(r.gate.commit(start))
    }

    @Test fun duplicateStartAndRendererCompletionCannotCommitTwice() {
        val r = Run(); val start = r.start()
        assertNull(r.gate.start(r.preparation))
        assertTrue(r.gate.commit(start)); assertFalse(r.gate.commit(start))
        assertNull(r.gate.start(r.preparation))
    }

    @Test fun queuedStartFromOldEpochCannotAdmitAfterPreparedRotation() {
        val r = Run(); val old = r.start()
        r.gate.beginCleanup(r.preparation, true); r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.beginEpoch(r.preparation, 2)); assertTrue(r.gate.prepared(r.preparation, 2))
        assertFalse(r.gate.permits(old)); assertFalse(r.gate.commit(old))
        assertTrue(r.gate.commit(r.start()))
    }

    @Test fun endClickUsesCurrentPreparationEvenBeforeItsDisplayRecomposes() {
        val r = Run()
        var current = r.preparation
        // Like the UI click handler, read the current ticket when invoked,
        // rather than copying it when the controls were last composed.
        val endClick = { r.gate.cancelInitial(current) }
        r.gate.stop(); r.gate.beginCleanup(r.preparation, false); r.gate.cleanup(r.preparation, true)
        current = checkNotNull(r.gate.reserve())
        assertTrue(r.gate.claim(current)); assertTrue(r.gate.beginEpoch(current, 2)); assertTrue(r.gate.prepared(current, 2))
        val queued = checkNotNull(r.gate.start(current))
        assertTrue(endClick())
        assertFalse(r.gate.permits(queued)); assertFalse(r.gate.commit(queued))
    }

    @Test fun queuedOldStopCannotResampleAndRetireANewerPreparation() {
        val r = Run(); val stopped = checkNotNull(r.gate.stop())
        // The cold Stop captured A, but its Handler continuation has not run.
        r.gate.beginCleanup(stopped, false); r.gate.cleanup(stopped, true)
        val successor = checkNotNull(r.gate.reserve())
        assertTrue(r.gate.claim(successor)); assertTrue(r.gate.beginEpoch(successor, 2)); assertTrue(r.gate.prepared(successor, 2))
        val next = checkNotNull(r.gate.start(successor))
        assertFalse(r.gate.owns(stopped)) // Production refuses all queued old Stop work here.
        r.gate.beginCleanup(stopped, false); r.gate.cleanup(stopped, true)
        assertTrue(r.gate.permits(next)); assertTrue(r.gate.commit(next))
    }

    @Test fun clockCommitEndAndStopShareTheActualAdmissionLinearisation() {
        val r = Run(); val first = r.start(); val second = 1_000_000_000L
        assertTrue(r.gate.commit(first, second))
        assertFalse(r.gate.cancelInitial(r.preparation, 4 * second)) // End preserves an admitted recording.
        r.gate.beginCleanup(r.preparation, true); r.gate.cleanup(r.preparation, true)
        assertTrue(r.gate.beginEpoch(r.preparation, 2)); assertTrue(r.gate.prepared(r.preparation, 2))
        assertTrue(r.gate.commit(r.start(), 8 * second))
        val running = r.gate.elapsed(r.preparation, 10 * second, true, null)
        assertEquals(9_000L, running.captureMs); assertEquals(3_000L, running.broadcastMs)
        r.gate.stop(11 * second)
        val stopped = r.gate.elapsed(r.preparation, 20 * second, true, null)
        assertEquals(10_000L, stopped.captureMs); assertEquals(3_000L, stopped.broadcastMs)
        assertNull(r.gate.reserve()) // Timing freeze is not actual resource cleanup.
    }

    @Test fun stopOrEndWinningBeforeCommitCannotBeResurrectedByALateClockStart() {
        val r = Run(); val queued = r.start()
        assertTrue(r.gate.cancelInitial(r.preparation, 10))
        assertFalse(r.gate.commit(queued, 20))
        assertNull(r.gate.elapsed(r.preparation, 30, true, null).captureMs)
        assertNull(r.gate.elapsed(r.preparation, 30, true, null).broadcastMs)
    }

    @Test fun staleTimingReadAndEndCannotFreezeSuccessorAndFinalElapsedSurvivesCleanup() {
        val r = Run(); assertTrue(r.gate.commit(r.start(), 1_000_000))
        r.gate.stop(3_000_000); r.gate.beginCleanup(r.preparation, false); r.gate.cleanup(r.preparation, true)
        assertEquals(2L, r.gate.elapsed(r.preparation, 4_000_000, true, null).captureMs)
        val next = checkNotNull(r.gate.reserve()); assertTrue(r.gate.claim(next)); assertTrue(r.gate.beginEpoch(next, 2)); assertTrue(r.gate.prepared(next, 2))
        assertTrue(r.gate.commit(checkNotNull(r.gate.start(next)), 10_000_000))
        assertFalse(r.gate.cancelInitial(r.preparation, 11_000_000))
        assertTrue(r.gate.elapsed(r.preparation, 12_000_000, true, 1).uncertain)
        assertEquals(2L, r.gate.elapsed(next, 12_000_000, true, null).broadcastMs)
    }
    @Test fun fixedPreparationRemainsExactThroughPendingStartAndRotationCleanup() {
        val gate = CaptureStartAdmission()
        val selection = checkNotNull(CaptureOrientationChoice.LANDSCAPE_TOP_LEFT.freeze(true, false, CardinalPose.REVERSE))
        val ticket = checkNotNull(gate.reserve(orientation = selection))
        assertSame(selection, ticket.orientation)
        assertNull(gate.reserve(orientation = CaptureOrientationChoice.PORTRAIT.freeze(true, true, CardinalPose.NATURAL)))
        assertFalse(gate.permitsOrientation(ticket)) // Permission wait cannot admit a physical/qualification override.
        assertTrue(gate.claim(ticket)); assertTrue(gate.beginEpoch(ticket, 1)); assertTrue(gate.prepared(ticket, 1))
        val start = checkNotNull(gate.start(ticket))
        assertFalse(gate.permitsOrientation(ticket)); assertTrue(gate.commit(start, 10))
        gate.beginCleanup(ticket, true); gate.cleanup(ticket, true)
        assertTrue(gate.beginEpoch(ticket, 2)); assertTrue(gate.prepared(ticket, 2))
        assertTrue(gate.commit(checkNotNull(gate.start(ticket)), 20))
        assertSame(selection, ticket.orientation)
        assertEquals(CardinalPose.NATURAL, ticket.orientation?.initialPose)
    }

    @Test fun queuedAutomaticPoseCannotMutateFixedOrAutomaticSuccessor() {
        for (nextChoice in listOf(CaptureOrientationChoice.PORTRAIT, CaptureOrientationChoice.AUTO)) {
            val gate = CaptureStartAdmission(); val old = checkNotNull(gate.reserve())
            assertTrue(gate.claim(old)); assertTrue(gate.beginEpoch(old, 1))
            val restart = OrientationRestartPlan()
            val delayed = { if (gate.permitsOrientation(old)) restart.request(CardinalPose.REVERSE, true, 10) else null }
            assertTrue(gate.stop(old, 1)); gate.beginCleanup(old, false); gate.cleanup(old, true)
            val frozen = checkNotNull(nextChoice.freeze(true, true, CardinalPose.CLOCKWISE_90))
            val next = checkNotNull(gate.reserve(orientation = frozen)); assertTrue(gate.claim(next))
            restart.request(frozen.initialPose, false, 2)
            assertNull(delayed())
            assertEquals(frozen.initialPose, restart.desiredPose); assertNull(restart.pending)
            assertTrue(gate.owns(next)); assertSame(frozen, next.orientation)
        }
    }

    @Test fun automaticCoalescesOnlyItsOwnPreservedRotationUntilSynchronousStop() {
        val gate = CaptureStartAdmission(); val ticket = checkNotNull(gate.reserve())
        assertTrue(gate.claim(ticket)); assertTrue(gate.beginEpoch(ticket, 1)); assertTrue(gate.prepared(ticket, 1))
        assertTrue(gate.commit(checkNotNull(gate.start(ticket)), 1))
        val restart = OrientationRestartPlan(); val first = checkNotNull(restart.request(CardinalPose.CLOCKWISE_90, true, 2))
        gate.beginCleanup(ticket, true)
        assertTrue(gate.permitsOrientation(ticket))
        if (gate.permitsOrientation(ticket)) restart.request(CardinalPose.REVERSE, true, 3)
        assertEquals(first, restart.pending); assertEquals(CardinalPose.REVERSE, restart.desiredPose)
        assertTrue(gate.stop(ticket, 4)); assertFalse(gate.permitsOrientation(ticket))
        val delayed = { if (gate.permitsOrientation(ticket)) restart.request(CardinalPose.CLOCKWISE_270, true, 5) }
        delayed(); assertEquals(CardinalPose.REVERSE, restart.desiredPose)
        gate.cleanup(ticket, false); assertNull(gate.reserve())
        gate.cleanup(ticket, true); assertNotNull(gate.reserve())
    }

    @Test fun microphonePreparationDiscardsAnyVideoSelectionAndNeverAdmitsPose() {
        val gate = CaptureStartAdmission()
        val ticket = checkNotNull(gate.reserve(CaptureIntent.AudioOnly48kMono,
            orientation = CaptureOrientationChoice.PORTRAIT.freeze(true, true, CardinalPose.NATURAL)))
        assertNull(ticket.orientation); assertFalse(gate.permitsOrientation(ticket))
        assertTrue(gate.claim(ticket)); assertTrue(gate.beginEpoch(ticket, 1)); assertFalse(gate.permitsOrientation(ticket))
    }
    @Test fun initialCameraIsFrozenBeforeClaimAndAppliedFrontSurvivesAutomaticRotation() {
        val gate = CaptureStartAdmission(); val a = checkNotNull(gate.reserve(cameraChoice = CameraSourceChoice.REAR))
        assertEquals(CameraSourceChoice.REAR, a.cameraChoice)
        assertNull(gate.reserve(cameraChoice = CameraSourceChoice.FRONT))
        assertTrue(gate.claim(a)); assertTrue(gate.beginEpoch(a, 1)); assertTrue(gate.prepared(a, 1))
        assertTrue(gate.commit(checkNotNull(gate.start(a)), 1))
        assertTrue(gate.selectCamera(a, CameraSourceChoice.FRONT))
        gate.beginCleanup(a, true); gate.cleanup(a, true)
        assertTrue(gate.beginEpoch(a, 2)); assertEquals(CameraSourceChoice.FRONT, gate.cameraChoice(a))
        assertEquals(CameraSourceChoice.REAR, a.cameraChoice) // Immutable original intent, distinct applied source.
        assertTrue(gate.stop(a, 2)); assertFalse(gate.selectCamera(a, CameraSourceChoice.REAR))
        gate.beginCleanup(a, false); gate.cleanup(a, true)
        val b = checkNotNull(gate.reserve(cameraChoice = CameraSourceChoice.REAR))
        assertFalse(gate.selectCamera(a, CameraSourceChoice.FRONT)); assertNull(gate.cameraChoice(a))
        assertEquals(CameraSourceChoice.REAR, gate.cameraChoice(b))
    }

    @Test fun microphoneIntentHasNoCameraSelectionOrDiscoveryAuthority() {
        val gate = CaptureStartAdmission(); val a = checkNotNull(gate.reserve(CaptureIntent.AudioOnly48kMono, cameraChoice = CameraSourceChoice.FRONT))
        assertNull(a.cameraChoice); assertNull(gate.cameraChoice(a)); assertFalse(gate.permitsCamera(a))
        assertFalse(gate.selectCamera(a, CameraSourceChoice.FRONT))
    }
    @Test fun initialStandbyIsImmutableThroughPreparedStartAndOrientationChild() {
        val admission=CaptureStartAdmission();val p=checkNotNull(admission.reserve(initialStandby=true))
        assertTrue(p.initialStandby);assertTrue(admission.claim(p));assertTrue(admission.beginEpoch(p,1));assertTrue(admission.prepared(p,1))
        assertTrue(admission.commit(checkNotNull(admission.start(p))))
        admission.beginCleanup(p,true);admission.cleanup(p,true)
        assertSame(p,admission.preparation);assertTrue(admission.beginEpoch(p,2));assertTrue(p.initialStandby)
        admission.stop(p);admission.beginCleanup(p,false);admission.cleanup(p,true)
        val successor=checkNotNull(admission.reserve());assertFalse(successor.initialStandby)
    }

}
