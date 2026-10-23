package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

class AudioInputFailureTest {
    @Test fun uncertainNativeSubmissionFaultsExactRetiringOwnerBeforeControllerDelivery() {
        for(pumping in listOf(false,true))for(interrupted in listOf(false,true)) {
            val run=CameraRunAdmission(1);val fault=AudioNativeFailure(run);val owner=Any();check(fault.bind(owner))
            val accepted=AtomicLong(0);val queuedNotice=ArrayBlockingQueue<Runnable>(1);val notices=AtomicInteger()
            if(AudioInputFailure.requiresFailure(true,pumping,true,interrupted)) {
                assertTrue(fault.fail(owner,"Native queue threw during graceful retirement"))
                queuedNotice.add(Runnable{notices.incrementAndGet()})
            }
            assertFalse(run.admit(12));assertEquals(0,notices.get());assertEquals(0L,accepted.get())
            assertEquals("Native queue threw during graceful retirement",fault.reason)
            queuedNotice.remove().run();assertEquals(1,notices.get())
        }
    }
    @Test fun returnedQueueAccountingFailureRetainsAcceptedInputButCannotBecomeCleanSource() {
        val run=CameraRunAdmission(1);val fault=AudioNativeFailure(run);val owner=Any();check(fault.bind(owner))
        val accepted=AtomicLong(0);accepted.addAndGet(1024) // Actual returned native call is never undone.
        val queueCallUnconfirmed=false;val submissionPending=true // Bookkeeping has not completed.
        assertTrue(AudioInputFailure.mayReturnBorrowedIndex(queueCallUnconfirmed))
        assertTrue(AudioInputFailure.requiresFailure(submissionPending,false,true,false))
        assertTrue(fault.fail(owner,"Returned native input bookkeeping failed"));assertFalse(run.admit(12))
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,10,100);p.fail(g)
        assertEquals(AudioGenerationLifecycle.Action.SEND_EOS,p.producersClosed(g,accepted.get()>0))
        p.abortEos(g);p.callbacksClosed(g);p.handoffIdle(g);p.released(g,true,20)
        assertEquals(1024L,accepted.get());assertTrue(p.actualClosed(g));assertTrue(p.isFailed());assertNull(p.begin())
    }
    @Test fun deliberatePreSubmissionRetirementInterruptionReturnsKnownBorrowWithoutFault() {
        val run=CameraRunAdmission(1);val fault=AudioNativeFailure(run);val owner=Any();check(fault.bind(owner))
        val indices=ArrayBlockingQueue<Int>(1)
        for(pumping in listOf(false,true))assertFalse(AudioInputFailure.requiresFailure(false,pumping,true,true))
        if(AudioInputFailure.mayReturnBorrowedIndex(false))assertTrue(indices.offer(7))
        assertEquals(7,indices.remove().toInt());assertNull(fault.reason);assertTrue(run.admit(12))
        // Unexpected errors still fault a running graceful tail; active input
        // interruption retains the existing non-retirement failure behaviour.
        assertTrue(AudioInputFailure.requiresFailure(false,true,true,false))
        assertTrue(AudioInputFailure.requiresFailure(false,true,false,true))
    }
    @Test fun throwingNativeQueueCannotRecycleItsUncertainIndexForEos() {
        val indices=ArrayBlockingQueue<Int>(1);val accepted=AtomicLong(0)
        if(AudioInputFailure.mayReturnBorrowedIndex(true))indices.offer(7)
        assertNull(indices.poll());assertEquals(0L,accepted.get())
        assertTrue(AudioInputFailure.requiresFailure(true,false,true,false))
        // A cancellation BEFORE native queue entry has different ownership.
        assertTrue(AudioInputFailure.mayReturnBorrowedIndex(false))
    }
    @Test fun stalePendingSubmissionFailureCannotFenceAFreshGeneration() {
        val run=CameraRunAdmission(1);val fault=AudioNativeFailure(run);val old=Any();val fresh=Any()
        check(fault.bind(old));check(fault.bind(fresh))
        assertTrue(AudioInputFailure.requiresFailure(true,false,true,false))
        assertFalse(fault.fail(old,"Retired submission"));assertNull(fault.reason);assertTrue(run.admit(12))
        assertTrue(fault.fail(fresh,"Actual current submission"));assertFalse(run.admit(12))
    }
}
