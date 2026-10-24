package dev.forgesworn.sapflow.broadcaster
import org.junit.Assert.*
import org.junit.Test
class AudioGenerationClockTest {
    @Test fun normalBaselineAndReplacementShareOneCursorAndRejectOldGeneration() {
        val p=AudioGenerationClock();val a=p.begin(1,0,false)
        assertEquals(AudioGenerationClock.PRIMING,p.output(a,-1));assertEquals(0L,p.output(a,0))
        assertThrows(IllegalStateException::class.java){p.begin(2,21333,true)}
        p.closed(a);val b=p.begin(2,p.replacementStartUs(100000),true)
        assertEquals(AudioGenerationClock.PRIMING,p.output(b,1000)) // POSITIVE priming, earlier than actual new input boundary.
        assertEquals(1024L,p.output(b,21333));assertEquals(2048L,p.output(b,42667))
        assertEquals(AudioGenerationClock.STALE,p.output(a,64000));assertEquals(3072L,p.nextOutputTick)
    }
    @Test fun freshCodecGapAndOverlapCannotBeNormalisedByLegacyTolerance() {
        for(bad in listOf(21334L,42667L)){
            val p=AudioGenerationClock();val a=p.begin(1,0,false);p.output(a,0);p.closed(a)
            val b=p.begin(2,21333,true)
            if(bad==21334L) assertEquals(1024L,p.output(b,bad)) // Sub-sample microsecond rounding is exact to the same tick.
            else assertThrows(IllegalStateException::class.java){p.output(b,bad)}
        }
    }
    @Test fun resumeAlignmentDropsActualOverlapAndZeroFillsOnlyMeasuredGap() {
        val p=ResumePcmAlignment();p.plan(1024,0,0,2048);assertEquals(0,p.outputBytes);assertEquals(2048,p.offsetBytes)
        p.plan(1024,AudioGenerationClock.microseconds(1000),0,2048)
        assertEquals(48,p.offsetBytes);assertEquals(0,p.zeroFrames);assertEquals(2000,p.copyBytes)
        p.plan(1024,AudioGenerationClock.microseconds(1050),0,2048)
        assertEquals(0,p.offsetBytes);assertEquals(26,p.zeroFrames);assertEquals(1996,p.copyBytes)
        assertThrows(IllegalStateException::class.java){p.plan(0,1_000_000,0,2048)}
    }
    @Test fun repeatedColdCyclesNeverResetTheGlobalSampleCounter() {
        val p=AudioGenerationClock();var next=0L
        repeat(4){n->val g=p.begin(n+1L,AudioGenerationClock.microseconds(next),true)
            repeat(3){assertEquals(next,p.output(g,AudioGenerationClock.microseconds(next)));next+=1024}
            p.closed(g)}
        assertEquals(12288L,p.nextOutputTick)
    }
}
