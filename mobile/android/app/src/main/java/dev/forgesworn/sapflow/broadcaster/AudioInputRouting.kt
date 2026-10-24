package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioRouting
import android.os.Handler
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/** Shared by codec epochs of one Preparation; only a new Prepare can repin Default. */
internal class AudioInputPin(requestedId: String?) {
    private var id = requestedId
    @Synchronized fun resolvedId(): String? = id
    @Synchronized fun accept(actual: String): Boolean {
        if (id != null && id != actual) return false
        id = actual; return true
    }
}
internal data class AudioInputOwner(val preparation: CaptureStartAdmission.Preparation, val epoch: Long, val sourceSerial: Long = 0)

/** Native getters only at startup and routing events; per-block work is a scalar gate. */
internal class AudioInputRouting(
    val owner: AudioInputOwner, private val pin: AudioInputPin,
    available: List<AudioInputOption>, private val read: () -> String?,
    private val failed: () -> Unit, private val changed: (String) -> Unit, private val beforeRead: () -> Unit = {}, private val afterWait: () -> Unit = {}, private val now: () -> Long = System::nanoTime,
) {
    private val policy = requireNotNull(AudioInputPolicy.create(owner, pin.resolvedId(), available)) { "Selected microphone is unavailable; choose an input before Prepare" }
    private val state = ReentrantLock()
    private val settled = state.newCondition()
    private var activeWaiters = 0
    private var oldestWaitStart: Long? = null
    private var settledAt: Long? = null
    private var checking = false
    private var closed = false
    private var fault = false
    private var failureText = "Microphone route changed, disconnected or is unsupported"
    private var verification = 0L
    private var verified = false
    private var armed = false
    private var activating = false
    private var pendingRoute = false
    private var detach: (() -> Unit)? = null
    private data class Verification(val revision: Long, val initial: Boolean)
    private fun reserveLocked(): Verification? {
        if (closed || fault || !armed && activating) return null
        val initial = !armed
        if (initial) activating = true
        if (verification == Long.MAX_VALUE) { failLocked(); return null }
        verification++
        checking = true
        return Verification(verification, initial)
    }
    fun verify(): Boolean {
        var notifyFailure = false
        val reservation = state.withLock {
            val wasFailed = fault
            reserveLocked().also { notifyFailure = fault && !wasFailed }
        }
        if (notifyFailure) failed()
        return reservation?.let(::perform) ?: false
    }
    private fun perform(reservation: Verification): Boolean {
        val revision = reservation.revision
        val initial = reservation.initial
        for (pass in 0..1) {
            beforeRead() // Cold dependency handoff, outside the state lock.
            val id = try { read() } catch (_: Exception) { null } // Outside the scalar/media lock.
            var notifyFailure = false
            var notifyChange = false
            var recheck = false
            val accepted = state.withLock {
                if (closed || fault || revision != verification) return false
                if (initial && pendingRoute) {
                    pendingRoute = false
                    if (pass == 0) recheck = true
                    else { failLocked(); notifyFailure = true }
                    false
                } else {
                    val settlement = now()
                    val elapsed = oldestWaitStart?.let { settlement - it }
                    if (elapsed != null && (elapsed < 0 || elapsed >= 250_000_000L)) {
                        failLocked("Microphone route verification exceeded 250 ms"); notifyFailure = true; false
                    } else if (id == null || policy.observe(owner, id) != AudioInputPolicy.Observation.ACCEPTED || !pin.accept(id)) {
                        failLocked(); notifyFailure = true; false
                    } else {
                        notifyChange = true; verified = true; armed = true; activating = false; checking = false
                        settledAt = settlement
                        settled.signalAll(); true
                    }
                }
            }
            if (notifyFailure) failed()
            if (notifyChange) changed(requireNotNull(id))
            if (!recheck) return accepted
        }
        return false
    }
    fun removed(ids: List<String>) {
        val notify = state.withLock {
            if (closed || fault) return
            val chosen = pin.resolvedId() ?: return
            if (chosen in ids) { failLocked(); true } else false
        }
        if (notify) failed()
    }
    fun routeChanged() {
        var notifyFailure = false
        val reservation = state.withLock {
            if (closed || fault) return
            if (!armed) { pendingRoute = true; return }
            // Close PCM admission before this first event-lock release.
            reserveLocked().also { notifyFailure = fault }
        }
        if (notifyFailure) failed()
        reservation?.let(::perform)
    }
    private fun failLocked(reason: String? = null) { if (reason != null) failureText = reason; fault = true; checking = false; policy.close(owner); settled.signalAll() }
    val failureDescription: String get() = state.withLock { failureText }
    fun hasFailed(): Boolean = state.withLock { fault }
    fun permitsPcm(): Boolean = state.withLock { !checking && !closed && !fault && verified && policy.permitsMedia(owner) }
    private fun awaitChecked(allowClosed: Boolean): Boolean {
        var timeoutFailure = false
        val allowed = state.withLock {
            if (!checking) return verified && !fault && (allowClosed || !closed) && (closed || policy.permitsMedia(owner))
            if (activeWaiters >= 2) {
                failLocked("Microphone route wait capacity exceeded"); timeoutFailure = true; false
            } else {
                activeWaiters++
                try {
                    val started = now()
                    if (oldestWaitStart == null) oldestWaitStart = started
                    while (checking && !closed && !fault) {
                        val elapsed = now() - started
                        if (elapsed < 0 || elapsed >= 250_000_000L) {
                            failLocked("Microphone route verification exceeded 250 ms"); timeoutFailure = true; break
                        }
                        settled.awaitNanos(250_000_000L - elapsed)
                        state.unlock()
                        try { afterWait() } finally { state.lock() }
                    }
                    val elapsedAtSettlement = settledAt?.let { it - started }
                    if (!closed && !fault && elapsedAtSettlement != null && (elapsedAtSettlement < 0 || elapsedAtSettlement >= 250_000_000L)) {
                        failLocked("Microphone route verification exceeded 250 ms"); timeoutFailure = true
                    }
                    verified && !fault && (allowClosed || !closed) && (closed || policy.permitsMedia(owner))
                } finally {
                    activeWaiters--
                    if (activeWaiters == 0) oldestWaitStart = null
                }
            }
        }
        if (timeoutFailure) failed()
        return allowed
    }
    fun awaitQueuedPcm(): Boolean = awaitChecked(allowClosed = true)
    /** Borrowed block only: a route wait never extends its existing 200 ms age. */
    fun awaitQueuedPcm(pool: PcmBlockPool, block: PcmBlockPool.Block): Boolean {
        if (!awaitQueuedPcm()) return false
        pool.checkAge(block, now())
        return true
    }
    /** Publish normal producer Stop before closing its route. A reader that was
     * paused before the scalar check must finish cleanly, preserving queued tail. */
    fun stopProducer(running: AtomicBoolean): Boolean = seal(running)
    fun admitReader(running: AtomicBoolean): Boolean {
        if (!awaitChecked(allowClosed = false)) {
            check(!running.get() && !hasFailed()) { "Microphone route is no longer admitted" }
            return false
        }
        return running.get()
    }
    fun close() { seal(null) }
    private fun seal(running: AtomicBoolean?): Boolean {
        val release: (() -> Unit)?
        val latchedFault: Boolean
        state.withLock {
            // Fault observation and normal Stop have one linearisation point.
            running?.set(false); closed = true; checking = false; policy.close(owner); settled.signalAll()
            latchedFault = fault; release = detach; detach = null
        }
        release?.invoke() // Native listener removal never holds the scalar lock.
        return latchedFault
    }
    companion object {
        private fun devices(context: Context): List<AudioDeviceInfo> =
            (context.getSystemService(Context.AUDIO_SERVICE) as AudioManager).getDevices(AudioManager.GET_DEVICES_INPUTS).asSequence()
                .take(64).filter { it.isSource && kind(it.type) != null }.take(32).toList()
        private fun kind(type: Int): AudioInputOption.Kind? = when (type) {
            AudioDeviceInfo.TYPE_BUILTIN_MIC -> AudioInputOption.Kind.BUILT_IN
            AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_LINE_ANALOG, AudioDeviceInfo.TYPE_LINE_DIGITAL -> AudioInputOption.Kind.WIRED
            AudioDeviceInfo.TYPE_USB_DEVICE, AudioDeviceInfo.TYPE_USB_ACCESSORY, AudioDeviceInfo.TYPE_USB_HEADSET -> AudioInputOption.Kind.USB
            else -> null
        }
        fun inventory(context: Context): List<AudioInputOption> = AudioInputOption.inventory(devices(context).mapNotNull {
            AudioInputOption.create(it.id.toString(), it.productName.toString(), requireNotNull(kind(it.type)))
        })
        fun attach(context: Context, record: AudioRecord, handler: Handler, owner: AudioInputOwner, pin: AudioInputPin,
                   failed: () -> Unit, changed: (String) -> Unit): AudioInputRouting {
            val available = inventory(context)
            val requested = pin.resolvedId()
            val preferred = requested?.let { id -> devices(context).firstOrNull { it.id.toString() == id } }
            require(requested == null || preferred != null) { "Selected microphone disconnected before Prepare" }
            require(record.setPreferredDevice(preferred)) { "Microphone preference was refused" }
            val route = AudioInputRouting(owner, pin, available, { record.routedDevice?.id?.toString() }, failed, changed)
            val listener = AudioRouting.OnRoutingChangedListener { routing ->
                if (routing === record) route.routeChanged()
            }
            val manager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
            val devices = object : AudioDeviceCallback() {
                override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>) {
                    // Match the chosen device directly; do not truncate the removed list
                    // before finding it or infer identity from a product name.
                    val chosen = pin.resolvedId() ?: return
                    if (removedDevices.any { it.id == chosen.toIntOrNull() }) route.removed(listOf(chosen))
                }
            }
            record.addOnRoutingChangedListener(listener, handler)
            try { manager.registerAudioDeviceCallback(devices, handler) }
            catch (error: Exception) { record.removeOnRoutingChangedListener(listener); throw error }
            route.detach = { record.removeOnRoutingChangedListener(listener); manager.unregisterAudioDeviceCallback(devices) }
            return route
        }
    }
}
