package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NoHandle
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

class IsolatedPublishingSinkTest {
    private class Sink : EncodedSink {
        override val status = "Synthetic publisher"
        var starts = 0; var configures = 0; var fences = 0
        var samples = 0; var lastSample: MediaSample? = null
        val closes = AtomicInteger()
        var startError: Exception? = null; var configureError: Exception? = null; var closeError: Exception? = null
        var fenceError: Exception? = null; var consumeError: Exception? = null
        var entered: CountDownLatch? = null; var release: CountDownLatch? = null
        override fun start() { starts++; startError?.let { throw it } }
        override fun configure(video: RustCodec?, audio: RustCodec?) { configures++; configureError?.let { throw it } }
        override fun consume(sample: MediaSample) { samples++; lastSample = sample; consumeError?.let { throw it } }
        override fun stopDestination() { fences++; fenceError?.let { throw it } }
        override fun close() { closes.incrementAndGet(); entered?.countDown(); release?.let { check(it.await(2, TimeUnit.SECONDS)) }; closeError?.let { throw it } }
    }

    @Test fun publishingStartupFailureDoesNotPreventRecorderCreationOrMediaAdmission() {
        val publisher = Sink().apply { startError = IllegalStateException("synthetic startup") }
        val errors = AtomicInteger(); val isolated = IsolatedPublishingSink(publisher) { errors.incrementAndGet() }
        val gate = CaptureStartAdmission(); val preparation = checkNotNull(gate.reserve())
        assertTrue(gate.claim(preparation)); assertTrue(gate.beginEpoch(preparation, 1)); assertTrue(gate.prepared(preparation, 1))
        val start = checkNotNull(gate.start(preparation))
        val recorder = Sink()
        recorder.start() // The actual capture starts recording resources before commit.
        assertTrue(gate.commit(start))
        isolated.configure(null, null); isolated.start()
        assertEquals(1, recorder.starts); assertEquals(1, publisher.starts); assertEquals(1, errors.get())
        assertFalse(gate.cancelInitial(preparation)) // The independent recording owns an admitted run.
        assertTrue(isolated.status.contains("local recording continues"))
        isolated.start(); isolated.configure(null, null)
        assertEquals(1, publisher.starts); assertEquals(1, publisher.configures); assertEquals(1, publisher.fences)
        // Generated no-handle test constructor: no payload/native operation.
        MediaSample(NoHandle).use { sample ->
            isolated.consume(sample); recorder.consume(sample)
            assertEquals(0, publisher.samples); assertEquals(1, recorder.samples)
            assertSame(sample, recorder.lastSample); assertFalse(sample.uniffiIsDestroyed)
        }
        isolated.close(); recorder.close()
    }

    @Test fun synchronousPublishingFailureDoesNotOwnOrCloseTheBorrowedRecordingHandle() {
        val publisher = Sink().apply { consumeError = IllegalStateException("synthetic publish") }
        val recorder = Sink(); val reports = AtomicInteger()
        val isolated = IsolatedPublishingSink(publisher) { reports.incrementAndGet() }
        MediaSample(NoHandle).use { sample ->
            isolated.consume(sample); recorder.consume(sample)
            isolated.consume(sample); recorder.consume(sample)
            assertEquals(1, publisher.samples); assertEquals(2, recorder.samples)
            assertSame(sample, publisher.lastSample); assertSame(sample, recorder.lastSample)
            assertFalse(sample.uniffiIsDestroyed); assertEquals(1, reports.get())
        }
        isolated.close(); recorder.close()
    }

    @Test fun configurationFailureFencesBeforeStartAndNotifiesOnceEvenIfReporterThrows() {
        val publisher = Sink().apply { configureError = IllegalArgumentException("synthetic config") }
        var reports = 0
        val isolated = IsolatedPublishingSink(publisher) { reports++; error("synthetic diagnostic failure") }
        isolated.configure(null, null); isolated.start(); isolated.configure(null, null)
        assertEquals(1, reports); assertEquals(1, publisher.configures); assertEquals(0, publisher.starts)
        assertEquals(1, publisher.fences)
        isolated.close()
    }

