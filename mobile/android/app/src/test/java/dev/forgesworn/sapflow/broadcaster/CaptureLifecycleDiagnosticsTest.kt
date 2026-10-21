package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CaptureLifecycleDiagnosticsTest {
    private val stopping = CaptureLifecycleDiagnostics.Context(21, 22, CardinalPose.REVERSE,
        CaptureLifecycleDiagnostics.CaptureStageName.STOPPING, 5)

    @Test fun recordingFailureDuringPendingRotationSurvivesNormalTransitionAndCleanup() {
        val diagnostics = CaptureLifecycleDiagnostics()
        diagnostics.beginEpoch(21)
        diagnostics.transition(CaptureLifecycleDiagnostics.Code.ORIENTATION_REQUEST, stopping)
        diagnostics.transition(CaptureLifecycleDiagnostics.Code.STOP_REQUEST, stopping)
        diagnostics.failure(CaptureLifecycleDiagnostics.Code.RECORDING_FAILURE, "Recording queue failed", stopping)
        diagnostics.transition(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED, stopping)
        assertEquals("Recording queue failed", diagnostics.message("Changing encoded orientation; finalising the previous file"))
        assertEquals(CaptureLifecycleDiagnostics.Code.RECORDING_FAILURE, diagnostics.firstFailure!!.code)
        assertEquals(stopping, diagnostics.firstFailure!!.context)
    }

    @Test fun cleanupWatchdogThenSuccessfulOwnerCompletionStillExplainsTheError() {
        val diagnostics = CaptureLifecycleDiagnostics()
        diagnostics.beginEpoch(21)
        diagnostics.failure(CaptureLifecycleDiagnostics.Code.CLEANUP_TIMEOUT, "Native cleanup exceeded six seconds", stopping)
        val released = stopping.copy(stage = CaptureLifecycleDiagnostics.CaptureStageName.ERROR, flags = 2047)
        diagnostics.transition(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED, released)
        diagnostics.failure(CaptureLifecycleDiagnostics.Code.CLEANUP_FAILURE, "Secondary cleanup error", released)
        assertEquals("Native cleanup exceeded six seconds", diagnostics.message("Changing encoded orientation; finalising the previous file"))
        assertEquals(CaptureLifecycleDiagnostics.Code.CLEANUP_TIMEOUT, diagnostics.firstFailure!!.code)
        assertEquals(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED, diagnostics.history()[1].code)
    }

    @Test fun staleEpochCannotOverwriteCurrentFailureAndManualPrepareRetainsBoundedHistory() {
        val diagnostics = CaptureLifecycleDiagnostics()
        diagnostics.beginEpoch(21)
        diagnostics.failure(CaptureLifecycleDiagnostics.Code.AUDIO_OWNER_FAILURE, "AAC failed", stopping)
        diagnostics.beginEpoch(23)
        assertNull(diagnostics.firstFailure)
        assertNull(diagnostics.failure(CaptureLifecycleDiagnostics.Code.GPU_FAILURE, "Old callback", stopping))
        assertEquals("Preparing", diagnostics.message("Preparing"))
        val current = stopping.copy(epoch = 23, generation = 23)
        repeat(100) { diagnostics.transition(CaptureLifecycleDiagnostics.Code.STOP_REQUEST, current) }
        assertEquals(16, diagnostics.transitionCount)
        val snapshot = diagnostics.history()
        diagnostics.transition(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED, current)
        assertEquals(CaptureLifecycleDiagnostics.Code.STOP_REQUEST, snapshot.last().code)
        assertEquals(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED, diagnostics.history().last().code)
    }

    @Test fun logsNeverIncludeFailureTextAndRepeatedFailureCodesAreCoalesced() {
        val diagnostics = CaptureLifecycleDiagnostics()
        diagnostics.beginEpoch(21)
        val first = diagnostics.failure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE,
            "rtmps://private.invalid secret=PRIVATE /private/recording.mp4", stopping)!!
        assertEquals("code=CAPTURE_FAILURE epoch=21 generation=22 pose=REVERSE stage=STOPPING flags=5", first.logLine())
        repeat(100) { assertNull(diagnostics.failure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE, "Repeated", stopping)) }
        assertEquals(1, diagnostics.transitionCount)
        assertEquals("rtmps://private.invalid secret=PRIVATE /private/recording.mp4", diagnostics.firstFailure!!.message)
        assertFalse(first.logLine().contains("PRIVATE"))
    }
}
