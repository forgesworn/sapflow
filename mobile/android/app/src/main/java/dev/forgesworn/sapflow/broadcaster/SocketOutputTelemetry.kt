package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/** Successful complete application writes, never peer receipt or decoded proof. */
enum class SocketOutputAccounting { RTMP_APPLICATION, RTMPS_APPLICATION, SRT_DATAGRAM }
enum class SocketOutputAvailability { NOT_ATTACHED, WARMING_UP, FRESH, STALE, UNKNOWN }
enum class SocketOutputPhase { PREPARED, ACTIVE, CLOSING, CLOSED }
data class SocketOutputView(
    val availability: SocketOutputAvailability,
    val bitsPerSecond: Long?,
    val completedBytes: Long?,
    val sampleAgeMs: Long?,
    val accounting: SocketOutputAccounting?,
    val phase: SocketOutputPhase,
    val ended: Boolean,
    val incomplete: Boolean,
    val destinationId: Long?,
    val epoch: Long?,
    val attempt: Long?,
) {
    companion object {
        fun empty() = SocketOutputView(SocketOutputAvailability.NOT_ATTACHED, null, null, null, null, SocketOutputPhase.PREPARED, false, false, null, null, null)
    }
}

/** Both implementations below read only owned scalar atomics. No IO, native
 * calls, owner posts or callbacks. Allocate snapshots only on cold reads. */
internal interface SocketOutputSource { fun socketSnapshot(nowNs: Long): SocketOutputSample }
internal interface SocketOutputProvider { val socketOutput: SocketOutputSource? }
internal data class SocketOutputSample(
    val identity: Any, val sampledAtNs: Long, val completedBytes: Long?,
    val accounting: SocketOutputAccounting?, val incomplete: Boolean,
    val phase: SocketOutputPhase, val stable: Boolean = true,
    val endedAtNs: Long? = null, val context: PublishingAttemptContext? = null,
    // Historical totals can remain a lower bound while a new, complete socket
    // supplies a valid rate window. Never bridge rate deltas across owners.
    val rateIdentity: Any = identity, val rateCompletedBytes: Long? = completedBytes,
    val rateIncomplete: Boolean = incomplete,
)

/** One counter per existing socket owner. The inline write boundary adds no
 * per-write closure allocation, payload copy, FFI call, dispatch or timer. */
internal class SocketOutputCounter(private val accounting: SocketOutputAccounting) : SocketOutputSource {
    private val bytes = AtomicLong()
    private val incomplete = AtomicBoolean()
    private val sealed = AtomicBoolean()

    inline fun write(expected: Int, operation: () -> Int): Int {
        val written = try { operation() } catch (error: Exception) { ambiguous(); throw error }
        if (expected > 0 && written == expected) completed(written.toLong())
        else if (written != 0) ambiguous()
        return written // Existing socket/media policy still handles zero/partial results.
    }

    @PublishedApi internal fun ambiguous() { incomplete.set(true) }
    @PublishedApi internal fun completed(count: Long) {
        if (count <= 0 || sealed.get()) return
        while (true) {
            val old = bytes.get()
            if (old < 0) return
            val next = if (count > Long.MAX_VALUE - old) -1 else old + count
            if (bytes.compareAndSet(old, next)) return
        }
    }
    /** Only after the actual writer has finished, never on a cancellation request. */
    fun seal() { sealed.set(true) }
    override fun socketSnapshot(nowNs: Long): SocketOutputSample {
        repeat(2) {
            val before = sealed.get(); val count = bytes.get(); val lost = incomplete.get(); val after = sealed.get()
            if (before == after) return SocketOutputSample(this, nowNs, count.takeIf { it >= 0 }, accounting,
                lost || count < 0, if (after) SocketOutputPhase.CLOSED else SocketOutputPhase.ACTIVE)
        }
        return SocketOutputSample(this, nowNs, null, accounting, true, SocketOutputPhase.CLOSING, stable = false)
    }
}

/** Current/closing scalar source plus one retired sum. There is no attempt
 * history or retained media. Cold lifecycle writes publish immutable topology;
 * UI reads retry at most once if that topology changes while being observed. */
