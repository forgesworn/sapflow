package dev.forgesworn.sapflow.broadcaster

/** One control owner and timer drive both selectors. The physical selector
 * keeps settling while qualification suppresses its output, so clearing an
 * override never relies on Android delivering another unchanged angle. */
internal class PoseInputPolicy(initial: CardinalPose, qualificationEnabled: Boolean = false) {
    enum class Result { DISABLED, INACTIVE, INVALID, LIMIT, ACCEPTED, CLEARED }
    private val physical = CardinalPoseSelector(initial)
    private var forced: CardinalPoseSelector? = null
    private var enabled = qualificationEnabled
    private var requests = 0
    var captureStopped = false
        private set
    var overridePose: CardinalPose? = null
        private set
    val pose: CardinalPose get() = forced?.pose ?: physical.pose
    val deadlineNs: Long? get() = listOfNotNull(physical.deadlineNs, forced?.deadlineNs).minOrNull()

    fun enableQualification(enabled: Boolean) {
        this.enabled = enabled
        if (!enabled) reset()
    }

    fun acceptPhysical(angle: Int, nowNs: Long): CardinalPose {
        physical.accept(angle, nowNs)
        return pose
    }

    fun request(value: String?, capturing: Boolean, nowNs: Long): Result {
        if (!enabled) return Result.DISABLED
        if (value == "CLEAR") {
            clearOverride()
            return Result.CLEARED
        }
        val requested = CardinalPose.entries.firstOrNull { it.name == value } ?: return Result.INVALID
        // The local Stop fence wins even before the asynchronous controller's
        // snapshot has changed from CAPTURING to STOPPING.
        if (!capturing || captureStopped) return Result.INACTIVE
        if (overridePose == requested) {
            forced?.let { if (it.pose != requested && it.deadlineNs == null) it.accept(requested.clockwiseDegrees, nowNs) }
            return Result.ACCEPTED
        }
        if (requests >= 16) return Result.LIMIT
        requests++
        forced = CardinalPoseSelector(pose).also { it.accept(requested.clockwiseDegrees, nowNs) }
        overridePose = requested
        return Result.ACCEPTED
    }

    fun settle(nowNs: Long, capturing: Boolean): CardinalPose {
        physical.settle(nowNs)
        forced?.let {
            if (capturing && !captureStopped) it.settle(nowNs) else it.cancelPending()
        }
        return pose
    }

    fun cancelPending() { physical.cancelPending(); forced?.cancelPending() }

    /** UI Stop cancels only the override. Physical settling and the foreground
     * request budget survive; no native pose notification is admitted until
     * explicit manual Prepare synchronises the last stable physical pose. */
    fun stopCapture() {
        captureStopped = true
        clearOverride()
    }

    fun prepareSession(): CardinalPose {
        clearOverride()
        captureStopped = false
        return pose
    }

    private fun clearOverride() { forced = null; overridePose = null }

    /** Foreground disposal disables the route and cancels its timer inputs. */
    fun reset() {
        enabled = false
        clearOverride()
        requests = 0
        captureStopped = true
        physical.cancelPending()
    }
}
