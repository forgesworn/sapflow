package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class DelayDiagnosticsTest {
    @Test fun maximumAndSlowCountSurviveSubsequentFastSamples() {
        val counters = DelayDiagnostics()
        counters.record(99_999_999)
        counters.record(120_000_000)
        counters.record(100_000_000)
        counters.record(1)
        assertEquals("n 4 avg 80000us max 120ms/slow 2", counters.text())
    }
}
