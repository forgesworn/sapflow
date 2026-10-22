package dev.forgesworn.sapflow.broadcaster

internal enum class CameraSourceChoice(val label: String) { REAR("Rear"), FRONT("Front") }

/** A source transaction is smaller than a codec epoch. Only the actual old
 * device closure permits texture replacement, then native open, then a valid
 * frame. No gesture renews the original deadline. */
internal class CameraSourcePolicy(private val budgetNs: Long = 6_000_000_000L, initialSerial: Long = 0) {
    enum class Phase { RETIRING, TEXTURE, OPENING, WAITING_FRAME, ACTIVE, CLOSED }
    class Operation internal constructor(val serial: Long, val choice: CameraSourceChoice, val startedNs: Long, val deadlineNs: Long)
    private var serial = initialSerial
    private var operation: Operation? = null
    private var closed = false
    private var phase = Phase.CLOSED
    init { require(budgetNs > 0 && initialSerial >= 0) }
    @Synchronized fun begin(choice: CameraSourceChoice, nowNs: Long): Operation? {
        if (closed || nowNs < 0 || serial == Long.MAX_VALUE || operation != null && phase != Phase.ACTIVE) return null
        val deadline = try { Math.addExact(nowNs, budgetNs) } catch (_: ArithmeticException) { return null }
        return Operation(++serial, choice, nowNs, deadline).also { operation = it; phase = Phase.RETIRING }
    }
    @Synchronized fun owns(value: Operation): Boolean = !closed && operation === value
    @Synchronized fun permits(value: Operation, nowNs: Long): Boolean = owns(value) && nowNs >= value.startedNs && (phase == Phase.ACTIVE || nowNs < value.deadlineNs)
    @Synchronized fun cameraClosed(value: Operation, nowNs: Long): Boolean = advance(value, nowNs, Phase.RETIRING, Phase.TEXTURE)
    @Synchronized fun textureReady(value: Operation, nowNs: Long): Boolean = advance(value, nowNs, Phase.TEXTURE, Phase.OPENING)
    @Synchronized fun configured(value: Operation, nowNs: Long): Boolean = advance(value, nowNs, Phase.OPENING, Phase.WAITING_FRAME)
    @Synchronized fun frame(value: Operation, nowNs: Long): Boolean {
        if (!permits(value, nowNs) || phase !in listOf(Phase.WAITING_FRAME, Phase.ACTIVE)) return false
        phase = Phase.ACTIVE; return true
    }
    @Synchronized fun active(value: Operation): Boolean = owns(value) && phase == Phase.ACTIVE
    @Synchronized fun acceptsControl(sourceToken: Long): Boolean = !closed && phase == Phase.ACTIVE && operation?.serial == sourceToken
    @Synchronized fun pending(): Boolean = !closed && operation != null && phase != Phase.ACTIVE
    @Synchronized fun close() { closed = true; phase = Phase.CLOSED }
    private fun advance(value: Operation, nowNs: Long, from: Phase, to: Phase): Boolean {
        if (!permits(value, nowNs) || phase != from) return false
        phase = to; return true
    }
}

internal data class CameraControlSettings(val zoom: Float = 1f, val focusLocked: Boolean = false,
    val exposure: Int = 0, val exposureLocked: Boolean = false, val whiteBalance: Int = 1, val whiteBalanceLocked: Boolean = false)

internal class CameraControlCapabilities(val minZoom: Float, val maxZoom: Float, val focusContinuous: Boolean,
    val focusLock: Boolean, val minExposure: Int, val maxExposure: Int, val exposureStep: Float,
    val exposureLock: Boolean, val whiteBalanceModes: List<Int>, val whiteBalanceLock: Boolean) {
    fun accepts(value: CameraControlSettings): Boolean = value.zoom.isFinite() && value.zoom in minZoom..maxZoom &&
        (!value.focusLocked || focusLock) && value.exposure in minExposure..maxExposure &&
        (!value.exposureLocked || exposureLock) && value.whiteBalance in whiteBalanceModes &&
        (!value.whiteBalanceLocked || whiteBalanceLock && value.whiteBalance == 1)
}

/** One native request plus one latest slider value; no Handler queue per
 * gesture and no settings copied/allocated by frame callbacks. */
internal class CameraControlMailbox {
    private var closed = false
    private var queued = false
    private var latest: CameraControlSettings? = null
    @Synchronized fun offer(value: CameraControlSettings): Boolean {
        if (closed) return false
        latest = value
        if (queued) return false
        queued = true; return true
    }
    @Synchronized fun take(): CameraControlSettings? { if (closed) return null; return latest.also { latest = null; queued = false } }
    @Synchronized fun close() { closed = true; latest = null }
}

