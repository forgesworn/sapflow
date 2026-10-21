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
}
