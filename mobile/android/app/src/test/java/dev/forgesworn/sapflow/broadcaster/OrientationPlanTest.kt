package dev.forgesworn.sapflow.broadcaster

import android.opengl.EGL14
import org.junit.Assert.*
import org.junit.Test

class OrientationPlanTest {
    @Test fun rawSensorAndNaturalDimensionsRemainSeparateFromOutputPose() {
        val rotations = mapOf(0 to listOf(0, 90, 180, 270), 90 to listOf(90, 180, 270, 0),
            180 to listOf(180, 270, 0, 90), 270 to listOf(270, 0, 90, 180))
        for (naturalPortrait in listOf(true, false)) for (sensor in listOf(0, 90, 180, 270)) {
            val naturalWidth = if (naturalPortrait) 720 else 1280
            val naturalHeight = if (naturalPortrait) 1280 else 720
            CardinalPose.entries.forEachIndexed { index, pose ->
                val plan = OrientationPlan.forRear(sensor, pose, naturalPortrait)
                assertEquals(rotations.getValue(sensor)[index], plan.rawToEncodedClockwiseDegrees)
                assertEquals(if (sensor % 180 == 0) naturalWidth else naturalHeight, plan.cameraWidth)
                assertEquals(if (sensor % 180 == 0) naturalHeight else naturalWidth, plan.cameraHeight)
                assertEquals(if (index % 2 == 0) naturalWidth else naturalHeight, plan.width)
                assertEquals(if (index % 2 == 0) naturalHeight else naturalWidth, plan.height)
                assertEquals(1280 * 720, plan.width * plan.height)
            }
        }
        assertThrows(IllegalArgumentException::class.java) { OrientationPlan.forRear(45, CardinalPose.NATURAL) }
    }

    // Independently specified Camera2 producer sampling coordinates. These
    // fixtures include GL's Y flip and sensor rotation, before our vertex stage.
    private fun producer(sensor: Int, sx: Float = 0.8f, sy: Float = 0.8f): FloatArray {
        val matrix = when (sensor) {
            0 -> floatArrayOf(1f, 0f, 0f, 0f, 0f, -1f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 1f, 0f, 1f)
            90 -> floatArrayOf(0f, -1f, 0f, 0f, -1f, 0f, 0f, 0f, 0f, 0f, 1f, 0f, 1f, 1f, 0f, 1f)
            180 -> floatArrayOf(-1f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 0f, 0f, 1f, 0f, 1f, 0f, 0f, 1f)
            else -> floatArrayOf(0f, 1f, 0f, 0f, 1f, 0f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 0f, 0f, 1f)
        }
        for (index in listOf(0, 4, 12)) matrix[index] *= sx
        for (index in listOf(1, 5, 13)) matrix[index] *= sy
        matrix[12] += (1f - sx) * 0.3f
        matrix[13] += (1f - sy) * 0.6f
        return matrix
    }

    /** Software shader oracle: raster output -> inverse vertex transform ->
     * original mesh UV -> complete producer matrix -> raw-buffer pixel UV. */
    private fun sample(position: FloatArray, texture: FloatArray, u: Float, v: Float): Pair<Float, Float> {
        val x = 2f * u - 1f; val y = 1f - 2f * v
        val meshX = position[0] * x + position[1] * y
        val meshY = position[4] * x + position[5] * y
        val s = (meshX + 1f) / 2f; val t = (meshY + 1f) / 2f
        return texture[0] * s + texture[4] * t + texture[12] to
            texture[1] * s + texture[5] * t + texture[13]
    }

    private fun expectedRaw(rotation: Int, u: Float, v: Float): Pair<Float, Float> = when (rotation) {
        0 -> u to v
        90 -> v to 1f - u
        180 -> 1f - u to 1f - v
        else -> 1f - v to u
    }

    private fun assertSample(expected: Pair<Float, Float>, actual: Pair<Float, Float>) {
        assertEquals(0.06f + 0.8f * expected.first, actual.first, 0.00001f)
        assertEquals(0.12f + 0.8f * expected.second, actual.second, 0.00001f)
    }

    @Test fun composedProducerAndEncoderKeepAsymmetricLabelsUprightInAllPhysicalPoses() {
        // Public CaptureRequest.JPEG_ORIENTATION rear-camera sample specifies
        // sensor + clockwise OrientationEventListener, not preview-doc signs.
        val rotations = mapOf(0 to listOf(0, 90, 180, 270), 90 to listOf(90, 180, 270, 0),
            180 to listOf(180, 270, 0, 90), 270 to listOf(270, 0, 90, 180))
        val labels = listOf(0f to 0f, 1f to 0f, 1f to 1f, 0f to 1f, 0.1f to 0.2f, 0.8f to 0.15f)
        for (portrait in listOf(true, false)) for (sensor in listOf(0, 90, 180, 270)) {
            CardinalPose.entries.forEachIndexed { index, pose ->
                val plan = OrientationPlan.forRear(sensor, pose, portrait)
                val texture = producer(sensor)
                CameraTextureGeometry.validate(texture, sensor, plan.cameraWidth, plan.cameraHeight)
                for ((u, v) in labels) assertSample(expectedRaw(rotations.getValue(sensor)[index], u, v), sample(plan.encoderPositionMatrix(), texture, u, v))
            }
        }
    }

