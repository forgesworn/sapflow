// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.nio.channels.WritableByteChannel

/** A synchronous borrow, not a staging buffer. The caller retains the native
 * range until this call returns. Only actual channel returns count as bytes. */
internal class RecordingPcmSink(private val channel: WritableByteChannel) {
    class Transfer internal constructor(
        val bytesReturned: Long, val writeReturns: Long, val complete: Boolean,
        val acceptanceUnknown: Boolean, val failure: Exception?,
    )
    private var writing = false
    private var failed = false
    fun write(range: ByteBuffer): Transfer {
        check(!writing && !failed) { "PCM sink is fenced" }
        writing = true
        val borrowed = range.duplicate()
        var bytes = 0L; var calls = 0L
        try {
            while (borrowed.hasRemaining()) {
                val before = borrowed.remaining()
                val returned = try { channel.write(borrowed) } catch (error: Exception) {
                    failed = true
                    return Transfer(bytes, calls, false, true, error)
                }
                // Record the actual return before any validation can fail.
                calls = Math.addExact(calls, 1)
                if (returned > 0) bytes = Math.addExact(bytes, returned.toLong())
                if (returned <= 0 || returned > before || before - borrowed.remaining() != returned) {
                    failed = true
                    return Transfer(bytes, calls, false, returned != 0 || before != borrowed.remaining(),
                        IllegalStateException("PCM sink made no valid progress"))
                }
            }
            return Transfer(bytes, calls, true, false, null)
        } finally { writing = false }
    }
}
