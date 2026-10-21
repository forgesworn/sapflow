package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.ArrayDeque
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

class CoalescedOwnedQueueTest {
    private class Owner(val id: Int) : AutoCloseable {
        val closes = AtomicInteger()
        override fun close() { check(closes.incrementAndGet() == 1) }
    }
    private class Fixture(maxCount: Int = 16, maxBytes: Long = 65536, consume: ((Owner) -> Unit)? = null) {
        var now = 0L
        val posted = ArrayDeque<Runnable>()
        val consumed = mutableListOf<Int>()
        val failures = mutableListOf<Exception>()
        var idle = 0
        val queue = CoalescedOwnedQueue<Owner>(maxCount, maxBytes, 250_000_000,
            { now }, { posted.addLast(it); true }, { consumed.add(it.id); consume?.invoke(it) },
            failures::add, { idle++ })
        fun turn() = posted.removeFirst().run()
    }

    @Test fun coalescesSixteenOwnersIntoTwoBoundedTurnsWithoutReordering() {
        val f = Fixture()
        val owners = List(16) { Owner(it) }
        owners.forEach { f.queue.offer(it, 4096) }
        assertEquals(1, f.posted.size)
        f.turn()
        assertEquals((0..7).toList(), f.consumed)
        assertEquals(8, f.queue.outstanding)
        assertEquals(1, f.posted.size)
        f.turn()
        assertEquals((0..15).toList(), f.consumed)
        assertEquals(0, f.queue.outstanding)
        assertTrue(f.posted.isEmpty())
        owners.forEach { assertEquals(1, it.closes.get()) }
    }

    @Test fun borrowedOwnerRetainsItsCountAndByteBudgetDuringCancellation() {
        lateinit var f: Fixture
        val first = Owner(1); val queued = Owner(2); val refused = Owner(3)
        f = Fixture(2, 6) { owner ->
            assertEquals(2, f.queue.outstanding)
            assertThrows(IllegalStateException::class.java) { f.queue.offer(refused, 1) }
            f.queue.close(drain = false)
            assertEquals(0, owner.closes.get())
            assertEquals(1, queued.closes.get())
            assertEquals(1, f.queue.outstanding)
            assertEquals(0, f.idle)
        }
        f.queue.offer(first, 3); f.queue.offer(queued, 3)
        f.turn()
        assertEquals(1, first.closes.get()); assertEquals(1, refused.closes.get())
        assertEquals(0, f.queue.outstanding); assertEquals(1, f.idle)
    }

    @Test fun byteRefusalClosesOnlyTheIncomingOwner() {
        val f = Fixture(16, 6)
        val accepted = Owner(1); val refused = Owner(2)
        f.queue.offer(accepted, 4)
        assertThrows(IllegalStateException::class.java) { f.queue.offer(refused, 3) }
        assertEquals(0, accepted.closes.get()); assertEquals(1, refused.closes.get())
        f.turn(); assertEquals(listOf(1), f.consumed)
    }

    @Test fun gracefulCloseDrainsAcceptedOwnersAndRejectsLateEpochWork() {
        val old = Fixture(); val next = Fixture()
        val accepted = List(10) { Owner(it) }; val late = Owner(99); val fresh = Owner(100)
        accepted.forEach { old.queue.offer(it, 10) }
        old.queue.close(drain = true)
        assertThrows(IllegalStateException::class.java) { old.queue.offer(late, 10) }
        next.queue.offer(fresh, 10)
        old.turn(); assertEquals(0, old.idle)
        old.turn(); assertEquals(1, old.idle)
        old.queue.close(drain = false); assertEquals(1, old.idle)
        assertEquals(0, fresh.closes.get())
        next.turn(); assertEquals(listOf(100), next.consumed)
        assertEquals((0..9).toList(), old.consumed)
        assertEquals(1, late.closes.get())
    }

    @Test fun staleAndFailedConsumersReleaseEveryAcceptedOwner() {
        val stale = Fixture(); val staleOwners = List(3) { Owner(it) }
        staleOwners.forEach { stale.queue.offer(it, 10) }
        stale.now = 250_000_001; stale.turn()
        assertTrue(stale.consumed.isEmpty()); assertEquals(1, stale.failures.size)
        staleOwners.forEach { assertEquals(1, it.closes.get()) }
        assertEquals(0, stale.queue.outstanding)
        val failed = Fixture(consume = { error("fanout refusal") })
        val failedOwners = List(3) { Owner(it) }; failedOwners.forEach { failed.queue.offer(it, 10) }
        failed.turn(); assertEquals(1, failed.failures.size)
        failedOwners.forEach { assertEquals(1, it.closes.get()) }
        assertEquals(0, failed.queue.outstanding)
    }

    @Test fun borrowedFanoutOverrunIsReportedAfterReturnWithoutConcurrentClose() {
        lateinit var f: Fixture
        val owner = Owner(1)
        f = Fixture { borrowed ->
            f.now = 250_000_001
            assertEquals(0, borrowed.closes.get())
            assertEquals(1, f.queue.outstanding)
        }
        f.queue.offer(owner, 10)
        f.turn()
        assertEquals(1, owner.closes.get())
        assertEquals(1, f.failures.size)
        assertTrue(f.failures.single().message!!.contains("fanout call exceeded"))
        assertEquals(0, f.queue.outstanding)
    }

    @Test fun concurrentCancellationCannotCloseTheCurrentlyBorrowedNativeHandle() {
        val entered = CountDownLatch(1); val resume = CountDownLatch(1)
        val f = Fixture { entered.countDown(); check(resume.await(1, TimeUnit.SECONDS)) }
        val borrowed = Owner(1); val queued = Owner(2)
        f.queue.offer(borrowed, 10); f.queue.offer(queued, 10)
        val task = f.posted.removeFirst()
        val worker = Thread { task.run() }.apply { start() }
        check(entered.await(1, TimeUnit.SECONDS))
        f.queue.close(drain = false)
        assertEquals(0, borrowed.closes.get()); assertEquals(1, queued.closes.get())
        resume.countDown(); worker.join(1000)
        assertFalse(worker.isAlive); assertEquals(1, borrowed.closes.get())
        assertEquals(0, f.queue.outstanding); assertEquals(1, f.idle)
    }
}
