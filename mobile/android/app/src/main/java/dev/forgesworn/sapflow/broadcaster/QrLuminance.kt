package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer

/** Checked Y-only copy. Image planes never escape to the decoder worker. */
internal object QrLuminance {
    const val MAX_PIXELS = 640 * 480
    class Shape(val width: Int, val height: Int)
    fun copy(plane: ByteBuffer, width: Int, height: Int, left: Int, top: Int, right: Int, bottom: Int,
        rowStride: Int, pixelStride: Int, target: ByteArray): Shape {
        require(width in 1..1280 && height in 1..720 && width.toLong() * height <= 1280L * 720)
        require(left in 0 until width && top in 0 until height && right > left && right <= width && bottom > top && bottom <= height)
        require(rowStride > 0 && pixelStride > 0 && pixelStride <= 16 && target.size == MAX_PIXELS)
        val cropWidth = right - left; val cropHeight = bottom - top
        val step = maxOf(1, (cropWidth + 639) / 640, (cropHeight + 479) / 480)
        val outWidth = (cropWidth + step - 1) / step; val outHeight = (cropHeight + step - 1) / step
        require(outWidth in 1..640 && outHeight in 1..480 && outWidth * outHeight <= target.size)
        val base = plane.position().toLong()
        require((width - 1).toLong() * pixelStride + 1 <= rowStride.toLong())
        val last = base + (bottom - 1).toLong() * rowStride + (right - 1).toLong() * pixelStride
        require(last >= base && last < plane.limit().toLong())
        var destination = 0
        for (y in 0 until outHeight) {
            val sourceRow = base + (top.toLong() + y.toLong() * step) * rowStride + left.toLong() * pixelStride
            for (x in 0 until outWidth) target[destination++] = plane.get((sourceRow + x.toLong() * step * pixelStride).toInt())
        }
        return Shape(outWidth, outHeight)
    }
}
