package dev.forgesworn.sapflow

import com.sun.jna.Native
import java.nio.ByteBuffer
import java.util.Collections
import java.util.IdentityHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

private fun freshStatus(status: UniffiRustCallStatus) {
    check(status.code == 0.toByte())
    check(status.error_buf.capacity == 0L && status.error_buf.len == 0L && status.error_buf.data == null)
}

private fun <T> success(block: (UniffiRustCallStatus) -> T): T =
    uniffiRustCallWithError(UniffiNullRustCallStatusErrorHandler, block)

private inline fun <reified E : Throwable> expectFailure(block: () -> Unit): E {
    try { block() } catch (error: Throwable) {
        check(error is E) { "Expected ${E::class.java.name}, received $error" }
        return error
    }
    error("Expected ${E::class.java.name}")
}

private fun reuseAndReset() {
    val stack = SapflowRustCallStatusPool.current()
    check(stack.depth == 0)
    val first = stack.acquire()
    val embedded = first.error_buf
    val borrowed = ByteBuffer.allocateDirect(8)
    first.code = 9
    first.error_buf.capacity = 17
    first.error_buf.len = 3
    first.error_buf.data = Native.getDirectBufferPointer(borrowed)
    // This pointer is foreign-owned, not a Rust error buffer. Reset must not
    // free/dispose it or replace its embedded JNA descriptor.
    stack.release(first, 0)
    repeat(1000) {
        success { status ->
            check(status === first && status.error_buf === embedded)
            freshStatus(status)
        }
    }
    borrowed.put(0, 42)
    check(borrowed.get(0) == 42.toByte() && stack.depth == 0)
}

private fun nestedAndOverflow() {
    fun nested(level: Int, statuses: MutableList<UniffiRustCallStatus>) {
        success { status ->
            freshStatus(status)
            check(statuses.none { it === status })
            statuses.add(status)
            if (level < 8) nested(level + 1, statuses)
            else {
                // Real constructors/argument lowering/destructors must still
                // work beyond the four cached levels, without a hard cap.
                val input = ByteBuffer.allocateDirect(1).put(7).apply { flip() }
                EncodedBuffer(input).use { buffer ->
                    check(buffer.byteAt(0uL) == 7.toUByte())
                    expectFailure<BindingException.IndexOutOfBounds> { buffer.byteAt(ULong.MAX_VALUE) }
                    check(buffer.len() == 1uL)
                }
                val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x90.toByte())).apply { flip() }
                MediaCodec.aac(asc).use { codec ->
                    codec.sample(input, 0, 0, 1024u, 48_000u, false).use { sample ->
                        NativeSampleFanout(4096uL, 4096uL, 1000u, false).use { queues ->
                            // Both receiver and sample Arc handles are lowered
                            // while the four-slot cache is already exhausted.
                            val outcome = queues.push(sample, 0uL)
                            check(outcome.publishingQueued && outcome.recordingQueued)
                            queues.popPublishing(0uL)!!.close()
                            queues.popRecording(0uL)!!.close()
                        }
                    }
                }
            }
            freshStatus(status)
        }
    }
    val first = mutableListOf<UniffiRustCallStatus>()
    val second = mutableListOf<UniffiRustCallStatus>()
    nested(1, first); nested(1, second)
    for (index in 0..3) check(first[index] === second[index])
    for (index in 4..7) check(first[index] !== second[index])
    check(SapflowRustCallStatusPool.current().depth == 0)
    expectFailure<IllegalArgumentException> {
        fun unwind(level: Int) {
            success { if (level == 8) throw IllegalArgumentException("synthetic callback failure") else unwind(level + 1) }
        }
        unwind(1)
    }
    check(SapflowRustCallStatusPool.current().depth == 0)
    success(::freshStatus)
}

private fun typedErrorAndNestedFree() {
    var observed: UniffiRustCallStatus? = null
    var embedded: RustBuffer.ByValue? = null
    val handler = object : UniffiRustCallStatusErrorHandler<BindingException> {
        override fun lift(error_buf: RustBuffer.ByValue): BindingException {
            check(SapflowRustCallStatusPool.current().depth == 1)
            check(observed!!.error_buf === embedded && error_buf === embedded)
            check(sdkAbiVersion() == 1u) // Separate nested status during lifting.
            // Actual generated converter performs a nested native buffer free.
            return BindingException.lift(error_buf)
        }
    }
    repeat(40) {
        expectFailure<BindingException.EmptyBuffer> {
            uniffiRustCallWithError(handler) { status ->
                if (observed == null) { observed = status; embedded = status.error_buf }
                check(observed === status && embedded === status.error_buf)
                UniffiLib.uniffi_sapflow_ffi_fn_constructor_encodedbuffer_new(
                    FfiConverterByRefBytes.lower(ByteBuffer.allocateDirect(0)), status)
            }
        }
        check(SapflowRustCallStatusPool.current().depth == 0)
        success(::freshStatus)
    }
}

