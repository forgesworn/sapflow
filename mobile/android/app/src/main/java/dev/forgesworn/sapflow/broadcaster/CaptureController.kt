package dev.forgesworn.sapflow.broadcaster

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.graphics.SurfaceTexture
import android.media.AudioFormat
import android.media.AudioTimestamp
import android.media.AudioRecord
import android.media.MediaCodec as PlatformCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.media.MediaRecorder
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.os.Build
import android.util.Range
import android.util.Size
import android.util.Log
import android.view.Surface
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import dev.forgesworn.sapflow.NativeSampleFanout
import dev.forgesworn.sapflow.MediaSample
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executor
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.math.sqrt

enum class CaptureStage { IDLE, PREPARING, PREPARED, CAPTURING, STOPPING, ERROR }

data class CaptureSnapshot(
    val stage: CaptureStage,
    val message: String,
    val videoEncoder: String,
    val audioEncoder: String,
    val videoSamples: Long,
    val audioSamples: Long,
    val drops: Long,
    val videoRejects: Long,
    val audioPrimingRejects: Long,
    val waitingKeyframeRejects: Long,
    val queueRejects: Long,
    val audioLevel: Float,
    val videoCaptureAgeMs: Long?,
    val publishingStatus: String,
    val recordingStatus: String,
    val queueDiagnostics: String,
    val sinkDiagnostics: String,
    val orientation: String,
    val renderer: String,
    val audioDiagnostics: String,
    val cleanupDiagnostics: String,
    val canPrepare: Boolean,
)

/** Native surfaces carry raw video. PCM stays in AudioRecord/MediaCodec input.
 * Only compressed direct buffers cross the synchronous borrowed-byte FFI. */
