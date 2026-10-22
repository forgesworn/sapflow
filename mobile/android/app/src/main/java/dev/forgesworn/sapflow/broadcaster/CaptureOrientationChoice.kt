package dev.forgesworn.sapflow.broadcaster

/** Pre-Prepare choice for actual encoded pixels, independent of display lock.
 * Left/right describe the portrait phone's top edge viewed from its screen.
 * A natural-landscape display uses a declared portrait reference at pose90;
 * this does not infer the position of a tablet's camera or earpiece. */
internal enum class CaptureOrientationChoice(val label: String) {
    AUTO("Automatic"), PORTRAIT("Portrait"), LANDSCAPE_TOP_LEFT("Landscape (top left)"), LANDSCAPE_TOP_RIGHT("Landscape (top right)");

    class Selection internal constructor(val choice: CaptureOrientationChoice, val naturalPortrait: Boolean, val initialPose: CardinalPose) {
        val automatic: Boolean get() = choice == AUTO
        /** Only Auto follows the latest stable pose after permission/storage
         * waits. Fixed choices retain their original geometry and basis. */
        fun atPrepare(latestStablePose: CardinalPose): Selection =
            if (automatic) Selection(choice, naturalPortrait, latestStablePose) else this
    }

    fun freeze(hasVideo: Boolean, naturalPortrait: Boolean, stablePose: CardinalPose): Selection? {
        if (!hasVideo) return null
        val portrait = if (naturalPortrait) 0 else 90
        val degrees = when (this) {
            AUTO -> stablePose.clockwiseDegrees
            PORTRAIT -> portrait
            LANDSCAPE_TOP_LEFT -> (portrait + 270) % 360
            LANDSCAPE_TOP_RIGHT -> (portrait + 90) % 360
        }
        return Selection(this, naturalPortrait, CardinalPose.fromDegrees(degrees))
    }
}
