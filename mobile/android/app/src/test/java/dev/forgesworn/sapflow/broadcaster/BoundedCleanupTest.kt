package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
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

    @Test(timeout = 3000) fun anInterruptedCallerReturnsFalseAndKeepsItsInterrupt() {
        // An RTMP reader reporting a refused publish is interrupted by the
        // owner closing the attempt; that used to escape and end the app.
        val result = AtomicReference<Any?>()
        val reader = thread {
            Thread.currentThread().interrupt()
            result.set(try { ReentrantLock().cleanupWithin(100) { error("must not run") } to Thread.currentThread().isInterrupted }
                catch (error: Throwable) { error })
        }
        reader.join(1000)
        assertEquals(false to true, result.get())
    }
}
