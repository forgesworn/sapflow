package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReferenceArray

/** Fixed shared-handle transfer, never a payload copy. One worker borrows one
 * item at a time; control cancellation closes only the unborrowed remainder. */
internal class OwnedBatch<T : AutoCloseable> private constructor() : AutoCloseable {
    private val owners = AtomicReferenceArray<T?>(MAX_SAMPLES)
    private val sizes = LongArray(MAX_SAMPLES)
    private val closed = AtomicBoolean(false)
    private val retainedSamples = AtomicInteger()
    private val retainedBytes = AtomicLong()
    private var cursor = 0 // worker only
    var size = 0
        private set
    var bytes = 0L
        private set
    val remainingSamples: Int get() = retainedSamples.get()
    val remainingBytes: Long get() = retainedBytes.get()

    fun consumeNext(consume: (T) -> Unit): Boolean {
        if (closed.get()) return false
        while (cursor < size) {
            val index = cursor++
            val owner = owners.getAndSet(index, null) ?: continue
            try { consume(owner) }
            finally { release(index, owner) }
            return true
        }
        return false
    }

    private fun release(index: Int, owner: T) {
        try { owner.close() }
        finally {
            retainedSamples.decrementAndGet()
            retainedBytes.addAndGet(-sizes[index])
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        var failure: Exception? = null
        for (index in 0 until size) {
            val owner = owners.getAndSet(index, null) ?: continue
            try { release(index, owner) }
            catch (error: Exception) { if (failure == null) failure = error else failure.addSuppressed(error) }
        }
        failure?.let { throw it }
    }

    companion object {
        const val MAX_SAMPLES = 8
        fun <T : AutoCloseable> collect(maxBytes: Long, pop: () -> T?, sizeOf: (T) -> Long): OwnedBatch<T> {
            require(maxBytes > 0)
            val batch = OwnedBatch<T>()
            try {
                repeat(MAX_SAMPLES) {
                    val owner = pop() ?: return batch
                    var admitted = false
                    try {
                        val bytes = sizeOf(owner)
                        require(bytes > 0 && bytes <= maxBytes - batch.bytes) { "Batch exceeded its native queue byte bound" }
                        batch.sizes[batch.size] = bytes
                        batch.owners.set(batch.size++, owner)
                        batch.bytes += bytes
                        batch.retainedSamples.incrementAndGet()
                        batch.retainedBytes.addAndGet(bytes)
                        admitted = true
                    } finally { if (!admitted) owner.close() }
                }
                return batch
            } catch (error: Exception) {
                try { batch.close() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
                throw error
            }
        }
    }
}
