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
import android.media.MediaCodec as PlatformCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
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
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executor
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

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
    val captureElapsedMs: Long? = null,
    val broadcastElapsedMs: Long? = null,
    val timingUncertain: Boolean = false,
    val socketOutput: SocketOutputView = SocketOutputView.empty(),
    val activeAudioInput: String = "Not verified",
    val backgroundAudioReady: Boolean = false,
    val standbyRequested: Boolean = false,
    val privacyStatus: String = "Live input selected",
    val privacyRevision: Long = 0,
    val privacyEpoch: Long = -1,
    val canResumePrivacy: Boolean = false,
    val canRequestStandby: Boolean = false,

)

/** Native surfaces carry raw video. PCM stays in AudioRecord/MediaCodec input.
 * Only compressed direct buffers cross the synchronous borrowed-byte FFI. */
class CaptureController internal constructor(private val context: Context,
    private val backgroundStartAllowed: (CaptureStartAdmission.Preparation) -> Boolean = { false }) {
    private val thread = HandlerThread("sapflow-codecs").apply { start() }
    private val handler = Handler(thread.looper)
    private val audioCallbacks = HandlerThread("sapflow-aac-callbacks", android.os.Process.THREAD_PRIORITY_AUDIO).apply { start() }
    private val audioHandler = Handler(audioCallbacks.looper)
    private var audioOwner: AudioCaptureOwner? = null
    private var audioOwnerClosed = true
    private val audioGenerations = AtomicLong()
    @Volatile private var privacy: PrivacyStandbyPolicy? = null
    @Volatile private var privacyRun: PrivacyStandbyPolicy.Run? = null
    @Volatile private var privacyCommand: PrivacyStandbyPolicy.Command? = null
    private var privacyTimer: Runnable? = null
    private var sourceMutation: PrivacyStandbyPolicy.SourceMutation? = null
    private var privacyDispatch=PrivacyStandbyDispatch()
    private var resumeCommand: PrivacyStandbyPolicy.Command? = null
    private val branchClosePending=BooleanArray(2)
    private var resumeMicrophoneReady = false
    private var resumeCameraReady = false
    @Volatile private var audioDiagnostics = "Audio hardware timing not sampled"
    private val acceptedVideo = AtomicLong()
    private val acceptedAudio = AtomicLong()
    private val drops = AtomicLong()
    private val videoRejects = AtomicLong()
    private val audioPrimingRejects = AtomicLong()
    private val waitingKeyframeRejects = AtomicLong()
    private val queueRejects = AtomicLong()
    @Volatile private var stage = CaptureStage.IDLE
    @Volatile private var message = "Prepare the camera and microphone"
    @Volatile private var videoEncoderName = "Not selected"
    @Volatile private var audioEncoderName = "Not selected"
    @Volatile private var audioLevel = 0f
    @Volatile private var videoCaptureAgeMs: Long? = null
    @Volatile private var publishStatus = "No publisher attached"
    @Volatile private var recordStatus = "No recorder attached"
    @Volatile private var recordingFailed = false
    @Volatile private var cameraOwner: CameraCaptureOwner? = null
    @Volatile private var sourcePolicy = CameraSourcePolicy()
    @Volatile private var sourceRequests = CameraRequestSlot()
    private var sourceRequest: CameraRequestSlot.Ticket? = null
    @Volatile private var runAdmission = CameraRunAdmission(-1)
    private var sourceSerial = 0L // Never reused across orientation epochs within this controller.
    private var sourceOperation: CameraSourcePolicy.Operation? = null
    private var cameraDesired = CameraSourceChoice.REAR
    private var sourceDeadline: Runnable? = null
    private var deferredCameraPose = false
    private val poseInputs = CameraPoseInput<CaptureStartAdmission.Preparation>()
    @Volatile private var cameraView = CameraSourceSnapshot()
    @Volatile private var controlMailbox = CameraControlMailbox()
    private var preview: Surface? = null
    private var cameraInput: Surface? = null
    private var encoderSurface: Surface? = null
    private var videoEncoder: PlatformCodec? = null
    private var audioPin: AudioInputPin? = null
    @Volatile private var activeAudioInput = "Not verified"
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
    @Volatile private var captureIntent: CaptureIntent = CaptureIntent.DEFAULT
    private val prepareAdmission = CapturePrepareAdmission()
    private val startAdmission = CaptureStartAdmission()
    private val scannerPermissions = ManualQrPermissionReplies<CaptureStartAdmission.ScannerTicket>()
    @Volatile private var scanner: ManualQrScanner? = null
    private var preparation: CaptureStartAdmission.Preparation? = null // codec owner only
    private var previewWidth = 1
    private var previewHeight = 1
    private var displayDegrees = 0
    private var naturalPortrait = true
    @Volatile private var observedNaturalPortrait = true
    @Volatile private var observedPose = CardinalPose.NATURAL
    private var autoStart = false
    private var startPending = false
    private var shutdownRequested = false
    private var teardownStarted = false
    private var cameraClosed = true
    private var rendererStopped = true
    private var rendererClosed = true
    private var rendererCloseRequested = false
    private val microphoneStopped get() = audioOwnerClosed
    private var audioStopReturned = true
    private var tailEnabled = false
    private var tailStarted = false
    private var tailDone = true
    private var videoEos = false
    private var audioEos = false
    private var audioEosQueued = false
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
    private data class TelemetryDestination(val preparation: CaptureStartAdmission.Preparation, val source: SocketOutputSource?, val attached: Boolean)
    @Volatile private var telemetryDestination: TelemetryDestination? = null
    private val socketSampler = SocketOutputSampler()
    private var lastTransportSyncNs = Long.MIN_VALUE
    private var recordingSink: EncodedSink? = null

    fun snapshot(): CaptureSnapshot {
        val now = System.nanoTime()
        val ticket = startAdmission.elapsedPreparation
        val destination = telemetryDestination?.takeIf { it.preparation === ticket }
        val sampled = socketSampler.sample(now, ticket, destination?.source, destination?.attached == true)
        val elapsed = startAdmission.elapsed(ticket, null, destination?.attached == true, socketSampler.destinationEnd())
        val socket = if (startAdmission.elapsedPreparation !== ticket) SocketOutputView.empty().copy(availability = SocketOutputAvailability.UNKNOWN, incomplete = true)
        else if (elapsed.broadcastEnded && destination?.attached == true) sampled.copy(bitsPerSecond = null,
            availability = SocketOutputAvailability.UNKNOWN, ended = true) else sampled
        val privacyView=privacy?.snapshot()
        return CaptureSnapshot(stage, message, videoEncoderName, audioOwner?.encoderName ?: audioEncoderName,
        acceptedVideo.get(), acceptedAudio.get(), drops.get(), videoRejects.get(), audioPrimingRejects.get(), waitingKeyframeRejects.get(), queueRejects.get(), audioOwner?.level ?: audioLevel, videoCaptureAgeMs,
        if (stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING)) publishingSink?.status ?: publishStatus else publishStatus,
        (if (!recordingFailed && stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING)) recordingSink?.status ?: recordStatus else recordStatus) + if (previousRecording.isNotEmpty()) " · Previous: ${previousRecording.take(240)}" else "",
        queueDiagnostics, "Publish drainer ${publishing?.diagnostics ?: "none"} · ${publishingSink?.diagnostics ?: ""}",
        orientationStatus, rendererStatus, audioOwner?.diagnostics ?: audioDiagnostics, "$cleanupPhase · $cleanupDiagnostics",
        !prepareAdmission.pending && !scannerPermissions.outstanding && startAdmission.idle && !shutdownRequested && !stoppingEpoch && !teardownStarted && stage in listOf(CaptureStage.IDLE, CaptureStage.ERROR) && videoEncoder == null && audioOwner == null && renderer == null,
            elapsed.captureMs, elapsed.broadcastMs, elapsed.uncertain, socket, activeAudioInput,
            startAdmission.preparation?.let(startAdmission::backgroundReady) == true,
            privacyView?.desired==PrivacyStandbyPolicy.Desired.STANDBY || privacyView?.phase in listOf(PrivacyStandbyPolicy.Phase.RESUMING,PrivacyStandbyPolicy.Phase.FAILED),
            when(privacyView?.phase) {
                PrivacyStandbyPolicy.Phase.ARMED -> "Standby selected · awaiting explicit Start"
                PrivacyStandbyPolicy.Phase.ENTERING_STANDBY -> "Standby pending · accepted old media may still drain"
                PrivacyStandbyPolicy.Phase.STANDBY -> (if(captureIntent.hasVideo) "Local generated picture/silence confirmed" else "Local generated silence confirmed")+" · remote buffers may contain old media"
                PrivacyStandbyPolicy.Phase.RESUMING -> "Resume pending · verifying fresh camera/microphone"
                PrivacyStandbyPolicy.Phase.FAILED -> "Privacy replacement failed · raw input remains fenced"
                else -> "Live input selected"
            },privacyView?.revision ?: 0,privacyView?.run?.epoch ?: -1,
            privacyView?.phase==PrivacyStandbyPolicy.Phase.STANDBY && stage==CaptureStage.CAPTURING,
            privacyView?.phase in listOf(PrivacyStandbyPolicy.Phase.LIVE,PrivacyStandbyPolicy.Phase.RESUMING) && (stage==CaptureStage.CAPTURING || stoppingEpoch && restart.pending!=null))
    }

    internal fun currentPreparation(): CaptureStartAdmission.Preparation? = startAdmission.preparation
    internal fun owns(value: CaptureStartAdmission.Preparation): Boolean = startAdmission.owns(value)
    internal fun backgroundReady(value: CaptureStartAdmission.Preparation): Boolean = startAdmission.backgroundReady(value)
    internal fun servicePromoted(value: CaptureStartAdmission.Preparation): Boolean = startAdmission.servicePromoted(value)
    internal fun continuesInBackground(value: CaptureStartAdmission.Preparation): Boolean =
        stage == CaptureStage.CAPTURING && startAdmission.continuesInBackground(value)
    /** Cold wake/resume check. No media timestamp clock or PCM-age policy changes. */
    internal fun revalidatePublishingAuthority() { publishingSession?.revalidateAuthority() }

    /** Reserve the actual immutable candidate before permission, grant, storage
     * or destination work. Scanner admission contends on this same monitor. */
    internal fun reservePreparation(intent: CaptureIntent, audioInputId: String?, backgroundAudio: Boolean,
        orientation: CaptureOrientationChoice.Selection?, cameraChoice: CameraSourceChoice, initialStandby: Boolean): CaptureStartAdmission.Preparation? {
        if (!snapshot().canPrepare) return null
        val frozen = orientation ?: CaptureOrientationChoice.AUTO.freeze(intent.hasVideo, observedNaturalPortrait, observedPose)
        return startAdmission.reserve(intent, audioInputId, backgroundAudio, frozen, cameraChoice, initialStandby)
    }
    internal fun pendingPreparation(ticket: CaptureStartAdmission.Preparation): Boolean = startAdmission.candidate(ticket)
    internal fun abandonPreparation(ticket: CaptureStartAdmission.Preparation) {
        if (!startAdmission.cancelCandidate(ticket)) return
        // The acknowledgement stays behind any already queued Prepare. It
        // cannot release a queued candidate while its Handler is withheld.
        handler.post { startAdmission.abandon(ticket) }
    }
    internal val scannerPermissionOutstanding: Boolean get() = scannerPermissions.outstanding
    internal fun scannerPermissionFor(client: Any): Boolean = scannerPermissions.pending()?.client === client
    internal fun restoreScannerPermissionDiscard() = scannerPermissions.restoreDiscard()
    internal fun reserveScannerPermission(ticket: CaptureStartAdmission.ScannerTicket): Boolean =
        startAdmission.ownsScanner(ticket) && scannerPermissions.reserve(ticket)
    internal fun receiveScannerPermission(): CaptureStartAdmission.ScannerTicket? = scannerPermissions.receive()
    internal fun reserveScanner(client: Any, event: (CaptureStartAdmission.ScannerTicket, ManualQrScanner.Event) -> Unit): CaptureStartAdmission.ScannerTicket? {
        if (!snapshot().canPrepare) return null
        val ticket = startAdmission.reserveScanner(client) ?: return null
        val owner = ManualQrScanner(context.applicationContext, handler, ticket, { startAdmission.ownsScanner(ticket) }, scannerEvent@{ reply ->
            if (reply is ManualQrScanner.Event.Finished) {
                if (!startAdmission.scannerClosed(ticket, true)) return@scannerEvent
                if (scanner?.ticket === ticket) scanner = null
            }
            event(ticket, reply)
        })
        scanner = owner
        owner.arm()
        return ticket
    }
    internal fun ownsScanner(ticket: CaptureStartAdmission.ScannerTicket): Boolean = startAdmission.ownsScanner(ticket)
    internal fun openScanner(ticket: CaptureStartAdmission.ScannerTicket, texture: SurfaceTexture): Boolean =
        scanner?.takeIf { it.ticket === ticket }?.open(texture) == true
    internal fun cancelScanner(ticket: CaptureStartAdmission.ScannerTicket) { scanner?.takeIf { it.ticket === ticket }?.cancel() }
    internal fun scannerPresentationClosed(ticket: CaptureStartAdmission.ScannerTicket) {
        scanner?.takeIf { it.ticket === ticket }?.presentationClosed()
    }

    internal fun prepare(ticket: CaptureStartAdmission.Preparation, previewSurface: Surface?, publisher: () -> EncodedSink?, recorder: () -> EncodedSink?): Boolean {
        val intent = ticket.intent
        val canPrepare = startAdmission.candidate(ticket) && !shutdownRequested && !stoppingEpoch && !teardownStarted &&
            stage in listOf(CaptureStage.IDLE, CaptureStage.ERROR) && videoEncoder == null && audioOwner == null && renderer == null
        val admitted = try { prepareAdmission.submit(intent, canPrepare, { handler.post(it) }) admitted@{ requestedIntent ->
            if (shutdownRequested || stage !in listOf(CaptureStage.IDLE, CaptureStage.ERROR)) { startAdmission.abandon(ticket); return@admitted }
            if (videoEncoder != null || renderer != null || teardownStarted) {
                message = lifecycle.message("Previous native cleanup is unfinished; capture cannot restart")
                startAdmission.abandon(ticket)
                return@admitted
            }
            if (!startAdmission.claim(ticket)) { startAdmission.abandon(ticket); return@admitted }
            preparation = ticket
            privacy = PrivacyStandbyPolicy(ticket, ticket.intent.hasVideo, ticket.initialStandby)
            privacyDispatch=PrivacyStandbyDispatch()
            cameraDesired = ticket.cameraChoice ?: CameraSourceChoice.REAR
            ticket.orientation?.let {
                check(restart.pending == null) { "Previous orientation cleanup is unfinished" }
                naturalPortrait = it.naturalPortrait
                // Automatic observes physical input while permission/preflight
                // is pending; fixed preserves its original reference exactly.
                val pose = if (it.automatic) observedPose else it.initialPose
                restart.request(pose, false, System.nanoTime())
            }
            audioPin = AudioInputPin(ticket.audioInputId); activeAudioInput = "Not verified"
            cleanupExpired = false; stopFailed = false
            publishingSession?.stopDestination(); publishingSession = null
            telemetryDestination = TelemetryDestination(ticket, null, false)
            captureIntent = requestedIntent // Exact immutable ticket, including topology.
            requestedIntent.videoProfile?.let { captureProfile = it }
            publishFactory = publisher
            recordFactory = recorder
            autoStart = false
            prepareEpoch(previewSurface)
        } } catch (error: Exception) { startAdmission.abandon(ticket); throw error }
        if (!admitted) { startAdmission.abandon(ticket); return false }
        return true
    }

    private fun prepareEpoch(previewSurface: Surface?) {
        if (shutdownRequested || cleanupExpired) return
        val intent = preparation ?: return
        if (!startAdmission.beginEpoch(intent, generation + 1)) return
        stage = CaptureStage.PREPARING
        message = if (captureIntent.hasVideo) "Preparing hardware pixel rotation and native audio" else "Preparing microphone and native AAC"
        generation++
        drainGeneration = generation
        runAdmission = CameraRunAdmission(drainGeneration)
        captureEpoch = drainGeneration
        lifecycle.beginEpoch(captureEpoch)
        teardownStarted = false
        stoppingEpoch = false
        rendererStopped = false
        rendererClosed = false
        rendererCloseRequested = false
        cameraClosed = true
        audioStopReturned = true
        audioOwnerClosed = false
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
            if (publishingSession == null) {
                publishingSession = publishingSink
                telemetryDestination = TelemetryDestination(intent, (publishingSink as? SocketOutputProvider)?.socketOutput, publishingSink != null)
            }
            val publishingEpoch = generation
            if (captureIntent.hasVideo) publishingSink?.setKeyframeRequester { handler.post {
                val now = System.nanoTime()
                if (captureIntent.hasVideo && publishingEpoch == generation && stage == CaptureStage.CAPTURING &&
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
        observedNaturalPortrait = naturalPortrait
        handler.post {
            preview = surface?.takeIf { it.isValid }
            previewWidth = maxOf(1, width); previewHeight = maxOf(1, height)
            displayDegrees = rotationDegrees
            this.naturalPortrait = preparation?.orientation?.naturalPortrait ?: naturalPortrait
            renderer?.setPreview(preview, previewWidth, previewHeight, displayDegrees)
        }
    }

    internal fun orientationChanged(pose: CardinalPose) {
        observedPose = pose // Only real physical/qualification input writes the future Auto shadow.
        val observation = poseInputs.record(startAdmission.preparation, pose)
        if (observation.owner != null) handler.post { applyOrientation(observation) }
    }

    private fun applyOrientation(observation: CameraPoseInput.Observation<CaptureStartAdmission.Preparation>) {
        val owner = observation.owner ?: return
        if (shutdownRequested || preparation !== owner || !startAdmission.permitsOrientation(owner) || !poseInputs.isCurrent(observation)) return
        if (sourcePolicy.pending() || privacy?.snapshot()?.pending!=null) { deferredCameraPose = true; return }
        val pose = observation.pose
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

    @SuppressLint("MissingPermission")
    private fun prepareOnHandler(previewSurface: Surface?, token: Long) {
        if (!captureIntent.hasVideo) {
            plan = null; preview = null; recordingFailed = false
            cameraView = CameraSourceSnapshot(status = "No camera · microphone only")
            videoEncoderName = "Not used · microphone only"; rendererStatus = "No camera or GPU"
            orientationStatus = "Microphone only · AAC-LC 48 kHz mono · 96 kbit/s"
            rendererStopped = true; rendererClosed = true; cameraClosed = true
            prepareAudio(token)
            val intent = preparation
            if (intent == null || !startAdmission.prepared(intent, token)) {
                stopOnHandler("Capture approval ended before microphone preparation completed", false); return
            }
            stage = CaptureStage.PREPARED
            traceLifecycle(CaptureLifecycleDiagnostics.Code.PREPARED)
            message = "Ready: microphone and native AAC"
            return
        }
        requireNotNull(previewSurface) { "Preview surface is unavailable" }
        require(previewSurface.isValid) { "Preview surface is unavailable" }
        preview = previewSurface
        recordingFailed = false
        val pose = restart.desiredPose
        cameraDesired = preparation?.let(startAdmission::cameraChoice) ?: cameraDesired
        val descriptor = CameraCaptureOwner.discover(context, cameraDesired, OrientationPlan.forRear(0, pose, naturalPortrait, captureProfile))
        val selected = descriptor.plan
        plan = selected
        sourceRequests.close(); sourceRequests = CameraRequestSlot(); sourceRequest = null
        sourcePolicy = CameraSourcePolicy(initialSerial = sourceSerial)
        val operation = checkNotNull(sourcePolicy.begin(cameraDesired, System.nanoTime()))
        sourceOperation = operation; sourceSerial = operation.serial
        val sourceAdmission = runAdmission.beginSource(operation)
        if (!sourceAdmission.admitted) {
            if (sourceAdmission.requiresFailure) fail("Camera run is no longer admitting a source")
            return // Ordinary Stop cancellation must not become a new camera fault.
        }
        check(sourcePolicy.cameraClosed(operation, System.nanoTime()))
        controlMailbox.close(); controlMailbox = CameraControlMailbox()
        cameraView = CameraSourceSnapshot(cameraDesired, busy = true, status = "Opening ${cameraDesired.label} camera")
        armSourceDeadline(operation)
        orientationStatus = "${captureProfile.label} · ${captureProfile.videoBps} bps AVC + ${captureProfile.audioBps} bps AAC · ${selected.width}×${selected.height} · ${pose.name} · native pixel rotation ${selected.encoderClockwiseDegrees}°"

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

        prepareAudio(token)
        video.start()
        val rawRun = runAdmission
        val renderingRun=checkNotNull(privacyRun);val renderingPolicy=checkNotNull(privacy)
        renderer = CameraGlRenderer(selected.cameraWidth, selected.cameraHeight, selected, sourceMapper(), token, handler,
            onReady = { input, diagnostic ->
                rendererStatus = diagnostic
                renderer?.setPreview(preview, previewWidth, previewHeight, displayDegrees)
                if(privacy?.snapshot()?.desired==PrivacyStandbyPolicy.Desired.STANDBY) {
                    runAdmission.retiredSource(operation);sourcePolicy.close();sourceDeadline?.let(handler::removeCallbacks);sourceDeadline=null
                    sourceMutation?.let { privacy?.sourceClosed(it,true,System.nanoTime()) };sourceMutation=null
                    renderer?.concealPreview()
                    cameraView=CameraSourceSnapshot(cameraDesired,status="Standby prepared · camera is not open")
                    preparation?.let { if(startAdmission.prepared(it,token)) {
                        stage=CaptureStage.PREPARED;message="Standby prepared · explicit Start sends generated picture and silence"
                        if(autoStart)startAdmission.start(it)?.let(::startOnHandler)
                    } }
                } else openSource(operation, descriptor, input, token)
            }, onFailure = { reason -> if (token == drainGeneration) {
                autoStart = false; restart.cancel()
                if (stoppingEpoch) { markFailure(CaptureLifecycleDiagnostics.Code.GPU_FAILURE, reason); stopReason = reason }
                else fail(reason, CaptureLifecycleDiagnostics.Code.GPU_FAILURE)
            } }, initialSourceToken = operation.serial, initialFront = cameraDesired == CameraSourceChoice.FRONT,
            initialLegacyMirror = descriptor.legacyMirror, rawAdmission = rawRun,
            onSourceFrame = { serial, observedAtNs -> sourceFrame(serial, token, observedAtNs) },
            rawAllowed = { renderingPolicy.permitsRaw(renderingRun) },
            previewAllowed = { renderingPolicy.permitsPreview(renderingRun) })
    }

    private fun sourceMapper(): CameraTimestampMapper = if (Build.VERSION.SDK_INT >= 33) CameraTimestampMapper.monotonic() else CameraTimestampMapper.realtime()

    internal fun cameraSnapshot(): CameraSourceSnapshot {
        val view = cameraView; val native = cameraOwner
        return CameraSourceSnapshot(view.requested, view.active, view.sourceToken, view.busy || sourceRequests.busy(),
            if (!view.busy && sourceRequests.busy()) "Checking camera capabilities" else view.status, view.capabilities,
            native?.requestedSettings ?: view.requestedSettings, native?.revision ?: 0, native?.confirmedRevision ?: 0,
            native?.nativeZoom?.takeIf { it.isFinite() }, native?.nativeExposure?.takeIf { it != Int.MIN_VALUE }, native?.nativeWhiteBalance?.takeIf { it >= 0 },
            native?.focusState?.takeIf { it >= 0 }, native?.exposureState?.takeIf { it >= 0 }, native?.whiteBalanceState?.takeIf { it >= 0 }, native?.controlsPending ?: false)
    }

    private fun sourceCurrent(operation: CameraSourcePolicy.Operation, epoch: Long): Boolean {
        val ticket = preparation ?: return false
        val policy=privacy ?: return false;val run=privacyRun ?:return false
        val cameraPermitted=policy.permitsPreview(run) || resumeCommand?.let { policy.permitsResume(it,System.nanoTime()) }==true
        return cameraPermitted && epoch == generation && !stoppingEpoch && startAdmission.permitsCamera(ticket) && sourcePolicy.owns(operation)
    }

    private fun armSourceDeadline(operation: CameraSourcePolicy.Operation) {
        sourceDeadline?.let(handler::removeCallbacks)
        val epoch = generation
        sourceDeadline = Runnable {
            if (sourcePolicy.owns(operation) && !sourcePolicy.active(operation)) {
                renderer?.sourceFrameNs(operation.serial)?.let { sourceFrame(operation.serial, epoch, it) }
            }
            if (sourcePolicy.owns(operation) && !sourcePolicy.active(operation)) fail("Camera switch exceeded six seconds; native cleanup must finish before another Prepare")
        }.also { handler.postDelayed(it, ((operation.deadlineNs - System.nanoTime()).coerceAtLeast(0) + 999_999) / 1_000_000) }
    }

    private fun openSource(operation: CameraSourcePolicy.Operation, descriptor: CameraCaptureOwner.Descriptor, input: Surface, epoch: Long) {
        if (!sourceCurrent(operation, epoch)) return
        if (!sourcePolicy.textureReady(operation, System.nanoTime())) { fail("Camera source deadline expired before native open"); return }
        cameraInput = input
        if (context.checkSelfPermission(android.Manifest.permission.CAMERA) != android.content.pm.PackageManager.PERMISSION_GRANTED) { fail("Camera permission is no longer available"); return }
        val manager = context.getSystemService(CameraManager::class.java)
        val native = CameraCaptureOwner(manager, descriptor, input, handler, { sourceCurrent(operation, epoch) },
            onConfigured = { if (sourceCurrent(operation, epoch) && !sourcePolicy.configured(operation, System.nanoTime())) fail("Camera source deadline expired while configuring") },
            onFailure = { if (sourceCurrent(operation, epoch)) fail("${descriptor.choice.label} camera source failed; no fallback was selected") })
        cameraOwner = native; cameraClosed = false
        native.open()
    }

    private fun sourceFrame(serial: Long, epoch: Long, observedAtNs: Long) {
        val operation = sourceOperation ?: return
        if (operation.serial != serial || !sourceCurrent(operation, epoch) || sourcePolicy.active(operation)) return
        if (runAdmission.failure != null) { fail("Camera source faulted before native frame admission completed"); return }
        if (!sourcePolicy.frame(operation, observedAtNs)) { fail("Camera source deadline expired before its first valid frame"); return }
        sourceDeadline?.let(handler::removeCallbacks); sourceDeadline = null
        sourceMutation?.let { privacy?.sourceClosed(it,true,System.nanoTime()) };sourceMutation=null
        sourceRequest?.let(sourceRequests::release); sourceRequest = null
        val native = cameraOwner ?: return
        cameraView = CameraSourceSnapshot(operation.choice, operation.choice, operation.serial, false,
            "${operation.choice.label} native frame confirmed · remote source change not verified", native.descriptor.capabilities, native.requestedSettings)
        if(resumeCommand!=null){resumeCameraReady=true;finishPrivacyResume();return}
        if (stage == CaptureStage.PREPARING) {
            val intent = preparation ?: return
            if (!startAdmission.prepared(intent, epoch)) { stopOnHandler("Capture approval ended before preparation completed", false); return }
            stage = CaptureStage.PREPARED
            traceLifecycle(CaptureLifecycleDiagnostics.Code.PREPARED)
            message = "Ready: upright ${plan?.width}×${plan?.height} hardware video and AAC"
            if (autoStart) startAdmission.start(intent)?.let(::startOnHandler)
            else restart.pending?.let {
                if (restart.complete(it.token, System.nanoTime()) == null) { fail("Orientation preparation deadline expired"); return }
                restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
            }
            if (!autoStart) restart.beginSession()
        } else if (stage == CaptureStage.CAPTURING) {
            // Same encoder/configuration/timeline. One cold IDR request helps
            // decoders see the new scene; no transport attempt is replaced.
            runCatching { videoEncoder?.setParameters(android.os.Bundle().apply { putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) }) }
        }
        if (deferredCameraPose) {
            deferredCameraPose = false
            preparation?.let { owner -> poseInputs.latest(owner)?.let(::applyOrientation) }
        }
    }

    internal fun switchCamera(ticket: CaptureStartAdmission.Preparation, choice: CameraSourceChoice, expectedSourceToken: Long): Boolean {
        if (privacyRun?.let { privacy?.permitsPreview(it) } != true || !startAdmission.canSwitchCamera(ticket) || !sourcePolicy.acceptsControl(expectedSourceToken) || cameraView.sourceToken != expectedSourceToken) return false
        val slot = sourceRequests
        val request = slot.reserve() ?: return false
        val requestedNs = System.nanoTime()
        val posted = handler.post {
            var handedOff = false
            try {
            if (slot !== sourceRequests || !slot.owns(request) || !startAdmission.canSwitchCamera(ticket) || (!sourcePolicy.acceptsControl(expectedSourceToken) || cameraView.sourceToken != expectedSourceToken) || preparation !== ticket || stage !in listOf(CaptureStage.PREPARED, CaptureStage.CAPTURING) || startPending || sourcePolicy.pending()) return@post
            val current = cameraOwner ?: return@post
            if (current.descriptor.choice == choice) return@post
            val descriptor = try { CameraCaptureOwner.discover(context, choice, checkNotNull(plan)) }
                catch (_: Exception) { cameraView = CameraSourceSnapshot(choice, current.descriptor.choice, cameraView.sourceToken, false,
                    "${choice.label} is unavailable for this canvas; current source retained", current.descriptor.capabilities, current.requestedSettings); return@post }
            val deadline = try { Math.addExact(requestedNs, 6_000_000_000L) } catch (_: ArithmeticException) { return@post }
            if (System.nanoTime() >= deadline) {
                cameraView = CameraSourceSnapshot(choice, current.descriptor.choice, cameraView.sourceToken, false,
                    "Camera preflight exceeded six seconds; current source retained", current.descriptor.capabilities, current.requestedSettings)
                return@post
            }
            val policy=privacy ?: return@post;val run=privacyRun ?: return@post
            val mutation=policy.reserveSource(run,policy.snapshot().revision) ?: return@post
            val operation = sourcePolicy.begin(choice, requestedNs) ?: run { policy.sourceClosed(mutation,true,System.nanoTime());return@post }
            sourceMutation=mutation
            if (!startAdmission.selectCamera(ticket, choice)) { sourcePolicy.close(); return@post }
            sourceOperation = operation; sourceSerial = operation.serial; cameraDesired = choice
            val sourceAdmission = runAdmission.beginSource(operation)
            if (!sourceAdmission.admitted) {
                if (sourceAdmission.requiresFailure) fail("Camera run is no longer admitting a source")
                return@post
            }
            sourceRequest = request; handedOff = true
            controlMailbox.close(); controlMailbox = CameraControlMailbox()
            renderer?.fenceSource()
            cameraView = CameraSourceSnapshot(choice, busy = true, status = "Switching to ${choice.label}; accepted output may still drain")
            armSourceDeadline(operation)
            val epoch = generation
            current.close {
                if (cameraOwner !== current) return@close
                cameraOwner = null; cameraClosed = true
                if (stoppingEpoch) { finishStoppedEpoch(); return@close }
                if (!sourceCurrent(operation, epoch)) return@close
                if (!sourcePolicy.cameraClosed(operation, System.nanoTime())) { fail("Camera source deadline expired during native close"); return@close }
                renderer?.replaceSource(operation.serial, descriptor.plan, choice == CameraSourceChoice.FRONT, descriptor.legacyMirror, sourceMapper()) { input ->
                    openSource(operation, descriptor, input, epoch)
                }
            }
            } finally { if (!handedOff) slot.release(request) }
        }
        if (!posted) slot.release(request)
        return posted
    }

    internal fun adjustCamera(ticket: CaptureStartAdmission.Preparation, sourceToken: Long, settings: CameraControlSettings): Boolean {
        val view = cameraView
        if (privacyRun?.let { privacy?.permitsPreview(it) } != true || !startAdmission.canSwitchCamera(ticket) || !sourcePolicy.acceptsControl(sourceToken) || view.sourceToken != sourceToken || view.busy || sourceRequests.busy() || view.capabilities?.accepts(settings) != true) return false
        val mailbox = controlMailbox
        if (mailbox.offer(settings)) handler.post {
            val value = mailbox.take() ?: return@post
            if (mailbox === controlMailbox && startAdmission.canSwitchCamera(ticket) && cameraView.sourceToken == sourceToken && sourcePolicy.acceptsControl(sourceToken) && !sourceRequests.busy()) cameraOwner?.adjust(value)
        }
        return true
    }

    private fun prepareAudio(token: Long) {
        val prepared = checkNotNull(preparation)
        val policy = checkNotNull(privacy)
        val rawRun = runAdmission
        val zero = policy.snapshot().desired == PrivacyStandbyPolicy.Desired.STANDBY
        var audioRun:PrivacyStandbyPolicy.Run?=null
        val audio = AudioCaptureOwner(context,prepared,token,captureIntent.audioBps,checkNotNull(audioPin),handler,audioHandler,
            audioGenerations,rawRun,
            rawAllowed = { val run = audioRun; run != null && policy.permitsRaw(run) },
            replacementAdmission = { cmd,now -> policy.replacementAdmission(cmd,now) },
            onFormat = { config -> if(token==drainGeneration) publishingSink?.configure(videoCodec,config) },
            onSample = { sample,tick,nativeGeneration,zeroOnly -> if(token==drainGeneration) {
                acceptedAudio.incrementAndGet(); submitSample(sample,token,rawRun,tick,nativeGeneration,zeroOnly)
            } },onValidated = {},onPriming = { audioPrimingRejects.incrementAndGet() },
            onFailure = { reason -> if(token==drainGeneration) fail(reason) },
            onRoute = { name -> if(token==drainGeneration) activeAudioInput=name })
        audioOwner=audio;audioCodec=audio.descriptor
        branchClosePending.fill(false)
        audio.prepare(zero)
        val run=checkNotNull(policy.attach(prepared,token,audio.descriptor,if(zero)null else audio.currentGeneration,System.nanoTime())) { "Privacy owner is unavailable" }
        privacyRun=run;audioRun=run
        if(captureIntent.hasVideo && !zero) sourceMutation=policy.reserveSource(run,policy.snapshot().revision)
        // A live owner is registered only after real native construction. A
        // zero-only initial owner must be admitted through zeroAudioReady.
        check(!audio.failed) { "Native AAC preparation failed" }
        queues = NativeSampleFanout(4uL * 1024uL * 1024uL, 8uL * 1024uL * 1024uL, 2000u, captureIntent.hasVideo)
        publishing = QueueDrainer(handler, queues!!, true, publishingSink, { fail(it) })
        recording = QueueDrainer(handler, queues!!, false, recordingSink, ::failRecording)
        if(publishingSink==null) closePrivacyBranch(run,PrivacyStandbyPolicy.Sink.PUBLISHING)
        else publishingSink?.setTerminalAdmissionListener { handler.post { closePrivacyBranch(run,PrivacyStandbyPolicy.Sink.PUBLISHING) } }
        if(recordingSink==null) closePrivacyBranch(run,PrivacyStandbyPolicy.Sink.RECORDING)
    }

    fun start() { startAdmission.preparation?.let(::start) }
    internal fun start(preparation: CaptureStartAdmission.Preparation) {
        if (preparation.intent.hasVideo && (sourcePolicy.pending() || sourceRequests.busy())) return
        val ticket = startAdmission.start(preparation) ?: return
        if (!handler.post { if (startAdmission.permits(ticket) && stage == CaptureStage.PREPARED) { autoStart = true; startOnHandler(ticket) } }) startAdmission.cancelInitial(ticket.preparation)
    }

    private fun startOnHandler(ticket: CaptureStartAdmission.Start) {
        if (stage != CaptureStage.PREPARED || startPending || !startAdmission.permits(ticket)) return
        startPending = true
        val token = generation
        try {
            clock.start()
            fun admit() {
                if (token != generation || stage != CaptureStage.PREPARED || !startAdmission.permits(ticket)) return
                try {
                    check(!captureIntent.hasVideo || plan?.pose == restart.desiredPose) { "Pose changed before encoder start" }
                    restart.pending?.let { check(restart.complete(it.token, System.nanoTime()) != null) { "Orientation restart deadline expired" } }
                    restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
                    recovery.begin(token)
                    syncRequests = 0; diagnosticAfterNs = 0
                    queueDiagnostics = "Queue diagnostics awaiting samples"
                    acceptedVideo.set(0); acceptedAudio.set(0); drops.set(0)
                    videoCaptureAgeMs = null
                    videoRejects.set(0); audioPrimingRejects.set(0); waitingKeyframeRejects.set(0); queueRejects.set(0)
                    recordingSink?.start()
                    if (ticket.preparation.backgroundAudio && !backgroundStartAllowed(ticket.preparation)) {
                        stop(ticket.preparation, "Background audio service admission changed before Start. Prepare again.")
                        return
                    }
                    if (!startAdmission.commit(ticket)) {
                        autoStart = false; restart.cancel()
                        stopOnHandler("Destination ended before capture admission; closing prepared recording owners", false)
                        return
                    }
                    publishingSink?.configure(videoCodec, audioCodec)
                    publishingSink?.start()
                    stage = CaptureStage.CAPTURING
                    traceLifecycle(CaptureLifecycleDiagnostics.Code.STARTED)
                    restart.beginSession()
                    startPending = false
                    publishStatus = publishingSink?.status ?: "No publisher attached · SDK validation only"
                    recordStatus = recordingSink?.status ?: "No recorder attached · no file is being written"
                    message = if (captureIntent.hasVideo) "Capturing upright hardware video into the shared SDK" else "Capturing microphone audio into the shared SDK"
                    if (captureIntent.hasVideo) videoEncoder?.setParameters(android.os.Bundle().apply { putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
                    val run=checkNotNull(privacyRun);val policy=checkNotNull(privacy)
                    val command=policy.beganMedia(run,System.nanoTime()) ?: policy.snapshot().pending
                    privacyCommand=command
                    if(command!=null) audioOwner!!.bindInitialStandby(command) { nativeGeneration ->
                        if(privacyRun===run && policy.zeroAudioReady(command,nativeGeneration,checkNotNull(audioCodec),System.nanoTime())) armPrivacy(command)
                    }
                    audioOwner!!.start(clock.mediaEpochNs(),command)
                    if(command!=null) { armPrivacy(command);renderer?.startSlate(command,clock.mediaEpochNs(),policy) }

                } catch (error: Exception) { fail("Start failed: ${error.message ?: error.javaClass.simpleName}") }
            }
            if (captureIntent.hasVideo) renderer!!.startEncoding(encoderSurface!!, token, ::admit) else admit()
        } catch (error: Exception) { fail("Start failed: ${error.message ?: error.javaClass.simpleName}") }
    }

    /** Main/UI call: this exact logical raw fence is synchronous, before any
     * codec queue, hardware close or opaque preview-cover delivery. */
    internal fun standby(ticket:CaptureStartAdmission.Preparation,epoch:Long):Boolean {
        val policy=privacy ?:return false;val run=privacyRun ?:return false
        if(!startAdmission.owns(ticket) || run.epoch!=epoch || stage!=CaptureStage.CAPTURING && !(stoppingEpoch && restart.pending!=null))return false
        val cmd=policy.standby(run,System.nanoTime())
        renderer?.concealPreview()
        if(cmd==null)return policy.snapshot().desired==PrivacyStandbyPolicy.Desired.STANDBY
        if(!privacyDispatch.admit(cmd))return true
        handler.post {
            if(privacyRun!==run || policy.snapshot().pending!==cmd || stoppingEpoch)return@post
            privacyCommand=cmd;resumeCommand=null;armPrivacy(cmd)
            sourceRequests.close();controlMailbox.close();sourcePolicy.close();renderer?.fenceSource()
            sourceDeadline?.let(handler::removeCallbacks);sourceDeadline=null
            val operation=sourceOperation
            val camera=cameraOwner
            fun closed(){
                operation?.let { runAdmission.retiredSource(it) }
                sourceMutation?.let { policy.sourceClosed(it,true,System.nanoTime()) };sourceMutation=null
                if(privacyRun===run && policy.permitsReplacement(cmd,System.nanoTime())) {
                    renderer?.startSlate(cmd,clock.mediaEpochNs(),policy)
                }
            }
            if(camera==null){cameraClosed=true;closed()}
            else camera.close {
                if(privacyRun!==run)return@close
                if(cameraOwner===camera){cameraOwner=null;cameraClosed=true}
                // Another exact source-close listener may have cleared the
                // pointer first. This callback is still its actual receipt.
                if(cameraOwner==null){closed();if(stoppingEpoch)finishStoppedEpoch()}
            }
            val audio=audioOwner ?:return@post
            audio.standby(cmd,{ old -> policy.audioClosed(cmd,old,System.nanoTime()) },{ fresh ->
                if(privacyRun===run)when(policy.replacementAdmission(cmd,System.nanoTime())) {
                    PrivacyStandbyPolicy.NativeAdmission.SUBMIT -> if(!policy.zeroAudioReady(cmd,fresh,audio.descriptor,System.nanoTime()))fail("Fresh zero-only AAC proof was rejected")
                    PrivacyStandbyPolicy.NativeAdmission.FAULT -> fail("Fresh zero-only AAC readiness exceeded its original deadline")
                    else -> Unit // actual ready callback was superseded by Stop/Auto/urgent command
                }
            })
            videoEncoder?.setParameters(android.os.Bundle().apply{putInt(PlatformCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME,0)})
        }
        return true
    }
    internal fun resumePrivacy(ticket:CaptureStartAdmission.Preparation,epoch:Long,revision:Long):Boolean {
        val policy=privacy ?:return false;val run=privacyRun ?:return false
        if(!startAdmission.owns(ticket) || run.epoch!=epoch || stage!=CaptureStage.CAPTURING)return false
        val cmd=policy.userResume(run,revision,System.nanoTime()) ?:return false
        handler.post {
            if(privacyRun!==run || policy.snapshot().pending!==cmd || stoppingEpoch)return@post
            resumeCommand=cmd;resumeMicrophoneReady=false;resumeCameraReady=!captureIntent.hasVideo;armPrivacy(cmd)
            val audio=audioOwner ?:return@post
            if(!audio.prepareResume(cmd){resumeMicrophoneReady=true;finishPrivacyResume()}){fail("Fresh microphone Resume could not be prepared");return@post}
            if(captureIntent.hasVideo)try {
                sourcePolicy=CameraSourcePolicy(initialSerial=sourceSerial);sourceRequests=CameraRequestSlot();controlMailbox=CameraControlMailbox()
                val descriptor=CameraCaptureOwner.discover(context,cameraDesired,checkNotNull(plan))
                val op=checkNotNull(sourcePolicy.begin(cameraDesired,cmd.startedNs));sourceSerial=op.serial;sourceOperation=op
                val admitted=runAdmission.beginSource(op)
                if(!admitted.admitted){if(admitted.requiresFailure)fail("Resume camera admission failed");return@post}
                check(sourcePolicy.cameraClosed(op,System.nanoTime()))
                armSourceDeadline(op)
                val nativeEpoch=generation
                renderer?.replaceSource(op.serial,descriptor.plan,cameraDesired==CameraSourceChoice.FRONT,descriptor.legacyMirror,sourceMapper()) { input -> openSource(op,descriptor,input,nativeEpoch) }
            }catch(error:Exception){fail("Fresh camera Resume failed: ${error.javaClass.simpleName}")}
        }
        return true
    }
    private fun finishPrivacyResume(){
        val cmd=resumeCommand ?:return;val policy=privacy ?:return;val audio=audioOwner ?:return
        if(!resumeMicrophoneReady || !resumeCameraReady)return
        when(policy.commitReadyResume(cmd,audio.currentGeneration,true,resumeCameraReady,System.nanoTime())) {
            PrivacyStandbyPolicy.NativeAdmission.SUBMIT -> Unit
            PrivacyStandbyPolicy.NativeAdmission.FAULT -> { fail("Resume approval expired or native readiness failed");return }
            else -> return // newer urgent Standby/Stop/Auto owns the exact fence
        }
        if(!audio.commitResume(audio.currentGeneration)){
            if(policy.permitsRaw(cmd.run))fail("Fresh microphone Resume did not commit")
            return // urgent Standby won after policy Resume and remains fenced
        }
        renderer?.endSlate();resumeCommand=null;privacyCommand=null;privacyTimer?.let(handler::removeCallbacks);privacyTimer=null
        preparation?.let { poseInputs.latest(it)?.let(::applyOrientation) }
    }
    private fun armPrivacy(command:PrivacyStandbyPolicy.Command){
        privacyTimer?.let(handler::removeCallbacks)
        val run=command.run
        privacyTimer=Runnable {
            if(privacyRun===run && privacy?.tick(run,System.nanoTime())==false && !stoppingEpoch)fail("Privacy replacement exceeded its original six-second deadline")
        }.also{handler.postDelayed(it,((command.deadlineNs-System.nanoTime()).coerceAtLeast(0)+999_999)/1_000_000)}
    }
    /** True native branch closure only. Busy retries use one cold owner slot,
     * original deadline, exact run, and can never detach a successor. */
    private fun closePrivacyBranch(run:PrivacyStandbyPolicy.Run,sink:PrivacyStandbyPolicy.Sink,deadline:Long=privacy?.snapshot()?.pending?.deadlineNs ?: (System.nanoTime()+6_000_000_000L)){
        if(privacyRun!==run || stoppingEpoch || branchClosePending[sink.ordinal])return
        val fanout=queues ?:return
        if(System.nanoTime()>=deadline){fail("Native branch admission closure could not be confirmed");return}
        branchClosePending[sink.ordinal]=true
        try {
            if(sink==PrivacyStandbyPolicy.Sink.PUBLISHING)fanout.closePublishingAdmission() else fanout.closeRecordingAdmission()
            privacy?.sinkTerminated(run,sink,System.nanoTime())
            branchClosePending[sink.ordinal]=false
        }catch(error:dev.forgesworn.sapflow.BindingException.Busy){
            handler.postDelayed({if(privacyRun===run){branchClosePending[sink.ordinal]=false;closePrivacyBranch(run,sink,deadline)}},10)
        }catch(error:Exception){branchClosePending[sink.ordinal]=false;fail("Native branch admission closure failed: ${error.javaClass.simpleName}")}
    }

    /** Borrowed immutable owner; all fanout calls remain on this handler. */
    private fun submitSample(sample: MediaSample, token: Long, rawRun: CameraRunAdmission, tick: Long, audioGeneration: Long? = null, zeroOnly: Boolean = false, independentIdr: Boolean = false) {
        val mediaNow=System.nanoTime()
        val outputRun=privacyRun
        if(outputRun!=null && privacy?.permitsOutput(outputRun,mediaNow)==false) {
            rawRun.fail();drops.incrementAndGet()
            if(token==drainGeneration) {
                if(stoppingEpoch)markFailure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE,"Privacy output deadline expired during accepted-tail cleanup")
                else fail("Privacy output deadline or owner changed before local fanout")
            }
            return
        }
        if (!rawRun.admit(mediaNow)) {
            drops.incrementAndGet()
            if (token == drainGeneration) {
                markFailure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE, "Camera source fault; new media admission stopped")
                if (!stoppingEpoch) fail("Camera source fault; new media admission stopped")
            }
            return
        }
        val nowNs = SystemClock.elapsedRealtimeNanos()
        val result = queues!!.push(sample, nowNs.toULong())
        val command=privacyCommand;val policy=privacy
        if(command!=null && policy!=null && policy.needsOutputProof(command) && policy.permitsReplacement(command,System.nanoTime())) {
            if(policy.branchesAccepted(result.publishingQueued,result.recordingQueued)) {
                val proof=if(audioGeneration!=null && zeroOnly) policy.zeroAudioEncoded(command,audioGeneration,tick,1024,System.nanoTime())
                    else if(audioGeneration==null) policy.pictureEncoded(command,tick,independentIdr,System.nanoTime()) else null
                if(proof!=null) {
                    if(result.publishingQueued) policy.accepted(proof,PrivacyStandbyPolicy.Sink.PUBLISHING,System.nanoTime())
                    if(result.recordingQueued) policy.accepted(proof,PrivacyStandbyPolicy.Sink.RECORDING,System.nanoTime())
                }
            }
        }
        if (captureIntent.hasVideo && recovery.observe(token, result.publishingWaitingKeyframe, result.publishingOverflows, nowNs)) {
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
        val rawRun = runAdmission
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
                    sample.use { submitSample(it, token, rawRun,tick,independentIdr=key) }
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
        startAdmission.preparation?.let { stop(it, reason) }
    }
    internal fun stop(value: CaptureStartAdmission.Preparation, reason: String): Boolean {
        if (!startAdmission.stop(value)) return false
        stopAdmitted(value, reason)
        return true
    }
    internal fun serviceInterrupted(value: CaptureStartAdmission.Preparation, reason: String): Boolean {
        if (!startAdmission.interrupt(value)) return false
        stopAdmitted(value, reason, failed = true)
        return true
    }
    private fun stopAdmitted(intent: CaptureStartAdmission.Preparation, reason: String, failed: Boolean = false) {
        privacy?.stop(intent)
        sourcePolicy.close(); sourceRequests.close(); controlMailbox.close(); runAdmission.cancelSourceBudget(); renderer?.fenceSource()
        publishingSession?.stopDestination() // Fence even while codec-handler work is queued.
        handler.post {
            if (!startAdmission.owns(intent)) return@post
            publishingSession?.stopDestination()
            autoStart = false; restart.cancel(); restartDeadline?.let(handler::removeCallbacks); restartDeadline = null
            stopOnHandler(reason, failed)
        }
    }

    private fun fail(reason: String, code: CaptureLifecycleDiagnostics.Code = CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE) {
        privacyRun?.let{privacy?.nativeFailed(it)}
        if (captureIntent.hasVideo) runAdmission.fail()
        if (stage == CaptureStage.STOPPING || stage == CaptureStage.ERROR || stage == CaptureStage.IDLE) return
        startAdmission.stop()
        sourcePolicy.close(); sourceRequests.close(); controlMailbox.close(); runAdmission.cancelSourceBudget(); renderer?.fenceSource()
        publishingSession?.stopDestination()
        markFailure(code, reason)
        stage = CaptureStage.STOPPING
        // Queue control cleanup after the callback has released its codec buffer.
        handler.post { stopOnHandler(reason.take(240), failed = true) }
    }

    private fun failRecording(reason: String) {
        if (recordingFailed) return
        privacyRun?.let{closePrivacyBranch(it,PrivacyStandbyPolicy.Sink.RECORDING)}
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

    private fun stopOnHandler(requestedReason: String, failed: Boolean) {
        sourcePolicy.close(); sourceRequests.close(); controlMailbox.close(); runAdmission.cancelSourceBudget(); renderer?.fenceSource()
        sourceDeadline?.let(handler::removeCallbacks); sourceDeadline = null; deferredCameraPose = false
        cameraView = CameraSourceSnapshot(status = "Camera stopping; waiting for native closure")
        activeAudioInput = "Not active"
        val terminalFailure = failed || audioOwner?.failed==true || runAdmission.failure != null
        val reason = when {
            audioOwner?.failed==true -> audioOwner?.failureReason ?: "AAC native owner failed"
            runAdmission.failure != null -> "Camera source fault (${runAdmission.failure}); any accepted recording prefix is incomplete"
            else -> requestedReason
        }
        stopReason = reason
        if (terminalFailure) markFailure(CaptureLifecycleDiagnostics.Code.CAPTURE_FAILURE, reason)
        if (recordingFailed && restart.pending != null) {
            stopReason = "Recording failed; prepare manually before changing capture orientation"
            markFailure(CaptureLifecycleDiagnostics.Code.RECORDING_FAILURE, stopReason)
        }
        if (terminalFailure) {
            publishingSession?.stopDestination()
            autoStart = false; restart.cancel()
            abortPublisherEarly()
        }
        traceLifecycle(CaptureLifecycleDiagnostics.Code.STOP_REQUEST)
        if (stoppingEpoch) { message = lifecycle.message(reason); return }
        val captureIntent = preparation ?: startAdmission.preparation
        captureIntent?.let { startAdmission.beginCleanup(it, restart.pending != null && !terminalFailure && !shutdownRequested) }
        if (videoEncoder == null && renderer == null && audioOwner == null && publishingSink == null && recordingSink == null) {
            captureIntent?.let { startAdmission.cleanup(it, true) }
            if (startAdmission.idle) preparation = null
            stage = if (stopFailed || cleanupExpired) CaptureStage.ERROR else CaptureStage.IDLE
            message = lifecycle.message(reason)
            finishControllerClose()
            return
        }
        privacyRun?.let { privacy?.fenceRun(it) }
        privacyTimer?.let(handler::removeCallbacks); privacyTimer=null
        stoppingEpoch = true
        cleanupPhase = "awaiting producer owners"
        tailEnabled = stage == CaptureStage.CAPTURING && !terminalFailure
        tailDone = !tailEnabled
        tailStarted = false
        videoEos = false; audioEos = false; audioEosQueued = false
        stage = CaptureStage.STOPPING
        message = lifecycle.message(reason)
        generation++ // Producer callbacks stop; drainGeneration retains old codec output.
        val token = drainGeneration
        val stopFence = generation
        val deadlineNs = minOf(restart.pending?.deadlineNs ?: Long.MAX_VALUE,privacy?.cleanupDeadlineNs ?: Long.MAX_VALUE,System.nanoTime()+6_000_000_000L)
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
        val nativeCamera = cameraOwner
        if (nativeCamera == null) cameraClosed = true
        else nativeCamera.close {
            if (cameraOwner === nativeCamera) { cameraOwner = null; cameraClosed = true; finishStoppedEpoch() }
        }
        val gpu = renderer
        if (gpu == null) { rendererStopped = true; rendererClosed = true }
        else gpu.stopEncoding(token) {
            if (drainGeneration == token) { rendererStopped = true; finishStoppedEpoch() }
        }
        val audio=audioOwner
        audioOwnerClosed=audio==null
        audio?.close(!terminalFailure,deadlineNs) { closed ->
            if(token==drainGeneration) {
                audioOwnerClosed=closed
                if(!closed || audio.failed) markFailure(CaptureLifecycleDiagnostics.Code.AAC_QUIESCENCE_FAILURE,"AAC actual cleanup is unconfirmed or incomplete",cleanupTimedOut=!closed)
                finishStoppedEpoch()
            }
        }
        finishStoppedEpoch()
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
                handler.postDelayed(tailDeadline, 500)
            } else tailDone = true
        }
        if ((!captureIntent.hasVideo || videoEos) && audioOwnerClosed) { tailDone = true; handler.removeCallbacks(tailDeadline) }
        updateCleanupDiagnostics()
        if (!audioOwnerClosed || (audioOwner?.outstanding ?: 0) != 0) return
        if (!tailDone || !rendererClosed || !cameraClosed || !publisherAbortDone) return
        teardownStarted = true
        drainGeneration = -1
        handler.removeCallbacks(tailDeadline)
        val oldVideo = videoEncoder; val oldInput = encoderSurface
        val oldPrivacyRun=privacyRun;val oldQueues = queues; val oldVideoCodec = videoCodec; val oldAudioCodec = audioCodec
        val oldPublishing = publishing; val oldRecording = recording; val oldRecordCleanup = recordingCleanup
        val oldPublishingSink = publishingSink; val oldRecordingSink = recordingSink
        val recordingRun = runAdmission
        val canFinaliseRecording = audioOwner?.failed!=true && recordingRun.recordingCanFinalise && !stopFailed
        controls.execute {
            cleanupPhase = "publisher close"
            val publishingStopped = runCatching { oldPublishing?.close() != false }.getOrDefault(false)
            cleanupPhase = "recording drain/finalisation"
            val recordingFinalised = runCatching { oldRecording?.close(graceful = canFinaliseRecording, drainQueued = canFinaliseRecording, drainViaHandler = canFinaliseRecording) != false }.getOrDefault(false)
            val recordingStopped = recordingFinalised || runCatching { oldRecording?.close() != false }.getOrDefault(false)
            runCatching { oldRecordCleanup?.join(1000) }
            val stopped = publishingStopped && recordingStopped && oldRecordCleanup?.isAlive != true
            var resourcesReleased = stopped
            if (stopped) {
                cleanupPhase = "codec/native release"
                // A partially configured codec may refuse stop; actual release
                // still determines whether its owner can be discarded.
                runCatching { oldVideo?.stop() }
                for (release in listOf<() -> Unit>(
                    { oldVideo?.release() },
                    { oldInput?.release() }, { oldQueues?.cancel() }, { oldQueues?.close() },
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
                videoEncoder = null; encoderSurface = null; audioOwner = null; publisherAbort = null
                oldPrivacyRun?.let { privacy?.runClosed(it) };privacyRun=null;privacyCommand=null
                queues = null; videoCodec = null; audioCodec = null; publishing = null; recording = null; recordingCleanup = null
                publishingSink = null; recordingSink = null; renderer = null; cameraOwner = null; cameraInput = null
                framing.clear(); audioLevel = 0f
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
            audioOwnerClosed, (audioOwner?.outstanding ?: 0) == 0, stopFailed, cleanupExpired).withIndex()) {
            if (value) flags = flags or (1 shl index)
        }
        return CaptureLifecycleDiagnostics.Context(captureEpoch, generation, plan?.pose ?: restart.desiredPose,
            CaptureLifecycleDiagnostics.CaptureStageName.valueOf(stage.name), flags)
    }

    private fun traceLifecycle(code: CaptureLifecycleDiagnostics.Code) {
        lifecycle.transition(code, lifecycleContext())?.let { Log.i("SapflowLifecycle", it.logLine()) }
    }

    private fun markFailure(code: CaptureLifecycleDiagnostics.Code, reason: String, cleanupTimedOut: Boolean = false) {
        startAdmission.freezeElapsed()
        stopFailed = true
        cleanupExpired = cleanupExpired || cleanupTimedOut
        autoStart = false
        restart.cancel()
        lifecycle.failure(code, reason, lifecycleContext())?.let { Log.w("SapflowLifecycle", it.logLine()) }
        message = lifecycle.message(reason)
    }

    private fun updateCleanupDiagnostics() {
        cleanupDiagnostics = "generation=$generation/drain=$drainGeneration stopping=$stoppingEpoch teardown=$teardownStarted cameraClosed=$cameraClosed GPUstopped=$rendererStopped GPUclosed=$rendererClosed AACactualClosed=$audioOwnerClosed handles=${audioOwner?.outstanding ?: 0} tail=$tailStarted/$tailDone firstFailure=${lifecycle.firstFailure?.code?.name ?: "none"}"
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
        startAdmission.preparation?.let { privacy?.stop(it) }
        startAdmission.stop()
        sourcePolicy.close(); sourceRequests.close(); controlMailbox.close(); runAdmission.cancelSourceBudget(); renderer?.fenceSource()
        publishingSession?.stopDestination()
        handler.post {
            shutdownRequested = true; autoStart = false; restart.cancel()
            publishingSession?.stopDestination()
            publishFactory = null; recordFactory = null
            stopOnHandler("Closed", failed = false)
        }
    }
}
