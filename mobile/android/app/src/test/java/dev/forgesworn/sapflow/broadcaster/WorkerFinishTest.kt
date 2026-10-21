package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import org.junit.Assert.*
import org.junit.Test

class WorkerFinishTest {
    private fun worker() = ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS, ArrayBlockingQueue(2))

    @Test(timeout = 3000) fun finalisationRunsAfterAcceptedMediaOnTheSameWorker() {
        val executor = worker()
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        val mediaThread = AtomicReference<Thread>()
        val finalThread = AtomicReference<Thread>()
        val completed = AtomicBoolean(false)
        executor.execute { mediaThread.set(Thread.currentThread()); held.countDown(); release.await() }
        assertTrue(held.await(1, TimeUnit.SECONDS))
        val control = thread { completed.set(executor.finishWithin(2000, { finalThread.set(Thread.currentThread()) }, { fail("normal completion must not abort") })) }
        try { assertNull(finalThread.get()) }
        finally { release.countDown(); control.join(2500); executor.shutdownNow() }
        assertTrue(completed.get())
        assertSame(mediaThread.get(), finalThread.get())
        assertNotSame(Thread.currentThread(), finalThread.get())
        assertTrue(executor.isTerminated)
    }

    @Test(timeout = 3000) fun stalledIoIsAbortedAndQueuedFinalisationCannotRunAfterTheDeadline() {
        val executor = worker()
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        val finalised = AtomicBoolean(false)
        val aborted = AtomicBoolean(false)
        executor.execute { held.countDown(); runCatching { release.await() } }
        assertTrue(held.await(1, TimeUnit.SECONDS))
        try {
            assertFalse(executor.finishWithin(50, { finalised.set(true) }, { aborted.set(true) }))
        } finally { release.countDown(); executor.shutdownNow(); executor.awaitTermination(1, TimeUnit.SECONDS) }
        assertTrue(aborted.get())
        assertFalse(finalised.get())
        assertTrue(executor.isTerminated)
    }
}
