package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CameraSourcePolicyTest {
    private fun activate(policy: CameraSourcePolicy, op: CameraSourcePolicy.Operation, now: Long) {
        assertTrue(policy.cameraClosed(op, now)); assertTrue(policy.textureReady(op, now))
        assertTrue(policy.configured(op, now)); assertTrue(policy.frame(op, now))
    }
    @Test fun actualCameraCloseThenTextureThenConfigureThenFrameIsTheOnlyAdmissionOrder() {
        val policy = CameraSourcePolicy(); val op = checkNotNull(policy.begin(CameraSourceChoice.REAR, 1))
        val calls = mutableListOf<String>()
        fun texture() { if (policy.cameraClosed(op, 2)) calls += "replace-texture" }
        fun open() { if (policy.textureReady(op, 3)) calls += "open-camera" }
        assertFalse(policy.configured(op, 1)); assertFalse(policy.frame(op, 1)); open(); assertTrue(calls.isEmpty())
        texture(); texture(); assertEquals(listOf("replace-texture"), calls)
        open(); open(); assertEquals(listOf("replace-texture", "open-camera"), calls)
        assertFalse(policy.frame(op, 3)); assertTrue(policy.configured(op, 4)); assertFalse(policy.active(op))
        assertTrue(policy.frame(op, 5)); assertTrue(policy.active(op)); assertFalse(policy.pending())
    }
    @Test fun switchingRetiresOldCallbacksAndHoldsOneOriginalDeadlineThroughSlowCleanup() {
        val policy = CameraSourcePolicy(6); val a = checkNotNull(policy.begin(CameraSourceChoice.REAR, 1)); activate(policy, a, 2)
        val b = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 3))
        assertFalse(policy.owns(a)); assertFalse(policy.frame(a, 4)); assertFalse(policy.cameraClosed(a, 4))
        assertNull(policy.begin(CameraSourceChoice.REAR, 5)); assertTrue(policy.pending())
        assertEquals(9L, b.deadlineNs)
        assertTrue(policy.cameraClosed(b, 8)); assertFalse(policy.textureReady(b, 9)); assertFalse(policy.configured(b, 9))
        assertFalse(policy.frame(b, 9)); assertFalse(policy.active(b))
    }
    @Test fun withheldTimerCannotAdmitALateNativeOwnerOrFirstFrame() {
        for (boundary in 0..3) {
            val policy = CameraSourcePolicy(6); val op = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 10))
            if (boundary > 0) assertTrue(policy.cameraClosed(op, 11))
            if (boundary > 1) assertTrue(policy.textureReady(op, 12))
            if (boundary > 2) assertTrue(policy.configured(op, 13))
            assertFalse(when(boundary) { 0 -> policy.cameraClosed(op, 16); 1 -> policy.textureReady(op, 16); 2 -> policy.configured(op, 16); else -> policy.frame(op, 16) })
            assertFalse(policy.active(op))
        }
    }
    @Test fun stopAtEveryHandoffBoundaryPreventsLateCompletionAndSuccessorResurrection() {
        for (boundary in 0..4) {
            val policy = CameraSourcePolicy(); val op = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 1))
            if (boundary > 0) assertTrue(policy.cameraClosed(op, 2))
            if (boundary > 1) assertTrue(policy.textureReady(op, 3))
            if (boundary > 2) assertTrue(policy.configured(op, 4))
            if (boundary > 3) assertTrue(policy.frame(op, 5))
            policy.close()
            assertFalse(policy.owns(op)); assertFalse(policy.frame(op, 6)); assertFalse(policy.cameraClosed(op, 6))
            assertFalse(policy.textureReady(op, 6)); assertFalse(policy.configured(op, 6)); assertNull(policy.begin(CameraSourceChoice.REAR, 6))
        }
    }
    @Test fun activeSourceDoesNotExpireFromHistoricalSwitchDeadlineButNewSwitchHasItsOwnBudget() {
        val policy = CameraSourcePolicy(6); val a = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 1)); activate(policy, a, 2)
        assertTrue(policy.permits(a, 100)); assertTrue(policy.active(a))
        val b = checkNotNull(policy.begin(CameraSourceChoice.REAR, 101)); assertEquals(107L, b.deadlineNs)
        assertFalse(policy.permits(b, 100)); assertFalse(policy.permits(b, 107))
        assertNull(CameraSourcePolicy().begin(CameraSourceChoice.REAR, Long.MAX_VALUE))
    }
    @Test fun latestSliderSlotRemainsBoundedAndCloseDiscardsPending() {
        val mailbox = CameraControlMailbox()
        assertTrue(mailbox.offer(CameraControlSettings(zoom = 1f)))
        repeat(1000) { assertFalse(mailbox.offer(CameraControlSettings(zoom = 2f + it))) }
        assertEquals(1001f, checkNotNull(mailbox.take()).zoom)
        assertNull(mailbox.take()); assertTrue(mailbox.offer(CameraControlSettings(zoom = 3f)))
        mailbox.close(); assertNull(mailbox.take()); assertFalse(mailbox.offer(CameraControlSettings()))
    }
    @Test fun nativeCapabilitiesRejectUnsupportedNonFiniteAndOutOfRangeAdjustments() {
        val caps = CameraControlCapabilities(1f, 4f, false, false, -2, 2, 0.5f, false, listOf(1, 5), false)
        assertTrue(caps.accepts(CameraControlSettings()))
        for (bad in listOf(CameraControlSettings(zoom = Float.NaN), CameraControlSettings(zoom = 0.9f), CameraControlSettings(zoom = 4.1f),
            CameraControlSettings(focusLocked = true), CameraControlSettings(exposure = 3), CameraControlSettings(exposureLocked = true),
            CameraControlSettings(whiteBalance = 7), CameraControlSettings(whiteBalanceLocked = true))) assertFalse(caps.accepts(bad))
        assertTrue(caps.accepts(CameraControlSettings(zoom = 4f, exposure = -2, whiteBalance = 5)))
    }
    @Test fun lateDeviceDeliveryAfterStopIsClosedAndCannotClaimNativeCleanupEarly() {
        val lifetime = CameraDeviceLifecycle(); lifetime.beginOpen()
        assertEquals(CameraDeviceLifecycle.Action.WAIT, lifetime.requestClose(false))
        assertFalse(lifetime.closed)
        var actualCloseCalls = 0
        if (lifetime.opened() == CameraDeviceLifecycle.Action.CLOSE_DEVICE) actualCloseCalls++
        assertEquals(1, actualCloseCalls)
        assertFalse(lifetime.closed)
        assertEquals(CameraDeviceLifecycle.Action.WAIT, lifetime.requestClose(true))
        lifetime.nativeClosed()
        assertTrue(lifetime.closed); assertEquals(CameraDeviceLifecycle.Action.COMPLETE, lifetime.requestClose(false))
    }
    @Test fun ordinaryCloseDoesNotReleaseUntilActualNativeReceiptAndFailedOpenCreatesNoOwner() {
        val lifetime = CameraDeviceLifecycle(); lifetime.beginOpen()
        assertEquals(CameraDeviceLifecycle.Action.ACCEPT, lifetime.opened())
        assertEquals(CameraDeviceLifecycle.Action.CLOSE_DEVICE, lifetime.requestClose(true))
        assertFalse(lifetime.closed); assertEquals(CameraDeviceLifecycle.Action.WAIT, lifetime.requestClose(true))
        lifetime.nativeClosed(); assertTrue(lifetime.closed)
        val failed = CameraDeviceLifecycle(); failed.beginOpen(); failed.failedOpenWithoutDevice()
        assertEquals(CameraDeviceLifecycle.Action.COMPLETE, failed.requestClose(false))
    }
    @Test fun sourceTokensDoNotRepeatAcrossCodecRotationSoOldControlsCannotBorrowTheNewSource() {
        val first = CameraSourcePolicy(); val old = checkNotNull(first.begin(CameraSourceChoice.REAR, 1)); activate(first, old, 2)
        val front = checkNotNull(first.begin(CameraSourceChoice.FRONT, 3)); activate(first, front, 4); first.close()
        val next = CameraSourcePolicy(initialSerial = front.serial)
        val rotated = checkNotNull(next.begin(CameraSourceChoice.FRONT, 5)); activate(next, rotated, 6)
        assertNotEquals(old.serial, rotated.serial); assertNotEquals(front.serial, rotated.serial)
        assertFalse(next.acceptsControl(old.serial)); assertFalse(next.acceptsControl(front.serial)); assertTrue(next.acceptsControl(rotated.serial))
        assertFalse(next.owns(front)); assertTrue(next.active(rotated))
        next.close(); assertFalse(next.acceptsControl(rotated.serial)); assertFalse(next.active(rotated))
    }
    @Test fun pendingSwitchReservesBeforeQueueAndDuplicateGesturesCannotBuildATaskBacklog() {
        val slot = CameraRequestSlot(); val first = checkNotNull(slot.reserve())
        repeat(1000) { assertNull(slot.reserve()) }
        assertTrue(slot.owns(first)); slot.release(first)
        val second = checkNotNull(slot.reserve()); slot.release(first)
        assertTrue(slot.owns(second)); assertNull(slot.reserve())
        slot.close(); assertFalse(slot.owns(second)); assertNull(slot.reserve())
    }
    @Test fun firstFrameDeadlineIsCheckedBeforeSubmissionEvenWhenCodecTimerIsWithheld() {
        val policy = CameraSourcePolicy(6); val op = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 10))
        assertTrue(policy.cameraClosed(op, 11)); assertTrue(policy.textureReady(op, 12)); assertTrue(policy.configured(op, 13))
        val native = CameraRunAdmission(1).also { assertTrue(it.beginSource(op).admitted) }
        val submits = mutableListOf<Long>()
        if (native.confirmSource(op.serial, 16)) submits += 16
        assertTrue(submits.isEmpty()); assertNull(native.confirmed(op.serial)); assertFalse(policy.active(op))
    }
    @Test fun timelyRealFrameReceiptSurvivesDelayedOwnerCallbackWithoutRenewingTheDeadline() {
        val policy = CameraSourcePolicy(6); val op = checkNotNull(policy.begin(CameraSourceChoice.FRONT, 10))
        assertTrue(policy.cameraClosed(op, 11)); assertTrue(policy.textureReady(op, 12)); assertTrue(policy.configured(op, 13))
        val native = CameraRunAdmission(1).also { assertTrue(it.beginSource(op).admitted) }
        assertTrue(native.confirmSource(op.serial, 15))
        assertTrue(native.confirmSource(op.serial, 30)); assertEquals(15L, native.confirmed(op.serial))
        // Owner timer is delivered at30; it consumes the actual native15 receipt.
        assertTrue(policy.frame(op, checkNotNull(native.confirmed(op.serial)))); assertTrue(policy.active(op))
        native.fail(); assertNull(native.confirmed(op.serial)); assertFalse(native.confirmSource(op.serial, 31))
        val bOp = checkNotNull(CameraSourcePolicy(6, op.serial).begin(CameraSourceChoice.FRONT, 40))
        val b = CameraRunAdmission(2).also { assertTrue(it.beginSource(bOp).admitted) }
        assertFalse(b.confirmSource(op.serial, 41)); assertNull(b.confirmed(op.serial))
        assertFalse(b.confirmSource(op.serial+1, 39)); assertFalse(b.confirmSource(op.serial+1, 46))
    }
    @Test fun liveSourceGapUsesLastSuccessfulEncoderSubmissionNotPreviewAndNeverRetimes() {
        val timeline = CameraSourceTimeline(20)
        assertTrue(timeline.frame(100, true, false)) // Prepared preview.
        assertTrue(timeline.frame(200, true, false)) // Preview switches have no media-gap obligation.
        assertTrue(timeline.frame(210, false, true)); timeline.submitted(210)
        assertTrue(timeline.frame(220, false, false)) // Preview alone cannot renew last submitted video.
        assertFalse(timeline.frame(230, true, true)) // Exact20 gap rejects before native submission/confirmation.
        assertFalse(timeline.frame(231, false, true)) // Sticky refusal.
        val timely = CameraSourceTimeline(20)
        assertTrue(timely.frame(100, false, true)); timely.submitted(100)
        assertTrue(timely.frame(119, true, true)); timely.submitted(119)
        assertFalse(timely.frame(118, false, true))
    }
    @Test fun pendingOldPoseReplayCannotOverwriteNewAdmittedQueuedObservationOrSuccessor() {
        val input = CameraPoseInput<Any>(); val a = Any(); val b = Any()
        val old = input.record(a, CardinalPose.CLOCKWISE_90)
        val newer = input.record(a, CardinalPose.CLOCKWISE_270) // Already admitted while source completion runs.
        val restart = OrientationRestartPlan()
        fun deliver(value: CameraPoseInput.Observation<Any>) {
            if (input.isCurrent(value) && value.owner === a) restart.request(value.pose, false, 1)
        }
        deliver(old); assertEquals(CardinalPose.NATURAL, restart.desiredPose)
        input.latest(a)?.let(::deliver)
        assertEquals(CardinalPose.CLOCKWISE_270, restart.desiredPose)
        deliver(newer); assertEquals(CardinalPose.CLOCKWISE_270, restart.desiredPose)
        input.record(b, CardinalPose.REVERSE)
        assertNull(input.latest(a)); deliver(old); deliver(newer)
        assertEquals(CardinalPose.CLOCKWISE_270, restart.desiredPose)
        assertSame(b, checkNotNull(input.latest(b)).owner)
    }
    @Test fun nativeControlRequestsHoldOneActualObservationAndOnlyLatestSliderUntilItsCompletion() {
        val native = CameraNativeControlQueue()
        assertTrue(native.offer(CameraControlSettings(zoom = 1f)))
        val first = checkNotNull(native.take()); assertTrue(native.pending())
        repeat(1000) { assertTrue(native.offer(CameraControlSettings(zoom = 2f+it))) }
        assertNull(native.take()); assertFalse(native.complete(first.serial+1))
        assertTrue(native.complete(first.serial)); val latest = checkNotNull(native.take())
        assertEquals(1001f, latest.settings.zoom)
        assertFalse(native.complete(first.serial)); assertTrue(native.matches(latest.serial))
        assertTrue(native.complete(latest.serial)); assertFalse(native.pending())
        assertTrue(native.offer(CameraControlSettings(zoom = 2f))); native.close()
        assertNull(native.take()); assertFalse(native.offer(CameraControlSettings()))
    }
    @Test fun focusTriggersAreSingleTransitionsAndStatusRefreshAlwaysUsesIdle() {
        val unlocked = CameraControlSettings(); val locked = unlocked.copy(focusLocked = true)
        assertEquals(CameraFocusTransition.START, CameraFocusTransition.between(unlocked,locked))
        assertEquals(CameraFocusTransition.IDLE, CameraFocusTransition.between(locked,locked))
        assertEquals(CameraFocusTransition.IDLE, CameraFocusTransition.between(locked,locked.copy(zoom=2f)))
        assertEquals(CameraFocusTransition.CANCEL, CameraFocusTransition.between(locked,unlocked))
        assertEquals(CameraFocusTransition.IDLE, CameraFocusTransition.between(unlocked,unlocked))
    }
    @Test fun whiteBalanceLockIsOnlyAcceptedForNativeAutomaticWhiteBalance() {
        val caps = CameraControlCapabilities(1f,4f,true,true,-2,2,.5f,true,listOf(1,5),true)
        assertTrue(caps.accepts(CameraControlSettings(whiteBalanceLocked=true)))
        assertFalse(caps.accepts(CameraControlSettings(whiteBalance=5,whiteBalanceLocked=true)))
        assertTrue(caps.accepts(CameraControlSettings(whiteBalance=5)))
    }
    @Test fun cameraFaultFencesCapturedRawReaderPumpAndFanoutWithoutWaitingForCodecHandler() {
        val run = CameraRunAdmission(1); val successor = CameraRunAdmission(2)
        val beforeReadReturn = java.util.concurrent.CountDownLatch(1)
        val returnRead = java.util.concurrent.CountDownLatch(1)
        val writes = mutableListOf<String>()
        val worker = Thread {
            check(run.admit(1)); beforeReadReturn.countDown()
            check(returnRead.await(2, java.util.concurrent.TimeUnit.SECONDS))
            if (run.admit(2)) writes += "publish-pcm"
            if (run.admit(3)) writes += "native-queue-input"
            if (run.admit(4)) writes += "fanout-new-media"
        }
        worker.start()
        try {
            assertTrue(beforeReadReturn.await(2, java.util.concurrent.TimeUnit.SECONDS))
            run.fail() // GL thread: owner callback/timer/Stop handler deliberately never runs.
            assertFalse(run.recordingCanFinalise)
            assertTrue(successor.admit(5)); assertTrue(successor.recordingCanFinalise)
        } finally { returnRead.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive); assertTrue(writes.isEmpty())
    }
    @Test fun normalStopAndRotationCancelOnlySourceBudgetWhileActualFaultStaysIncomplete() {
        val op = checkNotNull(CameraSourcePolicy(6).begin(CameraSourceChoice.FRONT, 10))
        val normal = CameraRunAdmission(1); assertTrue(normal.beginSource(op).admitted); normal.cancelSourceBudget()
        assertTrue(normal.admit(100)); assertTrue(normal.recordingCanFinalise)
        assertFalse(normal.confirmSource(op.serial, 15)) // Canceled source cannot resurrect.
        val faulted = CameraRunAdmission(2); assertTrue(faulted.beginSource(op).admitted)
        assertFalse(faulted.admit(16)); assertEquals(CameraRunAdmission.Fault.SOURCE_DEADLINE,faulted.failure)
        faulted.cancelSourceBudget(); faulted.fail()
        assertFalse(faulted.admit(17)); assertFalse(faulted.recordingCanFinalise)
        assertEquals(CameraRunAdmission.Fault.SOURCE_DEADLINE,faulted.failure)
        assertFalse(faulted.beginSource(op).admitted); assertFalse(faulted.confirmSource(op.serial, 15))
    }
    @Test fun heldOldExpiryReadRetriesTimelyNativeConfirmationRatherThanFalseFaulting() {
        val read = java.util.concurrent.CountDownLatch(1); val resume = java.util.concurrent.CountDownLatch(1)
        val once = java.util.concurrent.atomic.AtomicBoolean()
        val run = CameraRunAdmission(1) { if (once.compareAndSet(false,true)) { read.countDown(); check(resume.await(2,java.util.concurrent.TimeUnit.SECONDS)) } }
        val op = checkNotNull(CameraSourcePolicy(6).begin(CameraSourceChoice.FRONT, 10)); assertTrue(run.beginSource(op).admitted)
        val admitted = java.util.concurrent.atomic.AtomicBoolean()
        val worker = Thread { admitted.set(run.admit(30)) }; worker.start()
        try {
            assertTrue(read.await(2,java.util.concurrent.TimeUnit.SECONDS))
            assertTrue(run.confirmSource(op.serial, 15))
        } finally { resume.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive); assertTrue(admitted.get()); assertNull(run.failure)
        assertEquals(15L,run.confirmed(op.serial)); assertTrue(run.recordingCanFinalise)
    }
    @Test fun stopBeforeDelayedSourceBeginCannotRearmTheCapturedRun() {
        val run = CameraRunAdmission(1)
        val op = checkNotNull(CameraSourcePolicy(6).begin(CameraSourceChoice.FRONT, 10))
        val selected = java.util.concurrent.CountDownLatch(1); val resume = java.util.concurrent.CountDownLatch(1)
        val began = java.util.concurrent.atomic.AtomicBoolean(true)
        val failureCallbacks = java.util.concurrent.atomic.AtomicInteger()
        val worker = Thread {
            selected.countDown() // Source selection passed; owner handler is paused before beginSource.
            check(resume.await(2,java.util.concurrent.TimeUnit.SECONDS))
            val decision = run.beginSource(op)
            began.set(decision.admitted)
            if (decision.requiresFailure) failureCallbacks.incrementAndGet() // Same Controller failure branch.
        }; worker.start()
        try {
            assertTrue(selected.await(2,java.util.concurrent.TimeUnit.SECONDS))
            run.cancelSourceBudget() // Synchronous Stop wins before the queued source admission.
        } finally { resume.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive); assertFalse(began.get()); assertFalse(run.confirmSource(op.serial,15))
        assertEquals(0,failureCallbacks.get()); assertEquals(CameraRunAdmission.SourceAdmission.CANCELLED,run.beginSource(op))
        assertTrue(run.admit(100)); assertTrue(run.recordingCanFinalise); assertNull(run.failure)
        val successor = CameraRunAdmission(2); assertTrue(successor.beginSource(op).admitted); assertTrue(successor.confirmSource(op.serial,15))
        run.fail() // A real late GL fault still makes this old run incomplete, never its successor.
        assertFalse(run.admit(101)); assertFalse(run.recordingCanFinalise); assertTrue(successor.recordingCanFinalise)
        val fault = run.beginSource(op)
        if (fault.requiresFailure) failureCallbacks.incrementAndGet()
        assertEquals(CameraRunAdmission.SourceAdmission.FAULTED,fault); assertEquals(1,failureCallbacks.get())
    }
    @Test fun stopWinningAgainstHeldExpiryReadPreservesTailWithoutReopeningSourceAdmission() {
        val read = java.util.concurrent.CountDownLatch(1); val resume = java.util.concurrent.CountDownLatch(1)
        val run = CameraRunAdmission(1) { read.countDown(); check(resume.await(2,java.util.concurrent.TimeUnit.SECONDS)) }
        val op = checkNotNull(CameraSourcePolicy(6).begin(CameraSourceChoice.FRONT,10)); assertTrue(run.beginSource(op).admitted)
        val admitted = java.util.concurrent.atomic.AtomicBoolean()
        val worker = Thread { admitted.set(run.admit(30)) }; worker.start()
        try { assertTrue(read.await(2,java.util.concurrent.TimeUnit.SECONDS)); run.cancelSourceBudget() }
        finally { resume.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive); assertTrue(admitted.get()); assertTrue(run.recordingCanFinalise)
        assertFalse(run.beginSource(op).admitted); assertFalse(run.confirmSource(op.serial,15)); assertTrue(run.admit(100))
    }
    @Test fun heldNativeSubmissionKeepsOriginalPcmDeadlineAndLateSuccessCannotConfirm() {
        val run = CameraRunAdmission(1)
        val op = checkNotNull(CameraSourcePolicy(6).begin(CameraSourceChoice.FRONT,10)); assertTrue(run.beginSource(op).admitted)
        val inSwap = java.util.concurrent.CountDownLatch(1); val returnSwap = java.util.concurrent.CountDownLatch(1)
        val confirmed = java.util.concurrent.atomic.AtomicBoolean(true)
        val worker = Thread {
            check(run.admit(15)) // Validated geometry and pre-submit check, but no confirmation yet.
            inSwap.countDown(); check(returnSwap.await(2,java.util.concurrent.TimeUnit.SECONDS))
            confirmed.set(run.confirmSource(op.serial,17)) // Native swap returns after the original deadline.
        }; worker.start()
        try {
            assertTrue(inSwap.await(2,java.util.concurrent.TimeUnit.SECONDS))
            assertFalse(run.admit(16)) // PCM expires the run with all owner timers/callbacks withheld.
        } finally { returnSwap.countDown(); worker.join(2000) }
        assertFalse(worker.isAlive); assertFalse(confirmed.get()); assertNull(run.confirmed(op.serial))
        assertFalse(run.recordingCanFinalise); assertFalse(run.admit(18))
    }
    @Test fun privacyRetiresOnlyActualExactSourceBudgetWithoutUndoingStopCancellation() {
        val sources=CameraSourcePolicy();val a=checkNotNull(sources.begin(CameraSourceChoice.REAR,0))
        val run=CameraRunAdmission(1);assertTrue(run.beginSource(a).admitted)
        val foreign=checkNotNull(CameraSourcePolicy().begin(CameraSourceChoice.REAR,0))
        assertFalse(run.retiredSource(foreign));assertTrue(run.retiredSource(a));assertTrue(run.admit(7_000_000_000))
        sources.close();val b=checkNotNull(CameraSourcePolicy(initialSerial=a.serial).begin(CameraSourceChoice.FRONT,8_000_000_000))
        assertTrue(run.beginSource(b).admitted);run.cancelSourceBudget()
        assertFalse(run.retiredSource(b));assertEquals(CameraRunAdmission.SourceAdmission.CANCELLED,run.beginSource(b));assertTrue(run.admit(Long.MAX_VALUE))
    }

    @Test fun previewAbandonedBeforeSubmissionDetachesWithoutDrawingOrFaultingShow() {
        var draws=0;var detached=0;var classifications=0
        CameraGlRenderer.previewSubmission(false,{classifications++;false},{detached++}){draws++}
        assertEquals(0,draws);assertEquals(1,detached);assertEquals(0,classifications)
    }
    @Test fun nativePreviewLossDuringSubmissionUsesActualValidityAndDetachesOnlyPreview() {
        for(error in listOf(android.opengl.EGL14.EGL_BAD_SURFACE,android.opengl.EGL14.EGL_BAD_NATIVE_WINDOW,android.opengl.EGL14.EGL_SUCCESS)) {
            var valid=true;var submitted=0;var detached=0
            CameraGlRenderer.previewSubmission(valid,
                {CameraGlRenderer.isExpectedPreviewLoss(valid,error)},{detached++}) {
                submitted++;valid=false;throw IllegalStateException("Generated preview abandonment")
            }
            assertEquals(1,submitted);assertEquals(1,detached)
        }
    }
    @Test fun previewContextDisplayAndAllocationFailuresRemainFatalEvenWhenWindowIsInvalid() {
        for(error in listOf(android.opengl.EGL14.EGL_CONTEXT_LOST,android.opengl.EGL14.EGL_BAD_CONTEXT,android.opengl.EGL14.EGL_BAD_DISPLAY,android.opengl.EGL14.EGL_BAD_ALLOC)) {
            val actual=IllegalStateException("Generated EGL failure");var detached=0
            val thrown=assertThrows(IllegalStateException::class.java) {
                CameraGlRenderer.previewSubmission(true,
                    {CameraGlRenderer.isExpectedPreviewLoss(false,error)},{detached++}){throw actual}
            }
            assertSame(actual,thrown);assertEquals(0,detached)
        }
    }
    @Test fun expectedPreviewLossNeverHidesActualDetachCleanupFailure() {
        val actual=IllegalStateException("Generated detach failure")
        for(valid in listOf(false,true)) {
            val thrown=assertThrows(IllegalStateException::class.java) {
                CameraGlRenderer.previewSubmission(valid,{true},{throw actual}){error("Generated preview loss")}
            }
            assertSame(actual,thrown)
        }
    }

    private fun sourcePermit(run: CameraRunAdmission): Pair<RecordingSourceGate.FinishPermit, Any> {
        val gate = run.recordingSource; val owner = Any(); val prepared = Any(); val generation = Any()
        val key = gate.bindProducer(owner, prepared, run.epoch); val recorder = gate.bindRecorder(Any()); val writer = Any()
        gate.generationStarted(key, generation); gate.generationClosed(key, generation, 1024, true, true)
        recorder.writerOpened(writer); recorder.accepted(); recorder.consumed()
        return checkNotNull(recorder.drained(gate.terminal(key, owner, prepared, run.epoch))) to writer
    }
    @Test fun realSourceExpiryRefusesTheRecordingPublicationCutWithoutATimer() {
        val run = CameraRunAdmission(7); val (permit, writer) = sourcePermit(run)
        val policy = CameraSourcePolicy(); val operation = checkNotNull(policy.begin(CameraSourceChoice.REAR, 10))
        assertTrue(run.beginSource(operation).admitted); assertFalse(run.admit(operation.deadlineNs))
        synchronized(run.recordingSource.guard) { assertThrows(IllegalStateException::class.java) { run.recordingSource.requireCommitLocked(permit, writer) } }
    }
    @Test fun ordinaryStopBudgetCancellationDoesNotBecomeARecordingLoss() {
        val run = CameraRunAdmission(7); val (permit, writer) = sourcePermit(run)
        run.cancelSourceBudget(); assertTrue(run.admit(Long.MAX_VALUE))
        synchronized(run.recordingSource.guard) { run.recordingSource.commitLocked(permit, writer) }
    }

}
