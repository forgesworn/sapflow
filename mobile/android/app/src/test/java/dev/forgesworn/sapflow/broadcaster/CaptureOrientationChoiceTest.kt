package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CaptureOrientationChoiceTest {
    private fun select(choice: CaptureOrientationChoice, naturalPortrait: Boolean, pose: CardinalPose = CardinalPose.REVERSE) =
        checkNotNull(choice.freeze(true, naturalPortrait, pose))

    @Test fun fixedChoicesUseExplicitPortraitReferenceInBothNaturalDisplayBases() {
        for (pose in CardinalPose.entries) {
            for ((natural, expected) in listOf(true to listOf(0, 270, 90), false to listOf(90, 0, 180))) {
                for ((choice, degrees) in listOf(CaptureOrientationChoice.PORTRAIT, CaptureOrientationChoice.LANDSCAPE_TOP_LEFT,
                    CaptureOrientationChoice.LANDSCAPE_TOP_RIGHT).zip(expected)) {
                    val value = select(choice, natural, pose)
                    assertEquals(degrees, value.initialPose.clockwiseDegrees)
                    assertEquals(natural, value.naturalPortrait)
                    assertFalse(value.automatic)
                }
            }
        }
    }

    @Test fun fixedGeometrySurvivesPermissionWaitAndAutomaticReseedsLatestStablePose() {
        for (natural in listOf(true, false)) for (choice in CaptureOrientationChoice.entries) {
            val frozen = select(choice, natural, CardinalPose.CLOCKWISE_90)
            for (latest in CardinalPose.entries) {
                val prepared = frozen.atPrepare(latest)
                assertEquals(natural, prepared.naturalPortrait)
                assertEquals(choice, prepared.choice)
                if (choice == CaptureOrientationChoice.AUTO) {
                    assertTrue(prepared.automatic)
                    assertEquals(latest, prepared.initialPose)
                } else assertSame(frozen, prepared)
            }
        }
    }

    @Test fun fixedChoicesUseExistingRearGeometryForEverySensorOrientationAndQuality() {
        for (natural in listOf(true, false)) for (sensor in listOf(0, 90, 180, 270))
            for (profile in CaptureProfile.entries) for (choice in CaptureOrientationChoice.entries.filter { it != CaptureOrientationChoice.AUTO }) {
                val selection = select(choice, natural)
                val plan = OrientationPlan.forRear(sensor, selection.initialPose, natural, profile)
                assertEquals(choice == CaptureOrientationChoice.PORTRAIT, plan.isPortrait)
                assertEquals(profile.landscapeWidth, maxOf(plan.width, plan.height))
                assertEquals(profile.landscapeHeight, minOf(plan.width, plan.height))
                val reference = OrientationPlan.forRear(sensor, CardinalPose.NATURAL, natural, profile)
                assertEquals(reference.cameraWidth, plan.cameraWidth)
                assertEquals(reference.cameraHeight, plan.cameraHeight)
                assertEquals(selection.initialPose.clockwiseDegrees, plan.encoderClockwiseDegrees)
                val encoded = plan.encoderPositionMatrix()
                for (display in listOf(0, 90, 180, 270)) {
                    assertArrayEquals(reference.previewPositionMatrix(display), plan.previewPositionMatrix(display), 0f)
                    assertArrayEquals(encoded, plan.encoderPositionMatrix(), 0f)
                    assertEquals((720 - selection.initialPose.clockwiseDegrees - display) % 360, plan.previewRelativeClockwiseDegrees(display))
                }
            }
    }

    @Test fun microphoneOnlyHasNoChoiceGeometryOrOrientationEpoch() {
        for (choice in CaptureOrientationChoice.entries) for (natural in listOf(true, false)) for (pose in CardinalPose.entries)
            assertNull(choice.freeze(false, natural, pose))
    }

    @Test fun automaticPreservesAllExistingPhysicalPosesWithoutDisplayInference() {
        for (natural in listOf(true, false)) for (pose in CardinalPose.entries) {
            val selection = select(CaptureOrientationChoice.AUTO, natural, pose)
            assertEquals(pose, selection.initialPose)
            assertTrue(selection.automatic)
        }
        assertEquals("Automatic", CaptureOrientationChoice.AUTO.label)
        assertEquals("Landscape (top left)", CaptureOrientationChoice.LANDSCAPE_TOP_LEFT.label)
        assertEquals("Landscape (top right)", CaptureOrientationChoice.LANDSCAPE_TOP_RIGHT.label)
    }
}
