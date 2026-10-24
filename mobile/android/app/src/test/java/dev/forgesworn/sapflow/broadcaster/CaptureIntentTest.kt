package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CaptureIntentTest {
    private val audio = CaptureIntent.AudioOnly48kMono
    @Test fun audioHasNoVideoGeometryAndRequestsOnlyMicrophone() {
        assertFalse(audio.hasVideo); assertTrue(audio.hasAudio); assertNull(audio.videoProfile)
        assertEquals(listOf("android.permission.RECORD_AUDIO"), audio.requiredPermissions)
        assertTrue(audio.permissionsGranted { it == "android.permission.RECORD_AUDIO" })
        assertFalse(CaptureIntent.DEFAULT.permissionsGranted { it == "android.permission.RECORD_AUDIO" })
        assertFalse(audio.permissionsGranted { false })
        assertEquals(CaptureProfile.DEFAULT, CaptureIntent.DEFAULT.videoProfile)
    }
    @Test fun fixedAudioBudgetIsNotTheEngineeringWireCeiling() {
        assertEquals(96_000, audio.elementaryBps); assertEquals(1_000_000L, audio.minimumPublishBps)
        audio.requireGrant(1_000_000); assertEquals(125_000uL, audio.srtPacingBytesPerSecond(1_000_000))
        assertThrows(IllegalArgumentException::class.java) { audio.requireGrant(999_999) }
        assertEquals(4_000_000L, CaptureIntent.DEFAULT.minimumPublishBps)
        assertTrue(RecordingStorage.budget(audio).payloadBytes < RecordingStorage.budget(CaptureProfile.SD_480).payloadBytes)
    }
    @Test fun aacConfigurationAndSamplesNeverWaitForAbsentVideoOrIdr() {
        assertFalse(audio.codecsReady(false, false)); assertTrue(audio.codecsReady(false, true))
        assertThrows(IllegalArgumentException::class.java) { audio.codecsReady(true, true) }
        assertFalse(CaptureIntent.DEFAULT.codecsReady(false, true))
        val gate = FirstKeyframeGate(audio.hasVideo)
        repeat(3) { assertTrue(gate.admit({ error("No video metadata lookup for audio-only") }, { error("No IDR lookup") })) }
        assertFalse(FirstKeyframeGate().admit({ false }, { error("No audio IDR") }))
    }
    @Test fun exactNativeTrackTopologyRejectsHiddenVideoAndMissingAac() {
        audio.requireTrackMimes(listOf("audio/mp4a-latm"))
        CaptureIntent.DEFAULT.requireTrackMimes(listOf("audio/mp4a-latm", "video/avc"))
        for (bad in listOf(emptyList(), listOf("video/avc"), listOf("audio/mp4a-latm", "video/avc"), listOf("audio/mp4a-latm", "audio/mp4a-latm"))) {
            assertThrows(IllegalArgumentException::class.java) { audio.requireTrackMimes(bad) }
        }
        assertFalse(audio.matchesTracks(true, true)); assertFalse(audio.matchesTracks(false, false))
    }
    @Test fun queuedPrepareFreezesTopologyAndCancellationCannotAdoptSuccessor() {
        val admission = CapturePrepareAdmission(); val work = mutableListOf<Runnable>(); val adopted = mutableListOf<CaptureIntent>()
        assertTrue(admission.submit(audio, true, { work.add(it); true }, adopted::add))
        assertFalse(admission.submit(CaptureIntent.DEFAULT, true, { work.add(it); true }, adopted::add))
        admission.cancelPending(); work.removeAt(0).run(); assertTrue(adopted.isEmpty())
        assertTrue(admission.submit(CaptureIntent.DEFAULT, true, { work.add(it); true }, adopted::add))
        work.removeAt(0).run(); assertEquals(listOf(CaptureIntent.DEFAULT), adopted)
    }
    @Test fun startAndEndAreBoundToOriginalAudioPreparation() {
        val policy = CaptureStartAdmission(); val a = checkNotNull(policy.reserve(audio))
        assertSame(audio, a.intent); assertTrue(policy.claim(a)); assertTrue(policy.beginEpoch(a, 1)); assertTrue(policy.prepared(a, 1))
        val start = checkNotNull(policy.start(a)); policy.cancelInitial(a)
        assertFalse(policy.commit(start)); policy.beginCleanup(a, false); policy.cleanup(a, true)
        val b = checkNotNull(policy.reserve(CaptureIntent.DEFAULT)); assertFalse(policy.permits(start)); assertTrue(b.intent.hasVideo)
    }
    @Test fun audioStopRequiresActualAacEosButNoInventedVideoEos() {
        assertFalse(audio.tailComplete(false, false)); assertTrue(audio.tailComplete(false, true))
        assertFalse(CaptureIntent.DEFAULT.tailComplete(false, true))
        assertFalse(CaptureIntent.DEFAULT.tailComplete(true, false)); assertTrue(CaptureIntent.DEFAULT.tailComplete(true, true))
    }
}
