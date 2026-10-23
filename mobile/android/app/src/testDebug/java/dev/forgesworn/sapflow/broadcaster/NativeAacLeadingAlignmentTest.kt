package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class NativeAacLeadingAlignmentTest {
    private fun source(seed: Int = 0x13579) = ShortArray(8192) { NativeAacLeadingAlignment.sample(it, seed) }
    private fun delayed(source: ShortArray, lag: Int, gain: Double = 1.0) = ShortArray(8192) {
        if (it < lag) 0 else (source[it - lag] * gain).toInt().toShort()
    }
    @Test fun exactNonperiodicIntegerLagIncludingBounds() {
        val source = source()
        for (lag in listOf(0, 1, 1023, 2048, 4095)) {
            assertEquals(lag, NativeAacLeadingAlignment.measure(source, delayed(source, lag), 8192).lag)
        }
    }
    @Test fun quantisedPositiveGainDoesNotChangeIntegerLag() {
        val source = source()
        assertEquals(1701, NativeAacLeadingAlignment.measure(source, delayed(source, 1701, 0.75), 6144).lag)
    }
    @Test fun silenceRejected() {
        assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source(), ShortArray(8192), 8192) }
    }
    @Test fun wrongSourceRejected() {
        assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source(), delayed(source(0x54321), 2048), 8192) }
    }
    @Test fun polarityAndOutOfRangeGainRejected() {
        val source = source()
        for (gain in listOf(-1.0, 0.1, 1.8)) {
            assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source, delayed(source, 2048, gain), 8192) }
        }
    }
    @Test fun repeatedAnchorAmbiguityRejected() {
        val source = ShortArray(8192) { if (it % 2 == 0) 1000 else -1000 }
        assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source, delayed(source, 100), 8192) }
    }
    @Test fun distinctAnchorLagsRejected() {
        val source = source(); val decoded = delayed(source, 2048)
        for (i in 1024 until 2048) decoded[2048 + i] = source[i - 1]
        assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source, decoded, 8192) }
    }
    @Test fun insufficientOrOversizedDecodedExtentRejected() {
        val source = source()
        for (count in listOf(6143, 8193)) {
            assertThrows(IllegalArgumentException::class.java) { NativeAacLeadingAlignment.measure(source, delayed(source, 2048), count) }
        }
    }
}
