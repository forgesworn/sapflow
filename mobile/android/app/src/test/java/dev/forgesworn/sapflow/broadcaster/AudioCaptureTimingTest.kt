package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class AudioCaptureTimingTest {
    private val epoch = 10_000_000_000L

    @Test fun hardwareMappingKeepsCaptureTimeAcrossDifferentReadAvailability() {
        val timing = AudioCaptureTiming(epoch, 4096)
        assertEquals(10_000, timing.read(1024, 1024, epoch + 31_333_333, epoch + 32_000_000))
        assertEquals(31_333, timing.read(1024, 2400, epoch + 60_000_000, epoch + 65_000_000))
        assertEquals(2048, timing.framesRead)
        assertEquals(592, timing.backlogFrames)
    }

    @Test fun sustainedCaptureRemainsAt48kDespiteTimestampObservationIntervals() {
        val timing = AudioCaptureTiming(epoch, 4096)
        repeat(30_000) { unit ->
            val frame = (unit + 1L) * 1024
            val stamp = epoch + 10_000_000 + frame * 1_000_000_000 / 48_000
            val pts = timing.read(1024, frame, stamp, stamp + 1_000_000)
            assertEquals(10_000 + unit * 1024L * 1_000_000 / 48_000, pts)
        }
        assertEquals(30_720_000, timing.framesRead)
    }

    @Test fun overrunIsRefusedBeforeReadingTheRetainedRingAsContinuousPcm() {
        val timing = AudioCaptureTiming(epoch, 4096)
        timing.read(1024, 1024, epoch + 31_333_333, epoch + 32_000_000)
        val error = assertThrows(IllegalArgumentException::class.java) {
            timing.beforeRead(6000, epoch + 135_000_000, epoch + 136_000_000)
        }
        assertTrue(error.message!!.contains("before readCursor=1024 count=0 hwPosition=6000"))
        assertTrue(error.message!!.contains("capacityFrames=4096"))
        assertEquals(1024, timing.framesRead)
    }

    @Test fun readerStarvationFailsInsteadOfCompressingLostTime() {
        val timing = AudioCaptureTiming(epoch, 4096)
        timing.read(1024, 1024, epoch + 31_333_333, epoch + 32_000_000)
        assertThrows(IllegalArgumentException::class.java) {
            timing.read(1024, 12_000, epoch + 260_000_000, epoch + 261_000_000)
        }
        assertEquals(1024, timing.framesRead)
    }

    @Test fun staleRegressingAndPreEpochPositionsFailClosed() {
        assertThrows(IllegalArgumentException::class.java) { AudioCaptureTiming(epoch, 4096).read(1024, 1024, epoch, epoch + 101_000_000) }
        assertThrows(IllegalArgumentException::class.java) { AudioCaptureTiming(epoch, 4096).read(1024, 1024, epoch, epoch) }
        val timing = AudioCaptureTiming(epoch, 4096)
        timing.read(1024, 1024, epoch + 31_333_333, epoch + 32_000_000)
        assertThrows(IllegalArgumentException::class.java) { timing.read(1024, 1000, epoch + 40_000_000, epoch + 41_000_000) }
    }
}