internal class CameraSourceSnapshot(val requested: CameraSourceChoice? = null, val active: CameraSourceChoice? = null,
    val sourceToken: Long = 0, val busy: Boolean = false, val status: String = "Camera not prepared",
    val capabilities: CameraControlCapabilities? = null, val requestedSettings: CameraControlSettings? = null,
    val requestedRevision: Long = 0, val confirmedRevision: Long = 0, val nativeZoom: Float? = null,
    val nativeExposure: Int? = null, val nativeWhiteBalance: Int? = null, val focusState: Int? = null, val exposureState: Int? = null, val whiteBalanceState: Int? = null, val controlsPending: Boolean = false)

/** Actual device close is a receipt, never inferred from close() returning.
 * A late onOpened after Stop must itself close the newly delivered device. */
internal class CameraDeviceLifecycle {
    enum class Action { ACCEPT, CLOSE_DEVICE, WAIT, COMPLETE }
    var closing = false; private set
    var closed = false; private set
    private var opening = false
    private var hardwareCloseIssued = false
    fun beginOpen() { check(!opening && !closing && !closed); opening = true }
    fun opened(): Action {
        opening = false
        if (!closing && !closed) return Action.ACCEPT
        hardwareCloseIssued = true; return Action.CLOSE_DEVICE
    }
    fun requestClose(hasDevice: Boolean): Action {
        if (closed) return Action.COMPLETE
        closing = true
        if (hasDevice) {
            if (hardwareCloseIssued) return Action.WAIT
            hardwareCloseIssued = true; return Action.CLOSE_DEVICE
        }
        if (opening) return Action.WAIT
        closed = true; return Action.COMPLETE // No native open was ever admitted.
    }
    fun failedOpenWithoutDevice() { opening = false; closing = true; closed = true }
    fun nativeClosed() { opening = false; closing = true; closed = true }
}

/** Reserve before Handler enqueue; repeated gestures cannot queue more native
 * transactions while metadata lookup or actual device cleanup is delayed. */
internal class CameraRequestSlot {
    class Ticket internal constructor()
    private var current: Ticket? = null
    private var closed = false
    @Synchronized fun reserve(): Ticket? {
        if (closed || current != null) return null
        return Ticket().also { current = it }
    }
    @Synchronized fun busy(): Boolean = !closed && current != null
    @Synchronized fun owns(value: Ticket): Boolean = !closed && current === value
    @Synchronized fun release(value: Ticket) { if (current === value) current = null }
    @Synchronized fun close() { closed = true }
}

/** One immutable codec-run owner shared by GL, PCM workers and fanout.
 * Failure is only a scalar CAS. No callback/teardown/IO happens here. Source
 * confirmation and deadline expiry contend on the same state; ordinary Stop
 * cancels the source budget without converting accepted tail into a fault. */
