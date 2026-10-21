package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/** Normal Stop permits one worker-owned, nonblocking control write. Zero-write
 * readiness is deliberately not retried: shutdown is best effort, never a
 * delivery acknowledgement. Hard abort and the original deadline always win. */
internal class SrtShutdown {
    enum class Result { NOT_REQUESTED, DUPLICATE, ABORTED, EXPIRED, NO_CONTROL, INVALID_CONTROL, NATIVE_FAILED, NOT_WRITABLE, WRITE_FAILED, SUBMITTED }
    private val deadlineUs = AtomicLong(-1)
    private val aborted = AtomicBoolean(false)
    private val finished = AtomicBoolean(false)

    fun request(nowUs: Long): Long? {
        require(nowUs >= 0)
        if (aborted.get() || finished.get()) return null
        deadlineUs.compareAndSet(-1, Math.addExact(nowUs, BUDGET_US))
        return deadlineUs.get()
    }

    fun abort() { aborted.set(true) }

    /** Called only by the UDP owner, including on failure. Native cancellation
     * runs even when control transmission is forbidden, releasing media/keys. */
    fun finish(nowUs: () -> Long, stopNative: () -> Unit, takeControl: () -> ByteArray?, write: (ByteBuffer) -> Int): Result {
        if (!finished.compareAndSet(false, true)) return Result.DUPLICATE
        try { stopNative() } catch (_: Exception) { return Result.NATIVE_FAILED }
        val deadline = deadlineUs.get()
        if (aborted.get()) return Result.ABORTED
        if (deadline < 0) return Result.NOT_REQUESTED
        if (nowUs() >= deadline) return Result.EXPIRED
        val bytes = try { takeControl() } catch (_: Exception) { return Result.NATIVE_FAILED }
            ?: return Result.NO_CONTROL
        // SRT SHUTDOWN is a 16-byte control header, type 5, subtype 0.
        // A previously returned media datagram can never enter this path.
        if (bytes.size != 16 || bytes[0] != 0x80.toByte() || bytes[1] != 5.toByte() || bytes[2] != 0.toByte() || bytes[3] != 0.toByte()) return Result.INVALID_CONTROL
        if (aborted.get()) return Result.ABORTED
        if (nowUs() >= deadline) return Result.EXPIRED
        val sent = try { write(ByteBuffer.wrap(bytes)) } catch (_: Exception) { return Result.WRITE_FAILED }
        if (aborted.get()) return Result.ABORTED
        if (nowUs() >= deadline) return Result.EXPIRED
        return when (sent) {
            bytes.size -> Result.SUBMITTED
            0 -> Result.NOT_WRITABLE
            else -> Result.WRITE_FAILED
        }
    }

    companion object { const val BUDGET_US = 100_000L }
}
