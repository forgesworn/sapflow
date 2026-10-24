package dev.forgesworn.sapflow.broadcaster

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Rect
import android.graphics.SurfaceTexture
import android.hardware.camera2.*
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.os.Build
import android.os.Handler
import android.util.Range
import android.util.Size
import android.view.Surface
import java.util.concurrent.Executor

/** One actual CameraDevice lifetime. All methods/callbacks run on the codec
 * owner; public close is nonblocking. onClosed is the documented device-wide
 * hardware release receipt, including its child sessions/pending callbacks. */
internal class CameraCaptureOwner(private val manager: CameraManager, val descriptor: Descriptor,
    private val input: Surface, private val handler: Handler, private val current: () -> Boolean,
    private val onConfigured: () -> Unit, private val onFailure: () -> Unit) {
    class Descriptor(val id: String, val choice: CameraSourceChoice, val plan: OrientationPlan,
        val characteristics: CameraCharacteristics, val capabilities: CameraControlCapabilities) {
        val legacyMirror: Boolean get() = choice == CameraSourceChoice.FRONT && Build.VERSION.SDK_INT < 33
        fun defaults() = CameraControlSettings(whiteBalance = capabilities.whiteBalanceModes.firstOrNull { it == CaptureRequest.CONTROL_AWB_MODE_AUTO }
            ?: capabilities.whiteBalanceModes.first())
    }
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private val lifetime = CameraDeviceLifecycle()
    private val closing: Boolean get() = lifetime.closing
    private var failed = false
    @Volatile var revision = 0L; private set
    private var settings = descriptor.defaults()
    @Volatile private var desiredSettings = settings
    private val controlRequests = CameraNativeControlQueue()
    @Volatile var controlsPending = false; private set
    private val ratioZoom = Build.VERSION.SDK_INT >= 30 && descriptor.characteristics.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE) != null
    private val activeArray = descriptor.characteristics.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
    private val closeCallbacks = mutableListOf<() -> Unit>()
    @Volatile var confirmedRevision = 0L; private set
    @Volatile var nativeZoom = Float.NaN; private set
    @Volatile var nativeExposure = Int.MIN_VALUE; private set
    @Volatile var nativeWhiteBalance = -1; private set
    @Volatile var focusState = -1; private set
    @Volatile var exposureState = -1; private set
    @Volatile var whiteBalanceState = -1; private set
    val requestedSettings: CameraControlSettings get() = desiredSettings
    private class RequestTag(val revision: Long)
    private val results = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureCompleted(session: CameraCaptureSession, request: CaptureRequest, result: TotalCaptureResult) {
            if (closing || !current() || session !== this@CameraCaptureOwner.session) return
            val tag = request.tag as? RequestTag ?: return
            if (tag.revision != revision || !controlRequests.matches(tag.revision)) return
            nativeZoom = if (Build.VERSION.SDK_INT >= 30 && ratioZoom)
                result.get(CaptureResult.CONTROL_ZOOM_RATIO) ?: Float.NaN else result.get(CaptureResult.SCALER_CROP_REGION)?.let { crop ->
                    val active = activeArray
                    if (active != null && crop.width() > 0) active.width().toFloat() / crop.width() else Float.NaN
                } ?: Float.NaN
            nativeExposure = result.get(CaptureResult.CONTROL_AE_EXPOSURE_COMPENSATION) ?: Int.MIN_VALUE
            nativeWhiteBalance = result.get(CaptureResult.CONTROL_AWB_MODE) ?: -1
            focusState = result.get(CaptureResult.CONTROL_AF_STATE) ?: -1
            exposureState = result.get(CaptureResult.CONTROL_AE_STATE) ?: -1
            whiteBalanceState = result.get(CaptureResult.CONTROL_AWB_STATE) ?: -1
            confirmedRevision = tag.revision
            if (controlRequests.complete(tag.revision)) {
                controlsPending = controlRequests.pending()
                try { applyNext() } catch (_: Exception) { fail() }
            }
        }
        override fun onCaptureFailed(session: CameraCaptureSession, request: CaptureRequest, failure: CaptureFailure) {
            val tag = request.tag as? RequestTag ?: return
            if (!closing && current() && controlRequests.matches(tag.revision)) fail()
        }
    }
    @SuppressLint("MissingPermission") // Controller checks CAMERA immediately before this native call.
    fun open() {
        check(device == null); lifetime.beginOpen()
        try {
            manager.openCamera(descriptor.id, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    device = camera
                    if (lifetime.opened() == CameraDeviceLifecycle.Action.CLOSE_DEVICE) { runCatching { camera.close() }; return }
                    if (!current()) { close(); return }
                    try {
                        val output = OutputConfiguration(input)
                        if (Build.VERSION.SDK_INT >= 33) {
                            output.timestampBase = OutputConfiguration.TIMESTAMP_BASE_MONOTONIC
                            // API33 default AUTO reflects front SurfaceTexture output.
                            // NONE makes encoding explicitly unmirrored; preview reflects separately.
                            output.mirrorMode = OutputConfiguration.MIRROR_MODE_NONE
                        }
                        camera.createCaptureSession(SessionConfiguration(SessionConfiguration.SESSION_REGULAR, listOf(output), Executor { handler.post(it) }, object : CameraCaptureSession.StateCallback() {
                            override fun onConfigured(value: CameraCaptureSession) {
                                if (closing || !current() || device !== camera) { value.close(); return }
                                session = value
                                try { check(adjust(settings)); onConfigured() } catch (_: Exception) { fail() }
                            }
                            override fun onConfigureFailed(value: CameraCaptureSession) { value.close(); if (!closing) fail() }
                        }))
                    } catch (_: Exception) { fail() }
                }
                override fun onClosed(camera: CameraDevice) {
                    if (device === camera) device = null
                    session = null; lifetime.nativeClosed(); deliverClosed()
                }
                override fun onDisconnected(camera: CameraDevice) { device = camera; if (lifetime.opened() == CameraDeviceLifecycle.Action.CLOSE_DEVICE) runCatching { camera.close() } else fail() }
                override fun onError(camera: CameraDevice, error: Int) { device = camera; if (lifetime.opened() == CameraDeviceLifecycle.Action.CLOSE_DEVICE) runCatching { camera.close() } else fail() }
            }, handler)
        } catch (_: Exception) { lifetime.failedOpenWithoutDevice(); fail(); deliverClosed() }
    }
    fun adjust(value: CameraControlSettings): Boolean {
        if (closing || !current() || session == null || !descriptor.capabilities.accepts(value)) return false
        if (!controlRequests.offer(value)) return false
        desiredSettings = value; controlsPending = true
        return try { applyNext(); true } catch (_: Exception) { fail(); false }
    }
    private fun applyNext() {
        val next = controlRequests.take() ?: return
        val value = next.settings
        require(descriptor.capabilities.accepts(value))
        val camera = checkNotNull(device); val configured = checkNotNull(session)
        revision = next.serial
        val tag = RequestTag(revision)
        val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
            addTarget(input); setTag(tag)
            set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, Range(30, 30))
            val modes = (descriptor.characteristics.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf())
            val focus = if (value.focusLocked) CaptureRequest.CONTROL_AF_MODE_AUTO
                else if (descriptor.capabilities.focusContinuous) CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO else CaptureRequest.CONTROL_AF_MODE_OFF
            require(modes.contains(focus))
            set(CaptureRequest.CONTROL_AF_MODE, focus)
            set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, value.exposure)
            if (descriptor.capabilities.exposureLock) set(CaptureRequest.CONTROL_AE_LOCK, value.exposureLocked)
            set(CaptureRequest.CONTROL_AWB_MODE, value.whiteBalance)
            if (descriptor.capabilities.whiteBalanceLock) set(CaptureRequest.CONTROL_AWB_LOCK, value.whiteBalanceLocked)
            if (Build.VERSION.SDK_INT >= 30 && ratioZoom) set(CaptureRequest.CONTROL_ZOOM_RATIO, value.zoom)
            else {
                val active = checkNotNull(activeArray)
                val width = (active.width() / value.zoom).toInt().coerceAtLeast(1)
                val height = (active.height() / value.zoom).toInt().coerceAtLeast(1)
                val left = active.left + (active.width() - width) / 2; val top = active.top + (active.height() - height) / 2
                set(CaptureRequest.SCALER_CROP_REGION, Rect(left, top, left + width, top + height))
            }
            if (Build.VERSION.SDK_INT >= 31 && descriptor.characteristics.get(CameraCharacteristics.SCALER_AVAILABLE_ROTATE_AND_CROP_MODES)?.contains(CaptureRequest.SCALER_ROTATE_AND_CROP_NONE) == true)
                set(CaptureRequest.SCALER_ROTATE_AND_CROP, CaptureRequest.SCALER_ROTATE_AND_CROP_NONE)
            set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_IDLE)
        }
        configured.setRepeatingRequest(request.build(), null, handler)
        when (CameraFocusTransition.between(settings, value)) {
            CameraFocusTransition.START -> request.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START)
            CameraFocusTransition.CANCEL -> request.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_CANCEL)
            CameraFocusTransition.IDLE -> Unit
        }

        // One cold observation, not a repeating per-frame metadata callback.
        // AF convergence can be re-observed by explicit user status refresh.
        configured.capture(request.build(), results, handler)
        settings = value
    }
    fun close(onClosed: () -> Unit = {}) {
        if (lifetime.closed) { onClosed(); return }
        // Only switch and full capture Stop can each subscribe once.
        if (closeCallbacks.size < 2) closeCallbacks += onClosed
        controlRequests.close()
        val action = lifetime.requestClose(device != null)
        runCatching { session?.stopRepeating() }
        runCatching { session?.close() }
        when (action) {
            CameraDeviceLifecycle.Action.CLOSE_DEVICE -> runCatching { device?.close() }
            CameraDeviceLifecycle.Action.COMPLETE -> deliverClosed()
            else -> Unit
        }
    }

    private fun fail() {
        if (!failed) { failed = true; onFailure() }
        close()
    }
    private fun deliverClosed() { val callbacks = closeCallbacks.toList(); closeCallbacks.clear(); callbacks.forEach { it() } }

    companion object {
        fun discover(context: Context, choice: CameraSourceChoice, canvas: OrientationPlan): Descriptor {
            val manager = context.getSystemService(CameraManager::class.java)
            for (id in manager.cameraIdList.take(32)) {
                val c = manager.getCameraCharacteristics(id)
                val facing = if (choice == CameraSourceChoice.REAR) CameraCharacteristics.LENS_FACING_BACK else CameraCharacteristics.LENS_FACING_FRONT
                if (c.get(CameraCharacteristics.LENS_FACING) != facing) continue
                val sensor = c.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: continue
                if (sensor !in listOf(0, 90, 180, 270)) continue
                val plan = OrientationPlan.forCamera(sensor, canvas.pose, canvas.naturalPortrait, canvas.profile, choice == CameraSourceChoice.FRONT)
                if (Build.VERSION.SDK_INT < 33 && c.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) != CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME) continue
                if (c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)?.getOutputSizes(SurfaceTexture::class.java)?.contains(Size(plan.cameraWidth, plan.cameraHeight)) != true ||
                    c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)?.contains(Range(30, 30)) != true) continue
                val modes = (c.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf())
                if (!modes.contains(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO) && !modes.contains(CaptureRequest.CONTROL_AF_MODE_OFF)) continue
                val exposure = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE) ?: Range(0, 0)
                val wb = (c.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES) ?: intArrayOf()).filter { it in 1..8 }.take(8)
                if (wb.isEmpty()) continue
                val zoom = if (Build.VERSION.SDK_INT >= 30) c.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE) else null
                val maxZoom = zoom?.upper ?: c.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f
                val minZoom = zoom?.lower ?: 1f
                if (!minZoom.isFinite() || !maxZoom.isFinite() || minZoom <= 0 || minZoom > 1f || maxZoom < 1f) continue
                val capabilities = CameraControlCapabilities(minZoom, maxZoom, modes.contains(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO),
                    modes.contains(CaptureRequest.CONTROL_AF_MODE_AUTO), exposure.lower, exposure.upper,
                    c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)?.toFloat() ?: 0f,
                    c.get(CameraCharacteristics.CONTROL_AE_LOCK_AVAILABLE) == true, wb, c.get(CameraCharacteristics.CONTROL_AWB_LOCK_AVAILABLE) == true)
                return Descriptor(id, choice, plan, c, capabilities)
            }
            error("${choice.label} camera cannot provide the required native 720p30 source and timestamp contract")
        }
    }
}
