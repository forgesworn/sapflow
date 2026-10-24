// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import org.junit.Test
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.WritableByteChannel

/** JVM checks. These synthetic channels/borrow acknowledgements are not
 * MediaCodec output, native EOF or a production proof capability. */
private class Channel(private val operation: (ByteBuffer) -> Int) : WritableByteChannel {
    override fun write(source: ByteBuffer): Int = operation(source)
    override fun isOpen() = true
    override fun close() = Unit
}
private fun refuses(action: () -> Unit) {
    var refused = false
    try { action() } catch (_: IllegalStateException) { refused = true }
    check(refused)
}
class RecordingPcmSinkTest {
    @Test fun partialBorrowPreservesRangeAndStorage() {
        val original = ByteBuffer.allocateDirect(12)
        repeat(12) { original.put(it, it.toByte()) }; original.position(2); original.limit(10)
        val received = ArrayList<Byte>()
        val sink = RecordingPcmSink(Channel { borrowed ->
            check(borrowed.isDirect)
            val count = minOf(3, borrowed.remaining())
            repeat(count) { received.add(borrowed.get()) }; count
        })
        val returned = sink.write(original)
        check(returned.complete && returned.bytesReturned == 8L && returned.writeReturns == 3L)
        check(!returned.acceptanceUnknown && received == (2..9).map { it.toByte() })
        check(original.position() == 2 && original.limit() == 10)
    }
    @Test fun failedWriteRetainsOnlyActualReturnedBytes() {
        var calls = 0
        val sink = RecordingPcmSink(Channel { borrowed ->
            if (++calls == 1) { borrowed.position(borrowed.position() + 2); 2 }
            else { borrowed.get(); throw IOException("Actual failing call may have consumed bytes") }
        })
        val result = sink.write(ByteBuffer.wrap(byteArrayOf(1, 2, 3, 4)))
        check(!result.complete && result.acceptanceUnknown && result.bytesReturned == 2L && result.writeReturns == 1L)
        check(result.failure is IOException)
        refuses { sink.write(ByteBuffer.allocate(2)) }; check(calls == 2)
    }
    @Test fun zeroProgressFencesWithoutInventingAcceptance() {
        var calls = 0
        val sink = RecordingPcmSink(Channel { calls++; 0 })
        val result = sink.write(ByteBuffer.allocate(2))
        check(!result.complete && !result.acceptanceUnknown && result.bytesReturned == 0L && result.writeReturns == 1L)
        refuses { sink.write(ByteBuffer.allocate(2)) }; check(calls == 1)
    }
    @Test fun reentrantWriteCannotAcquireTheBorrow() {
        lateinit var sink: RecordingPcmSink
        var calls = 0
        sink = RecordingPcmSink(Channel { borrowed ->
            calls++
            refuses { sink.write(ByteBuffer.allocate(2)) }
            val count = borrowed.remaining(); borrowed.position(borrowed.limit()); count
        })
        val result = sink.write(ByteBuffer.allocate(4))
        check(result.complete && result.bytesReturned == 4L && calls == 1)
    }
    @Test fun realSinkFailureLeavesKnownNativeBorrowReleasable() {
        val owner = Any(); val generation = 1L
        val clock = ReturnedPcmClock(owner, generation, ReturnedPcmClock.Geometry(10_240, 0, 2))
        val range = clock.observe(owner, generation, 1, 0, 0, 4, 4, 21_333, 0)
        clock.sinkAttempt(owner, generation, range)
        val sink = RecordingPcmSink(Channel {
            // The caller's observed return is visible before arbitrary channel I/O.
            val pending = clock.snapshot()
            check(pending.returnedFrames == 2L && pending.sinkAcceptedFrames == 0L)
            throw IOException("Unknown sink disposition")
        })
        val result = sink.write(ByteBuffer.allocate(4))
        check(!result.complete && result.acceptanceUnknown)
        clock.sinkFailed(owner, generation, range, result.acceptanceUnknown)
        clock.releaseAttempt(owner, generation, range)
        var syntheticReleaseReturns = 0
        syntheticReleaseReturns++
        clock.releaseReturned(owner, generation, range)
        val observed = clock.snapshot()
        check(syntheticReleaseReturns == 1 && observed.releasesReturned == 1L)
        check(observed.sinkAcceptanceUnknown && !observed.nativeOwnershipUnknown && !observed.scheduleComplete)
        check(!observed.contentQualified && !observed.nativeSourceQualified && !observed.appAdmission)
    }
}