    @Test fun actualCloseFailureRemainsStickyAcrossDuplicateCloseAndNoOpDelegate() {
        val publisher = Sink().apply { closeError = IllegalStateException("synthetic retained owner") }
        val isolated = IsolatedPublishingSink(publisher) {}
        assertThrows(IllegalStateException::class.java) { isolated.close() }
        publisher.closeError = null // An idempotent delegate's later no-op is not proof of release.
        assertThrows(IllegalStateException::class.java) { isolated.close() }
        assertEquals(1, publisher.closes.get())
        assertTrue(isolated.diagnostics.contains("unconfirmed"))
    }

    @Test fun failedPartialStartupCleanupCannotBeMistakenForLaterNoOpClose() {
        val publisher = Sink().apply { startError = IllegalStateException("synthetic setup").apply { addSuppressed(IllegalStateException("synthetic retained socket")) } }
        val isolated = IsolatedPublishingSink(publisher) {}
        isolated.start()
        assertThrows(IllegalStateException::class.java) { isolated.close() }
        assertThrows(IllegalStateException::class.java) { isolated.close() }
        assertEquals(1, publisher.closes.get())
    }

    @Test fun coldFenceFailureIsSingleAndRetainsUnknownCleanup() {
        val publisher = Sink().apply { fenceError = IllegalStateException("synthetic fence") }
        val reports = AtomicInteger(); val isolated = IsolatedPublishingSink(publisher) { reports.incrementAndGet() }
        isolated.stopDestination(); isolated.start()
        assertEquals(1, publisher.fences); assertEquals(1, reports.get()); assertEquals(0, publisher.starts)
        assertThrows(IllegalStateException::class.java) { isolated.close() }
    }

    @Test fun simultaneousCloseWaitsForRealSingleDelegateCompletion() {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val publisher = Sink().apply { this.entered = entered; this.release = release }
        val isolated = IsolatedPublishingSink(publisher) {}
        val closed = CountDownLatch(2); val failure = AtomicReference<Throwable?>()
        val first = Thread { try { isolated.close() } catch (error: Throwable) { failure.set(error) } finally { closed.countDown() } }
        val second = Thread { try { isolated.close() } catch (error: Throwable) { failure.set(error) } finally { closed.countDown() } }
        first.start()
        try {
            assertTrue(entered.await(2, TimeUnit.SECONDS)); second.start()
            assertEquals(2L, closed.count)
        } finally { release.countDown(); first.join(2000); if (second.state != Thread.State.NEW) second.join(2000) }
        assertFalse(first.isAlive); assertFalse(second.isAlive); assertNull(failure.get())
        assertEquals(0L, closed.count); assertEquals(1, publisher.closes.get())
    }

    @Test fun actualTerminalCoordinatorEpochStillStartsQuietlyAcrossRecordingRotation() {
        var factories = 0
        val policy = object : ReconnectPolicyPort {
            override val attemptIdentity: SdkAttemptIdentity? = null
            override val nextDeadlineNs: Long? = null
            override fun start(nowNs: Long): ReconnectAction = error("Stopped destination cannot start")
            override fun failed(nowNs: Long): ReconnectAction = error("No active transport")
            override fun intentionalReplace(nowNs: Long): ReconnectAction = error("No active transport")
            override fun connected(nowNs: Long) = error("No active transport")
            override fun sending(nowNs: Long): ReconnectAction = error("No active transport")
            override fun peerProgress(source: PeerProgressSource, nowNs: Long): ReconnectAction = error("No active transport")
            override fun confirmed(attempt: SdkAttemptIdentity, nowNs: Long) = error("No decoder")
            override fun tick(nowNs: Long): ReconnectAction = error("No timer")
            override fun stop() {}
        }
        val owner = ReconnectingPublishingSink(policyFactory = { policy }, factory = { factories++; Sink() })
        val first = IsolatedPublishingSink(owner.newEpoch()) { fail("Terminal coordinator must not fail recording") }
        first.stopDestination(); first.start(); first.close()
        val recorder = Sink(); recorder.start()
        val rotated = IsolatedPublishingSink(owner.newEpoch()) { fail("Fenced rotation must stay dormant") }
        rotated.configure(null, null); rotated.start(); rotated.close(); recorder.close()
        assertEquals(1, recorder.starts); assertEquals(0, factories)
    }
}
