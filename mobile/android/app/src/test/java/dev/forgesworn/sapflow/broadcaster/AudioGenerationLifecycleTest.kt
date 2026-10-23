package dev.forgesworn.sapflow.broadcaster
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
class AudioGenerationLifecycleTest {
    @Test fun actualOwnerSequenceNeverCreatesSuccessorBeforeReleaseReturns() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());val calls=mutableListOf("native-create-A")
        fun execute(a:AudioGenerationLifecycle.Action){when(a){
            AudioGenerationLifecycle.Action.STOP_PRODUCERS->calls.add("stop-record-and-workers")
            AudioGenerationLifecycle.Action.SEND_EOS->calls.add("native-input-EOS")
            AudioGenerationLifecycle.Action.QUIESCE->calls.add("callback-barrier")
            AudioGenerationLifecycle.Action.RELEASE->calls.add("actual-stop-release")
            AudioGenerationLifecycle.Action.CLOSED->calls.add("closed-receipt")
            else->Unit}}
        execute(p.retire(g,true,10,16));assertNull(p.begin())
        execute(p.producersClosed(g));assertEquals(AudioGenerationLifecycle.Action.NONE,p.released(g,true,15));assertNull(p.begin())
        execute(p.eos(g));execute(p.callbacksClosed(g));assertNull(p.begin())
        execute(p.handoffIdle(g));assertNull(p.begin()) // Native release is deliberately withheld.
        execute(p.released(g,true,15));val next=checkNotNull(p.begin());calls.add("native-create-B")
        assertEquals(listOf("native-create-A","stop-record-and-workers","native-input-EOS","callback-barrier","actual-stop-release","closed-receipt","native-create-B"),calls)
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.released(g,true,15));assertTrue(p.owns(next))
    }
    @Test fun callbackAndHandoffClosureBothRequiredInEitherOrder() {
        for(firstCallback in listOf(true,false)){
            val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,1,7)
            assertEquals(AudioGenerationLifecycle.Action.QUIESCE,p.producersClosed(g))
            val first=if(firstCallback)p.callbacksClosed(g) else p.handoffIdle(g)
            assertEquals(AudioGenerationLifecycle.Action.NONE,first);assertNull(p.begin())
            val second=if(firstCallback)p.handoffIdle(g) else p.callbacksClosed(g)
            assertEquals(AudioGenerationLifecycle.Action.RELEASE,second)
        }
    }
    @Test fun unknownNativeReleaseRetainsCapacityAndOldEventsCannotReleaseSuccessor() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,1,7);p.producersClosed(g);p.callbacksClosed(g);p.handoffIdle(g)
        assertEquals(AudioGenerationLifecycle.Action.FAIL,p.released(g,false,2));assertFalse(p.actualClosed(g));assertNull(p.begin())
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.released(g,true,15));assertNull(p.begin())
    }
    @Test fun originalDeadlineAndEosFailureNeverPermitAReplacement() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,true,10,16)
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.retire(g,true,15,100));assertEquals(AudioGenerationLifecycle.Action.FAIL,p.tick(g,16))
        p.producersClosed(g);p.eos(g);p.callbacksClosed(g);p.handoffIdle(g);p.released(g,true,15)
        assertTrue(p.actualClosed(g));assertNull(p.begin())
        val q=AudioGenerationLifecycle();val h=checkNotNull(q.begin());q.retire(h,true,1,7);q.producersClosed(h)
        assertEquals(AudioGenerationLifecycle.Action.QUIESCE,q.abortEos(h));assertTrue(q.isFailed())
    }
    @Test fun exhaustedSerialAndClockRollbackFailClosed() {
        assertNull(AudioGenerationLifecycle(Long.MAX_VALUE).begin())
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,true,10,16)
        assertEquals(AudioGenerationLifecycle.Action.FAIL,p.tick(g,9));assertNull(p.begin())
    }
    @Test fun nativeReleaseAfterOriginalDeadlineCannotCreateEvenWhenTimerNeverRan() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,10,16)
        p.producersClosed(g);p.callbacksClosed(g);p.handoffIdle(g)
        assertEquals(AudioGenerationLifecycle.Action.CLOSED,p.released(g,true,16))
        assertTrue(p.actualClosed(g));assertTrue(p.isFailed());assertNull(p.begin())
    }

    @Test fun synchronousStopFenceBeforeColdRetirementWaitsThenPreservesOnlyItsAdmittedTail() {
        assertEquals(AudioRawAdmission.Action.RAW,AudioRawAdmission.action(true,false,false,false))
        repeat(100){assertEquals(AudioRawAdmission.Action.WAIT,AudioRawAdmission.action(false,false,false,false))}
        assertEquals(AudioRawAdmission.Action.TAIL,AudioRawAdmission.action(false,true,true,false))
        assertEquals(AudioRawAdmission.Action.CANCEL,AudioRawAdmission.action(false,true,false,false))
        assertEquals(AudioRawAdmission.Action.FAULT,AudioRawAdmission.action(true,true,true,true))
    }

    private class SafeStandby(video:Boolean=false) {
        val owner=Any();val descriptor=Any();val policy=PrivacyStandbyPolicy(owner,video,true,budgetNs=100)
        val run=checkNotNull(policy.attach(owner,1,descriptor,null,10))
        val safe=checkNotNull(policy.beganMedia(run,10))
        init {
            check(policy.zeroAudioReady(safe,1,descriptor,11))
            val audio=checkNotNull(policy.zeroAudioEncoded(safe,1,0,1024,11))
            policy.accepted(audio,PrivacyStandbyPolicy.Sink.PUBLISHING,11);policy.accepted(audio,PrivacyStandbyPolicy.Sink.RECORDING,11)
            if(video){policy.pictureSubmitted(safe,3000,11);val picture=checkNotNull(policy.pictureEncoded(safe,3000,true,11))
                policy.accepted(picture,PrivacyStandbyPolicy.Sink.PUBLISHING,11);policy.accepted(picture,PrivacyStandbyPolicy.Sink.RECORDING,11)}
            check(policy.snapshot().phase==PrivacyStandbyPolicy.Phase.STANDBY)
        }
    }
    private fun await(latch:CountDownLatch){check(latch.await(2,TimeUnit.SECONDS))}
    @Test fun heldZeroNativeIndexAcrossPolicyResumeAndModeCommitReturnsWithoutFault() {
        for(modeCommitted in listOf(false,true)) {
            val f=SafeStandby();val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
            val zeroMode=AtomicBoolean(true);val acquired=CountDownLatch(1);val continueWorker=CountDownLatch(1)
            val input=ArrayBlockingQueue<Int>(1).apply{add(7)};val pushes=AtomicInteger();val reports=AtomicInteger();val thrown=AtomicReference<Throwable?>()
            val worker=Thread { try {
                val cachedZero=zeroMode.get();check(cachedZero);val index=input.take();acquired.countDown();await(continueWorker)
                // Actual audio adapter uses this same production decision at
                // acquisition and immediately before queueInputBuffer.
                val action=PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,13),true,zeroMode.get(),false)
                if(action==PrivacyNativeWork.Action.SUBMIT)pushes.incrementAndGet()
                else {check(input.offer(index));if(action==PrivacyNativeWork.Action.FAULT)reports.incrementAndGet()}
            }catch(error:Throwable){thrown.set(error)} }
            worker.start();await(acquired)
            assertTrue(f.policy.resumed(resume,1,true,true,13))
            if(modeCommitted)zeroMode.set(false)
            continueWorker.countDown();worker.join(2000)
            assertFalse(worker.isAlive);assertNull(thrown.get());assertEquals(0,pushes.get());assertEquals(0,reports.get());assertEquals(7,input.single())
            zeroMode.set(false);assertEquals(AudioRawAdmission.Action.RAW,AudioRawAdmission.action(f.policy.permitsRaw(f.run),false,false,false))
        }
    }
    @Test fun heldZeroAtFinalNativeSubmissionRechecksTimelyResumeRatherThanUsingCachedSubmit() {
        val f=SafeStandby();val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
        val prepared=CountDownLatch(1);val release=CountDownLatch(1);val finalAction=AtomicReference<PrivacyNativeWork.Action>()
        val worker=Thread {
            assertEquals(PrivacyNativeWork.Action.SUBMIT,PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,12),true,true,false))
            prepared.countDown();await(release)
            finalAction.set(PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,13),true,false,false))
        }
        worker.start();await(prepared);assertTrue(f.policy.resumed(resume,1,true,true,13));release.countDown();worker.join(2000)
        assertFalse(worker.isAlive);assertEquals(PrivacyNativeWork.Action.RETRY,finalAction.get())
    }
    @Test fun heldGlDrawAcrossResumeDoesNotFaultOrAdvanceUnsubmittedTimeline() {
        val f=SafeStandby(video=true);val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
        val timeline=CameraSourceTimeline();check(timeline.frame(100,true,true));timeline.submitted(100)
        val drawn=CountDownLatch(1);val release=CountDownLatch(1);val swaps=AtomicInteger();val faults=AtomicInteger();val failure=AtomicReference<Throwable?>()
        val worker=Thread { try {
            check(PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,12),true,true,false)==PrivacyNativeWork.Action.SUBMIT)
            drawn.countDown();await(release)
            when(PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,13),true,false,false)){
                PrivacyNativeWork.Action.SUBMIT->{check(timeline.frame(200,true,true));swaps.incrementAndGet();timeline.submitted(200)}
                PrivacyNativeWork.Action.FAULT->faults.incrementAndGet()
                else->Unit
            }
        }catch(error:Throwable){failure.set(error)} }
        worker.start();await(drawn);assertTrue(f.policy.resumed(resume,1,true,true,13));release.countDown();worker.join(2000)
        assertFalse(worker.isAlive);assertNull(failure.get());assertEquals(0,swaps.get());assertEquals(0,faults.get())
        assertEquals(100L,timeline.lastSubmittedNs);assertTrue(timeline.frame(150,true,true));timeline.submitted(150)
    }
    @Test fun normalStopAutoAndNewStandbyCancelOldWorkWhileExactExpiryStillFaultsWithoutTimer() {
        for(stop in listOf(false,true)) {
            val f=SafeStandby();val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
            if(stop)f.policy.stop(f.owner) else f.policy.fenceRun(f.run)
            assertEquals(PrivacyNativeWork.Action.CANCEL,PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,13),true,true,false))
            assertEquals(PrivacyNativeWork.Action.FAULT,PrivacyNativeWork.action(f.policy.replacementAdmission(f.safe,resume.deadlineNs),true,true,false))
        }
        val f=SafeStandby();val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12));val urgent=checkNotNull(f.policy.standby(f.run,13))
        assertEquals(PrivacyStandbyPolicy.NativeAdmission.SUPERSEDED,f.policy.resumeAdmission(resume,13))
        assertFalse(f.policy.resumed(resume,1,true,true,13));assertFalse(f.policy.permitsRaw(f.run))
        assertEquals(resume.deadlineNs,urgent.deadlineNs)
        assertEquals(PrivacyNativeWork.Action.FAULT,PrivacyNativeWork.action(f.policy.replacementAdmission(urgent,urgent.deadlineNs),true,true,false))
    }
    @Test fun nativeFaultFencesExactRawAndAacAdmissionBeforeHeldControllerDelivery() {
        val run=CameraRunAdmission(1);val fault=AudioNativeFailure(run);val generation=Any();check(fault.bind(generation))
        val delivery=ArrayBlockingQueue<Runnable>(1);val notices=AtomicInteger()
        val callback=Thread {if(fault.fail(generation,"Generated native codec fault"))delivery.add(Runnable{notices.incrementAndGet()})}
        callback.start();callback.join(2000)
        assertFalse(callback.isAlive);assertFalse(run.admit(11));assertEquals(0,notices.get())
        assertEquals(AudioRawAdmission.Action.FAULT,AudioRawAdmission.action(true,false,false,fault.reason!=null))
        assertEquals(PrivacyNativeWork.Action.FAULT,PrivacyNativeWork.action(PrivacyStandbyPolicy.NativeAdmission.SUBMIT,true,true,fault.reason!=null))
        assertFalse(fault.bind(Any()));delivery.take().run();assertEquals(1,notices.get())
    }
    @Test fun heldOldNativeFaultCannotPoisonFreshGenerationBoundBeforeItsCompareAndSet() {
        val entered=CountDownLatch(1);val release=CountDownLatch(1);val run=CameraRunAdmission(1)
        val fault=AudioNativeFailure(run){entered.countDown();await(release)};val old=Any();val next=Any();check(fault.bind(old))
        val accepted=AtomicBoolean(true);val callback=Thread{accepted.set(fault.fail(old,"Old native callback"))}
        callback.start();await(entered);assertTrue(fault.bind(next));release.countDown();callback.join(2000)
        assertFalse(callback.isAlive);assertFalse(accepted.get());assertNull(fault.reason);assertTrue(run.admit(12))
    }

    @Test fun heldNativeReadinessAfterUrgentStandbyCannotFailOrReopenItsSuccessorCommand() {
        val f=SafeStandby();val resume=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
        val queued=CountDownLatch(1);val deliver=CountDownLatch(1);val action=AtomicReference<PrivacyStandbyPolicy.NativeAdmission>()
        val callback=Thread {queued.countDown();await(deliver);action.set(f.policy.commitReadyResume(resume,1,true,true,14))}
        callback.start();await(queued);val urgent=checkNotNull(f.policy.standby(f.run,13));deliver.countDown();callback.join(2000)
        assertFalse(callback.isAlive);assertEquals(PrivacyStandbyPolicy.NativeAdmission.SUPERSEDED,action.get())
        assertSame(urgent,f.policy.snapshot().pending);assertNull(f.policy.snapshot().fault);assertFalse(f.policy.permitsRaw(f.run))
        assertEquals(resume.deadlineNs,urgent.deadlineNs)
    }

    @Test fun realNativeClosureAndDelayedZeroReadinessAfterStopDoNotCreateOrFaultASuccessor() {
        val f=SafeStandby();val pending=checkNotNull(f.policy.userResume(f.run,f.safe.revision,12))
        val urgent=checkNotNull(f.policy.standby(f.run,13))
        val lifecycle=AudioGenerationLifecycle();val native=checkNotNull(lifecycle.begin())
        lifecycle.retire(native,false,13,urgent.deadlineNs);lifecycle.producersClosed(native)
        lifecycle.callbacksClosed(native);lifecycle.handoffIdle(native)
        f.policy.stop(f.owner)
        assertEquals(AudioGenerationLifecycle.Action.CLOSED,lifecycle.released(native,true,14))
        assertTrue(lifecycle.actualClosed(native))
        assertEquals(PrivacyStandbyPolicy.NativeAdmission.RETIRING,f.policy.replacementAdmission(urgent,14))
        assertEquals(PrivacyStandbyPolicy.NativeAdmission.RETIRING,f.policy.replacementAdmission(f.safe,14))
        assertEquals(PrivacyStandbyPolicy.NativeAdmission.FAULT,f.policy.replacementAdmission(urgent,pending.deadlineNs))
        assertNull(f.policy.snapshot().fault);assertFalse(f.policy.permitsRaw(f.run))
    }


    @Test fun joinedFirstTailOverridesTheEarlierNoInputRetirementSnapshot() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin())
        assertEquals(AudioGenerationLifecycle.Action.STOP_PRODUCERS,p.retire(g,false,10,100))
        assertEquals(AudioGenerationLifecycle.Action.SEND_EOS,p.producersClosed(g,true))
        assertEquals(AudioGenerationLifecycle.Stage.EOS,p.currentStage())
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.producersClosed(g,false))
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.callbacksClosed(g))
        assertFalse(p.actualClosed(g));assertNull(p.begin())
        assertEquals(AudioGenerationLifecycle.Action.QUIESCE,p.eos(g))
        p.callbacksClosed(g);p.handoffIdle(g);p.released(g,true,20)
        val next=checkNotNull(p.begin())
        assertEquals(AudioGenerationLifecycle.Action.NONE,p.producersClosed(g,true))
        assertTrue(p.owns(next))
    }
    @Test fun joinedZeroCannotEraseAnAlreadyObservedSuccessfulInput() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,true,10,100)
        assertEquals(AudioGenerationLifecycle.Action.SEND_EOS,p.producersClosed(g,false))
        assertEquals(AudioGenerationLifecycle.Stage.EOS,p.currentStage())
    }
    @Test fun firstTailMissingEosRemainsFailedAfterActualResourceRelease() {
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,10,100)
        assertEquals(AudioGenerationLifecycle.Action.SEND_EOS,p.producersClosed(g,true))
        assertEquals(AudioGenerationLifecycle.Action.QUIESCE,p.abortEos(g))
        p.callbacksClosed(g);assertEquals(AudioGenerationLifecycle.Action.RELEASE,p.handoffIdle(g))
        assertEquals(AudioGenerationLifecycle.Action.CLOSED,p.released(g,true,20))
        assertTrue(p.actualClosed(g));assertTrue(p.isFailed());assertNull(p.begin())
    }
}
