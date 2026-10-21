package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.Assert.*
import org.junit.Test

class SrtShutdownTest {
    private fun control() = ByteArray(16).also { it[0] = 0x80.toByte(); it[1] = 5; it[15] = 7 }

    @Test fun originalDeadlineCannotBeExtendedAndRequestHasNoNativeOrSocketSideEffects() {
        val shutdown = SrtShutdown()
        assertEquals(100_007L, shutdown.request(7))
        assertEquals(100_007L, shutdown.request(99_999))
        var stopped = 0
        assertEquals(SrtShutdown.Result.EXPIRED, shutdown.finish({ 100_007 }, { stopped++ },
            { fail("expired control must not be polled"); null }, { fail("expired control must not be sent"); 16 }))
        assertEquals(1, stopped)
        assertNull(shutdown.request(200_000))
    }

    @Test fun workerCancelsMediaBeforePollingAndWritesExactlyOneFreshShutdown() {
        val shutdown = SrtShutdown()
        shutdown.request(0)
        val order = mutableListOf<String>()
        var mediaRetained = true
        val expected = control()
        val result = shutdown.finish({ 1 }, { order.add("cancel"); mediaRetained = false }, {
            assertFalse(mediaRetained); order.add("poll"); expected
        }, { buffer ->
            order.add("write")
            val sent = ByteArray(buffer.remaining()); buffer.get(sent)
            assertArrayEquals(expected, sent)
            sent.size
        })
        assertEquals(SrtShutdown.Result.SUBMITTED, result) // Local submission only.
        assertEquals(listOf("cancel", "poll", "write"), order)
        assertEquals(SrtShutdown.Result.DUPLICATE, shutdown.finish({ 2 }, { fail("second stop") }, { fail("second poll"); null }, { fail("second write"); 0 }))
    }

    @Test fun zeroWriteIsNotRetriedAndMissingControlIsNormalBeforeHandshake() {
        for ((packet, expected) in listOf(control() to SrtShutdown.Result.NOT_WRITABLE, null to SrtShutdown.Result.NO_CONTROL)) {
            val shutdown = SrtShutdown(); shutdown.request(0)
            var writes = 0
            assertEquals(expected, shutdown.finish({ 1 }, {}, { packet }, { writes++; 0 }))
            assertEquals(if (packet == null) 0 else 1, writes)
            assertEquals(SrtShutdown.Result.DUPLICATE, shutdown.finish({ 2 }, {}, { control() }, { writes++; 16 }))
            assertEquals(if (packet == null) 0 else 1, writes)
        }
    }

    @Test fun hardAbortBeforeOrDuringNativeCancellationForbidsControlButStillReleasesNative() {
        for (abortBefore in listOf(true, false)) {
            val shutdown = SrtShutdown(); shutdown.request(0)
            var cancellations = 0
            if (abortBefore) shutdown.abort()
            val result = shutdown.finish({ 1 }, { cancellations++; shutdown.abort() },
                { fail("hard abort must not poll"); null }, { fail("hard abort must not send"); 0 })
            assertEquals(SrtShutdown.Result.ABORTED, result)
            assertEquals(1, cancellations)
        }
    }

    @Test fun packetPollingAndWriteCompletionCannotHideAnExpiredBudget() {
        for (stage in listOf("cancel", "poll", "write")) {
            val shutdown = SrtShutdown(); shutdown.request(0)
            var now = 0L
            var writes = 0
            val result = shutdown.finish({ now }, { if (stage == "cancel") now = 100_000 }, {
                if (stage == "poll") now = 100_000
                control()
            }, { writes++; if (stage == "write") now = 100_000; 16 })
            assertEquals(SrtShutdown.Result.EXPIRED, result)
            assertEquals(if (stage == "write") 1 else 0, writes)
        }
    }

