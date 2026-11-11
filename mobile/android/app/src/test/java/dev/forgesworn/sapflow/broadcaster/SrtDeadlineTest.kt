package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class SrtDeadlineTest {
    @Test fun missingDeadlinesDoNotCreateATimer() {
        assertNull(srtDeadlineUs(null, null, null, null))
    }

    @Test fun eachDeadlineSourceCanBeTheOnlyOrEarliestTimer() {
        for (winner in 0..3) {
            val deadlines = arrayOf<Long?>(51, 52, 53, 54)
            deadlines[winner] = 7
            for (mask in 0..15) {
                val values = deadlines.mapIndexed { index, value -> if (mask and (1 shl index) != 0) value else null }
                assertEquals(values.filterNotNull().minOrNull(), srtDeadlineUs(values[0], values[1], values[2], values[3]))
            }
        }
    }

    @Test fun equalDeadlinesKeepTheExactAbsoluteMicrosecondValue() {
        assertEquals(1_007L, srtDeadlineUs(1_007, 1_007, 1_007, 1_007))
        assertEquals(Long.MAX_VALUE, srtDeadlineUs(null, Long.MAX_VALUE, null, Long.MAX_VALUE))
    }

    @Test fun dueAndOverdueDeadlinesRemainDueInsteadOfBeingPostponed() {
        val now = 1_007L
        assertEquals(now, srtDeadlineUs(now + 1, now, null, now + 2))
        assertEquals(now - 1, srtDeadlineUs(now + 1, null, now - 1, now))
        assertEquals(-1L, srtDeadlineUs(0, -1, 1, Long.MAX_VALUE))
    }
}
