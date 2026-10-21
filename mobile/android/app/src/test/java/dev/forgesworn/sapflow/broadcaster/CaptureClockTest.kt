package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CaptureClockTest {
    @Test fun encoderAndAudioShareMonotonicEpochDespiteAccumulatedSuspendTime() {
        var monotonicUs = 5_000_000L
        val suspendOffsetUs = 16_000_000L
        val bootTimeUs = monotonicUs + suspendOffsetUs
        val clock = CaptureClock { monotonicUs }
        clock.start()
        monotonicUs += 40_000
        // An elapsedRealtime epoch would reject this valid first IDR.
        assertTrue(monotonicUs < bootTimeUs)
        assertEquals(3000L, clock.video(5_033_333, true))
        assertEquals(40_000L, clock.audioStartUs())
        assertEquals(1920L, clock.audio(clock.audioStartUs()))
        assertEquals(6L, clock.videoAgeMs(5_033_333))
        assertEquals(-1L, clock.videoAgeMs(monotonicUs + 1000))
        // BOOTTIME can gain another suspend offset without shifting media PTS.
        monotonicUs += 21_333
        assertEquals(2944L, clock.audio(clock.audioStartUs()))
        assertEquals(28L, clock.videoAgeMs(5_033_333))
    }

    @Test fun videoWaitsForIdrDropsDuplicateSlotsAndPreservesMissingFrames() {
        val clock = CaptureClock().apply { start(1_000_000) }
        assertNull(clock.video(999_999, true))
        assertNull(clock.video(1_000_000, false))
        assertEquals(0L, clock.video(1_000_001, true))
        assertNull(clock.video(1_001_000, false))
        assertEquals(3000L, clock.video(1_033_500, false))
        assertEquals(9000L, clock.video(1_100_000, false))
        assertNull(clock.video(1_050_000, true))
    }

    @Test fun aacPrimingAndMicrosecondRoundingDoNotRegressTheSampleClock() {
        val clock = CaptureClock().apply { start(10_000) }
        assertNull(clock.audio(-21_333))
        assertEquals(0L, clock.audio(0))
        assertEquals(1024L, clock.audio(21_333))
        assertEquals(2048L, clock.audio(42_666))
        assertEquals(3072L, clock.audio(64_000))
        assertThrows(IllegalArgumentException::class.java) { clock.audio(1_000_000) }
    }

    @Test fun explicitRestartRequiresANewIdrAndReanchorsAudio() {
        val clock = CaptureClock().apply { start(0) }
        assertEquals(0L, clock.video(0, true))
        assertEquals(4800L, clock.audio(100_000))
        clock.start(2_000_000)
        assertNull(clock.video(2_000_000, false))
        assertEquals(0L, clock.video(2_000_000, true))
        assertEquals(0L, clock.audio(0))
    }

    @Test fun hostileClockValuesFailWithoutArithmeticOverflow() {
        val clock = CaptureClock().apply { start(0) }
        assertThrows(IllegalArgumentException::class.java) { clock.video(Long.MAX_VALUE, true) }
        assertThrows(IllegalArgumentException::class.java) { clock.audio(Long.MAX_VALUE) }
        assertThrows(IllegalArgumentException::class.java) { clock.start(-1) }
    }
}
