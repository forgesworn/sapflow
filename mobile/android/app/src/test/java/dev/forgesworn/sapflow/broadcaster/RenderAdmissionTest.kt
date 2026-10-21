package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class RenderAdmissionTest {
    @Test fun callbackBurstHasOneScheduledTaskAndOnePendingScalar() {
        val admission = RenderAdmission()
        assertTrue(admission.offer())
        repeat(10_000) { assertFalse(admission.offer()) }
        assertTrue(admission.begin())
        assertFalse(admission.finish())
        assertFalse(admission.begin())
    }

    @Test fun arrivalDuringDrawCreatesOnlyOneFollowUpAndCloseFencesIt() {
        val admission = RenderAdmission()
        assertTrue(admission.offer()); assertTrue(admission.begin())
        repeat(100) { assertFalse(admission.offer()) }
        assertTrue(admission.finish()); assertTrue(admission.begin())
        assertFalse(admission.finish())
        assertTrue(admission.offer())
        admission.close()
        assertFalse(admission.begin()); assertFalse(admission.finish()); assertFalse(admission.offer())
    }

    @Test fun concurrentProducerCallbacksCannotAdmitMultipleOwners() {
        val admission = RenderAdmission()
        val workers = Executors.newFixedThreadPool(4)
        val ready = CountDownLatch(1)
        try {
            val offered = (0 until 16).map { workers.submit<Boolean> { ready.await(); admission.offer() } }
            ready.countDown()
            assertEquals(1, offered.count { it.get(1, TimeUnit.SECONDS) })
            assertTrue(admission.begin())
            admission.close()
            assertFalse(admission.finish())
        } finally { workers.shutdownNow(); assertTrue(workers.awaitTermination(1, TimeUnit.SECONDS)) }
    }
}
