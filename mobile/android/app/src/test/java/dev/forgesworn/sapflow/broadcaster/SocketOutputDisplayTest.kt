package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class SocketOutputDisplayTest {
    private fun snapshot(output: SocketOutputView) = CaptureSnapshot(
        stage = CaptureStage.IDLE, message = "", videoEncoder = "", audioEncoder = "",
        videoSamples = 0, audioSamples = 0, drops = 0, videoRejects = 0,
        audioPrimingRejects = 0, waitingKeyframeRejects = 0, queueRejects = 0,
        audioLevel = 0f, videoCaptureAgeMs = null, publishingStatus = "", recordingStatus = "",
        queueDiagnostics = "", sinkDiagnostics = "", orientation = "", renderer = "",
        audioDiagnostics = "", cleanupDiagnostics = "", canPrepare = true, socketOutput = output,
    )

    private fun output(phase: SocketOutputPhase, ended: Boolean, age: Long?) = SocketOutputView(
        SocketOutputAvailability.UNKNOWN, null, 123, age, SocketOutputAccounting.SRT_DATAGRAM,
        phase, ended, true, 9, 10, 11,
    )

    @Test fun finishedSnapshotsIgnoreOnlyAgeAndKeepFinalAccounting() {
        val finished = output(SocketOutputPhase.CLOSED, true, 0)
        val display = snapshot(finished).forDisplay()
        for (age in listOf(200L, 400L, 800L, 0L)) {
            val raw = snapshot(finished.copy(sampleAgeMs = age))
            assertEquals(display, raw.forDisplay())
            assertEquals(age, raw.socketOutput.sampleAgeMs)
        }
        assertEquals(finished.copy(sampleAgeMs = null), display.socketOutput)
        assertEquals("Final reading", socketOutputReadingLabel(display.socketOutput))
        assertNotEquals(display, snapshot(finished.copy(completedBytes = 124)).forDisplay())
        assertNotEquals(display, snapshot(finished.copy(incomplete = false)).forDisplay())
        assertNotEquals(display, snapshot(finished.copy(attempt = 12)).forDisplay())
        assertNotEquals(display, snapshot(finished.copy(phase = SocketOutputPhase.CLOSING)).forDisplay())
    }

    @Test fun activeClosingAndClosedWithoutEndKeepTheirAge() {
        for ((phase, ended) in listOf(SocketOutputPhase.ACTIVE to false, SocketOutputPhase.CLOSING to true, SocketOutputPhase.CLOSED to false)) {
            val raw = snapshot(output(phase, ended, 200))
            assertSame(raw, raw.forDisplay())
            assertEquals("sample age 200ms", socketOutputReadingLabel(raw.forDisplay().socketOutput))
        }
        assertEquals("sample age Unknown", socketOutputReadingLabel(null))
    }

    @Test fun samplerStaysRawAndNewBindingStillWarmsUp() {
        class Source(var closed: Boolean) : SocketOutputSource {
            var reads = 0
            override fun socketSnapshot(nowNs: Long): SocketOutputSample {
                reads++
                return SocketOutputSample(this, nowNs, 123, SocketOutputAccounting.SRT_DATAGRAM, false,
                    if (closed) SocketOutputPhase.CLOSED else SocketOutputPhase.ACTIVE,
                    endedAtNs = if (closed) 0 else null)
            }
        }
        val sampler = SocketOutputSampler()
        val old = Any(); val source = Source(true)
        val first = sampler.sample(0, old, source, true)
        val later = sampler.sample(200_000_000, old, source, true)
        assertEquals(200L, later.sampleAgeMs)
        assertEquals(snapshot(first).forDisplay(), snapshot(later).forDisplay())
        assertEquals(1, source.reads)
        sampler.sample(1_000_000_000, old, source, true)
        assertEquals(2, source.reads)
        val next = Source(false)
        val warm = sampler.sample(1_200_000_000, Any(), next, true)
        assertEquals(SocketOutputAvailability.WARMING_UP, warm.availability)
        assertEquals(0L, snapshot(warm).forDisplay().socketOutput.sampleAgeMs)
        assertEquals("sample age 0ms", socketOutputReadingLabel(warm))
    }
}
