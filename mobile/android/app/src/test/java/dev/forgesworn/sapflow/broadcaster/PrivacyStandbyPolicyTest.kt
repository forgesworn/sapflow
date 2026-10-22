package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

class PrivacyStandbyPolicyTest {
    private class Fixture(video: Boolean = true, standby: Boolean = false,
        sinks: PrivacyStandbyPolicy.Sinks = PrivacyStandbyPolicy.Sinks.BOTH, revision: Long = 0) {
        val owner = Any(); val descriptor = Any()
        val policy = PrivacyStandbyPolicy(owner, video, standby, sinks, 6, revision)
        val run = checkNotNull(policy.attach(owner, 1, descriptor, 1, 10))
        fun start() = policy.beganMedia(run, 10)
        fun pause(now: Long = 11) = checkNotNull(policy.standby(run, now))
        fun audio(c: PrivacyStandbyPolicy.Command, generation: Long = 2, now: Long = 12): PrivacyStandbyPolicy.MediaProof {
            check(policy.audioClosed(c, generation - 1, now))
            check(policy.zeroAudioReady(c, generation, descriptor, now))
            return checkNotNull(policy.zeroAudioEncoded(c, generation, 1024, 1024, now))
        }
        fun video(c: PrivacyStandbyPolicy.Command, now: Long = 12): PrivacyStandbyPolicy.MediaProof {
            check(policy.pictureSubmitted(c, 3000, now))
            return checkNotNull(policy.pictureEncoded(c, 3000, true, now))
        }
        fun accept(proof: PrivacyStandbyPolicy.MediaProof, now: Long = 12) {
            check(policy.accepted(proof, PrivacyStandbyPolicy.Sink.PUBLISHING, now))
            check(policy.accepted(proof, PrivacyStandbyPolicy.Sink.RECORDING, now))
        }
        fun confirmed(): PrivacyStandbyPolicy.Command {
            start(); val c = pause(); accept(video(c)); accept(audio(c)); return c
        }
    }
    @Test fun liveDefaultDoesNotCreateAStandbyDeadlineOrStartMediaOnAttach() {
        val f = Fixture(); val s = f.policy.snapshot()
        assertEquals(PrivacyStandbyPolicy.Phase.LIVE, s.phase); assertNull(s.pending)
        assertFalse(f.policy.permitsRaw(f.run,s.revision)); assertNull(f.start())
        assertTrue(f.policy.permitsRaw(f.run,s.revision))
    }
    @Test fun frozenPrestartStandbyHasNoPrepareClockOrOutputClaim() {
        val f = Fixture(standby = true)
        assertEquals(PrivacyStandbyPolicy.Phase.ARMED,f.policy.snapshot().phase)
        assertFalse(f.policy.permitsRaw(f.run,0)); assertTrue(f.policy.tick(f.run,1000))
        val c = checkNotNull(f.policy.beganMedia(f.run,1001)); assertEquals(1001L,c.startedNs); assertEquals(1007L,c.deadlineNs)
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
    }
    @Test fun exactGeneratedIdrAndFreshAudioMustEachReachBothActualSinkMarkers() {
        val f = Fixture(); f.start(); val c = f.pause(); val video = f.video(c); val audio = f.audio(c)
        assertTrue(f.policy.accepted(video,PrivacyStandbyPolicy.Sink.PUBLISHING,12))
        assertTrue(f.policy.accepted(video,PrivacyStandbyPolicy.Sink.PUBLISHING,12)) // Idempotent, not a second sink.
        f.accept(audio)
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        assertTrue(f.policy.accepted(video,PrivacyStandbyPolicy.Sink.RECORDING,13))
        assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
        assertFalse(f.policy.permitsRaw(f.run,c.revision))
    }
    @Test fun microphoneOnlyNeverNeedsOrAcceptsVideoEvidence() {
        val f = Fixture(video = false); assertNull(f.policy.reserveSource(f.run,0)); f.start(); val c = f.pause()
        assertFalse(f.policy.pictureSubmitted(c,3000,12)); assertNull(f.policy.pictureEncoded(c,3000,true,12))
        f.accept(f.audio(c)); assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
    }
    @Test fun oldAudioMustActuallyCloseBeforeFreshZeroOnlyGenerationWithRetainedDescriptor() {
        val f = Fixture(); f.start(); val c = f.pause()
        assertFalse(f.policy.zeroAudioReady(c,2,f.descriptor,12))
        assertFalse(f.policy.audioClosed(c,99,12)); assertTrue(f.policy.audioClosed(c,1,12))
        assertFalse(f.policy.zeroAudioReady(c,2,Any(),12)) // Equal ASC alone would not prove descriptor identity.
        assertFalse(f.policy.zeroAudioReady(c,1,f.descriptor,12))
        assertTrue(f.policy.zeroAudioReady(c,2,f.descriptor,12)); assertTrue(f.policy.zeroAudioReady(c,2,f.descriptor,12))
        assertTrue(f.policy.audioClosed(c,1,12)) // Duplicate old-close receipt remains scoped to the retired owner.
        assertFalse(f.policy.audioClosed(c,2,12)) // Fresh owner closure is never the old-owner receipt.
        assertFalse(f.policy.zeroAudioReady(c,3,f.descriptor,12)) // Cannot create an additional active encoder.
        assertNull(f.policy.zeroAudioEncoded(c,1,1024,1024,12))
        assertNull(f.policy.zeroAudioEncoded(c,2,1024,960,12))
        assertNull(f.policy.zeroAudioEncoded(c,2,-1,1024,12))
        assertNotNull(f.policy.zeroAudioEncoded(c,2,1024,1024,12))
    }
    @Test fun initialStandbyCanCreateFreshZeroEncoderWhenNoOldNativeAudioExists() {
        val owner = Any(); val descriptor = Any(); val p = PrivacyStandbyPolicy(owner,false,true,budgetNs=6)
        val r = checkNotNull(p.attach(owner,1,descriptor,null,10)); val c = checkNotNull(p.beganMedia(r,10))
        assertTrue(p.zeroAudioReady(c,1,descriptor,11)); assertNotNull(p.zeroAudioEncoded(c,1,0,1024,11))
    }
    @Test fun actualIdrAndMatchedGeneratedPtsAreRequiredRatherThanAKeyframeRequest() {
        val f = Fixture(); f.start(); val c = f.pause()
        assertNull(f.policy.pictureEncoded(c,3000,true,12))
        assertTrue(f.policy.pictureSubmitted(c,3000,12)); assertTrue(f.policy.pictureSubmitted(c,6000,12))
        assertNull(f.policy.pictureEncoded(c,3000,false,12)); assertNull(f.policy.pictureEncoded(c,9000,true,12))
        val proof = checkNotNull(f.policy.pictureEncoded(c,6000,true,12))
        assertSame(proof,f.policy.pictureEncoded(c,6000,true,12)); assertNull(f.policy.pictureEncoded(c,3000,true,12))
    }
    @Test fun audioProofCannotBeRelabelledOntoAnotherAccessUnit() {
        val f = Fixture(); f.start(); val c = f.pause(); val proof = f.audio(c)
        assertSame(proof,f.policy.zeroAudioEncoded(c,2,1024,1024,12))
        assertNull(f.policy.zeroAudioEncoded(c,2,2048,1024,12))
    }
    @Test fun repeatedStandbyCoalescesOriginalCommandWithoutDeadlineRenewal() {
        val f = Fixture(); f.start(); val c = f.pause()
        assertSame(c,f.policy.standby(f.run,15)); assertEquals(17L,c.deadlineNs)
        assertFalse(f.policy.permitsRaw(f.run,c.revision)); assertFalse(f.policy.tick(f.run,17))
        assertEquals(PrivacyStandbyPolicy.Fault.DEADLINE,f.policy.snapshot().fault)
    }
    @Test fun newerStandbySupersedesPendingResumeAndKeepsOriginalDeadline() {
        val f = Fixture(); f.confirmed(); val pauseRevision = f.policy.snapshot().revision
        val resume = checkNotNull(f.policy.userResume(f.run,pauseRevision,13))
        val newer = checkNotNull(f.policy.standby(f.run,14))
        assertEquals(resume.startedNs,newer.startedNs); assertEquals(resume.deadlineNs,newer.deadlineNs)
        assertTrue(newer.revision > resume.revision)
        assertFalse(f.policy.resumed(resume,2,true,true,14)); assertFalse(f.policy.permitsRaw(f.run,newer.revision))
        assertSame(newer,f.policy.standby(f.run,15))
    }
    @Test fun staleResumeUiRevisionCannotRequestOrCommitLive() {
        val f = Fixture(); f.confirmed(); val revision = f.policy.snapshot().revision
        assertNull(f.policy.userResume(f.run,revision-1,13))
        val resume = checkNotNull(f.policy.userResume(f.run,revision,13))
        assertFalse(f.policy.resumed(resume,1,true,true,14))
        assertFalse(f.policy.resumed(resume,2,false,true,14)); assertFalse(f.policy.resumed(resume,2,true,false,14))
        assertFalse(f.policy.permitsRaw(f.run,resume.revision)); assertTrue(f.policy.resumed(resume,2,true,true,14))
        assertTrue(f.policy.permitsRaw(f.run,resume.revision)); assertFalse(f.policy.permitsRaw(f.run,revision))
    }
    @Test fun audioOnlyExplicitResumeRequiresMicButNoCamera() {
        val f = Fixture(video=false); f.start(); val c=f.pause(); f.accept(f.audio(c))
        val resume=checkNotNull(f.policy.userResume(f.run,c.revision,13))
        assertTrue(f.policy.resumed(resume,2,true,false,14)); assertTrue(f.policy.permitsRaw(f.run,resume.revision))
    }
    @Test fun urgentStandbyFencesRawWhileSourceMutationIsHeldAndJoinsActualCleanup() {
        val f=Fixture(); f.start(); val source=checkNotNull(f.policy.reserveSource(f.run,0))
        assertNull(f.policy.reserveSource(f.run,0)); val c=f.pause()
        assertFalse(f.policy.permitsRaw(f.run,0)); assertFalse(f.policy.permitsRaw(f.run,c.revision))
        assertNull(f.policy.userResume(f.run,c.revision,12)); assertNull(f.policy.reserveSource(f.run,c.revision))
        f.accept(f.video(c)); f.accept(f.audio(c))
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        assertTrue(f.policy.sourceClosed(source,true,13)); assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
        assertFalse(f.policy.sourceClosed(source,true,14))
    }
    @Test fun uncertainSourceCleanupFailsClosedWithoutRawFallback() {
        val f=Fixture(); f.start(); val source=checkNotNull(f.policy.reserveSource(f.run,0)); val c=f.pause()
        assertFalse(f.policy.sourceClosed(source,false,12)); assertFalse(f.policy.permitsRaw(f.run,c.revision))
        assertFalse(f.policy.permitsReplacement(c,12)); assertEquals(PrivacyStandbyPolicy.Fault.NATIVE,f.policy.snapshot().fault)
    }
    @Test fun confirmedReplacementKeepsRunningPastHistoricalDeadlineAndDuringPendingResume() {
        val f=Fixture(); val c=f.confirmed()
        assertTrue(f.policy.permitsReplacement(c,1000)); assertTrue(f.policy.tick(f.run,1000))
        val resume=checkNotNull(f.policy.userResume(f.run,c.revision,1000))
        assertTrue(f.policy.permitsReplacement(c,1001)); assertFalse(f.policy.permitsRaw(f.run,resume.revision))
        assertTrue(f.policy.resumed(resume,2,true,true,1002)); assertFalse(f.policy.permitsReplacement(c,1002))
    }
    @Test fun pendingOrientationChildKeepsDesiredStandbyAndTheOneOriginalDeadline() {
        val f=Fixture(); f.start(); val c=f.pause(); val oldProof=f.video(c)
        assertTrue(f.policy.fenceRun(f.run)); assertNull(f.policy.attach(f.owner,2,Any(),3,12))
        assertTrue(f.policy.runClosed(f.run)); val next=checkNotNull(f.policy.attach(f.owner,2,Any(),3,13))
        val child=checkNotNull(f.policy.snapshot().pending)
        assertEquals(c.startedNs,child.startedNs); assertEquals(c.deadlineNs,child.deadlineNs)
        assertEquals(PrivacyStandbyPolicy.Desired.STANDBY,f.policy.snapshot().desired)
        assertFalse(f.policy.permitsRaw(next,child.revision)); assertFalse(f.policy.accepted(oldProof,PrivacyStandbyPolicy.Sink.RECORDING,14))
        assertFalse(f.policy.permitsReplacement(c,14)); assertTrue(f.policy.permitsReplacement(child,14))
        assertFalse(f.policy.tick(next,17))
    }
    @Test fun settledStandbyOrientationChildGetsANewBoundedConstructionOperationButNeverLive() {
        val f=Fixture(); f.confirmed(); assertTrue(f.policy.fenceRun(f.run)); assertTrue(f.policy.runClosed(f.run))
        val child=checkNotNull(f.policy.attach(f.owner,2,Any(),3,100)); val c=checkNotNull(f.policy.snapshot().pending)
        assertEquals(100L,c.startedNs); assertEquals(106L,c.deadlineNs); assertFalse(f.policy.permitsRaw(child,c.revision))
        assertFalse(f.policy.runClosed(f.run)); assertFalse(f.policy.fenceRun(f.run))
    }
    @Test fun deferredRawCallbackRechecksExactRunAndRevisionAfterPrivacyFence() {
        val f=Fixture(); f.start(); val entered=CountDownLatch(1); val release=CountDownLatch(1); val admitted=AtomicBoolean(true)
        val worker=Thread { entered.countDown(); check(release.await(2,TimeUnit.SECONDS)); admitted.set(f.policy.permitsRaw(f.run,0)) };worker.start()
        try { assertTrue(entered.await(2,TimeUnit.SECONDS)); f.pause() }
        finally { release.countDown();worker.join(2000) }
        assertFalse(worker.isAlive);assertFalse(admitted.get())
    }
    @Test fun stopIsStickyAtEveryProofBoundaryAndDoesNotStopAnotherPreparation() {
        for(step in 0..3) {
            val f=Fixture();f.start();val c=f.pause();val video=if(step>0)f.video(c) else null;val audio=if(step>1)f.audio(c) else null
            if(step>2)f.accept(checkNotNull(video))
            assertFalse(f.policy.stop(Any()));assertTrue(f.policy.stop(f.owner));assertFalse(f.policy.stop(f.owner))
            assertFalse(f.policy.permitsRaw(f.run,c.revision));assertFalse(f.policy.permitsReplacement(c,12))
            video?.let{assertFalse(f.policy.accepted(it,PrivacyStandbyPolicy.Sink.RECORDING,12))}
            audio?.let{assertFalse(f.policy.accepted(it,PrivacyStandbyPolicy.Sink.PUBLISHING,12))}
            assertNull(f.policy.userResume(f.run,c.revision,12));assertNull(f.policy.attach(f.owner,2,Any(),3,12))
            assertEquals(PrivacyStandbyPolicy.Phase.STOPPED,f.policy.snapshot().phase)
        }
    }
    @Test fun withheldTimerExpiryAndClockRollbackRefuseInputAndOutputProofs() {
        for(now in listOf(10L,17L,100L)) {
            val f=Fixture();f.start();val c=f.pause();assertFalse(f.policy.permitsReplacement(c,now))
            assertFalse(f.policy.pictureSubmitted(c,3000,now));assertFalse(f.policy.permitsRaw(f.run,c.revision))
            assertNotNull(f.policy.snapshot().fault);assertNull(f.policy.standby(f.run,now))
        }
    }
    @Test fun exhaustedRevisionAndDeadlineOverflowFailClosedBeforeAnyReplacement() {
        val f=Fixture(revision=Long.MAX_VALUE);f.start();assertNull(f.policy.standby(f.run,11))
        assertEquals(PrivacyStandbyPolicy.Fault.EXHAUSTED,f.policy.snapshot().fault);assertFalse(f.policy.permitsRaw(f.run,Long.MAX_VALUE))
        val other=Fixture();other.start();assertNull(other.policy.standby(other.run,Long.MAX_VALUE))
        assertEquals(PrivacyStandbyPolicy.Fault.CLOCK,other.policy.snapshot().fault);assertFalse(other.policy.permitsRaw(other.run,0))
    }
    @Test fun unmatchedSyntheticInputLedgerIsBoundedRatherThanGrowingUntilAnIdrArrives() {
        val f=Fixture();f.start();val c=f.pause()
        repeat(181){assertTrue(f.policy.pictureSubmitted(c,it*3000L,12))}
        assertFalse(f.policy.pictureSubmitted(c,181*3000L,12));assertEquals(PrivacyStandbyPolicy.Fault.EXHAUSTED,f.policy.snapshot().fault)
    }
    @Test fun unattachedSinkCannotBeInventedToConfirmLocalRecordingOnly() {
        val f=Fixture(video=false,sinks=PrivacyStandbyPolicy.Sinks.RECORDING_ONLY);f.start();val c=f.pause();val proof=f.audio(c)
        assertFalse(f.policy.accepted(proof,PrivacyStandbyPolicy.Sink.PUBLISHING,12))
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        assertTrue(f.policy.accepted(proof,PrivacyStandbyPolicy.Sink.RECORDING,12));assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
    }
    @Test fun staleEpochAndWrongPreparationCannotReplaceCurrentRun() {
        val f=Fixture();f.start();assertNull(f.policy.attach(Any(),2,Any(),2,11))
        assertTrue(f.policy.fenceRun(f.run));assertTrue(f.policy.runClosed(f.run));assertNull(f.policy.attach(f.owner,1,Any(),2,11))
        val next=checkNotNull(f.policy.attach(f.owner,2,Any(),2,11));assertNull(f.policy.standby(f.run,12))
        assertTrue(f.policy.permitsRaw(next,f.policy.snapshot().revision))
    }
    @Test fun heldOldAacRetirementCannotBeBypassedByResumeReadyBooleans() {
        val f=Fixture();f.start();val c=f.pause()
        assertNull(f.policy.userResume(f.run,c.revision,12)) // Old AAC actual close is deliberately withheld.
        assertFalse(f.policy.resumed(c,1,true,true,12));assertFalse(f.policy.permitsRaw(f.run,c.revision))
        assertTrue(f.policy.audioClosed(c,1,13));assertNull(f.policy.userResume(f.run,c.revision,13))
        assertTrue(f.policy.zeroAudioReady(c,2,f.descriptor,13));assertNull(f.policy.userResume(f.run,c.revision,13))
        f.accept(f.video(c,13),13)
        f.accept(checkNotNull(f.policy.zeroAudioEncoded(c,2,1024,1024,13)),13)
        val resume=checkNotNull(f.policy.userResume(f.run,c.revision,14))
        assertFalse(f.policy.audioClosed(c,1,14));assertTrue(f.policy.resumed(resume,2,true,true,15))
    }
    @Test fun prestartStandbyPriorityJoinsSourceCleanupThenArmsWithoutClaimingOutput() {
        val f=Fixture();val source=checkNotNull(f.policy.reserveSource(f.run,0));val c=f.pause()
        assertFalse(f.policy.permitsRaw(f.run,0));assertFalse(f.policy.pictureSubmitted(c,3000,12))
        assertFalse(f.policy.audioClosed(c,1,12));assertFalse(f.policy.zeroAudioReady(c,2,f.descriptor,12))
        assertTrue(f.policy.sourceClosed(source,true,13));assertEquals(PrivacyStandbyPolicy.Phase.ARMED,f.policy.snapshot().phase)
        assertNull(f.policy.snapshot().pending);assertFalse(f.policy.permitsRaw(f.run,c.revision))
        val start=checkNotNull(f.policy.beganMedia(f.run,100));assertEquals(100L,start.startedNs)
    }
    @Test fun explicitEndCanDetachPublishingWhileIndependentRecordingConfirmsStandby() {
        val f=Fixture(video=false);f.start()
        assertTrue(f.policy.sinkTerminated(f.run,PrivacyStandbyPolicy.Sink.PUBLISHING,11))
        assertTrue(f.policy.sinkTerminated(f.run,PrivacyStandbyPolicy.Sink.PUBLISHING,11))
        val c=f.pause(12);val audio=f.audio(c,now=13)
        assertFalse(f.policy.accepted(audio,PrivacyStandbyPolicy.Sink.PUBLISHING,13))
        assertTrue(f.policy.accepted(audio,PrivacyStandbyPolicy.Sink.RECORDING,13))
        assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
        assertEquals(PrivacyStandbyPolicy.Sink.RECORDING.bit,f.policy.snapshot().requiredSinks)
    }
    @Test fun transientReconnectUsesBothLocalQueueReceiptsWithoutAnySocketProof() {
        val f=Fixture(video=false);f.start();val c=f.pause();val audio=f.audio(c)
        // Network is absent from this policy: a validated local publishing
        // queue receipt counts, but transport loss does not detach that branch.
        assertTrue(f.policy.accepted(audio,PrivacyStandbyPolicy.Sink.RECORDING,12))
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        assertTrue(f.policy.accepted(audio,PrivacyStandbyPolicy.Sink.PUBLISHING,13))
        assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
    }
    @Test fun lateOldRunSinkDetachmentCannotChangeSuccessorRequirements() {
        val f=Fixture();f.start();assertTrue(f.policy.fenceRun(f.run));assertTrue(f.policy.runClosed(f.run))
        val child=checkNotNull(f.policy.attach(f.owner,2,Any(),2,11))
        assertFalse(f.policy.sinkTerminated(f.run,PrivacyStandbyPolicy.Sink.PUBLISHING,12))
        assertEquals(PrivacyStandbyPolicy.Sinks.BOTH.mask,f.policy.snapshot().requiredSinks)
        assertTrue(f.policy.sinkTerminated(child,PrivacyStandbyPolicy.Sink.PUBLISHING,12))
        assertTrue(f.policy.fenceRun(child));assertTrue(f.policy.runClosed(child))
        checkNotNull(f.policy.attach(f.owner,3,Any(),3,13))
        assertEquals(PrivacyStandbyPolicy.Sink.RECORDING.bit,f.policy.snapshot().requiredSinks)
    }
    @Test fun zeroRemainingSinksStillRequireActualFreshNativeOutputForEveryDeclaredTrack() {
        val f=Fixture();f.start();val c=f.pause()
        assertTrue(f.policy.sinkTerminated(f.run,PrivacyStandbyPolicy.Sink.PUBLISHING,12))
        assertTrue(f.policy.sinkTerminated(f.run,PrivacyStandbyPolicy.Sink.RECORDING,12))
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        f.audio(c)
        assertEquals(PrivacyStandbyPolicy.Phase.ENTERING_STANDBY,f.policy.snapshot().phase)
        f.video(c)
        assertEquals(PrivacyStandbyPolicy.Phase.STANDBY,f.policy.snapshot().phase)
        assertTrue(f.policy.permitsReplacement(c,1000));assertFalse(f.policy.permitsRaw(f.run,c.revision))
    }
    @Test fun heldPrestartSourceCleanupCannotAdmitGeneratedInputBeforeExplicitStart() {
        val f=Fixture();checkNotNull(f.policy.reserveSource(f.run,0));val c=f.pause()
        assertFalse(f.policy.permitsRaw(f.run,0));assertFalse(f.policy.permitsReplacement(c,12))
        assertFalse(f.policy.pictureSubmitted(c,3000,12));assertNull(f.policy.zeroAudioEncoded(c,2,0,1024,12))
        assertSame(c,f.policy.beganMedia(f.run,12)) // Start does not renew the original held-cleanup deadline.
        assertEquals(11L,c.startedNs);assertEquals(17L,c.deadlineNs)
        assertTrue(f.policy.permitsReplacement(c,12));assertFalse(f.policy.permitsRaw(f.run,c.revision))
        assertFalse(f.policy.permitsReplacement(c,17));assertFalse(f.policy.tick(f.run,17))
    }
    @Test fun invalidColdClockCannotStartRawOrArmStandbyAndFencesAnActiveRun() {
        val start=Fixture();assertNull(start.policy.beganMedia(start.run,-1))
        assertFalse(start.policy.permitsRaw(start.run,0));assertEquals(PrivacyStandbyPolicy.Fault.CLOCK,start.policy.snapshot().fault)
        val standby=Fixture();assertNull(standby.policy.standby(standby.run,-1))
        assertFalse(standby.policy.permitsRaw(standby.run,0));assertEquals(PrivacyStandbyPolicy.Fault.CLOCK,standby.policy.snapshot().fault)
        val active=Fixture();active.start();assertTrue(active.policy.permitsRaw(active.run,0))
        assertFalse(active.policy.tick(active.run,-1));assertFalse(active.policy.permitsRaw(active.run,0))
        val source=Fixture();val mutation=checkNotNull(source.policy.reserveSource(source.run,0))
        assertFalse(source.policy.sourceClosed(mutation,true,-1));assertEquals(PrivacyStandbyPolicy.Fault.CLOCK,source.policy.snapshot().fault)
    }
    @Test fun urgentStandbyDuringActualOrientationCleanupPersistsIntoChildWithoutOpeningRaw() {
        val f=Fixture();f.start();assertTrue(f.policy.fenceRun(f.run))
        val pending=f.pause(11)
        assertFalse(f.policy.permitsRaw(f.run));assertFalse(f.policy.permitsReplacement(pending,12))
        assertNull(f.policy.userResume(f.run,pending.revision,12))
        assertTrue(f.policy.runClosed(f.run))
        val child=checkNotNull(f.policy.attach(f.owner,2,Any(),null,12))
        val inherited=checkNotNull(f.policy.snapshot().pending)
        assertSame(child,inherited.run);assertEquals(pending.startedNs,inherited.startedNs);assertEquals(pending.deadlineNs,inherited.deadlineNs)
        assertEquals(PrivacyStandbyPolicy.Desired.STANDBY,f.policy.snapshot().desired)
        assertFalse(f.policy.permitsRaw(child));assertTrue(f.policy.permitsReplacement(inherited,12))
        assertFalse(f.policy.tick(child,pending.deadlineNs))
    }
    @Test fun stoppedOrientationOwnerCannotChangeSuccessorPrivacyIntent() {
        val f=Fixture();f.start();f.policy.fenceRun(f.run);f.policy.stop(f.owner)
        assertNull(f.policy.standby(f.run,11));assertFalse(f.policy.permitsRaw(f.run))
        assertNull(f.policy.attach(f.owner,2,Any(),null,12))
    }