private fun lifoRejectsMismatchedReleaseWithoutLosingOwners() {
    val stack = SapflowRustCallStatusPool.current()
    val outer = stack.acquire(); val inner = stack.acquire()
    expectFailure<IllegalStateException> { stack.release(outer, 0) }
    check(stack.depth == 2)
    expectFailure<IllegalStateException> { stack.release(outer, 1) }
    check(stack.depth == 2)
    stack.release(inner, 1); stack.release(outer, 0)
    success(::freshStatus)
}

private fun panicAndUnknownStatus() {
    val message = "synthetic panic status with real Rust-owned buffer"
    val panic = expectFailure<InternalException> {
        success { status ->
            status.error_buf.setValue(FfiConverterString.lower(message))
            status.code = 2
        }
    }
    check(panic.message == message)
    val unexpected = expectFailure<InternalException> {
        success { status ->
            status.error_buf.setValue(FfiConverterTypeBindingError.lower(BindingException.InvalidMedia()))
            status.code = 1
        }
    }
    check(unexpected.message == "Unexpected CALL_ERROR")
    check(expectFailure<InternalException> { success { it.code = 2 } }.message == "Rust panic")
    expectFailure<InternalException> { success { it.code = 17 } }
    check(SapflowRustCallStatusPool.current().depth == 0)
    success(::freshStatus)
}

private fun repeatedRealErrorsAndSuccess() {
    val bytes = ByteBuffer.allocateDirect(1).put(19).apply { flip() }
    EncodedBuffer(bytes).use { sample ->
        repeat(200) {
            expectFailure<BindingException.IndexOutOfBounds> { sample.byteAt(ULong.MAX_VALUE) }
            check(sample.len() == 1uL && sample.byteAt(0uL) == 19.toUByte())
        }
    }
    check(SapflowRustCallStatusPool.current().depth == 0)
}

private fun concurrentOwners() {
    val arrived = CountDownLatch(3)
    val resume = CountDownLatch(1)
    val errors = AtomicReference<Throwable?>()
    val statuses = Collections.synchronizedList(mutableListOf<UniffiRustCallStatus>())
    val threads = listOf("AUDIO", "publishing", "recording").map { name ->
        Thread({
            try {
                success { status ->
                    statuses.add(status)
                    arrived.countDown()
                    check(resume.await(10, TimeUnit.SECONDS))
                    val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x90.toByte())).apply { flip() }
                    val payload = ByteBuffer.allocateDirect(3).put(byteArrayOf(1, 2, 3)).apply { flip() }
                    MediaCodec.aac(asc).use { codec ->
                        repeat(64) { index ->
                            codec.sample(payload, index * 1024L, index * 1024L, 1024u, 48_000u, false).use { sample ->
                                check(sample.len() == 3uL)
                                NativeSampleFanout(4096uL, 4096uL, 1000u, false).use { queues ->
                                    val outcome = queues.push(sample, 0uL)
                                    check(outcome.publishingQueued && outcome.recordingQueued)
                                    queues.popPublishing(0uL)!!.close()
                                    queues.popRecording(0uL)!!.close()
                                }
                            }
                        }
                    }
                    freshStatus(status)
                }
                check(SapflowRustCallStatusPool.current().depth == 0)
            } catch (error: Throwable) { errors.compareAndSet(null, error) }
        }, "status-test-$name").apply { start() }
    }
    try { check(arrived.await(10, TimeUnit.SECONDS)) }
    finally { resume.countDown() }
    threads.forEach { it.join(15_000); check(!it.isAlive) }
    errors.get()?.let { throw it }
    val distinct = Collections.newSetFromMap(IdentityHashMap<UniffiRustCallStatus, Boolean>())
    distinct.addAll(statuses)
    check(distinct.size == 3)
}

private fun closeWhileInFlight() {
    val payload = ByteBuffer.allocateDirect(1).put(9).apply { flip() }
    val sample = EncodedBuffer(payload)
    val entered = CountDownLatch(1); val resume = CountDownLatch(1)
    val errors = AtomicReference<Throwable?>()
    val worker = Thread({
        try {
            sample.callWithHandle { handle ->
                entered.countDown()
                check(resume.await(10, TimeUnit.SECONDS))
                val size = success { status ->
                    UniffiLib.uniffi_sapflow_ffi_fn_method_encodedbuffer_len(handle, status)
                }
                check(size == 1L)
            }
            check(SapflowRustCallStatusPool.current().depth == 0)
        } catch (error: Throwable) { errors.set(error) }
    }, "status-test-close-race").apply { start() }
    try {
        check(entered.await(10, TimeUnit.SECONDS))
        sample.close() // Original owner waits for the generated call counter.
    } finally { resume.countDown() }
    worker.join(15_000); check(!worker.isAlive)
    errors.get()?.let { throw it }
    expectFailure<IllegalStateException> { sample.len() }
}

fun main() {
    val tests = listOf(::reuseAndReset, ::nestedAndOverflow, ::typedErrorAndNestedFree,
        ::lifoRejectsMismatchedReleaseWithoutLosingOwners, ::panicAndUnknownStatus,
        ::repeatedRealErrorsAndSuccess, ::concurrentOwners, ::closeWhileInFlight)
    tests.forEach { test -> test(); println("Status lease check passed: ${test.name}") }
    println("Sapflow generated Kotlin status lease checks passed: ${tests.size}")
}