    @Test fun oldMediaAndUnexpectedControlPacketsAreNeverTransmitted() {
        val invalid = listOf(ByteArray(1316), control().copyOf(17), control().also { it[0] = 0 },
            control().also { it[1] = 2 }, control().also { it[2] = 1 }, control().also { it[3] = 1 })
        for (packet in invalid) {
            val shutdown = SrtShutdown(); shutdown.request(0)
            assertEquals(SrtShutdown.Result.INVALID_CONTROL, shutdown.finish({ 1 }, {}, { packet },
                { fail("only a new shutdown may be sent"); 0 }))
        }
    }

    @Test fun nativeErrorsAndPartialOrFailedWritesRemainTerminalWithoutRetry() {
        for (stage in listOf("cancel", "poll", "partial", "negative", "write")) {
            val shutdown = SrtShutdown(); shutdown.request(0)
            var writes = 0
            val result = shutdown.finish({ 1 }, { if (stage == "cancel") error("cancel fault") }, {
                if (stage == "poll") error("poll fault")
                control()
            }, {
                writes++
                when (stage) { "partial" -> 8; "negative" -> -1; else -> error("write fault") }
            })
            assertEquals(if (stage in listOf("cancel", "poll")) SrtShutdown.Result.NATIVE_FAILED else SrtShutdown.Result.WRITE_FAILED, result)
            assertEquals(if (stage in listOf("cancel", "poll")) 0 else 1, writes)
            assertEquals(SrtShutdown.Result.DUPLICATE, shutdown.finish({ 2 }, {}, { control() }, { fail("no retry"); 16 }))
        }
    }

    @Test(timeout = 3000) fun cancellationDoesNotTransferNativeOrSocketOwnershipToTheControlThread() {
        val shutdown = SrtShutdown(); shutdown.request(0)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        var result: SrtShutdown.Result? = null
        val worker = thread(name = "test-udp-owner") {
            result = shutdown.finish({ 1 }, {
                assertEquals("test-udp-owner", Thread.currentThread().name)
                entered.countDown(); check(release.await(1, TimeUnit.SECONDS))
            }, { fail("abort arrived during native stop"); null }, { fail("aborted write"); 0 })
        }
        try {
            assertTrue(entered.await(1, TimeUnit.SECONDS))
            assertEquals(SrtShutdown.Result.DUPLICATE, shutdown.finish({ 2 }, { fail("foreign native stop") }, { null }, { 0 }))
            shutdown.abort()
        } finally { release.countDown(); worker.join(1000) }
        assertFalse(worker.isAlive)
        assertEquals(SrtShutdown.Result.ABORTED, result)
    }

    @Test fun stopBeforeStartAndDuplicateCloseCannotCreateOrRestartAnAttempt() {
        var starts = 0
        var closes = 0
        val shutdown = SrtShutdown()
        val attempt = ExplicitAttempt({ starts++ }, { closes++; shutdown.abort() })
        attempt.close(); attempt.close()
        assertThrows(IllegalStateException::class.java) { attempt.start() }
        assertEquals(0, starts); assertEquals(1, closes)
        assertNull(shutdown.request(0))
        assertEquals(SrtShutdown.Result.ABORTED, shutdown.finish({ 1 }, {}, { fail("no socket/native publisher"); null }, { fail("no socket"); 0 }))
    }

    @Test fun abortAfterPollingPreventsSendAndUnrequestedCleanupOnlyCancelsNative() {
        val shutdown = SrtShutdown(); shutdown.request(0)
        assertEquals(SrtShutdown.Result.ABORTED, shutdown.finish({ 1 }, {}, { shutdown.abort(); control() }, { fail("abort won"); 0 }))
        val unrequested = SrtShutdown()
        var cancelled = false
        assertEquals(SrtShutdown.Result.NOT_REQUESTED, unrequested.finish({ 1 }, { cancelled = true }, { fail("no normal stop"); null }, { 0 }))
        assertTrue(cancelled)
    }
}