    @Test fun ordinaryStandbyRepeatsDispatchOneRetirementButUrgentStandbySupersedesQueuedResume() {
        val f=Fixture();val slot=PrivacyStandbyDispatch();val safe=f.confirmed()
        assertTrue(slot.admit(safe));repeat(100){assertFalse(slot.admit(safe))}
        val resume=checkNotNull(f.policy.userResume(f.run,safe.revision,13));assertTrue(slot.admit(resume))
        val urgent=f.pause(14);assertTrue(slot.admit(urgent));repeat(100){assertFalse(slot.admit(urgent))}
        assertFalse(f.policy.permitsResume(resume,14));assertFalse(f.policy.permitsRaw(f.run))
        assertEquals(resume.deadlineNs,urgent.deadlineNs)
    }

    @Test fun withheldTimerCannotAdmitQueuedNativeOutputButExactNormalStopTailRemainsAllowed() {
        val f=Fixture();assertFalse(f.policy.permitsOutput(f.run,10));f.start()
        val c=f.pause();assertTrue(f.policy.permitsOutput(f.run,12))
        // No tick, callback or owner timer runs: the actual fanout gate refuses.
        assertFalse(f.policy.permitsOutput(f.run,c.deadlineNs))
        assertFalse(f.policy.permitsReplacement(c,c.deadlineNs))
        val live=Fixture();live.start();live.policy.stop(live.owner)
        assertTrue(live.policy.permitsOutput(live.run,100)) // existing compressed/native tail only
        assertFalse(live.policy.permitsRaw(live.run))
        live.policy.runClosed(live.run);assertFalse(live.policy.permitsOutput(live.run,101))
        val rotation=Fixture();rotation.start();rotation.policy.fenceRun(rotation.run)
        assertTrue(rotation.policy.permitsOutput(rotation.run,100));rotation.policy.nativeFailed(rotation.run)
        assertFalse(rotation.policy.permitsOutput(rotation.run,101))
    }

    @Test fun stopOrAutoTailCannotExtendAnAlreadyPendingPrivacyBoundary() {
        for(stop in listOf(true,false)) {
            val f=Fixture();f.start();val c=f.pause()
            if(stop)f.policy.stop(f.owner) else f.policy.fenceRun(f.run)
            assertEquals(c.deadlineNs,f.policy.cleanupDeadlineNs)
            assertTrue(f.policy.permitsOutput(f.run,c.deadlineNs-1))
            assertFalse(f.policy.permitsOutput(f.run,c.deadlineNs))
        }
    }

}
