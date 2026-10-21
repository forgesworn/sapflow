package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean

/** Publish the first failure before any socket/native teardown can wait. The
 * coordinator still requires actual old-owner cleanup before replacement. */
internal fun failAttemptOnce(closed: AtomicBoolean, notify: () -> Unit, cleanup: () -> Unit) {
    if (!closed.compareAndSet(false, true)) return
    try { notify() } finally { cleanup() }
}

/** Serial control-plane policy. No samples, sockets, timers or Android runtime. */
internal sealed interface ReconnectAction {
    data object Connect : ReconnectAction
    data class At(val ns: Long) : ReconnectAction
    data object None : ReconnectAction
    data object Expired : ReconnectAction
}
internal interface ReconnectPolicyPort {
    fun start(nowNs: Long): ReconnectAction
    fun failed(nowNs: Long): ReconnectAction
    fun intentionalReplace(nowNs: Long): ReconnectAction
    fun connected(nowNs: Long)
    fun confirmed(nowNs: Long)
    fun tick(nowNs: Long): ReconnectAction
    val deadlineNs: Long?
    fun stop()
}
internal interface PublishingAttempt : AutoCloseable { fun start() }
internal sealed interface PublishingAttemptEvent {
    data object Accepted : PublishingAttemptEvent
    data class Failed(val retryable: Boolean = true) : PublishingAttemptEvent
}

/** The logical destination survives media-epoch rotation. An attempt token
 * fences delayed callbacks independently of the native session generation. */
internal class ReconnectController(
    private val policy: ReconnectPolicyPort,
    private val now: () -> Long,
    private val permitted: () -> Boolean,
    private val factory: (Long, Long) -> PublishingAttempt,
    private val ready: (Long, Long) -> Unit,
) {
    private var epoch: Long? = null
    private var started = false
    private var stopped = false
    private var policyStopped = false
    private var pendingConnect = false
    private var active: PublishingAttempt? = null
    private var accepted = false
    var token = 0L; private set
    var nextWakeNs: Long? = null; private set
    var status = "Destination prepared · press Start"; private set
    val terminal: Boolean get() = stopped

    fun attach(value: Long) {
        check(!stopped && permitted()) { "Destination stopped" }
        check(epoch == null || epoch == value) { "Previous media epoch still owns transport" }
        epoch = value
        if (!started) { started = true; apply(policy.start(now())) }
        else if (policy.deadlineNs?.let { now() >= it } == true) tick()
        else if (pendingConnect) connect()
        else if (nextWakeNs?.let { now() >= it } == true) tick()
    }
    fun detach(value: Long) {
        if (epoch != value) return
        epoch = null
        if (active != null) {
            closeActive()
            if (!stopped) apply(policy.intentionalReplace(now()))
        }
    }
    fun event(value: Long, event: PublishingAttemptEvent) {
        if (stopped || !permitted() || active == null || value != token) return
        when (event) {
            PublishingAttemptEvent.Accepted -> if (!accepted) {
                accepted = true
                policy.connected(now())
                nextWakeNs = policy.deadlineNs
                status = "Origin transport accepted · media delivery unconfirmed"
                epoch?.let { ready(it, token) }
            }
            is PublishingAttemptEvent.Failed -> {
                val next = if (event.retryable) policy.failed(now()) else ReconnectAction.None
                closeActive()
                if (event.retryable) apply(next) else stop()
            }
        }
    }
    /** Trusted origin observation only. Socket/ACK/encoder callbacks cannot
     * enter this seam. Both media epoch and attempt must still be current. */
    fun observedMedia(value: Long, attempt: Long) {
        if (!stopped && permitted() && epoch == value && token == attempt && active != null && accepted) {
            policy.confirmed(now())
            nextWakeNs = policy.deadlineNs
            status = "Origin confirmed playable media"
        }
    }
    fun tick() { if (!stopped && permitted()) apply(policy.tick(now())) }
    private fun apply(action: ReconnectAction) {
        when (action) {
            ReconnectAction.Connect -> { nextWakeNs = policy.deadlineNs; pendingConnect = true; connect() }
            is ReconnectAction.At -> { nextWakeNs = action.ns; status = "Reconnecting · media delivery unconfirmed" }
            ReconnectAction.Expired -> { stop(); status = "Reconnect grace expired · capture/recording can continue" }
            ReconnectAction.None -> nextWakeNs = listOfNotNull(nextWakeNs?.takeIf { it > now() }, policy.deadlineNs).minOrNull()
        }
    }
    private fun connect() {
        if (stopped || !permitted() || epoch == null || !pendingConnect) return
        if (policy.deadlineNs?.let { now() >= it } == true) { apply(policy.tick(now())); return }
        check(active == null) { "Old attempt has not released ownership" }
        pendingConnect = false; accepted = false; token = Math.addExact(token, 1)
        status = "Connecting attempt $token · media delivery unconfirmed"
        try {
            active = factory(checkNotNull(epoch), token)
            active!!.start()
        } catch (_: Exception) {
            val next = policy.failed(now())
            closeActive()
            if (!stopped) apply(next)
        }
    }
    private fun closeActive() {
        val previous = active ?: return
        try { previous.close(); active = null; accepted = false }
        catch (error: Exception) {
            stopped = true; nextWakeNs = null; pendingConnect = false
            status = "Publisher cleanup incomplete · automatic retry cancelled"
            stopPolicy()
            throw error // Retain the owner and prohibit a replacement.
        }
    }
    private fun stopPolicy() {
        if (!policyStopped) { policyStopped = true; policy.stop() }
    }
    fun stop() {
        stopped = true; nextWakeNs = null; pendingConnect = false
        try { closeActive() } finally { stopPolicy() }
        status = "Publisher stopped"
    }
}
