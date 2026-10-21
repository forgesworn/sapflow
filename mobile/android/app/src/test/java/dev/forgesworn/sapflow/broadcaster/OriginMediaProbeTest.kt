package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

internal class OriginMediaProbeTest {
    private fun profile(prefix: String) = checkNotNull(V4vProfile.approved("https://api.example.invalid", "https://play.example.invalid:9443", prefix))
    @Test fun approvedLiveDirectoryReplacementHasNoAuthorityFromReceipt() {
        val path = "/live/event-fixture/abc_part42.mp4"
        for ((prefix, expected) in listOf("/live/" to "/live/event-fixture/abc_part42.mp4", "/media/live/" to "/media/live/event-fixture/abc_part42.mp4", "/origin/" to "/origin/event-fixture/abc_part42.mp4", "/origin/live/" to "/origin/live/event-fixture/abc_part42.mp4")) {
            assertEquals("https://play.example.invalid:9443$expected", ApprovedPlaybackRoute(profile(prefix)).resource(path))
        }
        for (bad in listOf("https://other.invalid$path", "//other.invalid$path", "/live/event-fixture/../abc_part42.mp4", "$path?token=x", "$path#x", "/live/event-fixture/%61bc_part42.mp4", "/api/auth/nostr/login", "/live/event-fixture/abc_part-1.mp4"))
            assertThrows(IllegalArgumentException::class.java) { ApprovedPlaybackRoute(profile("/live/")).resource(bad) }
    }
    private fun box(kind: String, size: Int = 8): ByteArray = byteArrayOf((size ushr 24).toByte(), (size ushr 16).toByte(), (size ushr 8).toByte(), size.toByte()) + kind.toByteArray() + ByteArray((size - 8).coerceAtLeast(0))
    @Test fun finiteMp4ShapeRefusesTruncationExtendedAndUnboundedBoxes() {
        validateProbeMp4(box("ftyp") + box("moov"), true)
        validateProbeMp4(box("moof") + box("mdat"), false)
        for (bad in listOf(byteArrayOf(), box("moof"), box("moof") + box("mdat").copyOf(7), box("ftyp") + box("moov"), box("moof") + box("mdat") + box("mdat"), byteArrayOf(0, 0, 0, 0, 109, 100, 97, 116), byteArrayOf(0, 0, 0, 1, 109, 100, 97, 116), ByteArray(OriginProbeLimits.PART_BYTES + 1)))
            assertThrows(IllegalArgumentException::class.java) { validateProbeMp4(bad, false) }
    }
    @Test fun outputEvidenceRequiresActualAdvancingFramesAndPcmCadence() {
        val audio = DecodedProbeProgress(false, 1)
        assertThrows(IllegalArgumentException::class.java) { audio.evidence("native-aac", true) }
        audio.output(2048, 1_000_000); audio.output(2048, 1_021_333)
        val result = audio.evidence("native-aac", true)
        assertEquals(4096L, result.decodedBytes); assertTrue(result.software); assertEquals(1_042_666L, result.lastPtsUs)
        assertThrows(IllegalArgumentException::class.java) { audio.output(2048, 1_021_333) }
        assertThrows(IllegalArgumentException::class.java) { audio.output(2048, 1_099_999) }
        assertThrows(IllegalArgumentException::class.java) { audio.output(3, 1_042_666) }
        val video = DecodedProbeProgress(true, 1280 * 720 * 4)
        video.output(1280 * 720, 1_000_000); video.output(1280 * 720, 1_033_333)
        assertEquals(2, video.evidence("hardware-avc", false).outputs)
    }
    @Test fun aacCoalescingUsesDecodedFramesAndExtentRatherThanCallbackCount() {
        val coalesced = DecodedProbeProgress(false, 1)
        coalesced.output(4096, 123_000)
        val single = coalesced.evidence("native-aac", true)
        assertEquals(1, single.outputs); assertEquals(42_666L, single.lastPtsUs - single.firstPtsUs)
        val split = DecodedProbeProgress(false, 1)
        split.output(1024, 123_000); split.output(1024, 133_666)
        assertThrows(IllegalArgumentException::class.java) { split.evidence("native-aac", true) }
        split.output(2048, 144_333)
        assertEquals(4096L, split.evidence("native-aac", true).decodedBytes)
        for (step in listOf(20_333L, 22_333L, 0L, 21_300L)) {
            val broken = DecodedProbeProgress(false, 1); broken.output(2048, 123_000)
            assertThrows(IllegalArgumentException::class.java) { broken.output(2048, 123_000 + step) }
        }
    }
    @Test fun cancellationAndOriginalDeadlineAreIndependentOfClockProgress() {
        var now = 10L
        val policy = OriginProbeCancellation({ now }, 100)
        assertEquals(90L, policy.remainingNs()); now = 100
        assertThrows(IllegalStateException::class.java) { policy.check() }
        now = 10; val stopped = OriginProbeCancellation({ now }, 100)
        stopped.cancel(); assertThrows(IllegalStateException::class.java) { stopped.check() }
    }
}
