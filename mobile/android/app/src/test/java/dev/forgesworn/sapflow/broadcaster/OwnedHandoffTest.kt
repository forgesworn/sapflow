package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class OwnedHandoffTest {
    private class Owner : AutoCloseable {
        var closes = 0
        override fun close() { closes++ }
    }
    @Test fun cancelledQueuedTaskReleasesItsOwnerAndCannotConsumeItLater() {
        val handoff = OwnedHandoff<Owner>()
        val owner = Owner()
        handoff.offer(owner)
        handoff.cancel()
        handoff.cancel()
        assertNull(handoff.take())
        assertEquals(1, owner.closes)
    }
    @Test fun workerTransferPreventsControlCancellationFromClosingItsBorrow() {
        val handoff = OwnedHandoff<Owner>()
        val owner = Owner()
        handoff.offer(owner)
        assertSame(owner, handoff.take())
        handoff.cancel()
        assertEquals(0, owner.closes)
        owner.close()
        assertEquals(1, owner.closes)
    }
    @Test fun secondOfferIsRefusedAndReleasesOnlyTheRefusedOwner() {
        val handoff = OwnedHandoff<Owner>()
        val first = Owner()
        val second = Owner()
        handoff.offer(first)
        assertThrows(IllegalStateException::class.java) { handoff.offer(second) }
        assertEquals(1, second.closes)
        assertEquals(0, first.closes)
        handoff.cancel()
        assertEquals(1, first.closes)
    }
}
