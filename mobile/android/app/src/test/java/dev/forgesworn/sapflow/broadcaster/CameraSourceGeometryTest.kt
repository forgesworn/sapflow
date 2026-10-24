package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CameraSourceGeometryTest {
    // Independent asymmetric producer/crop oracle, not pixels from a camera.
    private fun producer(sensor: Int): FloatArray = when(sensor) {
        0 -> floatArrayOf(.8f,0f,0f,0f,0f,-.8f,0f,0f,0f,0f,1f,0f,.06f,.92f,0f,1f)
        90 -> floatArrayOf(0f,-.8f,0f,0f,-.8f,0f,0f,0f,0f,0f,1f,0f,.86f,.92f,0f,1f)
        180 -> floatArrayOf(-.8f,0f,0f,0f,0f,.8f,0f,0f,0f,0f,1f,0f,.86f,.12f,0f,1f)
        else -> floatArrayOf(0f,.8f,0f,0f,.8f,0f,0f,0f,0f,0f,1f,0f,.06f,.12f,0f,1f)
    }
    private fun reflectNaturalInput(matrix: FloatArray): FloatArray = matrix.copyOf().apply {
        this[12] += this[0]; this[13] += this[1]; this[0] = -this[0]; this[1] = -this[1]
    }
    private fun sample(position: FloatArray, texture: FloatArray, u: Float, v: Float): Pair<Float,Float> {
        val x = 2*u-1; val y = 1-2*v
        val s = (position[0]*x+position[1]*y+1)/2; val t = (position[4]*x+position[5]*y+1)/2
        return texture[0]*s+texture[4]*t+texture[12] to texture[1]*s+texture[5]*t+texture[13]
    }
    @Test fun legacyFrontNormalisationKeepsFullCropAndUnmirroredEncodedAsymmetricPoints() {
        for (natural in listOf(true,false)) for (sensor in listOf(0,90,180,270)) for (pose in CardinalPose.entries) {
            val plan = OrientationPlan.forRear(sensor,pose,natural)
            val expected = producer(sensor); val legacy = reflectNaturalInput(expected)
            CameraTextureGeometry.normalise(legacy,sensor,plan.cameraWidth,plan.cameraHeight,true)
            assertArrayEquals(expected,legacy,.000001f)
            for ((u,v) in listOf(.13f to .27f,.81f to .64f,.32f to .91f)) {
                val a = sample(plan.encoderPositionMatrix(),expected,u,v); val b = sample(plan.encoderPositionMatrix(),legacy,u,v)
                assertEquals(a.first,b.first,.000001f); assertEquals(a.second,b.second,.000001f)
            }
        }
    }
    @Test fun explicitNoMirrorNeverAddsReflectionAndFrontPreviewMirrorsOnlyScreenX() {
        for (natural in listOf(true,false)) for (sensor in listOf(0,90,180,270)) for (pose in CardinalPose.entries) for (display in listOf(0,90,180,270)) {
            val plan = OrientationPlan.forRear(sensor,pose,natural); val texture = producer(sensor); val original = texture.copyOf()
            CameraTextureGeometry.normalise(texture,sensor,plan.cameraWidth,plan.cameraHeight,false)
            assertArrayEquals(original,texture,0f)
            val ordinary = plan.previewPositionMatrix(display); val front = plan.previewPositionMatrix(display,true)
            val a = sample(ordinary,texture,.17f,.39f); val b = sample(front,texture,.83f,.39f)
            assertEquals(a.first,b.first,.000001f); assertEquals(a.second,b.second,.000001f)
        }
    }
    @Test fun wrongDeclaredParityUnknownBasisShearAndCropFailClosed() {
        for (sensor in listOf(0,90,180,270)) {
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.normalise(producer(sensor),sensor,1280,720,true) }
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.normalise(reflectNaturalInput(producer(sensor)),sensor,1280,720,false) }
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.normalise(producer(sensor).apply { this[12]=2f },sensor,1280,720,false) }
            assertThrows(IllegalArgumentException::class.java) { CameraTextureGeometry.normalise(producer(sensor).apply { this[3]=.2f },sensor,1280,720,false) }
        }
    }
    private fun expectedRaw(rotation: Int, u: Float, v: Float): Pair<Float,Float> = when(rotation) {
        0 -> u to v; 90 -> v to 1f-u; 180 -> 1f-u to 1f-v; else -> 1f-v to u
    }
    @Test fun independentFrontAsymmetricCornerOracleUsesSensorMinusPhysicalPoseAndPlusDisplay() {
        // Android's facing-aware camera rotation contract, independently
        // enumerated rather than derived from the production plan properties.
        val rotations = mapOf(0 to listOf(0,270,180,90), 90 to listOf(90,0,270,180),
            180 to listOf(180,90,0,270), 270 to listOf(270,180,90,0))
        for (natural in listOf(true,false)) for (sensor in listOf(0,90,180,270)) for (profile in CaptureProfile.entries) {
            CardinalPose.entries.forEachIndexed { index, pose ->
                val plan = OrientationPlan.forCamera(sensor,pose,natural,profile,true)
                assertEquals(rotations.getValue(sensor)[index],plan.rawToEncodedClockwiseDegrees)
                val texture = producer(sensor)
                for ((u,v) in listOf(0f to 0f,1f to 0f,1f to 1f,0f to 1f,.13f to .71f)) {
                    val raw = expectedRaw(rotations.getValue(sensor)[index],u,v)
                    val encoded = sample(plan.encoderPositionMatrix(),texture,u,v)
                    assertEquals(.06f+.8f*raw.first,encoded.first,.000001f)
                    assertEquals(.12f+.8f*raw.second,encoded.second,.000001f)
                    for (display in listOf(0,90,180,270)) {
                        val expected = expectedRaw((sensor+display)%360,1-u,v)
                        val preview = sample(plan.previewPositionMatrix(display,true),texture,u,v)
                        assertEquals(.06f+.8f*expected.first,preview.first,.000001f)
                        assertEquals(.12f+.8f*expected.second,preview.second,.000001f)
                    }
                }
                val rear = OrientationPlan.forRear(sensor,pose,natural,profile)
                assertEquals(rear.width,plan.width); assertEquals(rear.height,plan.height)
                assertEquals(rear.cameraWidth,plan.cameraWidth); assertEquals(rear.cameraHeight,plan.cameraHeight)
            }
        }
    }
}
