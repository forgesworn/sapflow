package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.thread
import kotlin.concurrent.withLock
import org.junit.Assert.*
import org.junit.Test

class BoundedCleanupTest {
    @Test(timeout = 3000) fun stalledWriterDoesNotBlockStopAndCleanupCanRetryAfterIoUnwinds() {
        val lock = ReentrantLock()
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        val writer = thread {
            lock.withLock { held.countDown(); release.await() }
        }
        var released = false
        try {
            assertTrue(held.await(1, TimeUnit.SECONDS))
            assertFalse(lock.cleanupWithin(100) { released = true })
            assertFalse(released)
        } finally {
            release.countDown()
            writer.join(1000)
        }
        assertTrue(lock.cleanupWithin(100) { released = true })
        assertTrue(released)
        assertFalse(writer.isAlive)
    }
}
