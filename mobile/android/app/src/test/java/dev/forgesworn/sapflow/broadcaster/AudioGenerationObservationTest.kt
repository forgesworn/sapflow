package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class AudioGenerationObservationTest {
    private fun frozen(p: AudioGenerationObservation, generation: Long = 1): AudioGenerationObservation.Snapshot {
        p.producersJoined(generation)
        p.callbackBarrier(generation, true)
        p.handoffIdle(generation)
        return checkNotNull(p.freeze(generation))
    }
    private fun trim(p: AudioGenerationObservation, phase: AudioGenerationObservation.Phase,
                     kind: AudioGenerationObservation.Trim, state: AudioGenerationObservation.State,
                     reported: Int = 0, api: Int = 36) {
        p.trim(1, phase, kind, api, state, reported)
    }
    @Test fun capturedButUnsubmittedAndCancelledWorkCannotBecomeInputCounters() {
        val p = AudioGenerationObservation(1)
        // Production calls inputQueued only after its real native return. These
        // two unsuccessful fake submissions never deliver that scalar event.
        var failed = false
        fun queue(succeeds: Boolean) {
            if (!succeeds) { failed = true; return }
            p.inputQueued(1, 24, 0)
        }
        queue(false)
        assertTrue(failed)
        val s = frozen(p)
        assertEquals(0L, s.submittedBuffers)
        assertEquals(0L, s.submittedFrames)
        assertEquals(0L, s.submittedRawFrames)
        assertEquals(0L, s.submittedZeroFrames)
    }
    @Test fun rawZeroAndMixedResumeCountsReflectOnlySuccessfulSubmittedComponents() {
        val p = AudioGenerationObservation(1)
        p.inputQueued(1, 700, 0)
        p.inputQueued(1, 0, 1024)
        p.inputQueued(1, 800, 224)
        val s = frozen(p)
        assertEquals(3L, s.submittedBuffers)
        assertEquals(2748L, s.submittedFrames)
        assertEquals(1500L, s.submittedRawFrames)
        assertEquals(1248L, s.submittedZeroFrames)
        assertFalse(s.overflow)
    }
    @Test fun absentTrimsRemainAbsentRatherThanReportedZero() {
        val p = AudioGenerationObservation(1)
        trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.ABSENT)
        trim(p, AudioGenerationObservation.Phase.OUTPUT_EOS, AudioGenerationObservation.Trim.PADDING, AudioGenerationObservation.State.ABSENT)
        val s = frozen(p)
        assertEquals(AudioGenerationObservation.State.ABSENT, s.formatLeading.state)
        assertNull(s.formatLeading.reportedValue)
        assertEquals(AudioGenerationObservation.State.ABSENT, s.eosPadding.state)
        assertNull(s.eosPadding.reportedValue)
    }
    @Test fun explicitZeroRetainsItsPhaseAndIsDifferentFromMissingMetadata() {
        val p = AudioGenerationObservation(1)
        trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE)
        val s = frozen(p)
        assertEquals(AudioGenerationObservation.State.VALUE, s.formatLeading.state)
        assertEquals(0, s.formatLeading.reportedValue)
        assertEquals(36, s.formatLeading.api)
        assertEquals(AudioGenerationObservation.State.UNSEEN, s.eosLeading.state)
        assertNull(s.eosLeading.reportedValue)
    }
    @Test fun api29NeverTurnsAReportedScalarIntoAPlatformTrimFact() {
        val p = AudioGenerationObservation(1)
        trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE, 0, 29)
        val s = frozen(p)
        assertEquals(AudioGenerationObservation.State.API_UNAVAILABLE, s.formatLeading.state)
        assertNull(s.formatLeading.reportedValue)
        assertEquals(29, s.formatLeading.api)
    }
    @Test fun wrongTypeAndNativeFormatReadFailureRemainUnknownWithoutThrowing() {
        val p = AudioGenerationObservation(1)
        trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.WRONG_TYPE)
        trim(p, AudioGenerationObservation.Phase.OUTPUT_EOS, AudioGenerationObservation.Trim.PADDING, AudioGenerationObservation.State.READ_FAILED)
        val s = frozen(p)
        assertEquals(AudioGenerationObservation.State.WRONG_TYPE, s.formatLeading.state)
        assertEquals(AudioGenerationObservation.State.READ_FAILED, s.eosPadding.state)
        assertNull(s.formatLeading.reportedValue)
        assertNull(s.eosPadding.reportedValue)
    }
    @Test fun supportedBoundaryValuesAreRecordedWithoutClaimingAnEofReceipt() {
        val p = AudioGenerationObservation(1)
        trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE, 8191)
        trim(p, AudioGenerationObservation.Phase.OUTPUT_EOS, AudioGenerationObservation.Trim.PADDING, AudioGenerationObservation.State.VALUE, 1023)
        val s = frozen(p)
        assertEquals(8191, s.formatLeading.reportedValue)
        assertEquals(1023, s.eosPadding.reportedValue)
        assertEquals(0L, s.outputEosBuffers)
        assertEquals(0L, s.submittedFrames)
    }
    @Test fun negativeOverlargeHeadAnd1024PaddingRemainUnsupportedDiagnostics() {
        for ((kind, value) in listOf(AudioGenerationObservation.Trim.LEADING to -1,
            AudioGenerationObservation.Trim.LEADING to 8192, AudioGenerationObservation.Trim.PADDING to 1024)) {
            val p = AudioGenerationObservation(1)
            trim(p, AudioGenerationObservation.Phase.FORMAT, kind, AudioGenerationObservation.State.VALUE, value)
            val s = frozen(p)
            val observed = if (kind == AudioGenerationObservation.Trim.LEADING) s.formatLeading else s.formatPadding
            assertEquals(AudioGenerationObservation.State.UNSUPPORTED, observed.state)
            assertEquals(value, observed.reportedValue)
        }
    }
    @Test fun conflictingKnownValuesInOnePhaseOrAcrossPhasesBecomeConflict() {
        for (second in AudioGenerationObservation.Phase.values()) {
            val p = AudioGenerationObservation(1)
            trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE, 2112)
            trim(p, second, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE, 1024)
            val s = frozen(p)
            assertTrue(s.trimConflict)
            assertEquals(AudioGenerationObservation.State.CONFLICT, s.formatLeading.state)
        }
    }
    @Test fun anIdenticalRepeatedObservationIsIdempotent() {
        val p = AudioGenerationObservation(1)
        repeat(2) { trim(p, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, AudioGenerationObservation.State.VALUE, 2112) }
        val s = frozen(p)
        assertFalse(s.trimConflict)
        assertEquals(AudioGenerationObservation.State.VALUE, s.formatLeading.state)
        assertEquals(2112, s.formatLeading.reportedValue)
    }
    @Test fun originalNegativePtsPrimingConfigAndEmptyEosAreSeparateObservedBuffers() {
        val p = AudioGenerationObservation(1)
        p.output(1, 2, -10, 2, true, false, false)
        p.output(1, 20, -3, 0, false, false, true)
        p.primingFiltered(1)
        p.output(1, 0, 999, 4, false, true, true)
        val s = frozen(p)
        assertEquals(3L, s.outputBuffers)
        assertEquals(1L, s.configBuffers)
        assertEquals(1L, s.nonConfigDataBuffers)
        assertEquals(1L, s.emptyBuffers)
        assertEquals(1L, s.preStartBuffers)
        assertEquals(1L, s.outputEosBuffers)
        assertEquals(-3L, s.firstNonConfigPtsUs)
        assertEquals(-3L, s.lastNonConfigPtsUs)
        assertEquals(20L, s.nonConfigBytes)
        assertEquals(1L, s.primingFilteredBuffers)
        assertEquals(0L, s.constructedSamples)
    }
    @Test fun dataBearingEosDoesNotInventFrameCountOrInputCompletion() {
        val p = AudioGenerationObservation(1)
        p.output(1, 23, 30, 4, false, true, true)
        val s = frozen(p)
        assertEquals(1L, s.nonConfigDataBuffers)
        assertEquals(1L, s.outputEosBuffers)
        assertEquals(0L, s.submittedFrames)
        assertEquals(0L, s.inputEosQueued)
        assertEquals(AudioGenerationObservation.State.UNSEEN, s.eosPadding.state)
    }
    @Test fun constructedAndHandoffObservationsDoNotMeanRecorderAdmission() {
        val p = AudioGenerationObservation(1)
        p.constructed(1)
        p.handoffAttempt(1) // Throwing offer: no returned event.
        p.handoffAttempt(1); p.handoffOfferReturned(1)
        p.handoffEntered(1) // Throwing callback: no returned event.
        val s = frozen(p)
        assertEquals(1L, s.constructedSamples)
        assertEquals(2L, s.handoffAttempts)
        assertEquals(1L, s.handoffOfferReturns)
        assertEquals(1L, s.handoffCallbackEntries)
        assertEquals(0L, s.handoffCallbackReturns)
    }
    @Test fun noEosRemainsNoEosAfterRealClosureAndStopFailureIsRetained() {
        val p = AudioGenerationObservation(1)
        p.producersJoined(1); p.callbackBarrier(1, false); p.handoffIdle(1)
        val s = checkNotNull(p.freeze(1))
        assertFalse(s.nativeStopReturned)
        assertEquals(0L, s.outputEosBuffers)
        assertEquals(0L, s.inputEosQueued)
    }
    @Test fun freezeRequiresEveryActualBarrierInEitherCallbackHandoffOrder() {
        val noProducerJoin = AudioGenerationObservation(1)
        noProducerJoin.callbackBarrier(1, true); noProducerJoin.handoffIdle(1)
        assertNull(noProducerJoin.freeze(1))
        noProducerJoin.producersJoined(1)
        assertNotNull(noProducerJoin.freeze(1))
        for (callbackFirst in listOf(true, false)) {
            val p = AudioGenerationObservation(1)
            assertNull(p.freeze(1))
            p.producersJoined(1)
            if (callbackFirst) p.callbackBarrier(1, true) else p.handoffIdle(1)
            assertNull(p.freeze(1))
            if (callbackFirst) p.handoffIdle(1) else p.callbackBarrier(1, true)
            assertNotNull(p.freeze(1))
        }
    }
    @Test fun frozenOldSnapshotAndWrongGenerationEventsCannotMutateSuccessor() {
        val old = AudioGenerationObservation(1)
        old.inputQueued(1, 3, 0)
        val s = frozen(old)
        old.inputQueued(1, 999, 0); old.output(1, 9, 5, 0, false, true, true)
        assertSame(s, old.freeze(1))
        assertEquals(3L, s.submittedFrames)
        val next = AudioGenerationObservation(2)
        next.inputQueued(1, 999, 0); next.output(1, 9, 5, 0, false, true, true)
        next.trim(1, AudioGenerationObservation.Phase.FORMAT, AudioGenerationObservation.Trim.LEADING, 36, AudioGenerationObservation.State.VALUE, 0)
        next.producersJoined(1); next.callbackBarrier(1, true); next.handoffIdle(1)
        assertNull(next.freeze(2))
        assertNull(next.freeze(1))
        assertEquals(0L, frozen(next, 2).submittedFrames)
    }
    @Test fun scalarOverflowAndInvalidInputAreDiagnosticsRatherThanCaptureFailures() {
        val p = AudioGenerationObservation(1)
        p.inputQueued(1, Long.MAX_VALUE, 0)
        p.inputQueued(1, 1, 0)
        p.inputQueued(1, -1, 0)
        val s = frozen(p)
        assertTrue(s.overflow)
        assertTrue(s.invalidDiagnostic)
        assertEquals(Long.MAX_VALUE, s.submittedFrames)
        assertEquals(Long.MAX_VALUE, s.submittedRawFrames)
    }
    @Test fun originalPtsRegressionAndLateProducerEventRemainExplicitDiagnostics() {
        val p = AudioGenerationObservation(1)
        p.output(1, 2, 7, 0, false, false, true)
        p.output(1, 2, -2, 0, false, false, true)
        p.producersJoined(1); p.inputQueued(1, 9, 0)
        val s = frozen(p)
        assertTrue(s.ptsWentBackwards)
        assertTrue(s.invalidDiagnostic)
        assertEquals(0L, s.submittedFrames)
        assertEquals(7L, s.firstNonConfigPtsUs)
        assertEquals(-2L, s.lastNonConfigPtsUs)
    }
    @Test fun concurrentScalarOwnersJoinBeforeOneImmutableColdSnapshot() {
        val p = AudioGenerationObservation(1)
        val done = CountDownLatch(2)
        val input = Thread { try { repeat(200) { p.inputQueued(1, 7, 3) } } finally { done.countDown() } }
        val output = Thread { try { repeat(200) { p.output(1, 4, it.toLong(), 0, false, false, true) } } finally { done.countDown() } }
        input.start(); output.start()
        assertTrue(done.await(2, TimeUnit.SECONDS))
        input.join(1000); output.join(1000)
        assertFalse(input.isAlive); assertFalse(output.isAlive)
        val s = frozen(p)
        assertEquals(2000L, s.submittedFrames)
        assertEquals(1400L, s.submittedRawFrames)
        assertEquals(600L, s.submittedZeroFrames)
        assertEquals(200L, s.nonConfigDataBuffers)
        assertEquals(800L, s.nonConfigBytes)
    }
}
