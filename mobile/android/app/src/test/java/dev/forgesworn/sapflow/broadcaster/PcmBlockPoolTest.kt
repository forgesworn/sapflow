package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class PcmBlockPoolTest {
    @Test fun fixedCapacityIncludesReaderQueueAndEncoderOwners() {
        val pool = PcmBlockPool()
        assertEquals(16_384, pool.capacityBytes)
        val reader = pool.acquireReader()!!
        val others = List(7) { pool.acquireReader()!! }
        pool.publish(reader, 2048, 0, 0)
        val encoder = pool.acquireEncoder(0) { 1 }!!
        assertThrows(IllegalStateException::class.java) { pool.acquireReader() }
        pool.release(encoder)
        assertNotNull(pool.acquireReader())
        others.forEach(pool::release)
    }

    @Test fun delayedEncoderDoesNotStopReadingOrChangePcmOrderAndTime() {
        val pool = PcmBlockPool()
        var nextConsumed = 0L
        val blockNs = 1024L * 1_000_000_000 / 48_000
        repeat(6000) { unit ->
            val now = unit * blockNs
            val block = pool.acquireReader()!!
            block.buffer.putLong(0, unit.toLong())
            pool.publish(block, 2048, unit * 1024L * 1_000_000 / 48_000, now)
            // 127ms AAC-input scheduling stall every approximately2s.
            if (unit % 100 >= 6) while (pool.queued > 0) {
                val encoded = pool.acquireEncoder(0) { now }!!
                assertEquals(nextConsumed, encoded.buffer.getLong(0))
                assertEquals(nextConsumed * 1024 * 1_000_000 / 48_000, encoded.ptsUs)
                nextConsumed++
                pool.release(encoded)
            }
        }
        assertEquals(6000, nextConsumed)
        assertTrue(pool.highWater in 6..8)
    }

    @Test fun stalePcmFailsWithOriginalCaptureTime() {
        val pool = PcmBlockPool()
        val block = pool.acquireReader()!!
        pool.publish(block, 2048, 12, 12_000)
        val error = assertThrows(IllegalStateException::class.java) { pool.acquireEncoder(0) { 200_012_001 } }
        assertTrue(error.message!!.contains("capturedNs=12000"))
    }

    @Test fun cancellationRetiresQueuedBlocksAndAcceptsActualOwnersReturning() {
        val pool = PcmBlockPool()
        val reader = pool.acquireReader()!!
        val first = pool.acquireReader()!!; pool.publish(first, 2048, 0, 0)
        val encoder = pool.acquireEncoder(0) { 0 }!!
        val queued = pool.acquireReader()!!; pool.publish(queued, 2048, 0, 0)
        val failureEvidence = pool.diagnostics()
        assertTrue(failureEvidence.contains("reader=1 ready=1 encoder=1"))
        pool.cancel()
        assertTrue(failureEvidence.contains("ready=1"))
        assertEquals(0, pool.queued)
        assertNull(pool.acquireReader())
        assertNull(pool.acquireEncoder(0) { 0 })
        pool.release(reader); pool.release(encoder)
        assertTrue(pool.finished)
    }
}
