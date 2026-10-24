package dev.forgesworn.sapflow.broadcaster
import org.junit.Assert.*
import org.junit.Test
class ReplacementCadenceTest {
    @Test fun audioUsesExactRational1024FrameTimesWithoutAccumulatedRounding() {
        val p=ReplacementCadence(100,1024,48000)
        repeat(1000){n->val exact=100+n*1024L*1_000_000_000/48000;assertEquals(exact,p.claim(exact));assertEquals(-1L,p.claim(exact))}
    }
    @Test fun videoSkipsObsoleteSlotsWithoutSubmittingACatchupBurst() {
        val p=ReplacementCadence(0,1,30,skipObsolete=true)
        assertEquals(100_000_000L,p.claim(110_000_000));assertEquals(-1L,p.claim(110_000_000))
        assertEquals(133_333_333L,p.nextNs)
    }
    @Test fun futureSlateEpochWaitsForItsFirstDeadlineInsteadOfFailingPreparation() {
        val p=ReplacementCadence(100_000_000,1,30,skipObsolete=true)
        assertEquals(-1L,p.claim(90_000_000));assertEquals(100_000_000L,p.claim(100_000_000))
    }
    @Test fun stalledOrRegressedMediaClockFailsInsteadOfFabricatingFreshContent() {
        val p=ReplacementCadence(10,1024,48000);assertEquals(-1L,p.claim(9))
        assertThrows(IllegalStateException::class.java){p.claim(8)}
        assertThrows(IllegalStateException::class.java){ReplacementCadence(0,1024,48000).claim(200_000_001)}
    }
}