    @Test fun composedEncoderPreservesMetricPixelAspectInsteadOfStretchingQuarterTurn() {
        for (portrait in listOf(true, false)) for (sensor in listOf(90, 270)) for (pose in CardinalPose.entries) {
            val plan = OrientationPlan.forRear(sensor, pose, portrait)
            val texture = producer(sensor)
            val matrix = plan.encoderPositionMatrix()
            val centre = sample(matrix, texture, 0.5f, 0.5f)
            fun distance(point: Pair<Float, Float>): Double {
                val dx = (point.first - centre.first) * plan.cameraWidth
                val dy = (point.second - centre.second) * plan.cameraHeight
                return kotlin.math.sqrt((dx * dx + dy * dy).toDouble())
            }
            assertEquals(48.0, distance(sample(matrix, texture, 0.5f + 60f / plan.width, 0.5f)), 0.001)
            assertEquals(48.0, distance(sample(matrix, texture, 0.5f, 0.5f + 60f / plan.height)), 0.001)
        }
    }

    @Test fun previewComposesProducerToDisplayIndependentlyOfEncoderPoseIncludingRotationLock() {
        for (portrait in listOf(true, false)) for (sensor in listOf(90, 270)) for (pose in CardinalPose.entries) {
            val plan = OrientationPlan.forRear(sensor, pose, portrait)
            for (display in listOf(0, 90, 180, 270)) {
                val actual = sample(plan.previewPositionMatrix(display), producer(sensor), 0.2f, 0.7f)
                assertSample(expectedRaw((sensor - display + 360) % 360, 0.2f, 0.7f), actual)
                val extra = plan.previewRelativeClockwiseDegrees(display)
                val previewWidth = if (extra % 180 == 0) plan.width else plan.height
                val previewHeight = if (extra % 180 == 0) plan.height else plan.width
                assertEquals(if (display % 180 == 0) plan.naturalWidth else plan.naturalHeight, previewWidth)
                assertEquals(if (display % 180 == 0) plan.naturalHeight else plan.naturalWidth, previewHeight)
            }
        }
        assertThrows(IllegalArgumentException::class.java) { OrientationPlan.forRear(90, CardinalPose.NATURAL).previewPositionMatrix(1) }
    }

    @Test fun physicalRearCameraFixturesPutGroundBelowSkyForBothLandscapeDirections() {
        // Raw sensor-space ground positions for an upright world chart as the
        // rear camera turns through OEL 0,90,180,270. Independent fixed image
        // fixtures: sensor90 ground RIGHT,TOP,LEFT,BOTTOM; sensor270 the inverse.
        // These resolve the physical viewpoint, which a rotation-only oracle
        // with an assumed sign cannot establish.
        val ground = mapOf(
            90 to listOf(0.8f to 0.5f, 0.5f to 0.2f, 0.2f to 0.5f, 0.5f to 0.8f),
            270 to listOf(0.2f to 0.5f, 0.5f to 0.8f, 0.8f to 0.5f, 0.5f to 0.2f),
        )
        val rawCornerImages = mapOf(
            90 to listOf("BCDA", "CDAB", "DABC", "ABCD"),
            270 to listOf("DABC", "ABCD", "BCDA", "CDAB"),
        )
        val outputCorners = listOf(0f to 0f, 1f to 0f, 1f to 1f, 0f to 1f)
        for (naturalPortrait in listOf(true, false)) for (sensor in listOf(90, 270)) {
            CardinalPose.entries.forEachIndexed { index, pose ->
                val plan = OrientationPlan.forRear(sensor, pose, naturalPortrait)
                val below = ground.getValue(sensor)[index]
                val above = 1f - below.first to 1f - below.second
                assertSample(below, sample(plan.encoderPositionMatrix(), producer(sensor), 0.5f, 0.8f))
                assertSample(above, sample(plan.encoderPositionMatrix(), producer(sensor), 0.5f, 0.2f))
                val display = (360 - pose.clockwiseDegrees) % 360
                assertSample(below, sample(plan.previewPositionMatrix(display), producer(sensor), 0.5f, 0.8f))
                assertSample(above, sample(plan.previewPositionMatrix(display), producer(sensor), 0.5f, 0.2f))
                for (matrix in listOf(plan.encoderPositionMatrix(), plan.previewPositionMatrix(display))) {
                    outputCorners.forEachIndexed { corner, (u, v) ->
                        val sampled = sample(matrix, producer(sensor), u, v)
                        val rawX = (sampled.first - 0.06f) / 0.8f
                        val rawY = (sampled.second - 0.12f) / 0.8f
                        val rawCorner = if (rawY < 0.5f) { if (rawX < 0.5f) 0 else 1 } else { if (rawX < 0.5f) 3 else 2 }
                        assertEquals("ABCD"[corner], rawCornerImages.getValue(sensor)[index][rawCorner])
                    }
                }
            }
        }
    }

