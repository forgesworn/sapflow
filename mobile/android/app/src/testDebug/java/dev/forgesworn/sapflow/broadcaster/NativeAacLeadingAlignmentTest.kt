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

    private fun anchorBits(start: Int, lag: Int, correlation: Double, runner: Double, gain: Double) =
        listOf(start.toLong(), lag.toLong(), correlation.toRawBits(), runner.toRawBits(), gain.toRawBits())

    /** The frozen Double oracle chooses every expectation. Unexpected failures
     * propagate; only the original IllegalArgumentException refusal is compared. */
    private fun compareOracle(label: String, signal: ShortArray, decoded: ShortArray, frames: Int): Boolean {
        var expectedFailure: IllegalArgumentException? = null
        val expected = try { NativeAacLeadingAlignmentDoubleOracle.measure(signal, decoded, frames) }
            catch (error: IllegalArgumentException) { expectedFailure = error; null }
        var actualFailure: IllegalArgumentException? = null
        val actual = try { NativeAacLeadingAlignment.measure(signal, decoded, frames) }
            catch (error: IllegalArgumentException) { actualFailure = error; null }
        assertEquals("$label refusal type", expectedFailure?.javaClass, actualFailure?.javaClass)
        assertEquals("$label refusal details", expectedFailure?.message, actualFailure?.message)
        if (expected == null) return false
        val observed = checkNotNull(actual)
        assertEquals("$label candidate lag", expected.lag, observed.lag)
        val e0 = expected.first; val a0 = observed.first
        val e1 = expected.second; val a1 = observed.second
        assertEquals("$label first metadata bits",
            anchorBits(e0.sourceStart, e0.lag, e0.correlation, e0.runnerUp, e0.gain),
            anchorBits(a0.sourceStart, a0.lag, a0.correlation, a0.runnerUp, a0.gain))
        assertEquals("$label second metadata bits",
            anchorBits(e1.sourceStart, e1.lag, e1.correlation, e1.runnerUp, e1.gain),
            anchorBits(a1.sourceStart, a1.lag, a1.correlation, a1.runnerUp, a1.gain))
        return true
    }

    @Test fun integerKernelMatchesFrozenDoubleForBothSeedsLagsExtentsAndGains() {
        for (seed in listOf(0x13579, 0x24680)) {
            val signal = source(seed)
            for (lag in listOf(0, 1, 1023, 2048, 4095)) {
                val decoded = delayed(signal, lag)
                for (frames in listOf(6144, 8192)) {
                    assertEquals(true, compareOracle("seed=$seed lag=$lag frames=$frames", signal, decoded, frames))
                }
            }
            for (gain in listOf(0.5, 0.75, 1.5)) {
                compareOracle("seed=$seed quantised gain=$gain", signal, delayed(signal, 1701, gain), 6144)
            }
            val decoded = delayed(signal, 2048)
            for (frames in listOf(6143, 8193)) {
                assertEquals(false, compareOracle("seed=$seed invalid extent=$frames", signal, decoded, frames))
            }
            assertEquals(false, compareOracle("seed=$seed short backing array", signal, decoded.copyOf(6143), 6144))
            assertEquals(false, compareOracle("seed=$seed short source", signal.copyOf(8191), decoded, 8192))
        }
    }

    private fun nudgeAnchors(signal: ShortArray, decoded: ShortArray, direction: Int): ShortArray {
        val changed = decoded.copyOf()
        for (start in listOf(0, 1024)) {
            val at = (start until start + 1024).first { signal[it].toInt() != 0 }
            val sign = if (signal[at] > 0) 1 else -1
            changed[2048 + at] = (changed[2048 + at].toInt() + direction * sign).toShort()
        }
        return changed
    }

    private fun oracleAccepts(signal: ShortArray, decoded: ShortArray): Boolean = try {
        NativeAacLeadingAlignmentDoubleOracle.measure(signal, decoded, 8192); true
    } catch (_: IllegalArgumentException) { false }

    private fun corruption(signal: ShortArray, foreign: ShortArray, weight: Int, echo: Boolean) = ShortArray(8192) { i ->
        val at = i - 2048
        if (at < 0) 0 else {
            val first = signal[at].toInt()
            val other = if (echo) { if (at == 0) 0 else signal[at - 1].toInt() } else foreign[at].toInt()
            ((first * (4096 - weight) + other * weight) / 4096).toShort()
        }
    }

    @Test fun integerKernelMatchesFrozenDoubleForExtremesAndAdjacentThresholdCorruptions() {
        // Exercise the maximum Int product and Long window/dot sums, including
        // negative products, constant/periodic ambiguity and strict source floor.
        for (left in listOf(Short.MIN_VALUE, Short.MAX_VALUE)) {
            for (right in listOf(Short.MIN_VALUE, Short.MAX_VALUE)) {
                compareOracle("extremes $left/$right", ShortArray(8192) { left }, ShortArray(8192) { right }, 6144)
            }
        }
        val periodic = ShortArray(8192) { if (it % 2 == 0) Short.MIN_VALUE else Short.MAX_VALUE }
        assertEquals(false, compareOracle("extreme periodic tie", periodic, delayed(periodic, 100), 8192))
        for (value in listOf<Short>(256, 257)) {
            assertEquals(false, compareOracle("source floor $value", ShortArray(8192) { value }, ShortArray(8192), 6144))
        }
        for (seed in listOf(0x13579, 0x24680)) {
            val generated = source(seed)
            val extremes = ShortArray(8192) { if (generated[it] < 0) Short.MIN_VALUE else Short.MAX_VALUE }
            assertEquals(true, compareOracle("seed=$seed nonperiodic integer extremes", extremes, delayed(extremes, 2048), 6144))
            // Even source samples make the inclusive gain endpoints exact.
            // One PCM-unit correction per anchor straddles each gain boundary.
            val even = ShortArray(8192) { (generated[it].toInt() and -2).toShort() }
            val low = delayed(even, 2048, 0.5)
            assertEquals(true, compareOracle("seed=$seed gain exactly0.5", even, low, 8192))
            assertEquals(false, compareOracle("seed=$seed gain just below0.5", even, nudgeAnchors(even, low, -1), 8192))
            assertEquals(true, compareOracle("seed=$seed gain just above0.5", even, nudgeAnchors(even, low, 1), 8192))
            val high = delayed(even, 2048, 1.5)
            assertEquals(true, compareOracle("seed=$seed gain exactly1.5", even, high, 8192))
            assertEquals(true, compareOracle("seed=$seed gain just below1.5", even, nudgeAnchors(even, high, -1), 8192))
            assertEquals(false, compareOracle("seed=$seed gain just above1.5", even, nudgeAnchors(even, high, 1), 8192))
        }
        // Locate adjacent discrete corruption levels with only the old oracle.
        // No monotonicity assumption or optimised-result expectation is used:
        // the bracket always retains a known accepted and known refused input.
        val foreign = source(0x54321)
        for ((seed, echo) in listOf(0x13579 to false, 0x24680 to true)) {
            val signal = source(seed)
            var accepted = 0
            var refused = if (echo) 2048 else 4096
            assertEquals(true, oracleAccepts(signal, corruption(signal, foreign, accepted, echo)))
            assertEquals(false, oracleAccepts(signal, corruption(signal, foreign, refused, echo)))
            repeat(12) {
                if (refused - accepted > 1) {
                    val middle = (accepted + refused) / 2
                    if (oracleAccepts(signal, corruption(signal, foreign, middle, echo))) accepted = middle else refused = middle
                }
            }
            assertEquals(1, refused - accepted)
            assertEquals(true, compareOracle("seed=$seed echo=$echo accepted corruption=$accepted", signal, corruption(signal, foreign, accepted, echo), 8192))
            assertEquals(false, compareOracle("seed=$seed echo=$echo refused adjacent corruption=$refused", signal, corruption(signal, foreign, refused, echo), 8192))
        }
    }
}
