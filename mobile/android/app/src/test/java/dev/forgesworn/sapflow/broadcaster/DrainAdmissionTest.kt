package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

class DrainAdmissionTest {
    @Test fun continuedIngressSustainsThroughputWhileHandlerContinuationIsStarved() {
        val admission = DrainAdmission()
        var wakes = 0
        // Deliberately do not run the first queued handler continuation. Every
        // new ingress must still claim after the previous sample was closed.
        repeat(1000) {
            assertTrue(admission.claim())
            assertFalse(admission.claim())
            if (admission.complete()) wakes++
        }
        assertEquals(1, wakes)
        assertTrue(admission.claim())
        admission.resumed()
        assertFalse(admission.claim()) // Stale continuation cannot double-pop.
        assertTrue(admission.complete())
    }

    @Test fun idleCompletionWakesOnceAndEmptyQueueDoesNotPoll() {
        val admission = DrainAdmission()
        assertTrue(admission.claim())
        assertTrue(admission.complete())
        admission.resumed()
        assertTrue(admission.claim())
        admission.empty()
        assertTrue(admission.claim())
        admission.close()
        assertFalse(admission.complete())
        admission.resumed()
        assertFalse(admission.claim())
    }

    @Test fun racingCallersCannotOwnTwoSamples() {
        val admission = DrainAdmission()
        val ready = CountDownLatch(2)
        val start = CountDownLatch(1)
        val winners = AtomicInteger()
        val threads = List(2) {
            Thread {
                ready.countDown()
                check(start.await(1, TimeUnit.SECONDS))
                if (admission.claim()) winners.incrementAndGet()
            }.apply { start() }
        }
        try {
            assertTrue(ready.await(1, TimeUnit.SECONDS))
            start.countDown()
            threads.forEach { it.join(1000); assertFalse(it.isAlive) }
            assertEquals(1, winners.get())
        } finally { start.countDown(); threads.forEach { it.join(1000) } }
    }
}
