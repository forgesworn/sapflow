package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.assertTrue
import org.junit.Test

class AudioHandoffBoundsTest {
    @Test
    fun slotsHoldTheWholeResidenceOfAac() {
        // 48 kHz mono AAC: one unit per 1024 samples.
        val unitNs = 1024L * 1_000_000_000L / 48_000L
        assertTrue(AudioHandoffBounds.SLOTS * unitNs >= AudioHandoffBounds.MAX_AGE_NS)
    }

    @Test
    fun bytesCoverEverySlotAtTheLargestUnit() {
        assertTrue(AudioHandoffBounds.BYTES >= AudioHandoffBounds.SLOTS * 4096L)
    }

    @Test
    fun aStartUpStallOfAThirdOfASecondFits() {
        assertTrue(AudioHandoffBounds.MAX_AGE_NS > 343_000_000L)
    }
}
