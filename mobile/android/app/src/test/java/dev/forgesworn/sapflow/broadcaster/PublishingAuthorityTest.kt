package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Test

/** Generated scalar bytes only. These are the inline boundaries shared by the
 * actual SRT/RTMP adapters, not real network or native decoder qualification. */
class PublishingAuthorityTest {
    private val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)

    @Test fun queuedRtmpNativeOutputCannotWriteAfterSuspensionWithoutAnyTimerCallback() {
        var boot = 1L; val expiry = V4vGrantExpiry { boot }
        expiry.begin(7, identity, 100)
        val fences = AtomicInteger()
        val authority = PublishingAuthority({ fences.incrementAndGet() }, { expiry.admits(7) })
        var nativePushes = 0; var writes = 0
        val queued = withPublishingAuthority(authority) { nativePushes++; byteArrayOf(1, 2, 3) }
        boot = 100 // Native output was already produced; no control/timer owner runs.
        assertThrows(IllegalStateException::class.java) {
            withPublishingAuthority(authority) { writes++; queued.size }
        }
        assertEquals(1, nativePushes); assertEquals(0, writes); assertEquals(1, fences.get())
        assertEquals(7L, expiry.expiredCurrent())
        assertNull(expiry.refresh(identity.copy(revision = 2), 200))
    }

    @Test fun queuedSrtBorrowAndPendingRetransmissionAreBothFencedAtTheirActualBoundaries() {
        var boot = 1L; val expiry = V4vGrantExpiry { boot }
        expiry.begin(7, identity, 100)
        val authority = PublishingAuthority(permitted = { expiry.admits(7) })
        val offered = SingleSampleOffer<Any>()
        val borrowed = Any(); val ticket = offered.offer(borrowed, 1_000)
        val pendingDatagram = ByteBuffer.wrap(byteArrayOf(1, 2, 3))
        var pushes = 0; var writes = 0
        boot = 100 // No native tick, selector wake timer or coordinator cleanup delivered.
        try {
            assertThrows(IllegalStateException::class.java) {
                offered.process(1) { actual ->
                    assertSame(borrowed, actual)
                    withPublishingAuthority(authority) { pushes++; true }
                }
            }
            assertThrows(IllegalStateException::class.java) {
                withPublishingAuthority(authority) { writes++; pendingDatagram.position(pendingDatagram.limit()) }
            }
            assertEquals(0, pushes); assertEquals(0, writes); assertEquals(0, pendingDatagram.position())
        } finally { offered.cancel() }
        assertFalse(offered.await(ticket, 1)); assertNull(offered.deadlineUs())
    }

    @Test fun expiryDoesNotPreventNativeCancellationOrAllowShutdownWireOutput() {
        var boot = 1L; val expiry = V4vGrantExpiry { boot }; expiry.begin(7, identity, 100)
        val authority = PublishingAuthority(permitted = { expiry.admits(7) })
        val shutdown = SrtShutdown(); shutdown.request(1)
        var stops = 0; var writes = 0
        boot = 100
        val result = shutdown.finish({ 2 }, { stops++ }, { byteArrayOf(0x80.toByte(), 5, 0, 0) + ByteArray(12) }, {
            withPublishingAuthority(authority) { writes++; it.remaining() }
        })
        assertEquals(SrtShutdown.Result.WRITE_FAILED, result)
        assertEquals(1, stops); assertEquals(0, writes)
        assertEquals(SrtShutdown.Result.DUPLICATE, shutdown.finish({ 2 }, { stops++ }, { null }, { 0 }))
        assertEquals(1, stops)
    }

    @Test fun manualBoundaryDoesNotReadClockOrChangeOutput() {
        var calls = 0
        assertEquals(3, withPublishingAuthority(null) { calls++; 3 })
        assertEquals(1, calls)
    }

    @Test fun retiredOldAuthorityCannotAdmitOrRetireAnotherLogicalOwner() {
        var boot = 1L; val expiry = V4vGrantExpiry { boot }; expiry.begin(7, identity, 100)
        val oldFences = AtomicInteger()
        val old = PublishingAuthority({ oldFences.incrementAndGet() }, { expiry.admits(7) })
        expiry.clear()
        val newer = expiry.begin(8, identity.copy(generation = 2, sessionId = "b".repeat(32)), 200)
        boot = 100
        assertThrows(IllegalStateException::class.java) { withPublishingAuthority(old) { fail("Old transport must not write") } }
        assertEquals(1, oldFences.get()); assertTrue(expiry.admits(8)); assertNull(expiry.expired(newer))
    }

    /** The refresh captured 99 before its clock function is released. Expiry
     * then captures 100 from the SAME monotonic clock and holds its old state.
     * Releasing refresh first proves failed expiry CAS must retry deadline200. */
    @Test fun heldOldExpiryReadRetriesTimelyConcurrentRefresh() = race(refreshWins = true)
    @Test fun retirementWinningTheSameRaceCannotBeRevivedByLateRefreshCas() = race(refreshWins = false)

    private fun race(refreshWins: Boolean) {
        val boot = AtomicLong(1)
        val refreshEntered = CountDownLatch(1); val releaseRefresh = CountDownLatch(1)
        val expiryEntered = CountDownLatch(1); val releaseExpiry = CountDownLatch(1)
        val expiryHeld = AtomicBoolean()
        val refreshHeld = AtomicBoolean()
        val expiry = V4vGrantExpiry {
            val value = boot.get()
            when (Thread.currentThread().name) {
                "fixture-refresh" -> if (refreshHeld.compareAndSet(false, true)) { refreshEntered.countDown(); check(releaseRefresh.await(2, TimeUnit.SECONDS)) }
                "fixture-expiry" -> if (expiryHeld.compareAndSet(false, true)) { expiryEntered.countDown(); check(releaseExpiry.await(2, TimeUnit.SECONDS)) }
            }
            value
        }
        val original = expiry.begin(7, identity, 100)
        val refreshed = AtomicReference<V4vGrantExpiry.Ticket?>()
        val allowed = AtomicReference<Boolean?>()
        val failure = AtomicReference<Throwable?>()
        val fences = AtomicInteger()
        val authority = PublishingAuthority({ fences.incrementAndGet() }, { expiry.admits(7) })
        val refresh = Thread({ try { refreshed.set(expiry.refresh(identity.copy(revision = 2), 200)) } catch (error: Throwable) { failure.set(error) } }, "fixture-refresh")
        val retire = Thread({
            try { authority.requireValid(); allowed.set(true) }
            catch (error: Throwable) { if (fences.get() > 0) allowed.set(false) else failure.set(error) }
        }, "fixture-expiry")
        try {
            boot.set(99); refresh.start(); assertTrue(refreshEntered.await(2, TimeUnit.SECONDS))
            boot.set(100); retire.start(); assertTrue(expiryEntered.await(2, TimeUnit.SECONDS))
            if (refreshWins) {
                releaseRefresh.countDown(); refresh.join(2_000); assertFalse(refresh.isAlive)
                releaseExpiry.countDown(); retire.join(2_000)
                assertNotNull(refreshed.get()); assertEquals(true, allowed.get())
                assertEquals(0, fences.get()) // No false terminal callback from the held expired read.
                assertNull(expiry.expired(original)); assertTrue(expiry.admits(7))
            } else {
                releaseExpiry.countDown(); retire.join(2_000); assertFalse(retire.isAlive)
                releaseRefresh.countDown(); refresh.join(2_000)
                assertEquals(false, allowed.get()); assertNull(refreshed.get())
                assertEquals(1, fences.get())
                assertEquals(7L, expiry.expiredCurrent()); assertFalse(expiry.admits(7))
            }
            assertNull(failure.get())
        } finally {
            releaseRefresh.countDown(); releaseExpiry.countDown()
            if (refresh.state != Thread.State.NEW) refresh.join(2_000)
            if (retire.state != Thread.State.NEW) retire.join(2_000)
        }
        assertFalse(refresh.isAlive); assertFalse(retire.isAlive)
    }
}
