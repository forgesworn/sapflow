// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

/** Exercises the Probe's reporting policy without SDK, codec or file ownership. */
class NativeAacPairObservationStatusTest {
    private val keys = listOf("first:sdk", "first:native", "middle:sdk", "middle:native", "last:sdk", "last:native")
    private fun healthy() = NativeAacPairObservationStatus.Observation(true, null, true, true, true, true)

    @Test fun configureFailureWithoutStreamKeepsClosedResourcesSeparate() {
        val closed = NativeAacPairObservationStatus.acquiredResourcesClosed(
            acquisitionsKnown = true, decoderAcquired = true, decoderReleased = true,
            extractorAcquired = true, extractorReleased = true, streamAcquired = false,
            streamClosed = false, nativeIndexUncertain = false,
        )
        val observation = healthy().copy(acquiredResourcesClosed = closed,
            failure = "IllegalStateException: configure on Released", inputEos = false,
            outputEos = false, streamClosed = false, pcmAvailable = false)
        assertTrue(closed); assertFalse(observation.streamClosed); assertFalse(observation.completed)
        assertTrue(NativeAacPairObservationStatus.pcmReadbackAllowed(
            acquisitionsKnown = true, streamAcquired = false, streamClosed = false))
        val status = NativeAacPairObservationStatus(keys); status.record(keys[0], observation)
        assertTrue(status.mayContinue)
        assertEquals("first:sdk: IllegalStateException: configure on Released", status.firstFailure)
        assertFalse(status.allDecoderObservationsComplete)
    }

    @Test fun middleFailureSurvivesLaterHealthyObservations() {
        val status = NativeAacPairObservationStatus(keys)
        for ((index, key) in keys.withIndex()) status.record(key,
            if (index == 2) healthy().copy(failure = "configure failed", inputEos = false, outputEos = false,
                streamClosed = false, pcmAvailable = false) else healthy())
        assertEquals(6, status.observationsRecorded); assertEquals(5, status.completedObservations)
        assertEquals("middle:sdk: configure failed", status.firstFailure)
        assertTrue(status.mayContinue); assertFalse(status.allDecoderObservationsComplete)
    }

    @Test fun sixHealthyObservationsRequiredForCompletion() {
        val status = NativeAacPairObservationStatus(keys)
        for (key in keys) {
            assertFalse(status.allDecoderObservationsComplete); status.record(key, healthy())
        }
        assertTrue(status.allDecoderObservationsComplete); assertEquals(6, status.completedObservations)
        assertNull(status.firstFailure)
        assertThrows(IllegalStateException::class.java) { status.record(keys[0], healthy()) }
        val duplicate = NativeAacPairObservationStatus(keys); duplicate.record(keys[0], healthy())
        assertThrows(IllegalStateException::class.java) { duplicate.record(keys[0], healthy()) }
        assertEquals(1, duplicate.observationsRecorded); assertFalse(duplicate.allDecoderObservationsComplete)
    }

    @Test fun unknownAcquisitionOrCloseFencesFurtherObservations() {
        // A constructor may throw before returning an owner; a null stream
        // cannot establish that this same-path file is safe to inspect.
        assertFalse(NativeAacPairObservationStatus.pcmReadbackAllowed(
            acquisitionsKnown = false, streamAcquired = false, streamClosed = false))
        assertFalse(NativeAacPairObservationStatus.pcmReadbackAllowed(
            acquisitionsKnown = false, streamAcquired = false, streamClosed = true))
        assertTrue(NativeAacPairObservationStatus.pcmReadbackAllowed(
            acquisitionsKnown = true, streamAcquired = true, streamClosed = true))
        for (kind in 0..4) {
            val closed = NativeAacPairObservationStatus.acquiredResourcesClosed(
                acquisitionsKnown = kind != 0, decoderAcquired = true, decoderReleased = kind != 1,
                extractorAcquired = true, extractorReleased = kind != 2, streamAcquired = true,
                streamClosed = kind != 3, nativeIndexUncertain = kind == 4,
            )
            assertFalse(closed)
            if (kind == 0 || kind == 3) assertFalse(NativeAacPairObservationStatus.pcmReadbackAllowed(
                acquisitionsKnown = kind != 0, streamAcquired = true, streamClosed = kind != 3))
            val status = NativeAacPairObservationStatus(keys)
            status.record(keys[0], healthy().copy(acquiredResourcesClosed = closed))
            assertFalse(status.mayContinue); assertFalse(status.allDecoderObservationsComplete)
            assertThrows(IllegalStateException::class.java) { status.record(keys[1], healthy()) }
            assertEquals(1, status.observationsRecorded)
        }
    }

    @Test fun missingEosOrPcmCannotCompleteObservation() {
        for (observation in listOf(healthy().copy(inputEos = false), healthy().copy(outputEos = false),
            healthy().copy(pcmAvailable = false), healthy().copy(streamClosed = false))) {
            assertFalse(observation.completed)
            val status = NativeAacPairObservationStatus(keys); status.record(keys[0], observation)
            assertTrue(status.mayContinue); assertNotNull(status.firstFailure)
            assertEquals(0, status.completedObservations); assertFalse(status.allDecoderObservationsComplete)
        }
    }
}
