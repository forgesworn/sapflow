package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraAccessException
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.Handler
import android.os.SystemClock
import android.view.Surface
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** A scanner owns no Activity, audio session, encoder, recording or destination.
 * Native work uses the existing capture owner; only bounded luma enters the one
 * decoder. The controller keeps this owner until actual closure, even if its UI dies. */
internal class ManualQrScanner(private val context: Context, private val handler: Handler,
    val ticket: CaptureStartAdmission.ScannerTicket, private val current: () -> Boolean,
    private val event: (Event) -> Unit, beganNs: Long = SystemClock.elapsedRealtimeNanos()) {
    sealed class Event {
        class Geometry(val width: Int, val height: Int, val sensorOrientation: Int) : Event()
        data object CameraClosed : Event()
        data object Retained : Event()
        class Finished(val reply: ManualQrPolicy.Reply) : Event()
    }
    private val policy = ManualQrPolicy(beganNs)
    private val decoder = ManualQrDecoder()
    private val pixels = ByteArray(QrLuminance.MAX_PIXELS)
    private val worker = ThreadPoolExecutor(1, 1, 1, TimeUnit.SECONDS, ArrayBlockingQueue(1),
        { runnable -> Thread(runnable, "sapflow-qr-decode").apply { isDaemon = true } }).apply { allowCoreThreadTimeOut(true) }
    private var reader: ImageReader? = null // handler owner only
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var openIssued = false
    private var closing = false
    private var nativeReleased = false
    private var stoppedNotified = false
    private var retainedNotified = false
    @Volatile private var ownedTexture: SurfaceTexture? = null
    private var surface: Surface? = null
    private val closeRequested = AtomicBoolean(false)
    private val timeout = Runnable { cancel() }

    fun arm(): Boolean {
        val queued = handler.post {
            if (!policy.permits(now()) || !current()) { cancel(); return@post }
            handler.postDelayed(timeout, ((policy.deadlineNs - now()).coerceAtLeast(0) + 999_999) / 1_000_000)
        }
        if (!queued) { policy.cancel(); policy.nativeClosed(false); event(Event.Retained) }
        return queued
    }
    fun ownsTexture(texture: SurfaceTexture): Boolean = ownedTexture === texture
    /** Main only transfers the borrowed Texture once. Native start rechecks on its owner. */
    fun open(texture: SurfaceTexture): Boolean {
        if (!current() || !policy.claimOpen(now())) return false
        ownedTexture = texture
        if (!handler.post { openOnOwner(texture) }) {
            policy.nativeClosed(false); event(Event.Retained)
        }
        return true
    }
    fun cancel() {
        policy.cancel() // Synchronous before queued native teardown or late result.
        if (closeRequested.compareAndSet(false, true) && !handler.post(::closeOnOwner)) { policy.nativeClosed(false); event(Event.Retained) }
    }
    fun presentationClosed() {
        policy.presentationClosed()
        if (!handler.post(::notifyIfClosed)) policy.nativeClosed(false)
    }
    private fun now() = SystemClock.elapsedRealtimeNanos()
    private fun permitted(): Boolean = current() && policy.permits(now())
    @SuppressLint("MissingPermission") // Actual permission checked immediately before native open.
    private fun openOnOwner(texture: SurfaceTexture) {
        if (!permitted()) { closeOnOwner(); return }
        try {
            val manager = context.getSystemService(CameraManager::class.java)
            var selectedId: String? = null; var selectedWidth = 0; var selectedHeight = 0; var selectedRotation = 0
            for (id in manager.cameraIdList.take(32)) {
                val characteristics = manager.getCameraCharacteristics(id)
                if (characteristics.get(CameraCharacteristics.LENS_FACING) != CameraCharacteristics.LENS_FACING_BACK) continue
                val orientation = characteristics.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: continue
                if (orientation !in listOf(0, 90, 180, 270)) continue
                val outputs = characteristics.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: continue
                val sizes = outputs.getOutputSizes(ImageFormat.YUV_420_888) ?: continue
                val previews = outputs.getOutputSizes(SurfaceTexture::class.java)?.take(256) ?: continue
                val size = sizes.take(256).filter { it.width in 320..1280 && it.height in 240..720 && it in previews }
                    .minByOrNull { kotlin.math.abs(it.width.toLong() * it.height - 640L * 480) } ?: continue
                selectedId = id; selectedWidth = size.width; selectedHeight = size.height
                selectedRotation = orientation; break
            }
            val id = checkNotNull(selectedId)
            check(permitted())
            texture.setDefaultBufferSize(selectedWidth, selectedHeight)
            surface = Surface(texture)
            val output = ImageReader.newInstance(selectedWidth, selectedHeight, ImageFormat.YUV_420_888, 2)
            reader = output
            output.setOnImageAvailableListener({ value -> imageAvailable(value) }, handler)
            event(Event.Geometry(selectedWidth, selectedHeight, selectedRotation))
            check(permitted() && context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
            openIssued = true
            try { manager.openCamera(id, callbacks, handler) }
            catch (error: Exception) {
                // These documented synchronous refusals create no camera owner.
                if (error is CameraAccessException || error is SecurityException || error is IllegalArgumentException) openIssued = false
                else { policy.nativeClosed(false); retained() }
                throw error
            }
        } catch (_: Exception) { policy.cancel(); closeOnOwner() }
    }
    private val callbacks = object : CameraDevice.StateCallback() {
        override fun onOpened(camera: CameraDevice) {
            device = camera; openIssued = false
            if (closing || !permitted()) { policy.cancel(); closeOnOwner(); return }
            try {
                val imageOutput = checkNotNull(reader)
                val previewOutput = checkNotNull(surface)
                camera.createCaptureSession(SessionConfiguration(SessionConfiguration.SESSION_REGULAR,
                    listOf(OutputConfiguration(imageOutput.surface), OutputConfiguration(previewOutput)), Executor { handler.post(it) },
                    object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(value: CameraCaptureSession) {
                            session = value
                            if (closing || !permitted() || device !== camera) { policy.cancel(); closeOnOwner(); return }
                            try {
                                val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                                    addTarget(imageOutput.surface); addTarget(previewOutput)
                                    val modes = context.getSystemService(CameraManager::class.java).getCameraCharacteristics(camera.id)
                                        .get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
                                    if (modes.contains(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE))
                                        set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                                }.build()
                                check(permitted())
                                value.setRepeatingRequest(request, null, handler)
                            } catch (_: Exception) { cancel() }
                        }
                        override fun onConfigureFailed(value: CameraCaptureSession) {
                            session = value; policy.cancel(); closeOnOwner()
                        }
                    }))
            } catch (_: Exception) { cancel() }
        }
        override fun onDisconnected(camera: CameraDevice) {
            device = camera; openIssued = false; policy.cancel(); closeOnOwner()
        }
        override fun onError(camera: CameraDevice, error: Int) {
            device = camera; openIssued = false; policy.cancel(); closeOnOwner()
        }
        override fun onClosed(camera: CameraDevice) {
            if (!closing) policy.cancel()
            if (device === camera) device = null
            openIssued = false
            closeOnOwner()
        }
    }
    private fun imageAvailable(output: ImageReader) {
        // Listener removal doesn't retract an already queued callback.
        if (reader !== output || closing) return
        val image = try { output.acquireLatestImage() } catch (_: Exception) { cancel(); return } ?: return
        var shape: QrLuminance.Shape? = null
        val admitted = current() && policy.beginDecode(now())
        if (!current()) policy.cancel()
        try {
            if (admitted) {
                require(image.format == ImageFormat.YUV_420_888)
                val crop = image.cropRect
                val plane = image.planes[0]
                shape = QrLuminance.copy(plane.buffer, image.width, image.height, crop.left, crop.top, crop.right, crop.bottom,
                    plane.rowStride, plane.pixelStride, pixels)
            }
        } catch (_: Exception) { policy.cancel() }
        finally {
            // No Image/plane/native buffer survives into the decoder. This
            // preserves the two free acquisition slots needed by latest-image.
            try { image.close() } catch (_: Exception) { policy.nativeClosed(false); retained() }
        }
        val copied = shape
        if (!admitted) { if (policy.stopped) closeOnOwner(); return }
        if (copied == null || policy.stopped) { pixels.fill(0); policy.decodeReturned(); closeOnOwner(); return }
        try {
            worker.execute {
                try {
                    if (current() && policy.permits(now())) {
                        val text = decoder.decode(pixels, copied.width, copied.height)
                        if (text != null) policy.accept(text, now())
                    }
                } catch (_: Exception) { policy.cancel() }
                finally {
                    pixels.fill(0)
                    policy.decodeReturned()
                    handler.post { if (policy.stopped) closeOnOwner() else notifyIfClosed() }
                }
            }
        } catch (_: Exception) { pixels.fill(0); policy.decodeReturned(); policy.cancel(); closeOnOwner() }
    }
    private fun closeOnOwner() {
        closing = true
        handler.removeCallbacks(timeout)
        try { reader?.setOnImageAvailableListener(null, null) } catch (_: Exception) { policy.nativeClosed(false); retained() }
        session?.let {
            runCatching { it.stopRepeating() }; runCatching { it.abortCaptures() }
            try { it.close() } catch (_: Exception) { policy.nativeClosed(false); retained() }
        }
        session = null
        val camera = device
        if (camera != null) {
            try { camera.close() } catch (_: Exception) { policy.nativeClosed(false); retained() }
            return // Only the actual onClosed receipt releases hardware.
        }
        if (openIssued) return // Late open/error/close still owns this candidate.
        if (!nativeReleased) {
            var clean = !policy.cleanupUncertain
            try { reader?.close(); reader = null } catch (_: Exception) { clean = false }
            try { surface?.release(); surface = null } catch (_: Exception) { clean = false }
            try { ownedTexture?.release(); ownedTexture = null } catch (_: Exception) { clean = false }
            nativeReleased = true
            policy.nativeClosed(clean)
            worker.shutdown()
            if (!clean) retained()
        }
        notifyIfClosed()
    }
    private fun retained() {
        if (!retainedNotified) { retainedNotified = true; event(Event.Retained) }
    }
    private fun notifyIfClosed() {
        if (!policy.nativeAndDecoderClosed) return
        if (!stoppedNotified) { stoppedNotified = true; event(Event.CameraClosed) }
        policy.finish(now())?.let { event(Event.Finished(it)) }
    }
}
