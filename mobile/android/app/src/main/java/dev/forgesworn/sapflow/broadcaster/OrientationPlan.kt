package dev.forgesworn.sapflow.broadcaster

/** Physical clockwise turns from the device's natural position, independent
 * of Activity/display rotation and the user's screen-rotation lock. */
internal enum class CardinalPose(val clockwiseDegrees: Int) {
    NATURAL(0), CLOCKWISE_90(90), REVERSE(180), CLOCKWISE_270(270);

    companion object {
        fun fromDegrees(degrees: Int): CardinalPose = entries.first { it.clockwiseDegrees == degrees }
    }
}

/** Single control-owner selector. A cardinal candidate must stay within its
 * 30-degree admission region for 250ms. Motion/flat samples cancel a candidate
 * without changing the stable pose or starting a camera/transport restart. */
internal class CardinalPoseSelector(initial: CardinalPose = CardinalPose.NATURAL, private val stableNs: Long = 250_000_000L) {
    var pose = initial
        private set
    private var candidate: CardinalPose? = null
    private var candidateSinceNs = 0L
    private var lastNowNs = 0L

    init { require(stableNs > 0) }

    val deadlineNs: Long? get() = candidate?.let { Math.addExact(candidateSinceNs, stableNs) }

    fun cancelPending() { candidate = null }

    /** A control-owner timer drives this even if Android suppresses unchanged
     * integer-angle callbacks. No sensor callback is required at the deadline. */
    fun settle(nowNs: Long): CardinalPose {
        require(nowNs >= lastNowNs) { "Pose clock regressed" }
        lastNowNs = nowNs
        if (candidate != null && nowNs - candidateSinceNs >= stableNs) {
            pose = candidate!!; candidate = null
        }
        return pose
    }

    fun accept(angleDegrees: Int, nowNs: Long): CardinalPose {
        require(nowNs >= lastNowNs) { "Pose clock regressed" }
        lastNowNs = nowNs
        if (angleDegrees == -1) { candidate = null; return pose }
        require(angleDegrees in 0..359) { "Invalid device orientation angle" }
        val nearest = ((angleDegrees + 45) / 90 * 90) % 360
        val distance = minOf((angleDegrees - nearest + 360) % 360, (nearest - angleDegrees + 360) % 360)
        val next = CardinalPose.fromDegrees(nearest)
        if (distance > 30 || next == pose) candidate = null
        else if (candidate != next) { candidate = next; candidateSinceNs = nowNs }
        return settle(nowNs)
    }
}

/** One immutable encoder/transport epoch. Rotation changes actual pixels,
 * never MP4 display metadata. Rear-camera output is deliberately unmirrored. */
internal class OrientationPlan private constructor(
    val pose: CardinalPose,
    val naturalPortrait: Boolean,
    val sensorClockwiseDegrees: Int,
) {
    val naturalWidth = if (naturalPortrait) 720 else 1280
    val naturalHeight = if (naturalPortrait) 1280 else 720
    val width = if (pose.clockwiseDegrees % 180 == 0) naturalWidth else naturalHeight
    val height = if (pose.clockwiseDegrees % 180 == 0) naturalHeight else naturalWidth
    val isPortrait: Boolean get() = height > width
    // Camera2 stream dimensions remain in SENSOR buffer space. SurfaceTexture's
    // producer transform already rotates that space to device-natural space.
    val cameraWidth = if (sensorClockwiseDegrees % 180 == 0) naturalWidth else naturalHeight
    val cameraHeight = if (sensorClockwiseDegrees % 180 == 0) naturalHeight else naturalWidth
    // CaptureRequest.JPEG_ORIENTATION's rear-camera convention is sensor +
    // OrientationEventListener angle. The producer already supplies sensor.
    val rawToEncodedClockwiseDegrees = (sensorClockwiseDegrees + pose.clockwiseDegrees) % 360
    val encoderClockwiseDegrees = pose.clockwiseDegrees

    /** The sampled texture is already device-natural, not raw sensor pixels. */
    fun encoderPositionMatrix(): FloatArray = clockwisePositionMatrix(encoderClockwiseDegrees)

    /** Preview is aligned to the display independently of the encoder epoch. */
    fun previewPositionMatrix(displayRotationDegrees: Int): FloatArray {
        require(displayRotationDegrees in listOf(0, 90, 180, 270))
        return clockwisePositionMatrix((360 - displayRotationDegrees) % 360)
    }

    /** Relative dimensions of the encoded canvas when fitted into preview. */
    fun previewRelativeClockwiseDegrees(displayRotationDegrees: Int): Int {
        require(displayRotationDegrees in listOf(0, 90, 180, 270))
        return (720 - pose.clockwiseDegrees - displayRotationDegrees) % 360
    }

    companion object {
        private fun clockwisePositionMatrix(degrees: Int): FloatArray {
            require(degrees in listOf(0, 90, 180, 270))
            val (cosine, sine) = when (degrees) {
                0 -> 1f to 0f
                90 -> 0f to 1f
                180 -> -1f to 0f
                else -> 0f to -1f
            }
            return floatArrayOf(
                cosine, -sine, 0f, 0f,
                sine, cosine, 0f, 0f,
                0f, 0f, 1f, 0f,
                0f, 0f, 0f, 1f,
            )
        }

        fun forRear(sensorOrientationDegrees: Int, pose: CardinalPose, naturalPortrait: Boolean = true): OrientationPlan {
            require(sensorOrientationDegrees in listOf(0, 90, 180, 270)) { "Invalid camera sensor orientation" }
            return OrientationPlan(pose, naturalPortrait, sensorOrientationDegrees)
        }
    }
}