internal class LogicalSocketOutput : SocketOutputSource {
    private data class Owned(val context: PublishingAttemptContext, val source: SocketOutputSource?)
    private data class State(val retired: Long? = 0, val owned: Owned? = null,
        val last: PublishingAttemptContext? = null, val accounting: SocketOutputAccounting? = null,
        val incomplete: Boolean = false, val phase: SocketOutputPhase = SocketOutputPhase.PREPARED,
        val endedAtNs: Long? = null)
    private val topology = AtomicReference(State())
    private val owner = Any()

    fun attach(context: PublishingAttemptContext, source: SocketOutputSource?) = synchronized(owner) {
        val old = topology.get()
        topology.set(old.copy(owned = Owned(context, source), last = context,
            incomplete = old.incomplete || old.owned != null || source == null,
            phase = if (old.endedAtNs == null) SocketOutputPhase.ACTIVE else SocketOutputPhase.CLOSING))
    }
    fun closing(context: PublishingAttemptContext) = synchronized(owner) {
        val old = topology.get()
        if (old.owned?.context == context) topology.set(old.copy(phase = SocketOutputPhase.CLOSING))
    }
    fun unconfirmed(context: PublishingAttemptContext) = synchronized(owner) {
        val old = topology.get()
        if (old.owned?.context == context) topology.set(old.copy(incomplete = true, phase = SocketOutputPhase.CLOSING))
    }
    fun ended(nowNs: Long) = synchronized(owner) {
        val old = topology.get()
        if (old.endedAtNs == null) topology.set(old.copy(endedAtNs = nowNs.takeIf { it >= 0 },
            incomplete = old.incomplete || nowNs < 0, phase = if (old.owned == null) SocketOutputPhase.CLOSED else SocketOutputPhase.CLOSING))
    }
    /** Call after actual close returns, before a successor factory can start. */
    fun retired(context: PublishingAttemptContext, nowNs: Long) {
        val before = topology.get(); val owned = before.owned ?: return
        if (owned.context != context) return
        val final = read(owned.source, nowNs)
        synchronized(owner) {
            val old = topology.get()
            if (old.owned !== owned) return
            val mismatch = old.accounting != null && final?.accounting != old.accounting
            topology.set(old.copy(retired = add(old.retired, final?.completedBytes), owned = null,
                accounting = old.accounting ?: final?.accounting,
                incomplete = old.incomplete || final == null || !final.stable || final.incomplete || final.phase != SocketOutputPhase.CLOSED || mismatch,
                phase = if (old.endedAtNs == null) SocketOutputPhase.PREPARED else SocketOutputPhase.CLOSED))
        }
    }
    override fun socketSnapshot(nowNs: Long): SocketOutputSample {
        repeat(2) {
            val before = topology.get(); val owned = before.owned
            val active = owned?.let { read(it.source, nowNs) }
            if (topology.get() === before) {
                val total = if (owned == null) before.retired else add(before.retired, active?.completedBytes)
                val mismatch = active != null && before.accounting != null && active.accounting != before.accounting
                return SocketOutputSample(this, nowNs, total, before.accounting ?: active?.accounting,
                    before.incomplete || total == null || owned != null && (active == null || active.incomplete || !active.stable) || mismatch,
                    before.phase, active?.stable != false, before.endedAtNs, owned?.context ?: before.last,
                    rateIdentity = owned ?: this, rateCompletedBytes = active?.completedBytes,
                    rateIncomplete = active == null || active.incomplete || !active.stable || active.phase != SocketOutputPhase.ACTIVE || mismatch)
            }
        }
        return SocketOutputSample(this, nowNs, null, null, true, SocketOutputPhase.CLOSING, stable = false)
    }
    private fun read(source: SocketOutputSource?, nowNs: Long) = runCatching {
        source?.socketSnapshot(nowNs)?.takeIf { it.identity === source && it.sampledAtNs == nowNs }
    }.getOrNull()
    private fun add(a: Long?, b: Long?): Long? = if (a == null || b == null || a < 0 || b < 0 || b > Long.MAX_VALUE - a) null else a + b
}

