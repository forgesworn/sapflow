package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

internal class CaptureProfileTest {
    @Test fun defaultRemainsTheExisting720ProfileWithoutImplicitFallback() {
        val profile = CaptureProfile.DEFAULT
        assertEquals(CaptureProfile.HD_720, profile)
        assertEquals(1280, profile.landscapeWidth); assertEquals(720, profile.landscapeHeight)
        assertEquals(2_500_000, profile.videoBps); assertEquals(96_000, profile.audioBps)
        assertEquals(30, profile.frameRate); assertEquals(4_000_000L, profile.minimumPublishBps)
        assertThrows(IllegalArgumentException::class.java) { profile.requireGrant(1_000_000) }
    }
    @Test fun capEdgesAndPacerUseBitsAndBytesWithoutRoundingUp() {
        for (profile in CaptureProfile.entries) {
            assertThrows(IllegalArgumentException::class.java) { profile.requireGrant(profile.minimumPublishBps - 1) }
            profile.requireGrant(profile.minimumPublishBps)
            assertEquals((profile.minimumPublishBps / 8).toULong(), profile.srtPacingBytesPerSecond(profile.minimumPublishBps))
            assertEquals((profile.minimumPublishBps / 8).toULong(), profile.srtPacingBytesPerSecond(profile.minimumPublishBps + 7))
            assertEquals(1_250_000uL, profile.srtPacingBytesPerSecond(Long.MAX_VALUE))
            assertTrue(profile.elementaryBps < profile.minimumPublishBps)
        }
        assertEquals(696_000, CaptureProfile.SD_480.elementaryBps)
    }
    @Test fun hardwareMustSupportBothOrientationsThirtyFpsAlignmentAndCbr() {
        val p = CaptureProfile.SD_480
        val requests = mutableListOf<Pair<Int, Int>>()
        assertTrue(p.supportedBy(16, 16, 100_000, 4_000_000, true, true, true) { w, h, rate ->
            assertEquals(30.0, rate, 0.0); requests += w to h; true
        })
        assertEquals(listOf(848 to 480, 480 to 848), requests)
        assertFalse(p.supportedBy(16, 16, 100_000, 4_000_000, true, true, true) { w, _, _ -> w == 848 })
        assertFalse(p.supportedBy(32, 16, 100_000, 4_000_000, true, true, true) { _, _, _ -> true })
        assertFalse(p.supportedBy(16, 16, 700_000, 4_000_000, true, true, true) { _, _, _ -> true })
        assertFalse(p.supportedBy(16, 16, 100_000, 4_000_000, true, false, true) { _, _, _ -> true })
        assertFalse(p.supportedBy(16, 16, 100_000, 4_000_000, false, true, true) { _, _, _ -> true })
        assertFalse(p.supportedBy(16, 16, 100_000, 4_000_000, true, true, false) { _, _, _ -> true })
    }
    @Test fun lowerQualityKeepsTheSensorBasisAndExactContentAspectInEveryPose() {
        for (portrait in listOf(true, false)) for (sensor in listOf(90, 270)) for (pose in CardinalPose.entries) {
            val hd = OrientationPlan.forRear(sensor, pose, portrait)
            val sd = OrientationPlan.forRear(sensor, pose, portrait, CaptureProfile.SD_480)
            assertEquals(hd.cameraWidth, sd.cameraWidth); assertEquals(hd.cameraHeight, sd.cameraHeight)
            assertEquals(hd.sourceNaturalWidth, sd.sourceNaturalWidth); assertEquals(hd.sourceNaturalHeight, sd.sourceNaturalHeight)
            assertArrayEquals(hd.encoderPositionMatrix(), sd.encoderPositionMatrix(), 0f)
            assertEquals(848 * 480, sd.width * sd.height)
            val fit = CaptureProfile.fitViewport(sd.width, sd.height, sd.contentWidth, sd.contentHeight)
            assertEquals(fit[2].toLong() * sd.contentHeight, fit[3].toLong() * sd.contentWidth)
            assertEquals(3, sd.width - fit[2] + sd.height - fit[3])
            assertTrue(fit[0] >= 0 && fit[1] >= 0)
            val full = CaptureProfile.fitViewport(hd.width, hd.height, hd.contentWidth, hd.contentHeight)
            assertArrayEquals(intArrayOf(0, 0, hd.width, hd.height), full)
            for (display in listOf(0, 90, 180, 270)) assertArrayEquals(hd.previewPositionMatrix(display), sd.previewPositionMatrix(display), 0f)
        }
    }
    @Test fun queuedPrepareFencesQualityBeforeTheCodecOwnerRuns() {
        val admission = CapturePrepareAdmission(); var task: Runnable? = null; var actual: CaptureProfile? = null
        assertTrue(admission.submit(CaptureProfile.SD_480, true, { task = it; true }) { actual = it })
        assertTrue(admission.pending) // snapshot.canPrepare is now false synchronously.
        var factoryCalls = 0
        assertFalse(admission.submit(CaptureProfile.HD_720, true, { factoryCalls++; true }) { actual = it })
        assertEquals(0, factoryCalls); assertNull(actual)
        task!!.run()
        assertEquals(CaptureProfile.SD_480, actual); assertFalse(admission.pending)
    }
    @Test fun rejectedEnqueueAndExceptionsReleaseOnlyThePrepareReservation() {
        val admission = CapturePrepareAdmission()
        assertFalse(admission.submit(CaptureProfile.DEFAULT, false, { fail("inactive admission"); true }) {})
        assertFalse(admission.submit(CaptureProfile.DEFAULT, true, { false }) {})
        assertFalse(admission.pending)
        assertThrows(IllegalStateException::class.java) { admission.submit(CaptureProfile.DEFAULT, true, { error("closed handler") }) {} }
        assertFalse(admission.pending)
        var task: Runnable? = null
        assertTrue(admission.submit(CaptureProfile.DEFAULT, true, { task = it; true }) { error("hardware rejected") })
        assertThrows(IllegalStateException::class.java) { task!!.run() }
        assertFalse(admission.pending)
    }
    @Test fun stopOrEndRetiresQueuedPrepareAndOldTaskCannotTouchANewProfile() {
        val admission = CapturePrepareAdmission(); var old: Runnable? = null; var latest: Runnable? = null; var actual: CaptureProfile? = null
        assertTrue(admission.submit(CaptureProfile.SD_480, true, { old = it; true }) { actual = it })
        admission.cancelPending()
        assertTrue(admission.pending)
        assertFalse(admission.submit(CaptureProfile.HD_720, true, { fail("old owner retained"); true }) {})
        old!!.run(); assertNull(actual); assertFalse(admission.pending)
        assertTrue(admission.submit(CaptureProfile.HD_720, true, { latest = it; true }) { actual = it })
        old!!.run(); assertTrue(admission.pending); assertNull(actual)
        latest!!.run(); assertEquals(CaptureProfile.HD_720, actual); assertFalse(admission.pending)
    }
}
