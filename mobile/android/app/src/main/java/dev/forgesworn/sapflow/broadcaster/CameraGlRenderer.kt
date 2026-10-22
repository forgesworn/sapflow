package dev.forgesworn.sapflow.broadcaster

import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/** Camera texture -> upright hardware encoder surface + same-frame preview.
 * All GL/native image ownership belongs to one thread. No raw pixels enter an
 * application byte buffer. callbacks must outlive actual asynchronous close.
 * A driver stall closes admission and reports failure within the watchdog,
 * but completion is never falsely reported before GL really detaches/releases.
 */
internal class CameraGlRenderer(
    private val inputWidth: Int,
    private val inputHeight: Int,
    private val plan: OrientationPlan,
    timestamps: CameraTimestampMapper,
    private val captureEpoch: Long,
    private val callbacks: Handler,
    private val onReady: (Surface, String) -> Unit,
    private val onFailure: (String) -> Unit,
    initialSourceToken: Long = 0,
    initialFront: Boolean = false,
    initialLegacyMirror: Boolean = false,
    private val rawAdmission: CameraRunAdmission,
    private val onSourceFrame: (Long, Long) -> Unit = { _, _ -> },
) {
    private val thread = HandlerThread("sapflow-camera-gl").apply {
        require(inputWidth > 0 && inputHeight > 0) { "Invalid camera texture size" }
        val naturalWidth = if (plan.sensorClockwiseDegrees % 180 == 0) inputWidth else inputHeight
        val naturalHeight = if (plan.sensorClockwiseDegrees % 180 == 0) inputHeight else inputWidth
        require(naturalWidth.toLong() * plan.sourceNaturalHeight == naturalHeight.toLong() * plan.sourceNaturalWidth) { "Raw camera sensor buffer aspect does not match the natural texture canvas" }
        start()
    }
    private val owner = Handler(thread.looper)
    private var admission = RenderAdmission()
    private val requestedSource = AtomicLong(initialSourceToken)
    private var sourceToken = initialSourceToken
    private var sourcePlan = plan
    private var sourceWidth = inputWidth
    private var sourceHeight = inputHeight
    private var timestamps = timestamps
    private var front = initialFront
    private var legacyMirror = initialLegacyMirror
    private var firstFrame = true
    private val sourceTimeline = CameraSourceTimeline()
    private val live = AtomicBoolean(true)
    private val failed = AtomicBoolean(false)
    private val closing = AtomicBoolean(false)
    private val released = AtomicBoolean(false)
    private val requestedEncoding = AtomicLong(NO_ENCODING)
    private val operation = AtomicLong()
    private val closeGuard = Any()
    private val closeCallbacks = mutableListOf<() -> Unit>()
    // Everything below belongs exclusively to owner.
    private var display: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var displayInitialised = false
    private var context: EGLContext = EGL14.EGL_NO_CONTEXT
    private var config: EGLConfig? = null
    private var scratch: EGLSurface = EGL14.EGL_NO_SURFACE
    private var encoder: EGLSurface = EGL14.EGL_NO_SURFACE
    private var encoderToken = NO_ENCODING
    private var preview: EGLSurface = EGL14.EGL_NO_SURFACE
    private var previewSurface: Surface? = null // borrowed; SurfaceView may abandon it during relayout
    private var previewWidth = 0
    private var previewHeight = 0
    private var previewDisplayDegrees = 0
    private var previewViewport = IntArray(4)
    private val encoderViewport = fitViewport(plan.width, plan.height, plan.contentWidth, plan.contentHeight)
    private var encoderMatrix = plan.encoderPositionMatrix()
    private var previewMatrix = plan.previewPositionMatrix(0, initialFront)
    private val textureMatrix = FloatArray(16)
    private var textureName = 0
    private var cameraTexture: SurfaceTexture? = null
    private var cameraSurface: Surface? = null
    private var lastTextureNs = -1L
    private var geometryLogged = false
    private var program = 0
    private var positionAttribute = -1
    private var textureAttribute = -1
    private var positionUniform = -1
    private var textureUniform = -1
    private var samplerUniform = -1
    private val vertices = floats(floatArrayOf(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f))
    private val coordinates = floats(floatArrayOf(0f, 0f, 1f, 0f, 0f, 1f, 1f, 1f))
    private val render = Runnable { renderLatest() }

    init {
        owner.post { guarded("GPU initialisation", 3000) { initialise() } }
    }

    /** Borrowed Surface. Its dimensions are the actual holder buffer size;
     * displayRotationDegrees = Surface.ROTATION_* converted to 0/90/180/270. */
    fun setPreview(surface: Surface?, width: Int, height: Int, displayRotationDegrees: Int) {
        require(surface == null || (width > 0 && height > 0))
        val extra = sourcePlan.previewRelativeClockwiseDegrees(displayRotationDegrees)
        if (!live.get()) return
        owner.post {
            if (!live.get()) return@post
            guarded("Preview surface change") {
                detachPreview()
                // A queued replacement can already have been abandoned by
                // SurfaceView. Losing preview must not invalidate the encode.
                if (surface != null && surface.isValid) {
                    try {
                        preview = window(surface); previewSurface = surface
                        previewWidth = width; previewHeight = height
                        previewDisplayDegrees = displayRotationDegrees
                        previewMatrix = sourcePlan.previewPositionMatrix(displayRotationDegrees, front)
                        previewViewport = fitViewport(width, height,
                            if (extra % 180 == 0) plan.contentWidth else plan.contentHeight,
                            if (extra % 180 == 0) plan.contentHeight else plan.contentWidth)
                    } catch (error: Exception) {
                        if (!expectedPreviewLoss(surface)) throw error
                    }
                }
            }
        }
    }

    /** Explicit Start only. Callback means the borrowed encoder surface has
     * been installed, not that any frame, IDR or destination was accepted. */
    fun startEncoding(surface: Surface, token: Long, onStarted: () -> Unit) {
        require(token >= 0)
        check(live.get() && requestedEncoding.compareAndSet(NO_ENCODING, token)) { "GPU renderer is not ready for a new encoding epoch" }
        owner.post {
            if (!live.get() || requestedEncoding.get() != token) return@post
            guarded("Encoder surface start") {
                require(surface.isValid && encoder == EGL14.EGL_NO_SURFACE) { "Encoder surface is unavailable or already attached" }
                makeCurrent(scratch)
                encoder = window(surface); encoderToken = token
                callbacks.post { if (live.get() && requestedEncoding.get() == token) onStarted() }
            }
        }
    }

    /** Closes admission immediately. onStopped means the last submitted swap
     * returned and the encoder EGL surface is actually detached/destroyed. */
    fun stopEncoding(token: Long, onStopped: () -> Unit) {
        requestedEncoding.compareAndSet(token, NO_ENCODING)
        if (released.get()) { callbacks.post(onStopped); return }
        if (closing.get()) { close(onStopped); return }
        val posted = owner.post {
            // A watchdog/failure may have begun full cleanup after this stop
            // was queued. Its handle fields are not a detachment receipt;
            // wait for actual full release instead of reporting false EOS safety.
            if (closing.get()) { close(onStopped); return@post }
            guarded("Encoder surface stop") {
                if (encoderToken == token) {
                    makeCurrent(scratch)
                    destroyWindow(encoder)
                    encoder = EGL14.EGL_NO_SURFACE; encoderToken = NO_ENCODING
                }
                callbacks.post(onStopped)
            }
        }
        if (!posted && released.get()) callbacks.post(onStopped)
    }

    /** Closes image admission immediately; completion follows actual cleanup.
     * No borrowed preview/encoder Surface is released by this class. */
    fun close(onClosed: () -> Unit = {}) {
        synchronized(closeGuard) {
            if (released.get()) { callbacks.post(onClosed); return }
            closeCallbacks.add(onClosed)
        }
        initiateClose()
    }

    private fun initialise() {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        check(display != EGL14.EGL_NO_DISPLAY) { "No EGL display" }
        val version = IntArray(2)
        displayInitialised = EGL14.eglInitialize(display, version, 0, version, 1)
        check(displayInitialised) { "EGL initialisation failed" }
        val extensions = EGL14.eglQueryString(display, EGL14.EGL_EXTENSIONS).orEmpty().split(' ').toSet()
        check("EGL_ANDROID_recordable" in extensions && "EGL_ANDROID_presentation_time" in extensions) { "Required hardware recording EGL extensions unavailable" }
        val choices = arrayOfNulls<EGLConfig>(1); val count = IntArray(1)
        check(EGL14.eglChooseConfig(display, intArrayOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_SURFACE_TYPE, EGL14.EGL_WINDOW_BIT or EGL14.EGL_PBUFFER_BIT,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT, EGLExt.EGL_RECORDABLE_ANDROID, 1, EGL14.EGL_NONE,
        ), 0, choices, 0, 1, count, 0) && count[0] > 0) { "No recordable EGL config" }
        config = choices[0] ?: error("Missing recordable EGL config")
        context = EGL14.eglCreateContext(display, config, EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        check(context != EGL14.EGL_NO_CONTEXT) { "No GLES2 context" }
        scratch = EGL14.eglCreatePbufferSurface(display, config,
            intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0)
        check(scratch != EGL14.EGL_NO_SURFACE) { "No EGL owner surface" }
        makeCurrent(scratch)
        val renderer = GLES20.glGetString(GLES20.GL_RENDERER).orEmpty()
        check(renderer.isNotBlank() && listOf("swiftshader", "llvmpipe", "softpipe", "software").none { renderer.contains(it, ignoreCase = true) }) { "Hardware GPU rendering unavailable" }
        check(GLES20.glGetString(GLES20.GL_EXTENSIONS).orEmpty().split(' ').contains("GL_OES_EGL_image_external")) { "External camera textures unavailable" }
        val maximum = IntArray(1); GLES20.glGetIntegerv(GLES20.GL_MAX_TEXTURE_SIZE, maximum, 0)
        check(maximum[0] >= maxOf(inputWidth, inputHeight, plan.width, plan.height)) { "Camera canvas exceeds GPU texture limits" }
        val viewport = IntArray(2); GLES20.glGetIntegerv(GLES20.GL_MAX_VIEWPORT_DIMS, viewport, 0)
        check(viewport[0] >= plan.width && viewport[1] >= plan.height) { "Encoded canvas exceeds GPU viewport limits" }
        program = createProgram()
        positionAttribute = GLES20.glGetAttribLocation(program, "aPosition")
        textureAttribute = GLES20.glGetAttribLocation(program, "aTexture")
        positionUniform = GLES20.glGetUniformLocation(program, "uPosition")
        textureUniform = GLES20.glGetUniformLocation(program, "uTexture")
        samplerUniform = GLES20.glGetUniformLocation(program, "uSampler")
        check(listOf(positionAttribute, textureAttribute, positionUniform, textureUniform, samplerUniform).all { it >= 0 }) { "GPU shader interface unavailable" }
        createSource(sourceToken) { surface -> onReady(surface, renderer.take(160)) }
    }

    /** Cold fence only. Old queued producer notifications cannot submit new
     * raw frames; already accepted compressed output still drains normally. */
    fun fenceSource() { requestedSource.set(NO_ENCODING) }

    fun sourceFrameNs(token: Long): Long? = if (requestedSource.get() == token) rawAdmission.confirmed(token) else null

    /** Caller has the old CameraDevice.onClosed hardware receipt. Only its
     * texture/input is replaced; GL context, encoder EGL surface and PTS stay. */
    fun replaceSource(token: Long, next: OrientationPlan, front: Boolean, legacyMirror: Boolean,
        mapper: CameraTimestampMapper, onReady: (Surface) -> Unit) {
        check(next.width == plan.width && next.height == plan.height && next.pose == plan.pose && next.profile == plan.profile)
        owner.post {
            if (!live.get()) return@post
            guarded("Camera source replacement", 1000) {
                makeCurrent(scratch)
                cameraTexture?.setOnFrameAvailableListener(null); owner.removeCallbacks(render)
                admission.close()
                cameraSurface?.release(); cameraSurface = null
                cameraTexture?.release(); cameraTexture = null
                if (textureName != 0) { GLES20.glDeleteTextures(1, intArrayOf(textureName), 0); textureName = 0 }
                sourcePlan = next; sourceWidth = next.cameraWidth; sourceHeight = next.cameraHeight
                this.front = front; this.legacyMirror = legacyMirror; timestamps = mapper
                sourceToken = token; lastTextureNs = -1; geometryLogged = false; firstFrame = true
                encoderMatrix = next.encoderPositionMatrix()
                previewMatrix = next.previewPositionMatrix(previewDisplayDegrees, front)
                admission = RenderAdmission(); requestedSource.set(token)
                createSource(token, onReady)
            }
        }
    }

    private fun createSource(token: Long, ready: (Surface) -> Unit) {
        val texture = IntArray(1); GLES20.glGenTextures(1, texture, 0); textureName = texture[0]
        check(textureName != 0) { "Camera texture allocation failed" }
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, textureName)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
        glCheck()
        val source = SurfaceTexture(textureName); cameraTexture = source
        source.setDefaultBufferSize(sourceWidth, sourceHeight)
        val queue = admission
        source.setOnFrameAvailableListener({
            if (live.get() && requestedSource.get() == token && queue.offer()) owner.post(render)
        }, owner)
        val surface = Surface(source); cameraSurface = surface
        callbacks.post { if (live.get() && requestedSource.get() == token) ready(surface) }
    }

    private fun renderLatest() {
        if (!admission.begin()) return
        try {
            if (!live.get() || requestedSource.get() != sourceToken || rawAdmission.failure != null) return
            guarded("GPU frame submission") {
                makeCurrent(scratch)
                val source = cameraTexture ?: error("Camera texture is unavailable")
                source.updateTexImage()
                val sourceNs = source.timestamp
                check(sourceNs >= lastTextureNs) { "Camera texture timestamp regressed" }
                // updateTexImage latches the latest frame. A queued producer
                // notification may refer to an image already consumed.
                if (sourceNs == lastTextureNs) return@guarded
                lastTextureNs = sourceNs
                val pts = timestamps.map(sourceNs)
                val encoding = encoder != EGL14.EGL_NO_SURFACE && requestedEncoding.get() == encoderToken
                check(sourceTimeline.frame(pts, firstFrame, encoding)) { "Camera source regressed timing or exceeded the live video gap bound" }
                source.getTransformMatrix(textureMatrix)
                if (!geometryLogged) {
                    geometryLogged = true
                    Log.i("SapflowGeometry", "epoch=$captureEpoch sensor=${sourcePlan.sensorClockwiseDegrees} raw=${sourceWidth}x$sourceHeight natural=${plan.sourceNaturalWidth}x${plan.sourceNaturalHeight} pose=${plan.pose.name} encoded=${plan.width}x${plan.height} vertexCW=${sourcePlan.encoderClockwiseDegrees} preview=${previewWidth}x$previewHeight display=$previewDisplayDegrees STM=${textureMatrix.joinToString(prefix = "[", postfix = "]")}")
                }
                CameraTextureGeometry.normalise(textureMatrix, sourcePlan.sensorClockwiseDegrees, sourceWidth, sourceHeight, legacyMirror)
                if (requestedSource.get() != sourceToken) return@guarded
                if (!rawAdmission.admit(System.nanoTime())) { fail("Camera source deadline expired before frame submission"); return@guarded }
                if (encoder != EGL14.EGL_NO_SURFACE && requestedEncoding.get() == encoderToken) {
                    draw(encoder, plan.width, plan.height, encoderMatrix, encoderViewport)
                    check(EGLExt.eglPresentationTimeANDROID(display, encoder, pts)) { "Encoder presentation timestamp rejected" }
                    check(EGL14.eglSwapBuffers(display, encoder)) { "Encoder EGL submission failed" }
                    sourceTimeline.submitted(pts)
                }
                // A live source is not confirmed until its actual encoder
                // submission returns. PCM keeps the original source deadline
                // while EGL is blocked, even if the codec handler cannot run.
                if (firstFrame && !rawAdmission.confirmSource(sourceToken, System.nanoTime())) {
                    if (rawAdmission.failure != null) fail("Camera source deadline expired before its first valid native submission")
                    return@guarded // Ordinary Stop canceled the pending source without a fault.
                }
                if (rawAdmission.failure != null) return@guarded
                if (firstFrame) {
                    firstFrame = false
                    val confirmed = sourceToken
                    val observedAtNs = checkNotNull(rawAdmission.confirmed(confirmed))
                    callbacks.post { if (live.get() && requestedSource.get() == confirmed) onSourceFrame(confirmed, observedAtNs) }
                }
                if (preview != EGL14.EGL_NO_SURFACE && live.get() && requestedSource.get() == sourceToken) {
                    val borrowed = previewSurface ?: error("Preview native window owner unavailable")
                    if (!borrowed.isValid) detachPreview()
                    else try {
                        draw(preview, previewWidth, previewHeight, previewMatrix, previewViewport)
                        check(EGL14.eglSwapBuffers(display, preview)) { "Preview EGL submission failed" }
                    } catch (error: Exception) {
                        if (expectedPreviewLoss(borrowed)) detachPreview() else throw error
                    }
                }
            }
        } finally {
            if (admission.finish() && live.get()) owner.post(render)
        }
    }

    private fun draw(target: EGLSurface, width: Int, height: Int, matrix: FloatArray, fit: IntArray) {
        makeCurrent(target)
        GLES20.glViewport(0, 0, width, height)
        GLES20.glClearColor(0f, 0f, 0f, 1f); GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
        GLES20.glViewport(fit[0], fit[1], fit[2], fit[3])
        GLES20.glUseProgram(program)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, textureName)
        GLES20.glUniform1i(samplerUniform, 0)
        GLES20.glUniformMatrix4fv(positionUniform, 1, false, matrix, 0)
        GLES20.glUniformMatrix4fv(textureUniform, 1, false, textureMatrix, 0)
        vertices.position(0); coordinates.position(0)
        GLES20.glEnableVertexAttribArray(positionAttribute); GLES20.glEnableVertexAttribArray(textureAttribute)
        GLES20.glVertexAttribPointer(positionAttribute, 2, GLES20.GL_FLOAT, false, 0, vertices)
        GLES20.glVertexAttribPointer(textureAttribute, 2, GLES20.GL_FLOAT, false, 0, coordinates)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
        GLES20.glDisableVertexAttribArray(positionAttribute); GLES20.glDisableVertexAttribArray(textureAttribute)
        glCheck()
    }

    private fun window(surface: Surface): EGLSurface = EGL14.eglCreateWindowSurface(display, config, surface, intArrayOf(EGL14.EGL_NONE), 0).also {
        check(it != EGL14.EGL_NO_SURFACE) { "GPU window surface creation failed" }
    }

    private fun makeCurrent(surface: EGLSurface) {
        check(EGL14.eglMakeCurrent(display, surface, surface, context)) { "GPU owner context unavailable" }
        // Request no display-vsync throttling. Some drivers constrain this;
        // the independent operation watchdog still fences a stalled swap.
        EGL14.eglSwapInterval(display, 0)
    }

    private fun destroyWindow(surface: EGLSurface) {
        if (surface != EGL14.EGL_NO_SURFACE) check(EGL14.eglDestroySurface(display, surface)) { "GPU surface detach failed" }
    }

    private fun expectedPreviewLoss(surface: Surface): Boolean {
        val error = EGL14.eglGetError()
        return isExpectedPreviewLoss(surface.isValid, error)
    }

    private fun detachPreview() {
        makeCurrent(scratch)
        val previous = preview
        preview = EGL14.EGL_NO_SURFACE; previewSurface = null
        if (previous != EGL14.EGL_NO_SURFACE && !EGL14.eglDestroySurface(display, previous)) {
            val error = EGL14.eglGetError()
            check(error == EGL14.EGL_BAD_SURFACE || error == EGL14.EGL_BAD_NATIVE_WINDOW) { "Preview EGL detach failed" }
        }
    }

    private fun guarded(label: String, timeoutMs: Long = 100, body: () -> Unit) {
        val token = operation.incrementAndGet()
        val watchdog = Runnable { if (operation.get() == token) fail("$label exceeded its deadline; cleanup may still be finishing") }
        callbacks.postDelayed(watchdog, timeoutMs)
        try { body() }
        catch (_: Exception) { fail("$label failed; capture must be prepared again") }
        finally { operation.compareAndSet(token, token + 1); callbacks.removeCallbacks(watchdog) }
    }

    private fun fail(message: String) {
        rawAdmission.fail() // Exact captured run, synchronously before any queued owner notification.
        if (failed.compareAndSet(false, true)) callbacks.post { onFailure(message) }
        // Fence now, but the controller owns the CameraDevice close barrier.
        // It calls close only after that actual producer closure; destroying a
        // texture here could race an in-flight native producer.
        live.set(false); requestedSource.set(NO_ENCODING); requestedEncoding.set(NO_ENCODING); admission.close()
    }

    private fun initiateClose() {
        live.set(false); requestedSource.set(NO_ENCODING); requestedEncoding.set(NO_ENCODING); admission.close()
        if (!closing.compareAndSet(false, true)) return
        owner.post {
            guarded("GPU cleanup") {
                releaseOwned()
                val completed = synchronized(closeGuard) {
                    released.set(true)
                    closeCallbacks.toList().also { closeCallbacks.clear() }
                }
                for (completion in completed) callbacks.post(completion)
                thread.quitSafely()
            }
        }
    }

    private fun releaseOwned() {
        // Keep releasing independent owners after any one release error. A
        // partial initialisation must not strand its thread/texture/display.
        var cleanupError = false
        fun attempt(action: () -> Unit) { try { action() } catch (_: Exception) { cleanupError = true } }
        attempt { cameraTexture?.setOnFrameAvailableListener(null) }
        owner.removeCallbacks(render)
        if (context != EGL14.EGL_NO_CONTEXT && scratch != EGL14.EGL_NO_SURFACE) {
            attempt { makeCurrent(scratch) }
            attempt { if (textureName != 0) GLES20.glDeleteTextures(1, intArrayOf(textureName), 0) }
            attempt { if (program != 0) GLES20.glDeleteProgram(program) }
            if (preview != EGL14.EGL_NO_SURFACE) attempt { detachPreview() }
        }
        attempt { cameraSurface?.release(); cameraSurface = null }
        attempt { cameraTexture?.release(); cameraTexture = null }
        if (displayInitialised) {
            attempt { check(EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)) }
            attempt { destroyWindow(encoder) }; encoder = EGL14.EGL_NO_SURFACE; encoderToken = NO_ENCODING
            attempt { destroyWindow(preview) }; preview = EGL14.EGL_NO_SURFACE; previewSurface = null
            attempt { destroyWindow(scratch) }; scratch = EGL14.EGL_NO_SURFACE
            attempt { if (context != EGL14.EGL_NO_CONTEXT) check(EGL14.eglDestroyContext(display, context)) }
            // Even if an earlier detach failed, successful thread release and
            // display termination retire its EGL resources. Never report
            // completion if these final ownership-release operations fail.
            val threadReleased = EGL14.eglReleaseThread()
            val displayReleased = EGL14.eglTerminate(display)
            check(threadReleased && displayReleased) { "GPU native ownership release failed" }
        }
        context = EGL14.EGL_NO_CONTEXT; display = EGL14.EGL_NO_DISPLAY; displayInitialised = false
        textureName = 0; program = 0
        check(cameraSurface == null && cameraTexture == null) { "GPU native image owner cleanup failed" }
        if (cleanupError) fail("GPU cleanup required terminal resource release; prepare capture again")
    }

    private fun createProgram(): Int {
        val vertex = shader(GLES20.GL_VERTEX_SHADER, """
            uniform mat4 uPosition;
            uniform mat4 uTexture;
            attribute vec2 aPosition;
            attribute vec2 aTexture;
            varying vec2 vTexture;
            void main() {
                gl_Position = uPosition * vec4(aPosition, 0.0, 1.0);
                vTexture = (uTexture * vec4(aTexture, 0.0, 1.0)).xy;
            }
        """.trimIndent())
        var fragment = 0
        var linked = 0
        try {
            fragment = shader(GLES20.GL_FRAGMENT_SHADER, """
                #extension GL_OES_EGL_image_external : require
                precision mediump float;
                varying vec2 vTexture;
                uniform samplerExternalOES uSampler;
                void main() { gl_FragColor = texture2D(uSampler, vTexture); }
            """.trimIndent())
            linked = GLES20.glCreateProgram()
            check(linked != 0) { "GPU program allocation failed" }
            GLES20.glAttachShader(linked, vertex); GLES20.glAttachShader(linked, fragment); GLES20.glLinkProgram(linked)
            val status = IntArray(1); GLES20.glGetProgramiv(linked, GLES20.GL_LINK_STATUS, status, 0)
            check(status[0] != 0) { "GPU program link failed" }
            return linked
        } catch (error: Exception) { if (linked != 0) GLES20.glDeleteProgram(linked); throw error }
        finally { GLES20.glDeleteShader(vertex); if (fragment != 0) GLES20.glDeleteShader(fragment) }
    }

    private fun shader(type: Int, source: String): Int {
        val shader = GLES20.glCreateShader(type)
        check(shader != 0) { "GPU shader allocation failed" }
        GLES20.glShaderSource(shader, source); GLES20.glCompileShader(shader)
        val status = IntArray(1); GLES20.glGetShaderiv(shader, GLES20.GL_COMPILE_STATUS, status, 0)
        if (status[0] == 0) { GLES20.glDeleteShader(shader); error("GPU shader compilation failed") }
        return shader
    }

    private fun glCheck() { check(GLES20.glGetError() == GLES20.GL_NO_ERROR) { "GPU rendering operation failed" } }

    companion object {
        private const val NO_ENCODING = Long.MIN_VALUE
        private fun floats(values: FloatArray): FloatBuffer = ByteBuffer.allocateDirect(values.size * 4)
            .order(ByteOrder.nativeOrder()).asFloatBuffer().apply { put(values); position(0) }

        internal fun isExpectedPreviewLoss(surfaceValid: Boolean, eglError: Int): Boolean =
            eglError == EGL14.EGL_BAD_SURFACE || eglError == EGL14.EGL_BAD_NATIVE_WINDOW ||
                (!surfaceValid && eglError == EGL14.EGL_SUCCESS)

        internal fun fitViewport(width: Int, height: Int, imageWidth: Int, imageHeight: Int): IntArray =
            CaptureProfile.fitViewport(width, height, imageWidth, imageHeight)
    }
}
