package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class OrientationRestartPlanTest {
    @Test fun preparedChangeIsDormantAndRunningChangesCoalesceAtOriginalDeadline() {
        val plan = OrientationRestartPlan(budgetNs = 100)
        assertNull(plan.request(CardinalPose.REVERSE, running = false, 0))
        assertEquals(CardinalPose.REVERSE, plan.desiredPose)
        val first = plan.request(CardinalPose.CLOCKWISE_90, running = true, 10)!!
        assertEquals(110, first.deadlineNs)
        assertNull(plan.request(CardinalPose.CLOCKWISE_270, running = true, 70))
        assertNull(plan.request(CardinalPose.NATURAL, running = false, 90))
        assertEquals(first, plan.pending)
        assertEquals(CardinalPose.NATURAL, plan.complete(first.token, 109))
        assertNull(plan.complete(first.token, 109))
    }

    @Test fun timeoutAndRepeatedPoseChangesCannotInventANewGraceWindow() {
        val plan = OrientationRestartPlan(budgetNs = 100)
        val first = plan.request(CardinalPose.CLOCKWISE_90, true, 0)!!
        assertTrue(plan.expired(100))
        assertNull(plan.complete(first.token, 100))
        assertNull(plan.request(CardinalPose.REVERSE, true, 110))
        assertNull(plan.request(CardinalPose.NATURAL, true, 120))
        assertNull(plan.pending)
        plan.beginSession()
        val next = plan.request(CardinalPose.REVERSE, true, 130)!!
        assertNotEquals(first.token, next.token)
        assertEquals(230, next.deadlineNs)
        assertNull(plan.complete(first.token, 140))
        assertEquals(CardinalPose.REVERSE, plan.complete(next.token, 150))
    }

    @Test fun poseNotificationAtDeadlineCannotHideTimeoutFromControlTimer() {
        val plan = OrientationRestartPlan(budgetNs = 100)
        val first = plan.request(CardinalPose.CLOCKWISE_90, true, 0)!!
        assertNull(plan.request(CardinalPose.CLOCKWISE_90, true, 100))
        assertNull(plan.request(CardinalPose.REVERSE, true, 110))
        assertEquals(first, plan.pending)
        assertTrue(plan.expired(110))
        assertNull(plan.request(CardinalPose.NATURAL, true, 120))
        assertNull(plan.pending)
        plan.beginSession()
        val next = plan.request(CardinalPose.REVERSE, true, 130)!!
        assertNull(plan.complete(first.token, 240))
        assertEquals(next, plan.pending)
        assertTrue(plan.expired(240))
    }

    @Test fun stopBackgroundOrFailureCancellationFencesLateCompletion() {
        val plan = OrientationRestartPlan(budgetNs = 100)
        val ticket = plan.request(CardinalPose.CLOCKWISE_90, true, 0)!!
        plan.cancel()
        assertNull(plan.complete(ticket.token, 20))
        assertNull(plan.request(CardinalPose.REVERSE, true, 30))
        plan.beginSession()
        assertNotNull(plan.request(CardinalPose.NATURAL, true, 40))
        assertThrows(IllegalArgumentException::class.java) { plan.expired(39) }
    }

    @Test fun exactDeadlineAndHostileClockValuesFailClosed() {
        val plan = OrientationRestartPlan(budgetNs = 100)
        val ticket = plan.request(CardinalPose.REVERSE, true, 0)!!
        assertNull(plan.complete(ticket.token, 100))
        assertNull(plan.pending)
        assertThrows(IllegalArgumentException::class.java) { plan.request(CardinalPose.NATURAL, false, -1) }
        assertThrows(IllegalArgumentException::class.java) { OrientationRestartPlan(budgetNs = 0) }
        assertThrows(ArithmeticException::class.java) { OrientationRestartPlan().request(CardinalPose.REVERSE, true, Long.MAX_VALUE) }
    }
}
