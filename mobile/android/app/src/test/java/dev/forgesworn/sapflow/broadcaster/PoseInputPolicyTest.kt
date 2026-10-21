package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class PoseInputPolicyTest {
    @Test fun releaseDefaultRejectsEveryHookValueWithoutChangingPhysicalSelection() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL)
        for (value in CardinalPose.entries.map { it.name } + "CLEAR") {
            assertEquals(PoseInputPolicy.Result.DISABLED, policy.request(value, true, 0))
        }
        assertNull(policy.overridePose)
        policy.acceptPhysical(90, 0)
        assertEquals(CardinalPose.CLOCKWISE_90, policy.settle(250_000_000, false))
    }

    @Test fun invalidAndTransitionCommandsDoNotAcquireOverrideOrCancelRealSensorDwell() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL, true)
        policy.acceptPhysical(90, 0)
        for (value in listOf(null, "", "90", "NATURAL ", "start", "rtmp://invalid", "x".repeat(1000))) {
            assertEquals(PoseInputPolicy.Result.INVALID, policy.request(value, true, 1))
        }
        assertEquals(PoseInputPolicy.Result.INACTIVE, policy.request("REVERSE", false, 1))
        assertNull(policy.overridePose)
        assertEquals(250_000_000L, policy.deadlineNs)
        assertEquals(CardinalPose.CLOCKWISE_90, policy.settle(250_000_000, false))
    }

    @Test fun eachCardinalUsesTheRealQuarterSecondSelectorAndTimerWithoutAnotherEvent() {
        for (pose in CardinalPose.entries) {
            val initial = if (pose == CardinalPose.NATURAL) CardinalPose.REVERSE else CardinalPose.NATURAL
            val policy = PoseInputPolicy(initial, true)
            assertEquals(PoseInputPolicy.Result.ACCEPTED, policy.request(pose.name, true, 0))
            assertEquals(initial, policy.settle(249_999_999, true))
            assertEquals(pose, policy.settle(250_000_000, true))
            assertNull(policy.deadlineNs)
        }
    }

    @Test fun physicalDwellContinuesUnderOverrideAndClearNeedsNoFurtherSensorEvent() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL, true)
        policy.request("CLOCKWISE_90", true, 0)
        policy.acceptPhysical(-1, 10)
        policy.acceptPhysical(270, 20)
        assertEquals(CardinalPose.CLOCKWISE_90, policy.settle(250_000_000, true))
        assertEquals(250_000_020L, policy.deadlineNs)
        assertEquals(CardinalPose.CLOCKWISE_90, policy.settle(250_000_020, true))
        assertNull(policy.deadlineNs)
        assertEquals(PoseInputPolicy.Result.CLEARED, policy.request("CLEAR", true, 400_000_000))
        assertNull(policy.overridePose)
        assertEquals(CardinalPose.CLOCKWISE_270, policy.pose)
    }

    @Test fun explicitStopCancelsSyntheticDwellAndFencesStaleCapturingSnapshotUntilPrepare() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL, true)
        policy.acceptPhysical(270, 0)
        policy.request("REVERSE", true, 1)
        policy.stopCapture()
        assertTrue(policy.captureStopped)
        assertNull(policy.overridePose)
        // The remaining timer is physical only; it cannot notify the controller
        // while stopped. No further OEL event is required to restore this pose.
        assertEquals(250_000_000L, policy.deadlineNs)
        assertEquals(CardinalPose.CLOCKWISE_270, policy.settle(250_000_001, true))
        assertEquals(PoseInputPolicy.Result.INACTIVE, policy.request("REVERSE", true, 300_000_000))
        assertEquals(PoseInputPolicy.Result.CLEARED, policy.request("CLEAR", false, 300_000_001))
        assertEquals(CardinalPose.CLOCKWISE_270, policy.prepareSession())
        assertFalse(policy.captureStopped)
        assertEquals(PoseInputPolicy.Result.ACCEPTED, policy.request("REVERSE", true, 400_000_000))
        assertEquals(CardinalPose.REVERSE, policy.settle(650_000_000, true))
    }

    @Test fun clearDuringErrorAndDisposeNeverSettleAnUnfinishedSyntheticPose() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL, true)
        policy.acceptPhysical(270, 0)
        policy.request("REVERSE", true, 1)
        assertEquals(PoseInputPolicy.Result.CLEARED, policy.request("CLEAR", false, 2))
        assertNull(policy.overridePose)
        // CLEAR does not force an unsettled physical angle into the stable pose.
        assertEquals(CardinalPose.NATURAL, policy.pose)
        assertEquals(CardinalPose.CLOCKWISE_270, policy.settle(300_000_000, false))
        policy.request("REVERSE", true, 400_000_000)
        assertEquals(CardinalPose.CLOCKWISE_270, policy.settle(650_000_000, false))
        assertNull(policy.deadlineNs)
        policy.reset()
        assertTrue(policy.captureStopped)
        assertNull(policy.overridePose); assertNull(policy.deadlineNs)
        assertEquals(PoseInputPolicy.Result.DISABLED, policy.request("REVERSE", true, 700_000_000))
    }

    @Test fun finiteForegroundBudgetSurvivesClearStopAndPrepareButDisposalDisablesAdmission() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL, true)
        repeat(16) { index ->
            val pose = if (index % 2 == 0) "CLOCKWISE_90" else "REVERSE"
            assertEquals(PoseInputPolicy.Result.ACCEPTED, policy.request(pose, true, index.toLong()))
        }
        assertEquals(PoseInputPolicy.Result.LIMIT, policy.request("CLOCKWISE_270", true, 20))
        assertEquals(PoseInputPolicy.Result.CLEARED, policy.request("CLEAR", false, 21))
        policy.stopCapture()
        policy.prepareSession()
        assertEquals(PoseInputPolicy.Result.LIMIT, policy.request("CLOCKWISE_270", true, 22))
        policy.reset()
        assertEquals(PoseInputPolicy.Result.DISABLED, policy.request("CLOCKWISE_270", true, 23))
        policy.enableQualification(true)
        policy.prepareSession()
        assertEquals(PoseInputPolicy.Result.ACCEPTED, policy.request("CLOCKWISE_270", true, 24))
    }

    @Test fun disabledSensorPathRetainsProductionMotionFlatAndSettleBehaviour() {
        val policy = PoseInputPolicy(CardinalPose.NATURAL)
        val production = CardinalPoseSelector(CardinalPose.NATURAL)
        for ((angle, now) in listOf(90 to 0L, 135 to 100_000_000L, 90 to 200_000_000L,
            -1 to 300_000_000L, 270 to 400_000_000L)) {
            assertEquals(production.accept(angle, now), policy.acceptPhysical(angle, now))
            assertEquals(production.deadlineNs, policy.deadlineNs)
        }
        assertEquals(production.settle(650_000_000), policy.settle(650_000_000, false))
    }
}
