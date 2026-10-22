package dev.forgesworn.sapflow.broadcaster

import kotlin.math.abs

/** Camera2 unmirrored SurfaceTexture producer basis: sensor quarter-turn,
 * positive crop scales and GL's vertical flip. The complete producer matrix
 * remains authoritative for sampling; no crop or flip is discarded. */
internal object CameraTextureGeometry {
    private const val EPSILON = 0.00001f
    private val zeroFields = intArrayOf(2, 3, 6, 7, 8, 9, 11, 14)

    /** API33 output mirror NONE uses the unchanged producer matrix. Older
     * front AUTO declares a horizontal natural-texture reflection. Compose
     * its inverse in place, preserving crop/translation, then validate the
     * exact unmirrored sensor basis. No image-content inference or raw copy. */
    fun normalise(matrix: FloatArray, sensorClockwiseDegrees: Int, rawWidth: Int, rawHeight: Int, legacyFrontMirror: Boolean) {
        require(matrix.size == 16)
        if (legacyFrontMirror) {
            matrix[12] += matrix[0]; matrix[13] += matrix[1]
            matrix[0] = -matrix[0]; matrix[1] = -matrix[1]
        }
        validate(matrix, sensorClockwiseDegrees, rawWidth, rawHeight)
    }

    /** Allocation-free on success. Unexpected rotation/reflection/shear fails
     * explicitly instead of silently stretching or mirroring encoded pixels. */
    fun validate(matrix: FloatArray, sensorClockwiseDegrees: Int, rawWidth: Int, rawHeight: Int) {
        require(rawWidth > 0 && rawHeight > 0) { "Invalid raw camera dimensions" }
        require(matrix.size == 16) { "Invalid camera texture matrix size" }
        for (index in 0 until 16) require(matrix[index].isFinite()) { "Non-finite camera texture transform" }
        for (index in zeroFields) require(abs(matrix[index]) <= EPSILON) { "Non-affine camera texture transform" }
        require(abs(matrix[10] - 1f) <= EPSILON && abs(matrix[15] - 1f) <= EPSILON) { "Invalid camera texture transform scale" }
        val a = matrix[0]; val b = matrix[4]; val c = matrix[1]; val d = matrix[5]
        val expected = when (sensorClockwiseDegrees) {
            0 -> a > EPSILON && d < -EPSILON && abs(b) <= EPSILON && abs(c) <= EPSILON
            90 -> b < -EPSILON && c < -EPSILON && abs(a) <= EPSILON && abs(d) <= EPSILON
            180 -> a < -EPSILON && d > EPSILON && abs(b) <= EPSILON && abs(c) <= EPSILON
            270 -> b > EPSILON && c > EPSILON && abs(a) <= EPSILON && abs(d) <= EPSILON
            else -> false
        }
        require(expected) { "Camera texture producer basis differs from declared sensor orientation" }
        // A configured 16:9 stream may have a small filtering inset. Permit
        // two source pixels per axis, but reject an unrelated crop aspect
        // instead of stretching it across the configured encoded canvas.
        val scaleX = abs(a) + abs(b)
        val scaleY = abs(c) + abs(d)
        require(abs(scaleX - scaleY) <= 2f / rawWidth + 2f / rawHeight) { "Camera texture crop aspect differs from its configured buffer" }
        for (x in 0..1) for (y in 0..1) {
            val u = a * x + b * y + matrix[12]
            val v = c * x + d * y + matrix[13]
            require(u >= -EPSILON && u <= 1f + EPSILON && v >= -EPSILON && v <= 1f + EPSILON) { "Camera texture crop exceeds its buffer" }
        }
    }
}
