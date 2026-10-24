package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class RecordingStorageTest {
    @Test fun explicitHorizonUsesPayloadAndHeadroomWithoutARecordingTimer() {
        val low = RecordingStorage.budget(CaptureProfile.SD_480)
        val high = RecordingStorage.budget(CaptureProfile.HD_720)
        assertEquals(60, low.horizonSeconds)
        assertEquals(5_220_000, low.payloadBytes)
        assertTrue(high.requiredBytes > low.requiredBytes)
        assertTrue(low.requiredBytes > low.payloadBytes + 32L * 1024 * 1024)
        assertTrue(RecordingStorage.budget(CaptureProfile.HD_720, 120).payloadBytes == high.payloadBytes * 2)
    }
    @Test fun unavailableLowAndExactCapacityRemainDistinct() {
        val budget = RecordingStorage.budget(CaptureProfile.DEFAULT)
        assertEquals(RecordingStorage.State.AVAILABLE, RecordingStorage.check(budget) { budget.requiredBytes }.state)
        assertEquals(RecordingStorage.State.LOW, RecordingStorage.check(budget) { budget.requiredBytes - 1 }.state)
        assertEquals(RecordingStorage.State.UNAVAILABLE, RecordingStorage.check(budget) { null }.state)
        assertEquals(RecordingStorage.State.UNAVAILABLE, RecordingStorage.check(budget) { -1 }.state)
        assertEquals(RecordingStorage.State.UNAVAILABLE, RecordingStorage.check(budget) { error("storage unavailable") }.state)
    }
    @Test fun invalidAndOverflowingBudgetsNeverWrapIntoAvailableSpace() {
        assertThrows(IllegalArgumentException::class.java) { RecordingStorage.budget(CaptureProfile.DEFAULT, 0) }
        assertThrows(IllegalArgumentException::class.java) { RecordingStorage.budget(CaptureProfile.DEFAULT, reserveBytes = -1) }
        assertThrows(ArithmeticException::class.java) { RecordingStorage.budget(CaptureProfile.DEFAULT, Long.MAX_VALUE) }
        assertThrows(ArithmeticException::class.java) { RecordingStorage.budget(CaptureProfile.DEFAULT, reserveBytes = Long.MAX_VALUE) }
    }
}
