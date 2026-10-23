package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioWorkerCompletionTest {
    @Test fun completionBeforeDeferredStopSurvivesThreadStillReturningFromFinally() {
        val workers = AudioWorkerCompletion()
        workers.begin()
        workers.readerCompleted()
        workers.pumpCompleted()
        workers.observeStopped(readerAlive = true, pumpAlive = true)
        assertTrue(workers.stopped)
    }

    @Test fun stopBeforeCompletionWaitsForEachOwnerAndNeverFakesRelease() {
        val workers = AudioWorkerCompletion()
        workers.begin()
        workers.observeStopped(readerAlive = true, pumpAlive = true)
        assertFalse(workers.stopped)
        workers.readerCompleted()
        assertFalse(workers.stopped)
        workers.pumpCompleted()
        assertTrue(workers.stopped)
        workers.begin()
        assertFalse(workers.stopped)
    }

    @Test fun unstartedOrActuallyTerminatedWorkerDoesNotNeedAnotherCallback() {
        val workers = AudioWorkerCompletion()
        workers.begin()
        workers.observeStopped(readerAlive = false, pumpAlive = true)
        assertTrue(workers.readerDone)
        assertFalse(workers.stopped)
        workers.observeStopped(readerAlive = false, pumpAlive = false)
        assertTrue(workers.stopped)
    }
    @Test fun resumeStartsOnlyTheNewMicrophoneReaderWithoutLosingRunningZeroPumpOwnership() {
        val owner=AudioWorkerCompletion();owner.begin();owner.readerCompleted()
        assertFalse(owner.stopped);owner.readerBegan();assertFalse(owner.readerDone);assertFalse(owner.pumpDone)
        owner.readerCompleted();assertFalse(owner.stopped);owner.pumpCompleted();assertTrue(owner.stopped)
    }


    @Test fun actualJoinReadsTheFirstSubmittedTailAfterRetirementAndFinally() {
        val workers=AudioWorkerCompletion();workers.begin();workers.readerCompleted();workers.pumpCompleted()
        val input=java.util.concurrent.atomic.AtomicLong(0)
        val ready=java.util.concurrent.CountDownLatch(1);val release=java.util.concurrent.CountDownLatch(1)
        val joined=java.util.concurrent.CountDownLatch(1)
        val result=java.util.concurrent.atomic.AtomicReference<Long?>()
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,10,100)
        val pump=Thread { ready.countDown();release.await();input.addAndGet(1024) }
        val closer=Thread { result.set(workers.joinedInputFrames(null,pump,input));joined.countDown() }
        pump.start()
        try {
            assertTrue(ready.await(2,java.util.concurrent.TimeUnit.SECONDS));assertTrue(workers.stopped)
            closer.start()
            val waitDeadline=System.nanoTime()+java.util.concurrent.TimeUnit.SECONDS.toNanos(2)
            while(closer.isAlive && closer.state!=Thread.State.TIMED_WAITING && System.nanoTime()<waitDeadline)Thread.yield()
            // The exact production helper is now blocked in its real timed
            // Thread.join, not merely waiting for a deferred completion flag.
            assertEquals(Thread.State.TIMED_WAITING,closer.state);assertEquals(1L,joined.count)
            release.countDown();assertTrue(joined.await(2,java.util.concurrent.TimeUnit.SECONDS))
            assertEquals(1024L,checkNotNull(result.get()));assertFalse(pump.isAlive)
            assertEquals(AudioGenerationLifecycle.Action.SEND_EOS,p.producersClosed(g,checkNotNull(result.get())>0))
        } finally { release.countDown();pump.join(2000);if(closer.state!=Thread.State.NEW)closer.join(2000) }
        assertFalse(pump.isAlive);assertFalse(closer.isAlive)
    }
    @Test fun actualJoinedZeroInputSkipsEosOnlyAfterBothThreadsReturn() {
        val workers=AudioWorkerCompletion();val input=java.util.concurrent.atomic.AtomicLong(0)
        val reader=Thread {};val pump=Thread {};reader.start();pump.start()
        assertEquals(0L,checkNotNull(workers.joinedInputFrames(reader,pump,input)))
        assertFalse(reader.isAlive);assertFalse(pump.isAlive)
        val p=AudioGenerationLifecycle();val g=checkNotNull(p.begin());p.retire(g,false,10,100)
        assertEquals(AudioGenerationLifecycle.Action.QUIESCE,p.producersClosed(g,input.get()>0))
    }
    @Test fun interruptedActualJoinNeverBecomesAZeroInputDisposition() {
        val workers=AudioWorkerCompletion();val release=java.util.concurrent.CountDownLatch(1)
        val ready=java.util.concurrent.CountDownLatch(1)
        val pump=Thread { ready.countDown();release.await() };pump.start()
        try {
            assertTrue(ready.await(2,java.util.concurrent.TimeUnit.SECONDS))
            Thread.currentThread().interrupt()
            assertNull(workers.joinedInputFrames(null,pump,java.util.concurrent.atomic.AtomicLong(0)))
            assertTrue(Thread.interrupted());assertTrue(pump.isAlive)
        } finally { Thread.interrupted();release.countDown();pump.join(2000) }
        assertFalse(pump.isAlive)
    }
    @Test fun timedOutActualJoinRetainsUnknownInputInsteadOfInventingZero() {
        val workers=AudioWorkerCompletion();val release=java.util.concurrent.CountDownLatch(1)
        val ready=java.util.concurrent.CountDownLatch(1)
        val pump=Thread { ready.countDown();release.await() };pump.start()
        try {
            assertTrue(ready.await(2,java.util.concurrent.TimeUnit.SECONDS))
            assertNull(workers.joinedInputFrames(null,pump,java.util.concurrent.atomic.AtomicLong(0)))
            assertTrue(pump.isAlive)
        } finally { release.countDown();pump.join(2000) }
        assertFalse(pump.isAlive)
    }
    @Test fun invalidPostJoinInputCountCannotBecomeNoInput() {
        val workers=AudioWorkerCompletion()
        assertNull(workers.joinedInputFrames(null,null,java.util.concurrent.atomic.AtomicLong(-1)))
    }
    @Test fun selfJoinCannotCertifyInputOrWaitForItsOwnExit() {
        val workers=AudioWorkerCompletion();val input=java.util.concurrent.atomic.AtomicLong(1024)
        assertNull(workers.joinedInputFrames(Thread.currentThread(),null,input))
        assertNull(workers.joinedInputFrames(null,Thread.currentThread(),input))
    }
}
