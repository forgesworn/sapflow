package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.util.Random
import org.junit.Assert.*
import org.junit.Test

class QrLuminanceTest {
    @Test fun paddedRowsCropAndNonzeroBufferPositionAreHonoured() {
        val source = ByteBuffer.allocate(3 + 8 * 2 + 5).apply { position(3) }
        for (y in 0..2) for (x in 0..4) source.put(3 + y * 8 + x, (y * 10 + x).toByte())
        val target = ByteArray(QrLuminance.MAX_PIXELS)
        val shape = QrLuminance.copy(source, 5, 3, 1, 1, 4, 3, 8, 1, target)
        assertEquals(3, shape.width); assertEquals(2, shape.height)
        assertArrayEquals(byteArrayOf(11, 12, 13, 21, 22, 23), target.copyOf(6)); assertEquals(3, source.position())
    }
    @Test fun pixelStrideAndTruncatedFinalPaddingDoNotReadPadding() {
        val source = ByteBuffer.allocate(2 + 9 * 2 + 9).apply { position(2) }
        for (y in 0..2) for (x in 0..4) source.put(2 + y * 9 + x * 2, (y * 10 + x).toByte())
        val target = ByteArray(QrLuminance.MAX_PIXELS)
        val shape = QrLuminance.copy(source, 5, 3, 0, 0, 5, 3, 9, 2, target)
        assertEquals(5, shape.width); assertEquals(3, shape.height)
        assertEquals(24.toByte(), target[14]); assertEquals(2, source.position())
    }
    @Test fun largeSupportedPlaneIsBoundedIntegerSubsample() {
        val source = ByteBuffer.allocate(1280 * 720)
        source.put(718 * 1280 + 1278, 71)
        val target = ByteArray(QrLuminance.MAX_PIXELS)
        val shape = QrLuminance.copy(source, 1280, 720, 0, 0, 1280, 720, 1280, 1, target)
        assertEquals(640, shape.width); assertEquals(360, shape.height); assertEquals(71.toByte(), target[640 * 360 - 1])
    }
    @Test fun malformedExtentStrideOverflowAndSmallOutputRefuse() {
        val bytes = ByteBuffer.allocate(64); val target = ByteArray(QrLuminance.MAX_PIXELS)
        val cases = listOf(intArrayOf(8, 8, Int.MAX_VALUE, 0, 8, 8, 8, 1), intArrayOf(8, 8, 0, 0, 8, 8, Int.MAX_VALUE, 1),
            intArrayOf(8, 8, 0, 0, 8, 8, 8, -1), intArrayOf(8, 8, 0, 0, 8, 8, 1, 1),
            intArrayOf(1281, 8, 0, 0, 8, 8, 1281, 1), intArrayOf(8, 721, 0, 0, 8, 8, 8, 1))
        for (c in cases) try { QrLuminance.copy(bytes, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], target); fail("Expected bounded refusal") }
        catch (_: IllegalArgumentException) { }
        try { QrLuminance.copy(bytes, 8, 8, 0, 0, 8, 8, 8, 1, ByteArray(1)); fail("Small output") } catch (_: IllegalArgumentException) { }
        bytes.limit(63)
        try { QrLuminance.copy(bytes, 8, 8, 0, 0, 8, 8, 8, 1, target); fail("Missing final pixel") } catch (_: IllegalArgumentException) { }
    }
    @Test fun finiteGeneratedGeometryMutationsCannotReadOutsideThePlane() {
        val random = Random(0x71a03L); val source = ByteBuffer.allocate(64); val target = ByteArray(QrLuminance.MAX_PIXELS)
        repeat(2048) {
            val args = IntArray(8) { random.nextInt(40) - 12 }
            try { val result = QrLuminance.copy(source, args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], target)
                assertTrue(result.width * result.height <= target.size)
            } catch (_: IllegalArgumentException) { }
        }
    }
}
