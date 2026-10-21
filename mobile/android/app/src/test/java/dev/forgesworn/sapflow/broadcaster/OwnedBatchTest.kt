package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.ArrayDeque

class OwnedBatchTest {
    private class Owner(val id: Int, val bytes: Long = 1, val failClose: Boolean = false) : AutoCloseable {
        var closes = 0
        override fun close() { closes++; if (failClose) error("injected close failure") }
    }
    private fun collect(owners: ArrayDeque<Owner>, bytes: Long = 64) =
        OwnedBatch.collect(bytes, { owners.pollFirst() }, { it.bytes })

    @Test fun fixedEightHandleBatchPreservesOrderAndLeavesNinthInNativeSource() {
        val owners = List(9) { Owner(it) }
        val source = ArrayDeque(owners)
        val batch = collect(source)
        assertEquals(8, batch.size)
        assertEquals(8L, batch.bytes)
        assertEquals(1, source.size)
        val order = mutableListOf<Int>()
        batch.use { while (it.consumeNext { owner -> order.add(owner.id) }) {} }
        assertEquals((0..7).toList(), order)
        assertEquals(0, batch.remainingSamples)
        assertEquals(0L, batch.remainingBytes)
        assertTrue(owners.take(8).all { it.closes == 1 })
        assertEquals(0, owners[8].closes)
        owners[8].close()
    }

    @Test fun cancellationClosesRemainderWithoutClosingWorkersCurrentBorrow() {
        val owners = List(8) { Owner(it) }
        val batch = collect(ArrayDeque(owners))
        assertTrue(batch.consumeNext { current ->
            batch.close()
            assertEquals(0, current.closes)
            assertTrue(owners.drop(1).all { it.closes == 1 })
            assertEquals(1, batch.remainingSamples)
        })
        assertFalse(batch.consumeNext { fail("cancelled batch was consumed") })
        batch.close()
        assertTrue(owners.all { it.closes == 1 })
        assertEquals(0L, batch.remainingBytes)
    }

    @Test fun queuedBatchCancellationAndConsumeFailureReleaseEveryOwner() {
        val owners = List(8) { Owner(it) }
        val handoff = OwnedHandoff<OwnedBatch<Owner>>()
        handoff.offer(collect(ArrayDeque(owners)))
        handoff.cancel()
        assertNull(handoff.take())
        assertTrue(owners.all { it.closes == 1 })
        val second = List(8) { Owner(it) }
        assertThrows(IllegalStateException::class.java) {
            collect(ArrayDeque(second)).use { batch ->
                while (batch.consumeNext { if (it.id == 2) error("injected sink failure") }) {}
            }
        }
        assertTrue(second.all { it.closes == 1 })
    }

    @Test fun byteBoundAndCleanupFailureCannotLeakCollectedOwners() {
        val oversized = listOf(Owner(0, 3), Owner(1, 3))
        assertThrows(IllegalArgumentException::class.java) { collect(ArrayDeque(oversized), 4) }
        assertTrue(oversized.all { it.closes == 1 })
        val closeFailure = listOf(Owner(0, failClose = true), Owner(1))
        val batch = collect(ArrayDeque(closeFailure))
        assertThrows(IllegalStateException::class.java) { batch.close() }
        assertTrue(closeFailure.all { it.closes == 1 })
        assertEquals(0, batch.remainingSamples)
    }

    @Test fun measuredServiceCostsAreStableWithAdaptiveBatchingButNotSingleHandoffs() {
        // Independent virtual scheduler using the physical ten-minute means.
        // No sleeps, native SDK or timing-sensitive assertions.
        fun simulate(cap: Int): Pair<Long, Double> {
            val pending = ArrayDeque<Owner>()
            val periodNs = 10_000_000_000L / 772 // 77.2 ingress units/s
            var nextIngressNs = 0L
            var nowNs = 0L
            var accepted = 0
            var consumed = 0
            var batches = 0
            var peakBacklogNs = 0L
            fun arrive() {
                while (nextIngressNs <= nowNs) {
                    pending.add(Owner(accepted++))
                    nextIngressNs += periodNs
                }
                peakBacklogNs = maxOf(peakBacklogNs, pending.size * periodNs)
            }
            while (nowNs < 600_000_000_000L) {
                if (pending.isEmpty()) nowNs = maxOf(nowNs, nextIngressNs)
                nowNs += 6_157_000 // one handler return per batch
                arrive()
                var popped = 0
                val batch = OwnedBatch.collect(64, {
                    if (popped++ < cap) pending.pollFirst() else null
                }, { it.bytes })
                batches++
                nowNs += 259_000 // one worker dispatch per batch
                batch.use {
                    while (it.consumeNext {
                        nowNs += 8_255_000 // consume cost remains per sample
                        consumed++
                        arrive()
                    }) {}
                }
            }
            while (!pending.isEmpty()) pending.removeFirst().close()
            return peakBacklogNs to consumed.toDouble() / batches
        }
        val single = simulate(1)
        val batched = simulate(8)
        assertTrue("single-handoff backlog must exceed residence bound", single.first > 2_000_000_000)
        assertTrue("batching must keep backlog below100ms", batched.first < 100_000_000)
        // Required mean is handler+dispatch / (arrival period - consume cost)
        // = ~1.366; a mixture of one- and two-sample batches is sufficient.
        assertTrue("batching should adapt between one and two samples: ${batched.second}", batched.second in 1.36..2.0)
    }
}