/** Called by the existing display/snapshot path. At most one synchronous cold
 * read per second; no queued work or additional clock/timer. UI and controller
 * snapshot callers share this small monitor, never a codec/socket lock. */
internal class SocketOutputSampler {
    private var binding: Any? = null
    private var source: SocketOutputSource? = null
    private var attached = false
    private var lastReadNs: Long? = null
    private var baseline: SocketOutputSample? = null
    private var latest: SocketOutputSample? = null
    private var cached = SocketOutputView.empty()

    @Synchronized fun sample(nowNs: Long, binding: Any?, source: SocketOutputSource?, attached: Boolean): SocketOutputView {
        if (this.binding !== binding || this.source !== source || this.attached != attached) {
            this.binding = binding; this.source = source; this.attached = attached
            lastReadNs = null; baseline = null; latest = null; cached = SocketOutputView.empty()
        }
        if (!attached) return SocketOutputView.empty().also { cached = it }
        val previousRead = lastReadNs
        if (nowNs < 0 || previousRead != null && nowNs < previousRead) {
            baseline = null
            return view(latest, SocketOutputAvailability.UNKNOWN, null, null).also { cached = it }
        }
        if (previousRead != null && nowNs - previousRead < PERIOD_NS) return cached.copy(sampleAgeMs = latest?.let { if (nowNs >= it.sampledAtNs) (nowNs - it.sampledAtNs) / 1_000_000 else null })
        lastReadNs = nowNs
        val value = runCatching { source?.socketSnapshot(nowNs) }.getOrNull()
        latest = value
        val previous = baseline
        baseline = value?.takeIf { valid(it, nowNs) && it.endedAtNs == null }
        val availability: SocketOutputAvailability
        var rate: Long? = null
        when {
            value == null || !valid(value, nowNs) || value.endedAtNs != null -> availability = SocketOutputAvailability.UNKNOWN
            previous == null || previous.identity !== value.identity || previous.rateIdentity !== value.rateIdentity -> availability = SocketOutputAvailability.WARMING_UP
            value.sampledAtNs <= previous.sampledAtNs || value.rateCompletedBytes!! < previous.rateCompletedBytes!! -> { availability = SocketOutputAvailability.UNKNOWN; baseline = null }
            value.sampledAtNs - previous.sampledAtNs > MAX_AGE_NS -> availability = SocketOutputAvailability.STALE
            else -> {
                rate = rate(value.rateCompletedBytes!! - previous.rateCompletedBytes!!, value.sampledAtNs - previous.sampledAtNs)
                availability = if (rate == null) SocketOutputAvailability.UNKNOWN else SocketOutputAvailability.FRESH
            }
        }
        val age = value?.let { if (nowNs >= it.sampledAtNs) (nowNs - it.sampledAtNs) / 1_000_000 else null }
        return view(value, availability, rate, age).also { cached = it }
    }
    @Synchronized fun destinationEnd(): Long? = latest?.endedAtNs
    private fun valid(value: SocketOutputSample, now: Long): Boolean = value.stable && !value.rateIncomplete && value.phase == SocketOutputPhase.ACTIVE &&
        value.accounting != null && value.rateCompletedBytes != null && value.rateCompletedBytes >= 0 &&
        value.sampledAtNs >= 0 && value.sampledAtNs <= now && now - value.sampledAtNs <= MAX_AGE_NS
    private fun rate(bytes: Long, durationNs: Long): Long? = if (durationNs <= 0 || bytes < 0 || bytes > Long.MAX_VALUE / 8_000_000_000L) null else bytes * 8_000_000_000L / durationNs
    private fun view(value: SocketOutputSample?, state: SocketOutputAvailability, rate: Long?, age: Long?) = SocketOutputView(state, rate,
        value?.completedBytes, age, value?.accounting, value?.phase ?: SocketOutputPhase.PREPARED, value?.endedAtNs != null, value?.incomplete ?: true,
        value?.context?.destination, value?.context?.epoch, value?.context?.attempt)
    companion object { const val PERIOD_NS = 1_000_000_000L; const val MAX_AGE_NS = 3_000_000_000L }
}
