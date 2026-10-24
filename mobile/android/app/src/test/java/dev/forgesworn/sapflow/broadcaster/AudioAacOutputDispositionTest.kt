package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioAacOutputDispositionTest {
    private val d = AudioAacOutputDisposition

    @Test fun nonPartialDataRemainsUnknownCardinalityWithOrWithoutKnownCapacity() {
        for (capacity in listOf(-1, 23, 4096)) {
            val value = d.classify(0, 23, 0, true, capacity)
            assertEquals(d.DATA_CARDINALITY_UNKNOWN, d.kind(value))
            assertTrue(d.has(value, d.CARDINALITY_UNKNOWN))
            assertEquals(capacity == -1, d.has(value, d.CAPACITY_UNKNOWN))
            assertFalse(d.has(value, d.RANGE_INVALID))
        }
    }
    @Test fun configAndEmptyEosHaveNoImpliedAudioCardinality() {
        val config = d.classify(0, 2, 2, false)
        assertEquals(d.CONFIG, d.kind(config))
        assertTrue(d.has(config, d.CONFIG_FLAG))
        assertTrue(d.has(config, d.EARLY))
        assertFalse(d.has(config, d.CARDINALITY_UNKNOWN))
        val empty = d.classify(0, 0, 4, true, 0)
        assertEquals(d.EMPTY, d.kind(empty))
        assertTrue(d.has(empty, d.EMPTY_BUFFER))
        assertTrue(d.has(empty, d.EOS))
        assertFalse(d.has(empty, d.CARDINALITY_UNKNOWN))
    }
    @Test fun nonemptyEosRemainsDataRatherThanProofOfFramesOrCompletion() {
        val value = d.classify(5, 23, 4, true, 28)
        assertEquals(d.DATA_CARDINALITY_UNKNOWN, d.kind(value))
        assertTrue(d.has(value, d.EOS))
        assertTrue(d.has(value, d.CARDINALITY_UNKNOWN))
        assertFalse(d.has(value, d.EMPTY_BUFFER))
    }
    @Test fun partialConfigEosAndEarlyBitsSurviveThePrimaryCategory() {
        val value = d.classify(0, 2, 2 or 4 or 8, false, 2)
        assertEquals(d.PARTIAL, d.kind(value))
        for (flag in listOf(d.CONFIG_FLAG, d.PARTIAL_FLAG, d.EOS, d.EARLY)) assertTrue(d.has(value, flag))
        assertFalse(d.has(value, d.CARDINALITY_UNKNOWN))
        assertFalse(d.has(value, d.OTHER_FLAGS))
    }
    @Test fun partialDataAndPartialEmptyAreDistinctFromCompleteAudioFacts() {
        val data = d.classify(0, 20, 8, true)
        assertEquals(d.PARTIAL, d.kind(data))
        assertTrue(d.has(data, d.CARDINALITY_UNKNOWN))
        val empty = d.classify(0, 0, 8, true)
        assertEquals(d.PARTIAL, d.kind(empty))
        assertTrue(d.has(empty, d.EMPTY_BUFFER))
        assertFalse(d.has(empty, d.CARDINALITY_UNKNOWN))
    }
    @Test fun undescribedAndSignBitFlagsRemainObservableWithoutRejectingData() {
        for (flags in listOf(1, 16, 32, Int.MIN_VALUE, Int.MAX_VALUE)) {
            val value = d.classify(0, 20, flags, true)
            assertTrue(d.has(value, d.OTHER_FLAGS))
            assertEquals(flags and 2 == 0, d.has(value, d.CARDINALITY_UNKNOWN))
            assertFalse(d.has(value, d.RANGE_INVALID))
        }
    }
    @Test fun malformedAndOverflowRangesAreDiagnosticsWithOriginalFlagDisposition() {
        for ((offset, size, capacity) in listOf(Triple(-1, 1, -1), Triple(0, -1, -1),
            Triple(Int.MAX_VALUE, 1, -1), Triple(1, Int.MAX_VALUE, -1),
            Triple(Int.MAX_VALUE, Int.MAX_VALUE, -1), Triple(0, 2, 1), Triple(0, 0, -2))) {
            val value = d.classify(offset, size, 4 or 8, false, capacity)
            assertEquals(d.INVALID_RANGE, d.kind(value))
            assertTrue(d.has(value, d.RANGE_INVALID))
            assertTrue(d.has(value, d.PARTIAL_FLAG))
            assertTrue(d.has(value, d.EOS))
            assertTrue(d.has(value, d.EARLY))
        }
    }
    @Test fun maximumRepresentableAndExactCapacityRangesDoNotOverflow() {
        for ((offset, size, capacity) in listOf(Triple(Int.MAX_VALUE, 0, Int.MAX_VALUE),
            Triple(0, Int.MAX_VALUE, Int.MAX_VALUE), Triple(7, 9, 16), Triple(0, 0, 0))) {
            assertFalse(d.has(d.classify(offset, size, 0, true, capacity), d.RANGE_INVALID))
        }
    }
    @Test fun mixedConfigEmptyAndEosObservationsRetainAllFlags() {
        val value = d.classify(0, 0, 2 or 4, false, 0)
        assertEquals(d.CONFIG, d.kind(value))
        for (flag in listOf(d.CONFIG_FLAG, d.EMPTY_BUFFER, d.EOS, d.EARLY)) assertTrue(d.has(value, flag))
        assertFalse(d.has(value, d.CARDINALITY_UNKNOWN))
    }
}
