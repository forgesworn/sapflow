package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit

/** Exactly eight native PCM blocks across reader, queue and encoder ownership.
 * Platform-only copy into MediaCodec; none of these bytes enter FFI. */
internal class PcmBlockPool(private val blocks: Int = 8, private val frames: Int = 1024, private val maxAgeNs: Long = 200_000_000) {
    private enum class State { FREE, READER, READY, ENCODER, RETIRED }
    internal class Block internal constructor(val buffer: ByteBuffer) {
        internal var bytes = 0
        internal var ptsUs = 0L
        internal var capturedNs = 0L
    }
    private val guard = Any()
    private val free = ArrayBlockingQueue<Block>(blocks)
    private val ready = ArrayBlockingQueue<Block>(blocks)
    private val states = java.util.IdentityHashMap<Block, State>()
    private var closed = false
    @Volatile private var producerFinished = false
    val finished get() = producerFinished && ready.isEmpty()
    @Volatile var highWater = 0
        private set
    val queued get() = ready.size
    val capacityBytes get() = blocks * frames * 2
    init {
        require(blocks in 1..8 && frames in 1..1024 && maxAgeNs > 0)
        repeat(blocks) { Block(ByteBuffer.allocateDirect(frames * 2)).also { free.add(it); states[it] = State.FREE } }
    }
    fun acquireReader(): Block? = synchronized(guard) {
        if (closed) null else {
            val block = free.poll() ?: error("PCM pool exhausted ($blocks blocks, ${capacityBytes}B); AAC input did not keep up")
            check(states[block] == State.FREE); states[block] = State.READER; block.buffer.clear(); block
        }
    }
    fun finishReading() { producerFinished = true }
    fun publish(block: Block, bytes: Int, ptsUs: Long, capturedNs: Long) = synchronized(guard) {
        check(states[block] == State.READER)
        require(bytes in 1..block.buffer.capacity() && bytes % 2 == 0 && ptsUs >= 0 && capturedNs >= 0)
        if (closed) states[block] = State.RETIRED
        else {
            block.bytes = bytes; block.ptsUs = ptsUs; block.capturedNs = capturedNs
            states[block] = State.READY
            check(ready.offer(block)); highWater = maxOf(highWater, ready.size)
        }
    }
    fun acquireEncoder(timeoutMs: Long, nowNs: () -> Long): Block? {
        val block = ready.poll(timeoutMs, TimeUnit.MILLISECONDS) ?: return null
        return synchronized(guard) {
            check(states[block] == State.READY)
            if (closed) { states[block] = State.RETIRED; null }
            else {
                states[block] = State.ENCODER
                try { checkAge(block, nowNs()) } catch (error: Exception) { states[block] = State.RETIRED; throw error }
                block
            }
        }
    }
    fun checkAge(block: Block, nowNs: Long) {
        check(nowNs >= block.capturedNs && nowNs - block.capturedNs <= maxAgeNs) {
            "PCM block stale: capturedNs=${block.capturedNs} nowNs=$nowNs ageNs=${nowNs - block.capturedNs} limitNs=$maxAgeNs"
        }
    }
    fun release(block: Block) = synchronized(guard) {
        check(states[block] == State.READER || states[block] == State.ENCODER)
        states[block] = if (closed) State.RETIRED else State.FREE
        if (!closed) check(free.offer(block))
    }
    fun diagnostics(): String = synchronized(guard) {
        "closed=$closed producerFinished=$producerFinished free=${states.values.count { it == State.FREE }} reader=${states.values.count { it == State.READER }} ready=${states.values.count { it == State.READY }} encoder=${states.values.count { it == State.ENCODER }} retired=${states.values.count { it == State.RETIRED }} total=$blocks/${capacityBytes}B highWater=$highWater"
    }
    fun cancel() = synchronized(guard) {
        closed = true; producerFinished = true
        while (true) { val block = ready.poll() ?: break; states[block] = State.RETIRED }
        while (true) { val block = free.poll() ?: break; states[block] = State.RETIRED }
    }
}