class CaptureController(private val context: Context) {
    private val thread = HandlerThread("sapflow-codecs").apply { start() }
    private val handler = Handler(thread.looper)
    private val audioCallbacks = HandlerThread("sapflow-aac-callbacks", android.os.Process.THREAD_PRIORITY_AUDIO).apply { start() }
    private val audioHandler = Handler(audioCallbacks.looper)
    private var encodedAudio: EncodedAudioEpoch? = null
    private var audioOwnerQuiesced = true
    private var audioQuiesceRequested = false
    private var audioTailRequested = AtomicBoolean(false)
    private var audioInputAdmission = AtomicBoolean(false)
    private val pcmFrames = AtomicLong()
    private val queuedAudioFrames = AtomicLong()
    private val outputAudioUnits = AtomicLong()
    private val outputAudioPtsUs = AtomicLong()
    private val audioInputWaitMaxUs = AtomicLong()
    private val audioReadMaxUs = AtomicLong()
    @Volatile private var audioDiagnostics = "Audio hardware timing not sampled"
    @Volatile private var audioFailureDiagnostics = ""
    private var audioRuntimeAtStart = ""
    private var audioOwnerFailureReported = false
    private val runningAudio = AtomicBoolean(false)
    private val acceptedVideo = AtomicLong()
    private val acceptedAudio = AtomicLong()
    private val drops = AtomicLong()
    private val videoRejects = AtomicLong()
    private val audioPrimingRejects = AtomicLong()
    private val waitingKeyframeRejects = AtomicLong()
    private val queueRejects = AtomicLong()
    private val audioInputs = ArrayBlockingQueue<Int>(32)
    @Volatile private var stage = CaptureStage.IDLE
    @Volatile private var message = "Prepare the camera and microphone"
    @Volatile private var videoEncoderName = "Not selected"
    @Volatile private var audioEncoderName = "Not selected"
    @Volatile private var audioLevel = 0f
    @Volatile private var videoCaptureAgeMs: Long? = null
    @Volatile private var publishStatus = "No publisher attached"
    @Volatile private var recordStatus = "No recorder attached"
    @Volatile private var recordingFailed = false
    private var camera: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var preview: Surface? = null
    private var cameraInput: Surface? = null
    private var autofocusMode = CaptureRequest.CONTROL_AF_MODE_OFF
    private var encoderSurface: Surface? = null
    private var videoEncoder: PlatformCodec? = null
    private var audioEncoder: PlatformCodec? = null
    private var audioRecord: AudioRecord? = null
    private var audioThread: Thread? = null
    private var audioPumpThread: Thread? = null
    private var pcmPool: PcmBlockPool? = null
    private val audioWorkers = AudioWorkerCompletion()
    private var publisherAbort: Thread? = null
    private var publisherAbortDone = true
    @Volatile private var cleanupPhase = "idle"
    @Volatile private var cleanupDiagnostics = "No cleanup pending"
    private var videoCodec: RustCodec? = null
    private var audioCodec: RustCodec? = null
    private var queues: NativeSampleFanout? = null
    private var publishing: QueueDrainer? = null
    private var recording: QueueDrainer? = null
    private val framing = H264Framing()
    private val clock = CaptureClock()
    private val recovery = KeyframeRecovery()
    private var syncRequests = 0L
    private var diagnosticAfterNs = 0L
    @Volatile private var queueDiagnostics = "Queue diagnostics not sampled"
    private var generation = 0L
    private var drainGeneration = -1L
    private var recordingCleanup: Thread? = null
    private val controls = ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS, ArrayBlockingQueue(4),
        { task -> Thread(task, "sapflow-capture-cleanup") }, ThreadPoolExecutor.AbortPolicy())
    private val restart = OrientationRestartPlan()
    private var renderer: CameraGlRenderer? = null
    private var plan: OrientationPlan? = null
    private var captureProfile = CaptureProfile.DEFAULT
    private val prepareAdmission = CapturePrepareAdmission()
    private val startAdmission = CaptureStartAdmission()
    private var preparation: CaptureStartAdmission.Preparation? = null // codec owner only
    private var previewWidth = 1
    private var previewHeight = 1
    private var displayDegrees = 0
    private var naturalPortrait = true
    private var autoStart = false
    private var startPending = false
    private var shutdownRequested = false
    private var teardownStarted = false
    private var cameraClosed = true
    private var rendererStopped = true
    private var rendererClosed = true
    private var rendererCloseRequested = false
    private val microphoneStopped get() = audioWorkers.stopped
    private var audioStopReturned = true
    private var tailEnabled = false
    private var tailStarted = false
    private var tailDone = true
    private var videoEos = false
    private var audioEos = false
    private var audioEosQueued = false
    private val audioInputEndUs = AtomicLong()
    private var stopReason = "Stopped"
    private val lifecycle = CaptureLifecycleDiagnostics()
    private var captureEpoch = -1L
    private var stopFailed = false
    private var cleanupExpired = false
    private var publishFactory: (() -> EncodedSink?)? = null
    private var recordFactory: (() -> EncodedSink?)? = null
    @Volatile private var orientationStatus = "Orientation not prepared"
    @Volatile private var rendererStatus = "GPU not prepared"
    @Volatile private var previousRecording = ""
    private var stopDeadline: Runnable? = null
    private var restartDeadline: Runnable? = null
    private val tailDeadline = Runnable {
        if (!tailDone) {
            tailDone = true
            stopReason = "Encoder EOS exceeded 500ms; accepted recording prefix retained"
            markFailure(CaptureLifecycleDiagnostics.Code.ENCODER_EOS_TIMEOUT, stopReason)
            finishStoppedEpoch()
        }
    }

    /** Attach configured sinks before prepare. Their IO executes on separate
     * owned workers, with one in-flight immutable handle each. */
    private var publishingSink: EncodedSink? = null
    @Volatile private var publishingSession: EncodedSink? = null
    private var lastTransportSyncNs = Long.MIN_VALUE
    private var recordingSink: EncodedSink? = null

    fun snapshot() = CaptureSnapshot(stage, message, videoEncoderName, audioEncoderName,
        acceptedVideo.get(), acceptedAudio.get(), drops.get(), videoRejects.get(), audioPrimingRejects.get(), waitingKeyframeRejects.get(), queueRejects.get(), audioLevel, videoCaptureAgeMs,
        if (stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING)) publishingSink?.status ?: publishStatus else publishStatus,
        (if (!recordingFailed && stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING)) recordingSink?.status ?: recordStatus else recordStatus) + if (previousRecording.isNotEmpty()) " · Previous: ${previousRecording.take(240)}" else "",
        queueDiagnostics, "Publish drainer ${publishing?.diagnostics ?: "none"} · ${publishingSink?.diagnostics ?: ""}",
        orientationStatus, rendererStatus, audioDiagnostics + if (audioFailureDiagnostics.isNotEmpty()) " · $audioFailureDiagnostics" else "", "$cleanupPhase · $cleanupDiagnostics",
        !prepareAdmission.pending && startAdmission.idle && !shutdownRequested && !stoppingEpoch && !teardownStarted && stage in listOf(CaptureStage.IDLE, CaptureStage.ERROR) && videoEncoder == null && audioEncoder == null && renderer == null && audioRecord == null)

    internal fun prepare(previewSurface: Surface, publisher: () -> EncodedSink?, recorder: () -> EncodedSink?, profile: CaptureProfile = CaptureProfile.DEFAULT): CaptureStartAdmission.Preparation? {
        val canPrepare = snapshot().canPrepare
        val ticket = startAdmission.reserve() ?: return null
        val admitted = try { prepareAdmission.submit(profile, canPrepare, { handler.post(it) }) admitted@{ requestedProfile ->
            if (shutdownRequested || stage !in listOf(CaptureStage.IDLE, CaptureStage.ERROR)) { startAdmission.abandon(ticket); return@admitted }
            if (videoEncoder != null || renderer != null || teardownStarted) {
                message = lifecycle.message("Previous native cleanup is unfinished; capture cannot restart")
                startAdmission.abandon(ticket)
                return@admitted
            }
            if (!startAdmission.claim(ticket)) { startAdmission.abandon(ticket); return@admitted }
            preparation = ticket
            cleanupExpired = false; stopFailed = false
            publishingSession?.stopDestination(); publishingSession = null
            captureProfile = requestedProfile // Retained through every orientation restart.
            publishFactory = publisher
            recordFactory = recorder
            autoStart = false
            prepareEpoch(previewSurface)
        } } catch (error: Exception) { startAdmission.abandon(ticket); throw error }
        if (!admitted) { startAdmission.abandon(ticket); return null }
        return ticket
    }

    private fun prepareEpoch(previewSurface: Surface) {
        if (shutdownRequested || cleanupExpired) return
        val intent = preparation ?: return
        if (!startAdmission.beginEpoch(intent, generation + 1)) return
        stage = CaptureStage.PREPARING
        message = "Preparing hardware pixel rotation and native audio"
        generation++
        drainGeneration = generation
        captureEpoch = drainGeneration
        lifecycle.beginEpoch(captureEpoch)
        teardownStarted = false
        stoppingEpoch = false
        rendererStopped = false
        rendererClosed = false
        rendererCloseRequested = false
        cameraClosed = true
        audioStopReturned = true
        audioOwnerQuiesced = false; audioQuiesceRequested = false
        publisherAbortDone = true; publisherAbort = null
        cleanupPhase = "prepared ownership"; cleanupDiagnostics = "No stop requested"
        tailStarted = false
        tailDone = true
        startPending = false
        traceLifecycle(CaptureLifecycleDiagnostics.Code.PREPARING)
        try {
            val rawPublisher = publishFactory?.invoke()
            val publisherGeneration = generation
            var isolated: IsolatedPublishingSink? = null
            isolated = rawPublisher?.let { delegate -> IsolatedPublishingSink(delegate) { error -> handler.post {
                if (publisherGeneration == drainGeneration && publishingSink === isolated) {
                    publishStatus = "Publisher failed (${error.javaClass.simpleName}) · local recording continues"
                    abortPublisherEarly()
                }
            } } }
            publishingSink = isolated
            if (publishingSession == null) publishingSession = publishingSink
            val publishingEpoch = generation
            publishingSink?.setKeyframeRequester { handler.post {
                val now = System.nanoTime()
                if (publishingEpoch == generation && stage == CaptureStage.CAPTURING &&
                    (lastTransportSyncNs == Long.MIN_VALUE || now - lastTransportSyncNs >= 1_000_000_000L)) {
                    lastTransportSyncNs = now
                    try {
                        videoEncoder?.setParameters(android.os.Bundle().apply { putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                        syncRequests++
                    } catch (error: Exception) { fail("Transport recovery keyframe request failed: ${error.javaClass.simpleName}") }
                }
            } }
            recordingSink = recordFactory?.invoke()
            prepareOnHandler(previewSurface, generation)
        } catch (error: Exception) { fail("Prepare failed: ${error.message ?: error.javaClass.simpleName}") }
    }

    internal fun updatePreview(surface: Surface?, width: Int, height: Int, rotationDegrees: Int, naturalPortrait: Boolean) {
        handler.post {
            preview = surface?.takeIf { it.isValid }
            previewWidth = maxOf(1, width); previewHeight = maxOf(1, height)
            displayDegrees = rotationDegrees
            this.naturalPortrait = naturalPortrait
            renderer?.setPreview(preview, previewWidth, previewHeight, displayDegrees)
        }
    }

    internal fun orientationChanged(pose: CardinalPose) {
        handler.post {
            if (shutdownRequested) return@post
            val active = videoEncoder != null || renderer != null
            val ticket = restart.request(pose, active, System.nanoTime())
            if (ticket != null) {
                traceLifecycle(CaptureLifecycleDiagnostics.Code.ORIENTATION_REQUEST)
                restartDeadline?.let(handler::removeCallbacks)
                restartDeadline = Runnable {
                    if (restart.pending?.token == ticket.token && restart.expired(System.nanoTime())) {
                        markFailure(CaptureLifecycleDiagnostics.Code.ORIENTATION_TIMEOUT, "Orientation restart exceeded six seconds")
                        stopOnHandler("Orientation restart exceeded six seconds", failed = true)
                    }
                }.also { handler.postDelayed(it, ((ticket.deadlineNs - System.nanoTime()).coerceAtLeast(0) + 999_999) / 1_000_000) }
                stopOnHandler("Changing encoded orientation; finalising the previous file", failed = false)
            }
        }
    }

    @SuppressLint("MissingPermission")
    private fun prepareOnHandler(previewSurface: Surface, token: Long) {
        require(previewSurface.isValid) { "Preview surface is unavailable" }
        preview = previewSurface
        recordingFailed = false
        val manager = context.getSystemService(CameraManager::class.java)
        val pose = restart.desiredPose
        val cameraId = manager.cameraIdList.firstOrNull { id ->
            val c = manager.getCameraCharacteristics(id)
            val sensor = c.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: return@firstOrNull false
            val candidate = OrientationPlan.forRear(sensor, pose, naturalPortrait, captureProfile)
            c.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK &&
                (Build.VERSION.SDK_INT >= 33 || c.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) == CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME) &&
                c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)?.getOutputSizes(SurfaceTexture::class.java)?.contains(Size(candidate.cameraWidth, candidate.cameraHeight)) == true &&
                c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)?.contains(Range(30, 30)) == true
        } ?: error("No rear camera supports the upright 720p30 GPU input and a validated timestamp source")
        val characteristics = manager.getCameraCharacteristics(cameraId)
        val selected = OrientationPlan.forRear(characteristics.get(CameraCharacteristics.SENSOR_ORIENTATION)!!, pose, naturalPortrait, captureProfile)
        plan = selected
        orientationStatus = "${captureProfile.label} · ${captureProfile.videoBps} bps AVC + ${captureProfile.audioBps} bps AAC · ${selected.width}×${selected.height} · ${pose.name} · natural texture rotation ${selected.encoderClockwiseDegrees}°"
        autofocusMode = if (characteristics.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.contains(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO) == true)
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO else CaptureRequest.CONTROL_AF_MODE_OFF

        val videoInfo = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.firstOrNull { info ->
            info.isEncoder && info.isHardwareAccelerated && info.supportedTypes.any { it.equals(MediaFormat.MIMETYPE_VIDEO_AVC, true) } &&
                runCatching {
                    val caps = info.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                    val videoCaps = checkNotNull(caps.videoCapabilities)
                    captureProfile.supportedBy(videoCaps.widthAlignment, videoCaps.heightAlignment,
                        videoCaps.bitrateRange.lower, videoCaps.bitrateRange.upper,
                        caps.colorFormats.contains(MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface),
                        caps.encoderCapabilities?.isBitrateModeSupported(MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR) == true,
                        caps.profileLevels.any { it.profile == MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline },
                        videoCaps::areSizeAndRateSupported)
                }.getOrDefault(false)
        } ?: error("No hardware H264 baseline encoder supports ${captureProfile.label}30 CBR in both orientations")
        val video = PlatformCodec.createByCodecName(videoInfo.name)
        videoEncoder = video
        videoEncoderName = "${videoInfo.name} (hardware)"
        video.setCallback(videoCallback(), handler)
        video.configure(MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, selected.width, selected.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, captureProfile.videoBps)
            setInteger(MediaFormat.KEY_FRAME_RATE, 30)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
            setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
        }, null, null, PlatformCodec.CONFIGURE_FLAG_ENCODE)
        encoderSurface = video.createInputSurface()

        val audio = PlatformCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
        audioEncoder = audio
        audioEncoderName = audio.name
        audioTailRequested = AtomicBoolean(false)
        audioInputAdmission = AtomicBoolean(true)
        encodedAudio = createAudioEpoch(token)
        audio.setCallback(encodedAudio!!, audioHandler)
        audio.configure(MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, 48_000, 1).apply {
            setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            setInteger(MediaFormat.KEY_BIT_RATE, captureProfile.audioBps)
            setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 2048)
        }, null, null, PlatformCodec.CONFIGURE_FLAG_ENCODE)
        val minAudio = AudioRecord.getMinBufferSize(48_000, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        require(minAudio > 0) { "48 kHz mono microphone capture is unavailable" }
        audioRecord = AudioRecord.Builder()
            .setAudioSource(MediaRecorder.AudioSource.MIC)
            .setAudioFormat(AudioFormat.Builder().setSampleRate(48_000).setChannelMask(AudioFormat.CHANNEL_IN_MONO).setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
            .setBufferSizeInBytes(maxOf(minAudio * 2, 8192)).build()
        require(audioRecord?.state == AudioRecord.STATE_INITIALIZED) { "Microphone initialisation failed" }
        require(audioRecord?.sampleRate == 48_000 && audioRecord?.channelCount == 1 && audioRecord?.audioFormat == AudioFormat.ENCODING_PCM_16BIT) { "Microphone changed the requested 48k mono PCM format" }
        queues = NativeSampleFanout(4uL * 1024uL * 1024uL, 8uL * 1024uL * 1024uL, 2000u, true)
        publishing = QueueDrainer(handler, queues!!, true, publishingSink, { fail(it) })
        recording = QueueDrainer(handler, queues!!, false, recordingSink, ::failRecording)
        video.start()
        audio.start()
        val mapper = if (Build.VERSION.SDK_INT >= 33) CameraTimestampMapper.monotonic() else CameraTimestampMapper.realtime()
        renderer = CameraGlRenderer(selected.cameraWidth, selected.cameraHeight, selected, mapper, token, handler,
            onReady = { input, diagnostic ->
                if (token == generation && stage == CaptureStage.PREPARING) {
                    cameraInput = input
                    rendererStatus = diagnostic
                    renderer?.setPreview(preview, previewWidth, previewHeight, displayDegrees)
                    cameraClosed = false
                    try {
                        manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
                            override fun onOpened(device: CameraDevice) {
                                if (token != generation || stage != CaptureStage.PREPARING) { device.close(); return }
                                camera = device
                                val output = OutputConfiguration(input)
                                if (Build.VERSION.SDK_INT >= 33) output.timestampBase = OutputConfiguration.TIMESTAMP_BASE_MONOTONIC
                                device.createCaptureSession(SessionConfiguration(SessionConfiguration.SESSION_REGULAR, listOf(output), Executor { handler.post(it) }, object : CameraCaptureSession.StateCallback() {
                                    override fun onConfigured(configured: CameraCaptureSession) {
                                        if (token != generation || stage != CaptureStage.PREPARING) { configured.close(); return }
                                        try {
                                            session = configured
                                            val request = device.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                                                addTarget(input)
                                                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, Range(30, 30))
                                                set(CaptureRequest.CONTROL_AF_MODE, autofocusMode)
                                                if (Build.VERSION.SDK_INT >= 31 && characteristics.get(CameraCharacteristics.SCALER_AVAILABLE_ROTATE_AND_CROP_MODES)?.contains(CaptureRequest.SCALER_ROTATE_AND_CROP_NONE) == true)
                                                    set(CaptureRequest.SCALER_ROTATE_AND_CROP, CaptureRequest.SCALER_ROTATE_AND_CROP_NONE)
                                            }.build()
                                            configured.setRepeatingRequest(request, null, handler)
                                            if (restart.desiredPose != selected.pose) {
                                                stopOnHandler("Pose changed during preparation; using the latest pose", false)
                                                return
                                            }
                                            stage = CaptureStage.PREPARED
                                            val intent = preparation
                                            if (intent == null || !startAdmission.prepared(intent, token)) {
                                                stopOnHandler("Capture approval ended before preparation completed", false)
                                                return
                                            }
                                            traceLifecycle(CaptureLifecycleDiagnostics.Code.PREPARED)
                                            message = "Ready: upright ${selected.width}×${selected.height} hardware video and AAC"
                                            if (autoStart) startAdmission.start(intent)?.let(::startOnHandler)
                                            else restart.pending?.let {
                                                check(restart.complete(it.token, System.nanoTime()) != null) { "Orientation preparation deadline expired" }
                                                restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
                                            }
                                            if (!autoStart) restart.beginSession()
                                        } catch (error: Exception) { fail("Camera start failed: ${error.message}") }
                                    }
                                    override fun onConfigureFailed(failed: CameraCaptureSession) { failed.close(); if (token == generation) fail("Camera GPU input session was refused") }
                                }))
                            }
                            override fun onClosed(device: CameraDevice) {
                                if (token == drainGeneration) { cameraClosed = true; finishStoppedEpoch() }
                            }
                            override fun onDisconnected(device: CameraDevice) { device.close(); if (token == generation) fail("Camera disconnected") }
                            override fun onError(device: CameraDevice, code: Int) { device.close(); if (token == generation) fail("Camera interrupted (error $code)") }
                        }, handler)
                    } catch (error: Exception) { cameraClosed = true; fail("Camera open failed: ${error.message}") }
                }
            }, onFailure = { reason -> if (token == drainGeneration) {
                autoStart = false; restart.cancel()
                if (stoppingEpoch) { markFailure(CaptureLifecycleDiagnostics.Code.GPU_FAILURE, reason); stopReason = reason }
                else fail(reason, CaptureLifecycleDiagnostics.Code.GPU_FAILURE)
            } })
    }

    fun start() { startAdmission.preparation?.let(::start) }
    internal fun start(preparation: CaptureStartAdmission.Preparation) {
        val ticket = startAdmission.start(preparation) ?: return
        if (!handler.post { if (startAdmission.permits(ticket) && stage == CaptureStage.PREPARED) { autoStart = true; startOnHandler(ticket) } }) startAdmission.cancelInitial(ticket.preparation)
    }

    private fun startOnHandler(ticket: CaptureStartAdmission.Start) {
        if (stage != CaptureStage.PREPARED || startPending || !startAdmission.permits(ticket)) return
        startPending = true
        val token = generation
        try {
            clock.start()
            renderer!!.startEncoding(encoderSurface!!, token) {
                if (token != generation || stage != CaptureStage.PREPARED || !startAdmission.permits(ticket)) return@startEncoding
                try {
                    check(plan?.pose == restart.desiredPose) { "Pose changed before encoder start" }
                    restart.pending?.let { check(restart.complete(it.token, System.nanoTime()) != null) { "Orientation restart deadline expired" } }
                    restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
                    recovery.begin(token)
                    syncRequests = 0; diagnosticAfterNs = 0
                    queueDiagnostics = "Queue diagnostics awaiting samples"
                    acceptedVideo.set(0); acceptedAudio.set(0); drops.set(0)
                    videoCaptureAgeMs = null
                    videoRejects.set(0); audioPrimingRejects.set(0); waitingKeyframeRejects.set(0); queueRejects.set(0)
                    recordingSink?.start()
                    if (!startAdmission.commit(ticket)) {
                        autoStart = false; restart.cancel()
                        stopOnHandler("Destination ended before capture admission; closing prepared recording owners", false)
                        return@startEncoding
                    }
                    publishingSink?.configure(videoCodec, audioCodec)
                    publishingSink?.start()
                    stage = CaptureStage.CAPTURING
                    traceLifecycle(CaptureLifecycleDiagnostics.Code.STARTED)
                    restart.beginSession()
                    startPending = false
                    publishStatus = publishingSink?.status ?: "No publisher attached · SDK validation only"
                    recordStatus = recordingSink?.status ?: "No recorder attached · no file is being written"
                    message = "Capturing upright hardware video into the shared SDK"
                    videoEncoder!!.setParameters(android.os.Bundle().apply { putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                    encodedAudio!!.begin()
                    startAudio()
                } catch (error: Exception) { fail("Start failed: ${error.message ?: error.javaClass.simpleName}") }
            }
        } catch (error: Exception) { fail("Start failed: ${error.message ?: error.javaClass.simpleName}") }
    }

    private fun startAudio() {
        val record = audioRecord!!
        val codec = audioEncoder!!
        val epochNs = clock.mediaEpochNs()
        val pool = PcmBlockPool()
        pcmPool = pool
        runningAudio.set(true)
        audioWorkers.begin()
        pcmFrames.set(0); queuedAudioFrames.set(0); outputAudioUnits.set(0); outputAudioPtsUs.set(0)
        audioInputWaitMaxUs.set(0); audioReadMaxUs.set(0)
        audioInputEndUs.set(0)
        val token = drainGeneration
        val runtimeAtStart = runtimeDiagnostics()
        audioRuntimeAtStart = runtimeAtStart; audioFailureDiagnostics = ""; audioOwnerFailureReported = false
        val failureReported = AtomicBoolean(false)
        fun reportFailure(owner: String, error: Exception) {
            if (error is InterruptedException && !runningAudio.get()) return
            if (!failureReported.compareAndSet(false, true)) return
            val failedPool = pool.diagnostics()
            runningAudio.set(false); pool.cancel()
            handler.post { if (token == drainGeneration) {
                val reason = "Audio $owner failed: ${error.message ?: error.javaClass.simpleName}"
                audioFailureDiagnostics = "FAILED $owner PCM=${pcmFrames.get()} input=${queuedAudioFrames.get()} output=${outputAudioUnits.get()} poolBeforeCancel={$failedPool}: ${error.message ?: error.javaClass.simpleName} · runtimeStart={$runtimeAtStart} runtimeFailure={${runtimeDiagnostics()}}"
                if (stoppingEpoch) {
                    markFailure(if (owner == "AAC pump") CaptureLifecycleDiagnostics.Code.AUDIO_PUMP_FAILURE else CaptureLifecycleDiagnostics.Code.AUDIO_READER_FAILURE, reason)
                    tailEnabled = false; tailDone = true
                    stopReason = reason; abortPublisherEarly(); finishStoppedEpoch()
                } else fail(reason, if (owner == "AAC pump") CaptureLifecycleDiagnostics.Code.AUDIO_PUMP_FAILURE else CaptureLifecycleDiagnostics.Code.AUDIO_READER_FAILURE)
            } }
        }
        audioPumpThread = Thread({
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
            try {
                var permissionCheckNs = 0L
                while (runningAudio.get() || !pool.finished) {
                    val block = pool.acquireEncoder(100, System::nanoTime) ?: continue
                    try {
                        var index: Int? = null
                        val waitStart = System.nanoTime()
                        while (index == null) {
                            pool.checkAge(block, System.nanoTime())
                            index = audioInputs.poll(20, TimeUnit.MILLISECONDS)
                        }
                        audioInputWaitMaxUs.accumulateAndGet((System.nanoTime() - waitStart) / 1000, ::maxOf)
                        pool.checkAge(block, System.nanoTime())
                        val input = codec.getInputBuffer(index) ?: error("AAC input buffer unavailable")
                        require(input.capacity() >= block.bytes) { "AAC input buffer smaller than PCM block" }
                        block.buffer.position(0); block.buffer.limit(block.bytes)
                        input.clear(); input.put(block.buffer) // One explicit platform-only PCM copy.
                        val pumpNow = System.nanoTime()
                        if (pumpNow >= permissionCheckNs) {
                            permissionCheckNs = pumpNow + 200_000_000
                            check(record.activeRecordingConfiguration?.isClientSilenced != true) { "Microphone was silenced by the platform" }
                        }
                        var square = 0.0
                        block.buffer.order(ByteOrder.nativeOrder())
                        for (offset in 0 until block.bytes step 2) {
                            val value = block.buffer.getShort(offset).toDouble() / 32768.0
                            square += value * value
                        }
                        audioLevel = sqrt(square / (block.bytes / 2)).toFloat().coerceIn(0f, 1f)
                        codec.queueInputBuffer(index, 0, block.bytes, block.ptsUs, 0)
                        queuedAudioFrames.addAndGet(block.bytes / 2L)
                        audioInputEndUs.set(block.ptsUs + block.bytes / 2L * 1_000_000 / 48_000)
                    } finally { pool.release(block) }
                }
            } catch (error: Exception) { reportFailure("AAC pump", error) }
            finally { handler.post { if (token == drainGeneration) { audioWorkers.pumpCompleted(); queueAudioEos(); finishStoppedEpoch() } } }
        }, "sapflow-aac-input").apply { start() }
        audioThread = Thread({
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
            var owned: PcmBlockPool.Block? = null
            var timingDiagnostics: (() -> String)? = null
            try {
                synchronized(record) { if (runningAudio.get()) record.startRecording() }
                require(record.recordingState == AudioRecord.RECORDSTATE_RECORDING) { "Microphone did not start" }
                val captureStartedNs = System.nanoTime()
                val capacityFrames = record.bufferSizeInFrames.toLong()
                val timing = AudioCaptureTiming(epochNs, capacityFrames)
                timingDiagnostics = { timing.observation }
                val stamp = AudioTimestamp()
                var diagnosticAt = 0L
                while (runningAudio.get()) {
                    val block = pool.acquireReader() ?: break
                    owned = block
                    val beforeStatus = record.getTimestamp(stamp, AudioTimestamp.TIMEBASE_MONOTONIC)
                    if (beforeStatus == AudioRecord.SUCCESS) {
                        timing.beforeRead(stamp.framePosition, stamp.nanoTime, System.nanoTime())
                    } else check(timing.framesRead == 0L && System.nanoTime() - captureStartedNs <= capacityFrames * 1_000_000_000 / 48_000) { "Microphone hardware timestamp unavailable within capture capacity; status=$beforeStatus ${timing.observation}" }
                    val readStart = System.nanoTime()
                    val count = record.read(block.buffer, block.buffer.capacity(), AudioRecord.READ_BLOCKING)
                    val nowNs = System.nanoTime()
                    audioReadMaxUs.accumulateAndGet((nowNs - readStart) / 1000, ::maxOf)
                    if (!runningAudio.get()) break
                    require(count > 0 && count % 2 == 0) { "Microphone read failed ($count); ${timing.observation}" }
                    pcmFrames.addAndGet(count / 2L)
                    val afterStatus = record.getTimestamp(stamp, AudioTimestamp.TIMEBASE_MONOTONIC)
                    check(afterStatus == AudioRecord.SUCCESS) { "Microphone hardware timestamp unavailable after read; status=$afterStatus ${timing.observation}" }
                    val ptsUs = timing.read(count / 2, stamp.framePosition, stamp.nanoTime, System.nanoTime())
                    pool.publish(block, count, ptsUs, epochNs + ptsUs * 1000)
                    owned = null
                    if (nowNs >= diagnosticAt) {
                        diagnosticAt = nowNs + 200_000_000
                        audioDiagnostics = "PCM ${pcmFrames.get()}f · AAC input ${queuedAudioFrames.get()}f/PTS ${audioInputEndUs.get()}us · output ${outputAudioUnits.get()}AU/PTS ${outputAudioPtsUs.get()}us/valid ${acceptedAudio.get()}AU · elapsed ${(nowNs - captureStartedNs) / 1_000_000}ms · hardware ${stamp.framePosition}f · backlog ${timing.backlogFrames}/${record.bufferSizeInFrames}f · stamp ${timing.timestampAgeUs}us · input wait max ${audioInputWaitMaxUs.get()}us/read ${audioReadMaxUs.get()}us · ${encodedAudio?.diagnostics ?: "AAC owner unavailable"} · indices ${audioInputs.size} · PCM ready ${pool.queued}/8 high ${pool.highWater}/16KiB"
                    }
                }
            } catch (error: Exception) {
                // AudioRecord.stop may invalidate a concurrent read/timestamp.
                // A normal Stop already closed producer admission; real capture
                // faults and owner-release faults still take the failure path.
                if (runningAudio.get() || failureReported.get()) reportFailure("reader", IllegalStateException("${error.message}; latest={${timingDiagnostics?.invoke() ?: "No hardware observation"}} readMaxUs=${audioReadMaxUs.get()} inputWaitMaxUs=${audioInputWaitMaxUs.get()}", error))
            }
            finally {
                try { owned?.let { pool.release(it) } }
                catch (error: Exception) { reportFailure("reader owner release", error) }
                finally {
                    pool.finishReading()
                    handler.post { if (token == drainGeneration) { audioWorkers.readerCompleted(); queueAudioEos(); finishStoppedEpoch() } }
                }
            }
        }, "sapflow-microphone").apply { start() }
    }

    private fun createAudioEpoch(token: Long): EncodedAudioEpoch {
        val inputs = audioInputAdmission
        val tail = audioTailRequested
        return EncodedAudioEpoch(audioHandler, handler,
            onInput = { index ->
                if (inputs.get()) {
                    if (!audioInputs.offer(index)) handler.post { if (token == drainGeneration) fail("AAC input buffer bound exceeded") }
                    else if (tail.get()) handler.post { if (token == drainGeneration && stoppingEpoch) { queueAudioEos(); finishStoppedEpoch() } }
                }
            },
            onFormat = { config ->
                if (token == drainGeneration) {
                    audioCodec = config // Borrowed until the AUDIO quiescence barrier transfers it.
                    try { publishingSink?.configure(videoCodec, audioCodec) }
                    catch (error: Exception) { fail("AAC sink configuration rejected: ${error.message}") }
                }
            },
            onSample = { sample ->
                if (token == drainGeneration && (stage == CaptureStage.CAPTURING || (stoppingEpoch && tailEnabled && !stopFailed))) submitSample(sample, token)
            },
            onValidated = { acceptedAudio.incrementAndGet() },
            onPriming = { drops.incrementAndGet(); audioPrimingRejects.incrementAndGet() },
            onRawOutput = { pts -> outputAudioUnits.incrementAndGet(); outputAudioPtsUs.set(pts) },
            onEos = { if (token == drainGeneration) { audioEos = true; finishStoppedEpoch() } },
            onFailure = { reason -> if (token == drainGeneration) {
                if (!audioOwnerFailureReported) {
                    audioOwnerFailureReported = true
                    if (audioFailureDiagnostics.isEmpty()) audioFailureDiagnostics = "AAC owner failure: $reason · ${encodedAudio?.diagnostics} · runtimeStart={$audioRuntimeAtStart} runtimeFailure={${runtimeDiagnostics()}}"
                }
                if (stoppingEpoch) {
                    markFailure(CaptureLifecycleDiagnostics.Code.AUDIO_OWNER_FAILURE, reason)
                    tailEnabled = false; tailDone = true
                    stopReason = reason; abortPublisherEarly(); finishStoppedEpoch()
                } else fail(reason, CaptureLifecycleDiagnostics.Code.AUDIO_OWNER_FAILURE)
            } },
            onHandoffIdle = { if (token == drainGeneration) finishStoppedEpoch() })
    }

    /** Borrowed immutable owner; all fanout calls remain on this handler. */
    private fun submitSample(sample: MediaSample, token: Long) {
        val nowNs = SystemClock.elapsedRealtimeNanos()
        val result = queues!!.push(sample, nowNs.toULong())
        if (recovery.observe(token, result.publishingWaitingKeyframe, result.publishingOverflows, nowNs)) {
            handler.post {
                if (token == generation && stage == CaptureStage.CAPTURING && recovery.take(token)) {
                    try {
                        videoEncoder?.setParameters(android.os.Bundle().apply { putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                        syncRequests++
                    } catch (error: Exception) { fail("Recovery keyframe request failed: ${error.javaClass.simpleName}") }
                }
            }
        }
        if (!result.publishingQueued) {
            drops.incrementAndGet()
            if (result.publishingWaitingKeyframe) waitingKeyframeRejects.incrementAndGet() else queueRejects.incrementAndGet()
        }
        if (result.recordingFailed) failRecording("Recording stopped: its queue exceeded the byte/time bound")
        if (nowNs >= diagnosticAfterNs) {
            diagnosticAfterNs = nowNs + 200_000_000
            val stats = queues!!.stats()
            queueDiagnostics = "Publish queue ${stats.publishingBytes}B/${stats.publishingMediaMs}ms · high ${stats.publishingHighWaterBytes}B/${stats.publishingHighWaterMediaMs}ms · dropped ${stats.publishingDrops} · resets ${stats.publishingOverflows} (bytes ${stats.publishingOverflowBytes}, samples ${stats.publishingOverflowSamples}, media ${stats.publishingOverflowMedia}, age ${stats.publishingOverflowResidence}) · IDR requests $syncRequests"
        }
        publishing!!.available()
        recording?.available()
    }


    private fun videoCallback(): PlatformCodec.Callback {
        val token = drainGeneration
        return object : PlatformCodec.Callback() {
            override fun onInputBufferAvailable(codec: PlatformCodec, index: Int) = Unit
            override fun onOutputFormatChanged(codec: PlatformCodec, format: MediaFormat) {
                if (token != drainGeneration) return
                try {
                    require(format.getInteger(MediaFormat.KEY_WIDTH) == plan?.width && format.getInteger(MediaFormat.KEY_HEIGHT) == plan?.height) { "Encoder changed the requested ${captureProfile.label} profile" }
                    check(videoCodec == null) { "Video format changed; prepare a new epoch" }
                    val first = format.getByteBuffer("csd-0")
                    val second = format.getByteBuffer("csd-1")
                    val sps = H264Framing.parameterSet(first, 7) ?: error("Encoder supplied no SPS")
                    val pps = H264Framing.parameterSet(second, 8) ?: H264Framing.parameterSet(first, 8) ?: error("Encoder supplied no PPS")
                    videoCodec = RustCodec.h264(sps, pps)
                    publishingSink?.configure(videoCodec, audioCodec)
                } catch (error: Exception) { fail("Video configuration rejected: ${error.message}") }
            }
            override fun onOutputBufferAvailable(codec: PlatformCodec, index: Int, info: PlatformCodec.BufferInfo) {
                try {
                    if (token != drainGeneration) return
                    if (info.flags and PlatformCodec.BUFFER_FLAG_END_OF_STREAM != 0) videoEos = true
                    if ((stage != CaptureStage.CAPTURING && !(tailEnabled && !tailDone)) || info.size == 0 || info.flags and PlatformCodec.BUFFER_FLAG_CODEC_CONFIG != 0) return
                    val buffer = codec.getOutputBuffer(index) ?: error("Video output buffer unavailable")
                    buffer.position(info.offset); buffer.limit(info.offset + info.size)
                    videoCaptureAgeMs = clock.videoAgeMs(info.presentationTimeUs)
                    val key = info.flags and PlatformCodec.BUFFER_FLAG_KEY_FRAME != 0
                    val tick = clock.video(info.presentationTimeUs, key) ?: run { drops.incrementAndGet(); videoRejects.incrementAndGet(); return }
                    val avcc = framing.sample(buffer)
                    val sample = (videoCodec ?: error("Video configuration not ready")).sample(avcc, tick, tick, 3000u, 90_000u, key)
                    acceptedVideo.incrementAndGet()
                    sample.use { submitSample(it, token) }
                } catch (error: Exception) { fail("Encoded video rejected: ${error.message ?: error.javaClass.simpleName}") }
                finally { runCatching { codec.releaseOutputBuffer(index, false) }; if (stoppingEpoch) handler.post { finishStoppedEpoch() } }
            }
            override fun onError(codec: PlatformCodec, error: PlatformCodec.CodecException) {
                if (token != drainGeneration) return
                if (stoppingEpoch) {
                    tailDone = true
                    stopReason = "Video encoder failed during EOS: ${error.diagnosticInfo}"
                    markFailure(CaptureLifecycleDiagnostics.Code.VIDEO_ENCODER_FAILURE, stopReason)
                    handler.post { finishStoppedEpoch() }
                } else fail("Video encoder failed: ${error.diagnosticInfo}", CaptureLifecycleDiagnostics.Code.VIDEO_ENCODER_FAILURE)
            }
        }
    }

    internal fun cancelPendingPrepare(ticket: CaptureStartAdmission.Preparation?) {
        if (ticket == null || !startAdmission.cancelInitial(ticket)) return
        handler.post {
            if (!startAdmission.owns(ticket)) return@post
            autoStart = false; restart.cancel(); restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
            stopOnHandler("Destination ended before capture began", failed = false)
        }
    }

    fun stop(reason: String = "Stopped") {
        val intent = startAdmission.stop() ?: return
        publishingSession?.stopDestination() // Fence even while codec-handler work is queued.
        handler.post {
            if (!startAdmission.owns(intent)) return@post
            publishingSession?.stopDestination()
            autoStart = false; restart.cancel(); restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
            stopOnHandler(reason, failed = false)
        }
    }

    private fun fail(reason: String, code: CaptureLifecycleDiagnostics.Code = CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE) {
        if (stage == CaptureStage.STOPPING || stage == CaptureStage.ERROR || stage == CaptureStage.IDLE) return
        startAdmission.stop()
        publishingSession?.stopDestination()
        markFailure(code, reason)
        stage = CaptureStage.STOPPING
        // Queue control cleanup after the callback has released its codec buffer.
        handler.post { stopOnHandler(reason.take(240), failed = true) }
    }

    private fun failRecording(reason: String) {
        if (recordingFailed) return
        recordingFailed = true
        recordStatus = reason.take(240)
        if (restart.pending != null) markFailure(CaptureLifecycleDiagnostics.Code.RECORDING_FAILURE, reason)
        val failed = recording
        recording = null
        // Drain the accepted prefix and finalise on its worker. The bounded
        // control wait must not block ongoing codec callbacks or publishing.
        val token = generation
        recordingCleanup = Thread({
            val complete = runCatching { failed?.close(graceful = true, drainQueued = true, drainViaHandler = true) != false }.getOrDefault(false)
            handler.post {
                if (token == generation) recordStatus = if (complete) recordingSink?.status ?: reason else "$reason · partial file retained"
            }
        }, "sapflow-record-cleanup").apply { start() }
    }

    private var stoppingEpoch = false

    private fun stopOnHandler(reason: String, failed: Boolean) {
        stopReason = reason
        if (failed) markFailure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE, reason)
        if (recordingFailed && restart.pending != null) {
            stopReason = "Recording failed; prepare manually before changing capture orientation"
            markFailure(CaptureLifecycleDiagnostics.Code.RECORDING_FAILURE, stopReason)
        }
        if (failed) {
            publishingSession?.stopDestination()
            autoStart = false; restart.cancel()
            abortPublisherEarly()
        }
        traceLifecycle(CaptureLifecycleDiagnostics.Code.STOP_REQUEST)
        if (stoppingEpoch) { message = lifecycle.message(reason); return }
        val captureIntent = preparation ?: startAdmission.preparation
        captureIntent?.let { startAdmission.beginCleanup(it, restart.pending != null && !failed && !shutdownRequested) }
        if (videoEncoder == null && renderer == null && audioEncoder == null && publishingSink == null && recordingSink == null && audioRecord == null) {
            captureIntent?.let { startAdmission.cleanup(it, true) }
            if (startAdmission.idle) preparation = null
            stage = if (stopFailed || cleanupExpired) CaptureStage.ERROR else CaptureStage.IDLE
            message = lifecycle.message(reason)
            finishControllerClose()
            return
        }
        stoppingEpoch = true
        cleanupPhase = "awaiting producer owners"
        tailEnabled = stage == CaptureStage.CAPTURING && !failed
        tailDone = !tailEnabled
        tailStarted = false
        videoEos = false; audioEos = false; audioEosQueued = false
        stage = CaptureStage.STOPPING
        message = lifecycle.message(reason)
        generation++ // Producer callbacks stop; drainGeneration retains old codec output.
        val token = drainGeneration
        val stopFence = generation
        val deadlineNs = restart.pending?.deadlineNs ?: (System.nanoTime() + 6_000_000_000L)
        stopDeadline?.let(handler::removeCallbacks)
        stopDeadline = Runnable {
            if (generation == stopFence && stoppingEpoch) {
                tailEnabled = false; tailDone = true
                markFailure(CaptureLifecycleDiagnostics.Code.CLEANUP_TIMEOUT,
                    "Native cleanup exceeded six seconds at $cleanupPhase; retained owners prevent another capture epoch", cleanupTimedOut = true)
                stage = CaptureStage.ERROR
                updateCleanupDiagnostics()
                finishStoppedEpoch()
            }
        }.also { handler.postDelayed(it, ((deadlineNs - System.nanoTime()).coerceAtLeast(0) + 999_999) / 1_000_000) }
        runCatching { session?.stopRepeating() }
        runCatching { session?.close() }; session = null
        runCatching { camera?.close() }
        val gpu = renderer
        if (gpu == null) { rendererStopped = true; rendererClosed = true }
        else gpu.stopEncoding(token) {
            if (drainGeneration == token) { rendererStopped = true; finishStoppedEpoch() }
        }
        runningAudio.set(false)
        audioTailRequested.set(true)
        audioThread?.interrupt()
        if (failed) { pcmPool?.cancel(); audioPumpThread?.interrupt() }
        audioWorkers.observeStopped(audioThread?.isAlive == true, audioPumpThread?.isAlive == true)
        val record = audioRecord
        audioStopReturned = record == null
        if (record != null) {
            controls.execute {
                runCatching { synchronized(record) { if (record.recordingState == AudioRecord.RECORDSTATE_RECORDING) record.stop() } }
                handler.post { if (drainGeneration == token) { audioStopReturned = true; finishStoppedEpoch() } }
            }
        }
        finishStoppedEpoch()
    }

    private fun queueAudioEos(index: Int? = null) {
        if (!tailStarted || audioEosQueued || !microphoneStopped || !audioStopReturned) return
        val input = index ?: audioInputs.poll() ?: return
        try {
            audioEncoder?.queueInputBuffer(input, 0, 0, audioInputEndUs.get(), PlatformCodec.BUFFER_FLAG_END_OF_STREAM)
            audioEosQueued = true
        } catch (error: Exception) {
            tailDone = true
            stopReason = "AAC EOS failed; accepted recording prefix retained: ${error.javaClass.simpleName}"
            markFailure(CaptureLifecycleDiagnostics.Code.AAC_EOS_FAILURE, stopReason)
        }
    }

    private fun finishStoppedEpoch() {
        if (!stoppingEpoch || teardownStarted) return
        val token = drainGeneration
        if (cameraClosed && rendererStopped && !rendererCloseRequested) {
            rendererCloseRequested = true
            val gpu = renderer
            if (gpu == null) rendererClosed = true
            else gpu.close { if (drainGeneration == token) { rendererClosed = true; finishStoppedEpoch() } }
        }
        if (rendererStopped && microphoneStopped && audioStopReturned && !tailStarted) {
            tailStarted = true
            if (tailEnabled) {
                try { videoEncoder?.signalEndOfInputStream() }
                catch (error: Exception) {
                    tailDone = true
                    stopReason = "Video EOS failed; accepted prefix retained: ${error.javaClass.simpleName}"
                    markFailure(CaptureLifecycleDiagnostics.Code.VIDEO_EOS_FAILURE, stopReason)
                }
                queueAudioEos()
                handler.postDelayed(tailDeadline, 500)
            } else tailDone = true
        }
        if (videoEos && audioEos) { tailDone = true; handler.removeCallbacks(tailDeadline) }
        if (tailDone && !audioQuiesceRequested) {
            audioQuiesceRequested = true
            val epoch = encodedAudio
            if (epoch == null) audioOwnerQuiesced = true
            else try {
                epoch.quiesce { transferred ->
                    if (token == drainGeneration) {
                        audioCodec = transferred
                        audioOwnerQuiesced = true
                        epoch.finishHandoff(drain = !stopFailed && videoEos && audioEos)
                        finishStoppedEpoch()
                    }
                }
            } catch (error: Exception) {
                markFailure(CaptureLifecycleDiagnostics.Code.AAC_QUIESCENCE_FAILURE,
                    "AAC owner quiescence unavailable; retained resources prevent another epoch: ${error.message}", cleanupTimedOut = true)
                stage = CaptureStage.ERROR
                abortPublisherEarly()
            }
        }
        updateCleanupDiagnostics()
        if (!audioOwnerQuiesced || (encodedAudio?.outstanding ?: 0) != 0) return
        if (!tailDone || !rendererClosed || !cameraClosed || !microphoneStopped || !audioStopReturned || !publisherAbortDone) return
        audioInputAdmission.set(false)
        teardownStarted = true
        drainGeneration = -1
        handler.removeCallbacks(tailDeadline)
        val oldVideo = videoEncoder; val oldAudio = audioEncoder; val oldInput = encoderSurface
        val oldRecord = audioRecord; val oldQueues = queues; val oldVideoCodec = videoCodec; val oldAudioCodec = audioCodec
        val oldPublishing = publishing; val oldRecording = recording; val oldRecordCleanup = recordingCleanup
        val oldPublishingSink = publishingSink; val oldRecordingSink = recordingSink
        controls.execute {
            cleanupPhase = "publisher close"
            val publishingStopped = runCatching { oldPublishing?.close() != false }.getOrDefault(false)
            cleanupPhase = "recording drain/finalisation"
            val recordingFinalised = runCatching { oldRecording?.close(graceful = true, drainQueued = true, drainViaHandler = true) != false }.getOrDefault(false)
            val recordingStopped = recordingFinalised || runCatching { oldRecording?.close() != false }.getOrDefault(false)
            runCatching { oldRecordCleanup?.join(1000) }
            val stopped = publishingStopped && recordingStopped && oldRecordCleanup?.isAlive != true
            var resourcesReleased = stopped
            if (stopped) {
                cleanupPhase = "codec/native release"
                // A partially configured codec may refuse stop; actual release
                // still determines whether its owner can be discarded.
                runCatching { oldVideo?.stop() }; runCatching { oldAudio?.stop() }
                for (release in listOf<() -> Unit>(
                    { oldVideo?.release() }, { oldAudio?.release() },
                    { oldInput?.release() }, { oldRecord?.release() }, { oldQueues?.cancel() }, { oldQueues?.close() },
                    { oldVideoCodec?.close() }, { oldAudioCodec?.close() },
                    { if (oldPublishing == null) oldPublishingSink?.close() },
                    { if (oldRecording == null) oldRecordingSink?.close() },
                )) runCatching(release).onFailure { resourcesReleased = false }
            }
            val finalRecordStatus = oldRecordingSink?.status ?: "No recorder attached; no file written"
            handler.post {
                stopDeadline?.let(handler::removeCallbacks); stopDeadline = null
                if (!resourcesReleased) {
                    preparation?.let { startAdmission.cleanup(it, false) }
                    publishingSession?.stopDestination()
                    markFailure(CaptureLifecycleDiagnostics.Code.CLEANUP_FAILURE,
                        "Capture/sink cleanup unfinished; retained resources prevent a new epoch", cleanupTimedOut = true)
                    stage = CaptureStage.ERROR
                    previousRecording = finalRecordStatus
                    return@post
                }
                if (oldRecording != null && !recordingFinalised) {
                    stopReason = "Recording finalisation failed; accepted prefix retained and automatic restart cancelled"
                    markFailure(CaptureLifecycleDiagnostics.Code.RECORDING_FINALISATION_FAILURE, stopReason)
                }
                videoEncoder = null; audioEncoder = null; encoderSurface = null; audioRecord = null; audioThread = null; audioPumpThread = null; pcmPool = null; publisherAbort = null; encodedAudio = null
                queues = null; videoCodec = null; audioCodec = null; publishing = null; recording = null; recordingCleanup = null
                publishingSink = null; recordingSink = null; renderer = null; camera = null; cameraInput = null
                audioInputs.clear(); framing.clear(); audioLevel = 0f
                teardownStarted = false; stoppingEpoch = false
                cleanupPhase = "completed"; updateCleanupDiagnostics()
                traceLifecycle(CaptureLifecycleDiagnostics.Code.CLEANUP_COMPLETED)
                previousRecording = finalRecordStatus
                recordStatus = finalRecordStatus
                publishStatus = "Publisher stopped"
                val pending = restart.pending
                val surface = preview?.takeIf { it.isValid }
                val resume = pending != null && !cleanupExpired && !stopFailed && !shutdownRequested && !restart.expired(System.nanoTime()) && surface != null
                if (!resume) startAdmission.stop()
                preparation?.let { startAdmission.cleanup(it, true) }
                if (resume && preparation?.let(startAdmission::owns) == true) { prepareEpoch(checkNotNull(surface)); return@post }
                if (pending != null) {
                    autoStart = false; restart.cancel()
                    if (!cleanupExpired && !stopFailed && !shutdownRequested && surface == null)
                        stopReason = "Preview unavailable after orientation change; prepare again"
                }
                preparation = null
                stage = if (stopFailed || cleanupExpired) CaptureStage.ERROR else CaptureStage.IDLE
                publishingSession?.stopDestination()
                message = lifecycle.message(stopReason)
                publishFactory = null; recordFactory = null
                finishControllerClose()
            }
        }
    }

    /** Approximate process totals sampled only at start/failure, never per read.
     * These are correlation evidence, not a measurement of an individual pause. */
    private fun runtimeDiagnostics(): String = listOf("gc-count", "gc-time", "blocking-gc-count", "blocking-gc-time", "bytes-allocated").joinToString(" ") { key ->
        "$key=${android.os.Debug.getRuntimeStat("art.gc.$key") ?: "unavailable"}"
    }

    /** Fixed bit positions are documented with the qualification receipt. No
     * exception text, destination, filename, credentials or media enters logs. */
    private fun lifecycleContext(): CaptureLifecycleDiagnostics.Context {
        var flags = 0
        for ((index, value) in listOf(stoppingEpoch, teardownStarted, cameraClosed, rendererStopped,
            rendererClosed, microphoneStopped, audioStopReturned, publisherAbortDone, tailDone,
            audioOwnerQuiesced, (encodedAudio?.outstanding ?: 0) == 0, stopFailed, cleanupExpired).withIndex()) {
            if (value) flags = flags or (1 shl index)
        }
        return CaptureLifecycleDiagnostics.Context(captureEpoch, generation, plan?.pose ?: restart.desiredPose,
            CaptureLifecycleDiagnostics.CaptureStageName.valueOf(stage.name), flags)
    }

    private fun traceLifecycle(code: CaptureLifecycleDiagnostics.Code) {
        lifecycle.transition(code, lifecycleContext())?.let { Log.i("SapflowLifecycle", it.logLine()) }
    }

    private fun markFailure(code: CaptureLifecycleDiagnostics.Code, reason: String, cleanupTimedOut: Boolean = false) {
        stopFailed = true
        cleanupExpired = cleanupExpired || cleanupTimedOut
        autoStart = false
        restart.cancel()
        lifecycle.failure(code, reason, lifecycleContext())?.let { Log.w("SapflowLifecycle", it.logLine()) }
        message = lifecycle.message(reason)
    }

    private fun updateCleanupDiagnostics() {
        cleanupDiagnostics = "generation=$generation/drain=$drainGeneration stopping=$stoppingEpoch teardown=$teardownStarted cameraClosed=$cameraClosed GPUstopped=$rendererStopped GPUclosed=$rendererClosed GPUcloseRequested=$rendererCloseRequested readerDone=${audioWorkers.readerDone}/alive=${audioThread?.isAlive == true} pumpDone=${audioWorkers.pumpDone}/alive=${audioPumpThread?.isAlive == true} AudioRecordStop=$audioStopReturned publisherAbort=$publisherAbortDone tail=$tailStarted/$tailDone EOS=$videoEos/$audioEos AACownerQuiesced=$audioOwnerQuiesced AACquiesceRequested=$audioQuiesceRequested AAChandles=${encodedAudio?.outstanding ?: 0} firstFailure=${lifecycle.firstFailure?.code?.name ?: "none"} transitions=${lifecycle.transitionCount}"
    }

    private fun abortPublisherEarly() {
        if (publisherAbort != null || publishingSink == null) return
        publisherAbortDone = false
        val sink = publishingSink
        val token = drainGeneration
        publisherAbort = Thread({
            val result = runCatching { sink?.close() }
            handler.post { if (token == drainGeneration) {
                publisherAbortDone = true
                if (result.isFailure) publishStatus = "Publisher aborted; cleanup reported ${result.exceptionOrNull()?.javaClass?.simpleName}"
                updateCleanupDiagnostics(); finishStoppedEpoch()
            } }
        }, "sapflow-publisher-abort").apply { start() }
    }

    private fun finishControllerClose() {
        if (shutdownRequested && !stoppingEpoch) { controls.shutdown(); audioCallbacks.quitSafely(); thread.quitSafely() }
    }

    fun close() {
        startAdmission.stop()
        publishingSession?.stopDestination()
        handler.post {
            shutdownRequested = true; autoStart = false; restart.cancel()
            publishingSession?.stopDestination()
            publishFactory = null; recordFactory = null
            stopOnHandler("Closed", failed = false)
        }
    }
}
