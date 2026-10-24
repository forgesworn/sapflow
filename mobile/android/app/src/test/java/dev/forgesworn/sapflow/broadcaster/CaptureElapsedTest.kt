package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CaptureElapsedTest {
    private val second = 1_000_000_000L
    @Test fun preparationHasNoElapsedAndFirstCommitDoesNotResetOnRotation() {
        val clock = CaptureElapsed()
        assertNull(clock.snapshot(second, true, null).captureMs)
        clock.commit(2 * second)
        clock.commit(20 * second)
        assertEquals(21_000L, clock.snapshot(23 * second, true, null).captureMs)
        assertEquals(21_000L, clock.snapshot(23 * second, true, null).broadcastMs)
    }
    @Test fun endFreezesBroadcastWhileCaptureAndRecordingIntentContinue() {
        val clock = CaptureElapsed(); clock.commit(second); clock.end(6 * second)
        val value = clock.snapshot(20 * second, true, null)
        assertEquals(19_000L, value.captureMs); assertEquals(5_000L, value.broadcastMs); assertTrue(value.broadcastEnded)
        clock.end(25 * second); clock.stop(30 * second)
        val stopped = clock.snapshot(100 * second, true, null)
        assertEquals(29_000L, stopped.captureMs); assertEquals(5_000L, stopped.broadcastMs)
    }
    @Test fun historicalActualDestinationEndRetainsOriginalTimeAndLocalOnlyHasNoBroadcastClock() {
        val clock = CaptureElapsed(); clock.commit(second)
        clock.snapshot(20 * second, true, null)
        val ended = clock.snapshot(21 * second, true, 8 * second)
        assertEquals(7_000L, ended.broadcastMs); assertEquals(20_000L, ended.captureMs)
        assertNull(clock.snapshot(22 * second, false, null).broadcastMs)
    }
    @Test fun destinationAlreadyEndedBeforeCaptureCommitDoesNotInventBroadcastDuration() {
        val clock = CaptureElapsed(); clock.commit(5 * second)
        assertEquals(0L, clock.snapshot(10 * second, true, second).broadcastMs)
    }
    @Test fun rollbackAndFutureTerminalTimeAreUnknownRatherThanNegativeOrExtended() {
        val clock = CaptureElapsed(); clock.commit(5 * second); clock.snapshot(10 * second, true, null)
        val rollback = clock.snapshot(9 * second, true, null)
        assertTrue(rollback.uncertain); assertNull(rollback.captureMs)
        clock.reset(); clock.commit(second)
        assertTrue(clock.snapshot(2 * second, true, 3 * second).uncertain)
    }
    @Test fun maximumTimestampCannotOverflowAndNewRunResetsPreviousStop() {
        val clock = CaptureElapsed(); clock.commit(0); clock.stop(Long.MAX_VALUE)
        assertEquals(Long.MAX_VALUE / 1_000_000, clock.snapshot(Long.MAX_VALUE, true, null).captureMs)
        clock.reset(); assertNull(clock.snapshot(second, true, null).captureMs)
        clock.commit(2 * second); assertEquals(1_000L, clock.snapshot(3 * second, true, null).broadcastMs)
    }
}