internal class CameraRunAdmission(val epoch: Long, private val beforeExpiry: (() -> Unit)? = null) {
    enum class Fault { CAMERA_SOURCE, SOURCE_DEADLINE, TIMING }
    enum class SourceAdmission {
        ADMITTED, CANCELLED, FAULTED, BUSY;
        val admitted: Boolean get() = this == ADMITTED
        val requiresFailure: Boolean get() = this == FAULTED || this == BUSY
    }
    private sealed interface State {
        class Live(val pending: CameraSourcePolicy.Operation? = null, val token: Long? = null, val confirmedNs: Long? = null) : State
        class Failed(val fault: Fault) : State
        data object Cancelled : State
    }
    private val state = java.util.concurrent.atomic.AtomicReference<State>(State.Live())
    val failure: Fault? get() = (state.get() as? State.Failed)?.fault
    val recordingCanFinalise: Boolean get() = failure == null
    fun fail(reason: Fault = Fault.CAMERA_SOURCE) {
        while (true) { val old = state.get(); if (old is State.Failed || state.compareAndSet(old, State.Failed(reason))) return }
    }
    fun beginSource(operation: CameraSourcePolicy.Operation): SourceAdmission {
        while (true) {
            val value = state.get()
            if (value === State.Cancelled) return SourceAdmission.CANCELLED
            val old = value as? State.Live ?: return SourceAdmission.FAULTED
            if (old.pending != null) return SourceAdmission.BUSY
            if (state.compareAndSet(old, State.Live(operation))) return SourceAdmission.ADMITTED
        }
    }
    fun confirmSource(token: Long, nowNs: Long): Boolean {
        while (true) {
            val old = state.get() as? State.Live ?: return false
            val pending = old.pending ?: return old.token == token
            if (pending.serial != token) return false
            if (nowNs < pending.startedNs || nowNs >= pending.deadlineNs) {
                val reason = if (nowNs < pending.startedNs) Fault.TIMING else Fault.SOURCE_DEADLINE
                if (state.compareAndSet(old, State.Failed(reason))) return false
            } else if (state.compareAndSet(old, State.Live(token = token, confirmedNs = nowNs))) return true
        }
    }
    fun confirmed(token: Long): Long? = (state.get() as? State.Live)?.let { if (it.token == token) it.confirmedNs else null }
    fun admit(nowNs: Long): Boolean {
        while (true) {
            val value = state.get()
            if (value === State.Cancelled) return true // Accepted tail still drains after ordinary Stop.
            val old = value as? State.Live ?: return false
            val pending = old.pending ?: return true
            if (nowNs >= pending.startedNs && nowNs < pending.deadlineNs) return true
            beforeExpiry?.invoke() // Optional deterministic cold expiry-race scheduling port.
            val reason = if (nowNs < pending.startedNs) Fault.TIMING else Fault.SOURCE_DEADLINE
            if (state.compareAndSet(old, State.Failed(reason))) return false
        }
    }
    fun cancelSourceBudget() {
        while (true) { val old = state.get() as? State.Live ?: return; if (state.compareAndSet(old, State.Cancelled)) return }
    }
}

/** One run-wide source timeline. Only a successful encoder swap advances the
 * video-submission highwater. Preview does not hide a real publishing gap. */
internal class CameraSourceTimeline(private val maxSourceGapNs: Long = 2_000_000_000L) {
    private var lastMappedNs = -1L
    private var lastSubmittedNs = -1L
    private var failed = false
    fun frame(ptsNs: Long, firstSourceFrame: Boolean, encoding: Boolean): Boolean {
        if (failed) return false
        if (ptsNs < 0 || ptsNs <= lastMappedNs || firstSourceFrame && encoding && lastSubmittedNs >= 0 && ptsNs - lastSubmittedNs >= maxSourceGapNs) {
            failed = true; return false
        }
        lastMappedNs = ptsNs; return true
    }
    fun submitted(ptsNs: Long) { check(!failed && ptsNs == lastMappedNs && ptsNs > lastSubmittedNs); lastSubmittedNs = ptsNs }
}

/** Physical observations are minted only by the real input path. Deferred
 * replay consumes the existing latest observation, never overwrites that
 * shadow or mints a newer serial for an older pose. */
internal class CameraPoseInput<Owner> {
    class Observation<Owner> internal constructor(val owner: Owner?, val pose: CardinalPose)
    private var current: Observation<Owner>? = null
    @Synchronized fun record(owner: Owner?, pose: CardinalPose): Observation<Owner> = Observation(owner, pose).also { current = it }
    @Synchronized fun latest(owner: Owner): Observation<Owner>? = current?.takeIf { it.owner === owner }
    @Synchronized fun isCurrent(value: Observation<Owner>): Boolean = current === value
}

/** Native results are requested only for cold control changes/explicit status
 * refresh. One in-flight native request plus one latest settings cell. */
internal class CameraNativeControlQueue {
    class Request internal constructor(val serial: Long, val settings: CameraControlSettings)
    private var serial = 0L
    private var active: Request? = null
    private var latest: CameraControlSettings? = null
    private var closed = false
    fun offer(settings: CameraControlSettings): Boolean { if (closed || serial == Long.MAX_VALUE) return false; latest = settings; return true }
    fun take(): Request? {
        if (closed || active != null) return null
        val settings = latest ?: return null
        latest = null
        return Request(++serial, settings).also { active = it }
    }
    fun matches(value: Long): Boolean = !closed && active?.serial == value
    fun complete(value: Long): Boolean { if (!matches(value)) return false; active = null; return true }
    fun pending(): Boolean = active != null || latest != null
    fun close() { closed = true; latest = null }
}

internal enum class CameraFocusTransition { IDLE, START, CANCEL;
    companion object {
        fun between(previous: CameraControlSettings, next: CameraControlSettings): CameraFocusTransition =
            if (previous.focusLocked == next.focusLocked) IDLE else if (next.focusLocked) START else CANCEL
    }
}
