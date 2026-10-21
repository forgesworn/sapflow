package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.Assert.*
import org.junit.Test

class SingleSampleOfferTest {
    @Test fun admissionHoldsOneOriginalHandleAndBackpressureCannotAppendAnother() {
        val slot = SingleSampleOffer<Any>()
        val value = Any()
        val ticket = slot.offer(value, 100)
        assertThrows(IllegalStateException::class.java) { slot.offer(Any(), 100) }
        slot.process(1) { assertSame(value, it); false }
        assertEquals(100L, slot.deadlineUs())
        slot.process(2) { assertSame(value, it); true }
        assertTrue(slot.await(ticket, 1))
        assertNull(slot.deadlineUs())
    }
    @Test fun timedOutOfferCanNeverBeUsedLater() {
        val slot = SingleSampleOffer<Any>()
        val ticket = slot.offer(Any(), 100)
        assertFalse(slot.await(ticket, 1))
        slot.process(2) { fail("cancelled borrowed handle must not be used"); true }
        val next = slot.offer(Any(), 200)
        slot.process(3) { true }
        assertTrue(slot.await(next, 1))
    }
    @Test(timeout = 3000) fun cancellationWaitsForBoundedProtocolUseBeforeReleasingTheBorrow() {
        val slot = SingleSampleOffer<Any>()
        val ticket = slot.offer(Any(), 100)
        val using = CountDownLatch(1)
        val release = CountDownLatch(1)
        val cancelled = CountDownLatch(1)
        val protocol = thread { slot.process(1) { using.countDown(); release.await(); true } }
        assertTrue(using.await(1, TimeUnit.SECONDS))
        val control = thread { slot.cancel(); cancelled.countDown() }
        try {
            val deadline = System.nanoTime() + 1_000_000_000
            while (slot.deadlineUs() != null && System.nanoTime() < deadline) Thread.yield()
            assertNull(slot.deadlineUs())
            assertFalse(cancelled.await(30, TimeUnit.MILLISECONDS))
        }
        finally { release.countDown(); protocol.join(1000); control.join(1000) }
        assertFalse(slot.await(ticket, 1))
        assertFalse(protocol.isAlive || control.isAlive)
    }
    @Test fun expiredOfferFailsBeforeAnyProtocolUseAndCanBeCancelled() {
        val slot = SingleSampleOffer<Any>()
        val ticket = slot.offer(Any(), 100)
        assertThrows(IllegalStateException::class.java) { slot.process(100) { fail("expired sample"); true } }
        slot.cancel()
        assertFalse(slot.await(ticket, 1))
    }
}
