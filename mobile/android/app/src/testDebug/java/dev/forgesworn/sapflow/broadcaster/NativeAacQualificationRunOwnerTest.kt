// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

/** Real thread/owner lifecycle only; no SDK, codec or native completion proof. */
class NativeAacQualificationRunOwnerTest {
    private fun key() = NativeAacQualificationRunOwner.Invocation("generated-pair", "a".repeat(32), "b".repeat(32), "c".repeat(64))
    private fun join(owner: NativeAacQualificationRunOwner) {
        val worker = checkNotNull(owner.worker)
        worker.join(5000)
        assertFalse("Actual fixture worker must terminate", worker.isAlive)
    }

    @Test fun actualConfigurationHandoffReattachesOneWorkerWithoutRestart() {
        val slot = NativeAacQualificationRunOwner.Slot(); val invocation = key()
        val owner = checkNotNull(slot.acquire(invocation))
        val entered = CountDownLatch(1); val release = CountDownLatch(1); val calls = AtomicInteger()
        slot.start(owner, Any()) { calls.incrementAndGet(); entered.countDown(); release.await(5, TimeUnit.SECONDS) }
        try {
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            owner.destroyed(changingConfigurations = true, handedOff = true)
            assertFalse(owner.cancelled.get())
            assertSame(owner, slot.reattach(owner, invocation))
            assertThrows(IllegalStateException::class.java) { slot.start(owner, Any()) { calls.incrementAndGet(); true } }
            assertEquals(1, calls.get())
        } finally { release.countDown(); join(owner) }
        assertTrue(owner.cleanupConfirmed); assertFalse(slot.hasOwner())
        assertSame(owner, slot.reattach(owner, invocation)) // Completed result only.
    }

    @Test fun genuineDestroyOrMissingActualHandoffCancelsAndInterruptsWorker() {
        for ((changing, handedOff) in listOf(false to false, true to false, false to true)) {
            val slot = NativeAacQualificationRunOwner.Slot(); val owner = checkNotNull(slot.acquire(key()))
            val entered = CountDownLatch(1); val release = CountDownLatch(1); val interrupted = CountDownLatch(1)
            slot.start(owner, Any()) {
                entered.countDown()
                try { release.await(5, TimeUnit.SECONDS) }
                catch (_: InterruptedException) { interrupted.countDown(); true } // Checked fixture cleanup, not media success.
            }
            try {
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                owner.destroyed(changing, handedOff)
                assertTrue(owner.cancelled.get())
                assertTrue(interrupted.await(5, TimeUnit.SECONDS))
            } finally { release.countDown(); join(owner) }
            assertTrue(owner.cleanupConfirmed); assertFalse(slot.hasOwner())
            assertNull(slot.reattach(owner, key())) // Cancellation cannot resume.
        }
    }

    @Test fun mismatchedInvocationCancelsExactOwnerButForeignSlotCannotTouchIt() {
        val original = key()
        for (other in listOf(original.copy(action = "other"), original.copy(runId = "d".repeat(32)),
            original.copy(inputId = "e".repeat(32)), original.copy(inputManifestSHA256 = "f".repeat(64)))) {
            val slot = NativeAacQualificationRunOwner.Slot(); val owner = checkNotNull(slot.acquire(original))
            val entered = CountDownLatch(1); val release = CountDownLatch(1)
            slot.start(owner, Any()) {
                entered.countDown()
                try { release.await(5, TimeUnit.SECONDS) } catch (_: InterruptedException) { true }
            }
            try {
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                assertNull(NativeAacQualificationRunOwner.Slot().reattach(owner, original))
                assertFalse(owner.cancelled.get())
                assertNull(slot.reattach(owner, other)); assertTrue(owner.cancelled.get())
            } finally { release.countDown(); join(owner) }
        }
    }

    @Test fun unknownCleanupRetainsSlotAndRefusesAnotherPhysicalWorker() {
        val slot = NativeAacQualificationRunOwner.Slot(); val owner = checkNotNull(slot.acquire(key()))
        val calls = AtomicInteger()
        slot.start(owner, Any()) { calls.incrementAndGet(); false }
        join(owner)
        assertTrue(owner.finished); assertFalse(owner.cleanupConfirmed); assertTrue(slot.hasOwner())
        assertNull(slot.acquire(key().copy(runId = "d".repeat(32))))
        assertSame(owner, slot.reattach(owner, key())) // Display failed facts, not restart.
        assertThrows(IllegalStateException::class.java) { slot.start(owner, Any()) { calls.incrementAndGet(); true } }
        slot.retire(owner)
        assertTrue(slot.hasOwner()); assertEquals(1, calls.get())
    }
}