    @Test fun producerValidatorKeepsCropAndFilteringInsetButRejectsOtherBasesAndDistortion() {
        for (sensor in listOf(0, 90, 180, 270)) {
            val plan = OrientationPlan.forRear(sensor, CardinalPose.NATURAL)
            CameraTextureGeometry.validate(producer(sensor), sensor, plan.cameraWidth, plan.cameraHeight)
            CameraTextureGeometry.validate(producer(sensor, 1f - 2f / plan.cameraWidth, 1f - 2f / plan.cameraHeight), sensor, plan.cameraWidth, plan.cameraHeight)
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.validate(producer(sensor, 0.5f, 0.8f), sensor, plan.cameraWidth, plan.cameraHeight) }
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.validate(producer((sensor + 90) % 360), sensor, plan.cameraWidth, plan.cameraHeight) }
            for (bad in listOf(producer(sensor).apply { this[12] = 2f }, producer(sensor).apply { this[0] = Float.NaN }, producer(sensor).apply { this[3] = 0.1f })) {
                assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.validate(bad, sensor, plan.cameraWidth, plan.cameraHeight) }
            }
        }
    }

    @Test fun stableCandidateSettlesWithoutAnotherSensorCallback() {
        val selector = CardinalPoseSelector()
        assertEquals(CardinalPose.NATURAL, selector.accept(90, 0))
        assertEquals(250_000_000L, selector.deadlineNs)
        assertEquals(CardinalPose.NATURAL, selector.settle(249_999_999))
        assertEquals(CardinalPose.CLOCKWISE_90, selector.settle(250_000_000))
        assertNull(selector.deadlineNs)
        assertEquals(CardinalPose.CLOCKWISE_90, selector.accept(-1, 300_000_000))
    }

    @Test fun briefPulseBoundaryFlatAndPauseCancelPendingPose() {
        for (cancel in listOf(0, 45, -1)) {
            val selector = CardinalPoseSelector()
            selector.accept(90, 0)
            assertEquals(CardinalPose.NATURAL, selector.accept(cancel, 100_000_000))
            assertNull(selector.deadlineNs)
            assertEquals(CardinalPose.NATURAL, selector.settle(500_000_000))
        }
        val paused = CardinalPoseSelector()
        paused.accept(90, 0); paused.cancelPending()
        assertEquals(CardinalPose.NATURAL, paused.settle(500_000_000))
        assertNull(paused.deadlineNs)
    }

    @Test fun candidateReplacementKeepsOneDeadlineAndOnlyCommitsStableLatestPose() {
        val selector = CardinalPoseSelector()
        selector.accept(90, 0)
        selector.accept(92, 100_000_000)
        assertEquals(250_000_000L, selector.deadlineNs)
        selector.accept(270, 200_000_000)
        assertEquals(450_000_000L, selector.deadlineNs)
        assertEquals(CardinalPose.NATURAL, selector.settle(250_000_000))
        assertEquals(CardinalPose.CLOCKWISE_270, selector.settle(450_000_000))
        assertThrows(IllegalArgumentException::class.java) { selector.accept(360, 500_000_000) }
        assertThrows(IllegalArgumentException::class.java) { selector.accept(-2, 500_000_000) }
        assertThrows(IllegalArgumentException::class.java) { selector.settle(0) }
    }

    @Test fun previewViewportFitsWithoutStretchOrCrop() {
        assertArrayEquals(intArrayOf(0, 0, 720, 1280), CameraGlRenderer.fitViewport(720, 1280, 720, 1280))
        assertArrayEquals(intArrayOf(200, 0, 600, 1000), CameraGlRenderer.fitViewport(1000, 1000, 600, 1000))
        assertArrayEquals(intArrayOf(0, 219, 1000, 562), CameraGlRenderer.fitViewport(1000, 1000, 1280, 720))
        assertThrows(IllegalArgumentException::class.java) { CameraGlRenderer.fitViewport(0, 1, 1, 1) }
    }

    @Test fun previewAbandonmentDoesNotMaskContextOrAllocationFailure() {
        assertTrue(CameraGlRenderer.isExpectedPreviewLoss(false, EGL14.EGL_SUCCESS))
        for (valid in listOf(true, false)) {
            assertTrue(CameraGlRenderer.isExpectedPreviewLoss(valid, EGL14.EGL_BAD_SURFACE))
            assertTrue(CameraGlRenderer.isExpectedPreviewLoss(valid, EGL14.EGL_BAD_NATIVE_WINDOW))
            for (error in listOf(EGL14.EGL_CONTEXT_LOST, EGL14.EGL_BAD_CONTEXT, EGL14.EGL_BAD_DISPLAY, EGL14.EGL_BAD_ALLOC)) {
                assertFalse(CameraGlRenderer.isExpectedPreviewLoss(valid, error))
            }
        }
        assertFalse(CameraGlRenderer.isExpectedPreviewLoss(true, EGL14.EGL_SUCCESS))
    }
}
