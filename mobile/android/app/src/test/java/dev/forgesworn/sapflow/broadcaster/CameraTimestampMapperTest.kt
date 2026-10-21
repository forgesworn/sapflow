package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CameraTimestampMapperTest {
    @Test fun explicitMonotonicFramesPreserveTheirPresentationTimeAndAge() {
        var now = 5_000_000_000L
        val mapper = CameraTimestampMapper.monotonic { now }
        assertEquals(now - 20_000_000, mapper.map(now - 20_000_000))
        now += 33_333_333
        assertEquals(now - 10_000_000, mapper.map(now - 10_000_000))
        assertThrows(IllegalArgumentException::class.java) { mapper.map(now - 10_000_000) }
        assertThrows(IllegalArgumentException::class.java) { mapper.map(now + 6_000_000) }
    }

    @Test fun realtimeCameraUsesMeasuredSuspendOffsetAndNeverCallbackDelay() {
        var mono = 5_000_000_000L
        val suspend = 16_000_000_000L
        val mapper = CameraTimestampMapper.realtime(bootNs = { mono + suspend }, monotonicNs = { mono })
        assertEquals(mono - 50_000_000, mapper.map(mono + suspend - 50_000_000))
        mono += 2_000_000_000
        assertEquals(mono - 70_000_000, mapper.map(mono + suspend - 70_000_000))
    }

    @Test fun changedOffsetAndUncertainCalibrationRejectTheEpoch() {
        var mono = 5_000_000_000L
        var suspend = 16_000_000_000L
        val mapper = CameraTimestampMapper.realtime({ mono + suspend }, { mono })
        mapper.map(mono + suspend)
        mono += 2_000_000_000; suspend += 10_000_000
        assertThrows(IllegalArgumentException::class.java) { mapper.map(mono + suspend) }
        var boot = 1_000_000_000L
        assertThrows(IllegalArgumentException::class.java) {
            CameraTimestampMapper.realtime({ boot.also { boot += 2_000_000 } }, { 0 })
        }
    }

    @Test fun narrowestBracketIsChosenWithCheckedMidpointArithmetic() {
        val boot = listOf(1000L, 1100L, 2000L, 2020L, 3000L, 3060L).iterator()
        val mono = listOf(100L, 1050L, 2080L).iterator()
        assertEquals(CameraTimestampMapper.Calibration(960, 10), CameraTimestampMapper.calibrate({ boot.next() }, { mono.next() }))
        val extreme = CameraTimestampMapper.calibrate({ Long.MAX_VALUE }, { 0 })
        assertEquals(Long.MAX_VALUE, extreme.offsetNs)
        assertEquals(0, extreme.uncertaintyNs)
    }

    @Test fun staleUnknownOrRegressingSourceValuesNeverBecomeValidMediaTime() {
        var now = 5_000_000_000L
        val mapper = CameraTimestampMapper.monotonic { now }
        assertThrows(IllegalArgumentException::class.java) { mapper.map(-1) }
        assertThrows(IllegalArgumentException::class.java) { mapper.map(0) }
        mapper.map(now)
        now -= 1
        assertThrows(IllegalArgumentException::class.java) { mapper.map(now + 100) }
        assertThrows(IllegalArgumentException::class.java) { CameraTimestampMapper.realtime({ -1 }, { 0 }) }
    }
}
