package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.content.Intent
import android.app.Activity
import android.net.Uri
import java.lang.ref.WeakReference
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.hardware.display.DisplayManager
import android.view.OrientationEventListener
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.CancellationSignal
import android.os.SystemClock
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.viewinterop.AndroidView

class MainActivity : ComponentActivity() {
    private lateinit var capture: CaptureController
    private lateinit var audioOwner: AudioBroadcastOwner
    private val runtimeClient = Any()
    private var v4v: V4vAppOwner? = null
    private var useV4v by mutableStateOf(false)
    private var prepareRequest = 0L
    private var preparedV4v: V4vCaptureAdmission.Prepared? = null
    private var preparedCapture: CaptureStartAdmission.Preparation? = null
    private var preparedV4vCapture: CaptureStartAdmission.Preparation? = null
    private class PreparationIntent(val request: Long, val capture: CaptureIntent, val record: Boolean, val v4v: Boolean,
        val srt: Boolean, val endpoint: String, val key: String, val passphrase: String, val srtPlaintext: Boolean, val rtmpPlaintext: Boolean, val audioInputId: String?, val backgroundAudio: Boolean)
    private var pendingPreparation: PreparationIntent? = null
    private val permissionReplies = PendingPermissionReply<PreparationIntent>()
    private var permissionReplyOutstanding by mutableStateOf(false)
    private var preparationPending by mutableStateOf(false)
    private var recordingPreflight: RecordingLibraryAdmission.Ticket? = null
    private lateinit var recordingLibrary: RecordingLibraryOwner
    private var libraryVisible by mutableStateOf(false)
    private var libraryEntries by mutableStateOf<List<RecordingCatalogue.Entry>>(emptyList())
    private var libraryBusy by mutableStateOf(false)
    private var libraryMessage by mutableStateOf<String?>(null)
    private var libraryScan: RecordingLibraryAdmission.Ticket? = null
    private val exportDocument = registerForActivityResult(ActivityResultContracts.CreateDocument("video/mp4")) { finishDocumentExport(it) }
    private var v4vState by mutableStateOf<V4vView?>(null)
    private var v4vEvents by mutableStateOf<List<Pair<String, String>>>(emptyList())
    private var v4vEvent by mutableStateOf("")
    private var acknowledgeLoss by mutableStateOf(false)
    private val signerResult = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        v4v?.signerRelay?.deliver(it.resultCode == Activity.RESULT_OK, it.data)
    }
    private val launchSigner: (Intent) -> Unit = { signerResult.launch(it) }

    private var preview: Surface? = null
    private var previewWidth = 1
    private var previewHeight = 1
    private lateinit var poseSelector: PoseInputPolicy
    private var qualificationPoseControlEnabled = false
    private lateinit var orientation: OrientationEventListener
    private var poseTracking = false

    @Suppress("DEPRECATION")
    private fun captureDisplay() = if (android.os.Build.VERSION.SDK_INT >= 30) display ?: windowManager.defaultDisplay else windowManager.defaultDisplay

    private fun displayRotation(): Int = captureDisplay().rotation

    private fun displayPose() = CardinalPose.fromDegrees((360 - displayRotation() * 90) % 360)

    private fun updatePreview() {
        val degrees = displayRotation() * 90
        // Physical mode dimensions are independent of the Activity's current
        // viewport/configuration transition, unlike a transient window aspect.
        val mode = captureDisplay().mode
        val naturalPortrait = mode.physicalHeight >= mode.physicalWidth
        capture.updatePreview(preview, previewWidth, previewHeight, degrees, naturalPortrait)
    }
    private var snapshot by mutableStateOf<CaptureSnapshot?>(null)
    private var permissionMessage by mutableStateOf<String?>(null)
    private var surfaceReady by mutableStateOf(false)
    private var destinationUrl by mutableStateOf("")
    private var destinationKey by mutableStateOf("")
    private var allowPlaintextDevelopment by mutableStateOf(false)
    private var useSrt by mutableStateOf(false)
    private var srtPassphrase by mutableStateOf("")
    private var srtPlaintextLab by mutableStateOf(false)
    private val manualActions = ManualProfileActions()
    private lateinit var manualIO: ManualDestinationProfileIO
    private var manualBusy by mutableStateOf(false)
    private var manualName by mutableStateOf("")
    private var manualSaved by mutableStateOf<List<ManualDestinationProfiles.Saved>>(emptyList())
    private var manualSelected: String? = null
    private var manualReview by mutableStateOf<ManualProfileActions.Review?>(null)
    private var manualReviewSaved: String? = null
    private var manualConsent by mutableStateOf(false)
    private var manualMessage by mutableStateOf<String?>(null)
    private val manualPicker: ManualProfilePicker<Uri> get() = manualIO.picker
    private var manualForeground = false
    private val importManualDocument = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        manualPicker.returned(runtimeClient, uri)
        if (manualForeground) resumeManualDocument()
    }
    private var recordLocally by mutableStateOf(true)
    private var captureProfile by mutableStateOf(CaptureProfile.DEFAULT)
    private var microphoneOnly by mutableStateOf(false)
    private var backgroundAudio by mutableStateOf(false)
    private var selectedAudioInputId by mutableStateOf<String?>(null)
    private var audioInputOptions by mutableStateOf<List<AudioInputOption>>(emptyList())
    private val selectedIntent: CaptureIntent get() = if (microphoneOnly) CaptureIntent.AudioOnly48kMono else CaptureIntent.AudioVideo(captureProfile)
    private val ui = Handler(Looper.getMainLooper())
    private val settlePose = object : Runnable {
        override fun run() {
            if (!poseTracking) return
            val previous = poseSelector.pose
            val maySettle = poseSelector.overridePose == null || capture.snapshot().stage == CaptureStage.CAPTURING
            val next = poseSelector.settle(System.nanoTime(), maySettle)
            if (next != previous && !poseSelector.captureStopped) capture.orientationChanged(next)
            schedulePoseSettlement()
        }
    }
    private fun schedulePoseSettlement() {
        ui.removeCallbacks(settlePose)
        poseSelector.deadlineNs?.let { deadline ->
            ui.postDelayed(settlePose, ((deadline - System.nanoTime()).coerceAtLeast(0) + 999_999) / 1_000_000)
        }
    }

    /** Called only by the qualification-source Application; release has no
     * receiver, Application override or external route to these methods. */
    internal fun enableQualificationPoseControl(enabled: Boolean) {
        qualificationPoseControlEnabled = enabled
        if (::poseSelector.isInitialized) poseSelector.enableQualification(enabled)
        if (!enabled) ui.removeCallbacks(settlePose)
    }

    internal fun requestQualificationPose(value: String?): PoseInputPolicy.Result {
        if (!poseTracking) return PoseInputPolicy.Result.INACTIVE
        val previous = poseSelector.pose
        val capturing = capture.snapshot().stage == CaptureStage.CAPTURING
        val result = poseSelector.request(value, capturing, System.nanoTime())
        if (result == PoseInputPolicy.Result.CLEARED && capturing && !poseSelector.captureStopped && poseSelector.pose != previous) {
            capture.orientationChanged(poseSelector.pose)
        }
        if (result == PoseInputPolicy.Result.ACCEPTED || result == PoseInputPolicy.Result.CLEARED) schedulePoseSettlement()
        return result
    }
    private val displayChanges = object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(displayId: Int) = Unit
        override fun onDisplayRemoved(displayId: Int) = Unit
        override fun onDisplayChanged(displayId: Int) { if (poseTracking && displayId == captureDisplay().displayId) updatePreview() }
    }
    private val refresh = object : Runnable {
        override fun run() {
            snapshot = capture.snapshot()
            v4vState = v4v?.control?.view
            libraryBusy = !recordingLibrary.idle
            if (recordingLibrary.cleanupRetained) libraryMessage = "File cleanup was not confirmed. Existing recordings are preserved; further file operations are blocked."
            ui.postDelayed(this, 200)
        }
    }
    private val permissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        val intent = permissionReplies.receive()
        permissionReplyOutstanding = permissionReplies.outstanding
        if (intent != null && currentPreparation(intent)) {
            if (preparationPermissions(intent).all { result[it] == true || checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) {
                permissionMessage = null
                prepareCapture(intent)
            } else {
                cancelPreparation()
                permissionMessage = if (intent.capture.hasVideo) "Camera and microphone permission are required. Enable them in system settings if permission was permanently denied." else "Microphone and, for background mode, notification permission are required. Enable them in system settings if denied."
            }
        } else if (pendingPreparation === intent && intent != null) cancelPreparation()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        manualIO = ManualDestinationProfileIO.application(applicationContext)
        if (savedInstanceState?.getBoolean("sapflow.manual-picker.discard") == true) manualPicker.restoreDiscard(runtimeClient)
        audioOwner = AudioBroadcastOwner.application(applicationContext)
        capture = audioOwner.capture
        preparedCapture = capture.currentPreparation()
        preparedCapture?.let { retained ->
            // Observe the exact process-owned intent; never reconstruct destination credentials.
            microphoneOnly = !retained.intent.hasVideo
            retained.intent.videoProfile?.let { captureProfile = it }
            backgroundAudio = retained.backgroundAudio
        }
        audioOwner.continuingShow()?.let { retained ->
            useV4v = retained.usesV4v
            recordLocally = retained.recordsLocally
            if (retained.usesV4v) preparedV4vCapture = retained.preparation
        }
        recordingLibrary = RecordingLibraryOwner.application(applicationContext)
        try { v4v = V4vAppOwner.configured(applicationContext); v4v?.signerRelay?.attach(launchSigner) }
        catch (_: Exception) { permissionMessage = "Approved V4V configuration is invalid; connector disabled" }
        poseSelector = PoseInputPolicy(displayPose(), qualificationPoseControlEnabled)
        capture.orientationChanged(poseSelector.pose)
        orientation = object : OrientationEventListener(this) {
            override fun onOrientationChanged(angle: Int) {
                if (!poseTracking) return
                val previous = poseSelector.pose
                val next = poseSelector.acceptPhysical(angle, System.nanoTime())
                if (next != previous && !poseSelector.captureStopped) capture.orientationChanged(next)
                schedulePoseSettlement()
            }
        }
        setContent {
            MaterialTheme(colorScheme = darkColorScheme(primary = Color(0xFFFFAA62), background = Color(0xFF111416))) {
                BoxWithConstraints(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).windowInsetsPadding(WindowInsets.safeDrawing)) {
                    if (maxWidth > maxHeight) {
                        Row(Modifier.fillMaxSize()) { PreviewPane(Modifier.weight(1f).fillMaxHeight()); Controls(Modifier.width(300.dp).fillMaxHeight()) }
                    } else {
                        Column(Modifier.fillMaxSize()) { PreviewPane(Modifier.weight(1f).fillMaxWidth()); Controls(Modifier.weight(1f).fillMaxWidth()) }
                    }
                }
                RecordingLibraryPanel(libraryVisible, libraryEntries, libraryBusy, libraryMessage, captureIdleForFiles(),
                    onDismiss = { libraryVisible = false }, onRefresh = { loadRecordings() },
                    onExport = ::chooseRecordingExport, onCancelExport = ::cancelLibraryOperation)
            }
        }
    }

    @Composable
    private fun PreviewPane(modifier: Modifier) {
        Box(modifier, contentAlignment = Alignment.Center) {
            if (!selectedIntent.hasVideo) Text("Microphone only · no camera preview")
            else AndroidView(factory = { context -> SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) { preview = holder.surface; surfaceReady = true; updatePreview() }
                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                        preview = holder.surface; previewWidth = width; previewHeight = height; updatePreview()
                    }
                    override fun surfaceDestroyed(holder: SurfaceHolder) { surfaceReady = false; preview = null; updatePreview() }
                })
            } }, modifier = Modifier.fillMaxSize())
            Text("SAPFLOW  ·  NATIVE CAPTURE", Modifier.align(Alignment.TopStart).padding(16.dp), color = Color.White)
        }
    }

    @Composable
    private fun Controls(modifier: Modifier) {
        Column(modifier.verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        Text("Create, freely", style = MaterialTheme.typography.headlineSmall)
                        Text("Free V4V in every build", color = MaterialTheme.colorScheme.primary)
                        val current = snapshot
                        Text(current?.stage?.name ?: "IDLE", style = MaterialTheme.typography.labelLarge)
                        Text(permissionMessage ?: current?.message ?: if (microphoneOnly) "Prepare the microphone" else "Prepare the camera and microphone")
                        Text(current?.orientation ?: if (microphoneOnly) "AAC-LC 48 kHz mono · 96 kbit/s" else "${captureProfile.label}30 · H264 · AAC 48 kHz", style = MaterialTheme.typography.bodySmall)
                        val qualityConfigurable = !preparationPending && !libraryBusy && current?.canPrepare != false && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            listOf(false to "Video", true to "Microphone").forEach { (audioOnly, label) ->
                                OutlinedButton(enabled = qualityConfigurable, onClick = {
                                    if (!preparationPending && recordingLibrary.idle && capture.snapshot().canPrepare && microphoneOnly != audioOnly) {
                                        prepareRequest++; preparedV4v = null; v4v?.stopPublisher()
                                        microphoneOnly = audioOnly
                                    }
                                }) { Text(if (microphoneOnly == audioOnly) "$label selected" else label) }
                            }
                        }
                        Text("Microphone input", style = MaterialTheme.typography.labelLarge)
                        OutlinedButton(enabled = qualityConfigurable, onClick = {
                            if (!preparationPending && recordingLibrary.idle && capture.snapshot().canPrepare) selectedAudioInputId = null
                        }) { Text(if (selectedAudioInputId == null) "System Default selected" else "System Default") }
                        audioInputOptions.forEach { input ->
                            OutlinedButton(enabled = qualityConfigurable, onClick = {
                                if (!preparationPending && recordingLibrary.idle && capture.snapshot().canPrepare) selectedAudioInputId = input.id
                            }) { Text("${input.name} · ${input.kind}" + if (selectedAudioInputId == input.id) " selected" else "") }
                        }
                        OutlinedButton(enabled = qualityConfigurable, onClick = {
                            if (!preparationPending && recordingLibrary.idle && capture.snapshot().canPrepare) audioInputOptions = AudioInputRouting.inventory(this@MainActivity)
                        }) { Text("Refresh microphone inputs") }
                        if (selectedAudioInputId != null && audioInputOptions.none { it.id == selectedAudioInputId }) Text("Selected microphone unavailable")
                        Text("Active microphone: ${current?.activeAudioInput ?: "Not verified"}", style = MaterialTheme.typography.bodySmall)
                        if (microphoneOnly) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Checkbox(backgroundAudio, { backgroundAudio = it }, enabled = qualityConfigurable)
                                Text("Continue microphone capture when locked", style = MaterialTheme.typography.bodySmall)
                            }
                            Text("Choose before Prepare. Background capture requires notifications and provides Stop and save.", style = MaterialTheme.typography.bodySmall)
                        }
                        Text("Capture quality · choose before Prepare", style = MaterialTheme.typography.labelMedium)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (!microphoneOnly) CaptureProfile.entries.forEach { quality ->
                                OutlinedButton(enabled = qualityConfigurable, onClick = {
                                    if (!preparationPending && recordingLibrary.idle && capture.snapshot().canPrepare && captureProfile != quality) {
                                        prepareRequest++; preparedV4v = null; v4v?.stopPublisher()
                                        captureProfile = quality
                                    }
                                }) { Text(if (captureProfile == quality) "${quality.label} selected" else quality.label) }
                            }
                        }
                        OutlinedButton(
                            onClick = { prepareWithPermissions() },
                            enabled = !preparationPending && !permissionReplyOutstanding && !libraryBusy && (microphoneOnly || surfaceReady) && current?.canPrepare != false && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR),
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Prepare") }
                        Button(
                            onClick = { startWithAuth() },
                            enabled = current?.stage == CaptureStage.PREPARED &&
                                (preparedCapture?.backgroundAudio != true || current.backgroundAudioReady),
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Start capture") }
                        if (current?.stage == CaptureStage.PREPARED && preparedCapture?.backgroundAudio == true && !current.backgroundAudioReady) {
                            Text("Waiting for the microphone service", style = MaterialTheme.typography.bodySmall)
                        }
                        Button(
                            onClick = {
                                ui.removeCallbacks(settlePose)
                                poseSelector.stopCapture()
                                prepareRequest++
                                cancelPreparation()
                                v4v?.stopPublisher()
                                capture.stop()
                                schedulePoseSettlement()
                                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                            },
                            enabled = preparationPending || current?.stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING, CaptureStage.STOPPING),
                            colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF833D38)),
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Stop") }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = recordLocally, onCheckedChange = { recordLocally = it },
                                enabled = !preparationPending && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR))
                            Text("Record private MP4 on this device", style = MaterialTheme.typography.bodySmall)
                        }
                        if (preparationPending) Text("Checking capture preparation", style = MaterialTheme.typography.bodySmall)
                        else if (permissionReplyOutstanding) Text("Finish the permission request before preparing again.", style = MaterialTheme.typography.bodySmall)
                        OutlinedButton(enabled = !preparationPending && !libraryBusy, onClick = { libraryVisible = true; loadRecordings() }, modifier = Modifier.fillMaxWidth()) { Text("Private recordings") }
                        Text("Microphone", style = MaterialTheme.typography.labelMedium)
                        LinearProgressIndicator(progress = { current?.audioLevel ?: 0f }, modifier = Modifier.fillMaxWidth())
                        Text("Video ${current?.videoSamples ?: 0} · Audio ${current?.audioSamples ?: 0} · Ingress rejects ${current?.drops ?: 0}", style = MaterialTheme.typography.bodySmall)
                        fun elapsed(value: Long?): String = value?.let { val seconds = it / 1000; "${seconds / 3600}:${((seconds / 60) % 60).toString().padStart(2, '0')}:${(seconds % 60).toString().padStart(2, '0')}" } ?: "Not measured"
                        Text("Capture elapsed ${elapsed(current?.captureElapsedMs)} · Broadcast elapsed ${elapsed(current?.broadcastElapsedMs)}", style = MaterialTheme.typography.bodySmall)
                        val output = current?.socketOutput
                        val rate = when {
                            output == null || output.availability == SocketOutputAvailability.NOT_ATTACHED -> "Not attached"
                            output.ended -> "Ended · rate unavailable"
                            output.bitsPerSecond != null -> if (output.bitsPerSecond in 1..999) "<1 kbit/s" else "${output.bitsPerSecond / 1000} kbit/s"
                            else -> "Unknown (${output.availability.name.lowercase().replace('_', ' ')})"
                        }
                        Text("Socket outbound $rate", style = MaterialTheme.typography.bodySmall)
                        Text("Completed writes ${output?.completedBytes?.let { "$it B" } ?: "Unknown"}${if (output?.incomplete == true) " · incomplete accounting" else ""} · sample age ${output?.sampleAgeMs?.let { "${it}ms" } ?: "Unknown"}", style = MaterialTheme.typography.bodySmall)
                        Text("Measured locally; delivery and playback are unconfirmed. Capture elapsed is not saved recording duration.", style = MaterialTheme.typography.bodySmall)
                        Text("Video timing/IDR ${current?.videoRejects ?: 0} · AAC priming ${current?.audioPrimingRejects ?: 0} · Wait IDR ${current?.waitingKeyframeRejects ?: 0} · Other ingress rejects ${current?.queueRejects ?: 0}", style = MaterialTheme.typography.bodySmall)
                        Text(current?.queueDiagnostics ?: "Queue diagnostics not sampled", style = MaterialTheme.typography.bodySmall)
                        Text(current?.cleanupDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text(current?.audioDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text(current?.renderer ?: "GPU not prepared", style = MaterialTheme.typography.bodySmall)
                        Text(current?.sinkDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text("Encoded video capture age ${current?.videoCaptureAgeMs?.let { "${it}ms" } ?: "Not measured"}", style = MaterialTheme.typography.bodySmall)
                        Text(current?.publishingStatus ?: "No publisher attached", style = MaterialTheme.typography.bodySmall)
                        Text(current?.recordingStatus ?: "No recorder attached", style = MaterialTheme.typography.bodySmall)
                        val configurable = !preparationPending && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR)
                        V4vControls(configurable)
                        Text("Manual destination · bounded reconnect", style = MaterialTheme.typography.labelMedium)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = useSrt, onCheckedChange = { manualEdited(); useSrt = it }, enabled = configurable && !useV4v)
                            Text("Use SRT over UDP", style = MaterialTheme.typography.bodySmall)
                        }
                        OutlinedTextField(
                            value = destinationUrl, onValueChange = { manualEdited(); destinationUrl = it.take(4096) },
                            label = { Text(if (useSrt) "SRT numeric address and port" else "RTMPS application URL") },
                            placeholder = { Text(if (useSrt) "srt://192.0.2.1:8890" else "rtmps://origin.invalid/live") },
                            enabled = configurable && !useV4v, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        OutlinedTextField(
                            value = destinationKey, onValueChange = { manualEdited(); destinationKey = it.take(4096) },
                            label = { Text(if (useSrt) "SRT stream ID (optional)" else "Stream key") }, visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                            enabled = configurable && !useV4v, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        if (useSrt) {
                            OutlinedTextField(
                                value = srtPassphrase, onValueChange = { manualEdited(); srtPassphrase = it.take(79) },
                                label = { Text("AES-256 passphrase · 10 to 79 bytes") },
                                visualTransformation = PasswordVisualTransformation(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                                enabled = configurable && !useV4v, singleLine = true, modifier = Modifier.fillMaxWidth(),
                            )
                            Row {
                                Checkbox(checked = srtPlaintextLab, onCheckedChange = { manualEdited(); srtPlaintextLab = it }, enabled = configurable && !useV4v)
                                Text("Plaintext SRT lab only · clear passphrase", style = MaterialTheme.typography.bodySmall)
                            }
                        } else {
                            Row {
                                Checkbox(checked = allowPlaintextDevelopment, onCheckedChange = { manualEdited(); allowPlaintextDevelopment = it }, enabled = configurable && !useV4v)
                                Text("Allow plaintext RTMP for development", style = MaterialTheme.typography.bodySmall)
                            }
                        }
                        ManualProfileControls(configurable && !useV4v)
                        Text("Leave blank for SDK validation only. No transport downgrade.", style = MaterialTheme.typography.bodySmall)
                        Text("Video: ${current?.videoEncoder ?: "Not selected"}", style = MaterialTheme.typography.bodySmall)
                        Text("Audio: ${current?.audioEncoder ?: "Not selected"}", style = MaterialTheme.typography.bodySmall)
                        Text("Only an opted-in, started microphone show can continue when this screen closes.", style = MaterialTheme.typography.bodySmall)
        }
    }


    private fun manualAllowed(): Boolean = !useV4v && manualForeground && poseTracking && !isDestroyed && !preparationPending && !permissionReplies.outstanding &&
        ::capture.isInitialized && ::recordingLibrary.isInitialized && recordingLibrary.idle && capture.snapshot().canPrepare

    private fun manualEdited() {
        manualPicker.invalidate(runtimeClient)
        manualActions.invalidate(); manualReview = null; manualConsent = false; manualReviewSaved = null
    }
    private fun dismissManualReview(selected: ManualProfileActions.Review) {
        if (manualActions.cancel(selected)) {
            manualReview = null; manualConsent = false; manualReviewSaved = null
        }
    }
    private fun manualTicket(): ManualProfileActions.Ticket? {
        val ticket = manualActions.begin(manualAllowed()) ?: return null
        manualReview = null; manualConsent = false; manualReviewSaved = null; manualMessage = null; manualBusy = true
        return ticket
    }
    private fun <T> manualSubmit(ticket: ManualProfileActions.Ticket, action: (ManualDestinationProfiles) -> T,
        deadlineNs: Long? = null, cancel: (() -> Unit)? = null,
        reply: (MainActivity, ManualProfileActions.Ticket, Result<T>) -> Unit) {
        val screen = WeakReference(this)
        val actions = manualActions
        if (!manualIO.submit({ store -> check(actions.current(ticket, true)); action(store) }, deadlineNs, cancel) { result ->
            val current = screen.get() ?: return@submit
            if (current.manualActions.current(ticket, current.manualAllowed())) reply(current, ticket, result)
            else current.manualActions.finish(ticket, false)
            current.manualBusy = current.manualActions.busy
        }) {
            manualActions.finish(ticket, false); manualBusy = false
            manualMessage = "A destination file operation is still finishing. Wait before trying again."
        }
    }
    private fun refreshManualProfiles() {
        val ticket = manualTicket() ?: return
        manualSubmit(ticket, { it.list() }) { current, active, result ->
            if (current.manualActions.finish(active, current.manualAllowed())) result.fold(
                onSuccess = { current.manualSaved = it }, onFailure = { current.manualMessage = "Saved destinations could not be read. Existing data was preserved." })
        }
    }
    private fun saveManualProfile() {
        if (!manualAllowed()) return
        val profile = try { ManualDestinationProfile.fields(manualName, if (useSrt) "srt" else "rtmp", destinationUrl,
            if (useSrt) "" else destinationKey, if (useSrt) destinationKey else "", if (useSrt) srtPassphrase else "",
            if (useSrt) srtPlaintextLab else allowPlaintextDevelopment) }
        catch (_: Exception) { manualMessage = "Check the profile name, destination and separate credentials before saving."; return }
        val selected = manualSelected
        val ticket = manualTicket() ?: return
        manualSubmit(ticket, { store -> val id = store.save(selected, profile); id to store.list() }) { current, active, result ->
            if (current.manualActions.finish(active, current.manualAllowed())) result.fold(onSuccess = {
                current.manualSelected = it.first; current.manualSaved = it.second; current.manualMessage = "Destination saved on this device."
            }, onFailure = { current.manualMessage = "The saved action could not be confirmed. Check saved profiles before retrying." })
        }
    }
    private fun loadManualProfile(id: String) {
        val ticket = manualTicket() ?: return
        manualSubmit(ticket, { it.load(id) }) { current, active, result ->
            result.fold(onSuccess = { profile ->
                if (current.manualActions.reviewed(active, profile, current.manualAllowed())) {
                    current.manualReview = current.manualActions.review; current.manualReviewSaved = id
                }
            }, onFailure = {
                current.manualActions.finish(active, false); current.manualMessage = "The saved destination could not be loaded. Existing data was preserved."
            })
        }
    }
    private fun deleteManualProfile() {
        val selected = manualSelected ?: return
        val ticket = manualTicket() ?: return
        manualSubmit(ticket, { store -> store.delete(selected); store.list() }) { current, active, result ->
            if (current.manualActions.finish(active, current.manualAllowed())) result.fold(onSuccess = {
                current.manualSelected = null; current.manualSaved = it; current.manualMessage = "Saved destination deleted."
            }, onFailure = { current.manualMessage = "The delete action could not be confirmed. Check saved profiles before retrying." })
        }
    }
    private fun chooseManualDocument() {
        val ticket = manualTicket() ?: return
        if (!manualPicker.begin(runtimeClient, ticket)) {
            manualActions.finish(ticket, false); manualBusy = false; manualMessage = "An earlier document picker is still pending. Close it before importing again."; return
        }
        try { importManualDocument.launch(arrayOf("application/json")) }
        catch (_: Exception) { manualPicker.returned(runtimeClient, null); manualPicker.resume(runtimeClient, false); manualActions.finish(ticket, false); manualBusy = false; manualMessage = "The document picker could not open." }
    }
    private fun resumeManualDocument() {
        val delivery = manualPicker.resume(runtimeClient, manualAllowed()) ?: return
        val ticket = delivery.ticket ?: return
        val uri = delivery.value
        if (!delivery.valid || uri == null || uri.scheme != "content" || !manualActions.current(ticket, manualAllowed())) {
            manualActions.finish(ticket, false); manualBusy = manualActions.busy; return
        }
        val io = manualIO
        val deadline = Math.addExact(SystemClock.elapsedRealtimeNanos(), 5_000_000_000L)
        val signal = CancellationSignal()
        manualSubmit(ticket, { io.readDocument(uri, deadline, signal) }, deadline, { signal.cancel() }) { current, active, result ->
            result.fold(onSuccess = { profile ->
                if (current.manualActions.reviewed(active, profile, current.manualAllowed())) current.manualReview = current.manualActions.review
            }, onFailure = {
                current.manualActions.finish(active, false); current.manualMessage = "The selected document could not be validated. Wait for any pending file cleanup before trying again."
            })
        }
    }
    @Composable private fun ManualProfileControls(configurable: Boolean) {
        OutlinedTextField(value = manualName, onValueChange = { manualEdited(); manualName = it.take(48) }, label = { Text("Profile name") },
            enabled = configurable, singleLine = true, modifier = Modifier.fillMaxWidth())
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(enabled = configurable && !manualBusy, onClick = { saveManualProfile() }) { Text("Save") }
            OutlinedButton(enabled = configurable && !manualBusy && manualSelected != null, onClick = { deleteManualProfile() }) { Text("Delete saved") }
        }
        OutlinedButton(enabled = configurable && !manualBusy, onClick = { refreshManualProfiles() }) { Text("Load saved destinations") }
        manualSaved.forEach { saved -> OutlinedButton(enabled = configurable && !manualBusy, onClick = { loadManualProfile(saved.id) }) { Text("Load ${saved.name}") } }
        OutlinedButton(enabled = configurable && !manualBusy, onClick = { chooseManualDocument() }) { Text("Import reviewed JSON file") }
        manualMessage?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        if (manualBusy) Text("Destination file operation pending", style = MaterialTheme.typography.bodySmall)
        manualReview?.let { selected ->
            val review = selected.profile.review
            AlertDialog(onDismissRequest = { dismissManualReview(selected) }, title = { Text("Review manual destination") }, text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("${if (review.transport == "srt") "SRT" else "RTMP"} · ${review.security}")
                    Text("${if (review.host.contains(':')) "[${review.host}]" else review.host}:${review.port}")
                    Text("Apply changes fields only. Save and Prepare remain separate actions.")
                    if (review.plaintext) Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(manualConsent, { if (manualActions.setConsent(selected, it)) manualConsent = it }); Text("I allow this unencrypted destination")
                    }
                }
            }, confirmButton = {
                Button(enabled = configurable && (!review.plaintext || manualConsent), onClick = {
                    val saved = manualReviewSaved
                    val applied = manualActions.apply(selected, manualAllowed()) { profile ->
                        useSrt = profile.transport == "srt"; destinationUrl = profile.endpoint
                        destinationKey = if (useSrt) profile.streamID else profile.streamKey
                        srtPassphrase = if (useSrt) profile.passphrase else ""
                        // A secure imported endpoint never grants future plaintext consent.
                        srtPlaintextLab = useSrt && profile.review.plaintext
                        allowPlaintextDevelopment = !useSrt && profile.review.plaintext
                        manualName = if (saved == null) "" else profile.name
                        manualSelected = saved
                    }
                    if (applied && manualReview === selected) { manualReview = null; manualConsent = false; manualReviewSaved = null }
                }) { Text("Apply") }
            }, dismissButton = { OutlinedButton(onClick = { dismissManualReview(selected) }) { Text("Cancel") } })
        }
    }

    private fun startWithAuth() {
        val captureIntent = preparedCapture ?: return
        fun start() {
            audioOwner.revalidateAuthority()
            if (audioOwner.awaitingPromotion(captureIntent)) {
                permissionMessage = "Waiting for the microphone service. Start will be available when it is ready."
                return
            }
            if (!audioOwner.canStart(captureIntent)) {
                if (capture.stop(captureIntent, "Background audio service is not ready or permission changed. Prepare again.")) v4v?.stopPublisher()
                permissionMessage = "Background service and notification permission are required before Start. Prepare again."
                return
            }
            capture.start(captureIntent)
            if (!captureIntent.backgroundAudio) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        if (!useV4v) start()
        else {
            val request = prepareRequest
            val binding = preparedV4v ?: return
            v4v?.grant(captureIntent.intent) { permit, error ->
                if (request != prepareRequest || !useV4v || preparedV4v != binding || preparedCapture != captureIntent) return@grant
                if (error == null && permit != null && v4v?.canStart(binding, permit, captureIntent.intent) == true && poseTracking && capture.snapshot().stage == CaptureStage.PREPARED) start()
                else permissionMessage = error?.message?.take(160) ?: "V4V start permission no longer matches this prepared capture"
            }
        }
    }
    private fun v4vCommand(action: V4vSessionEngine.() -> Unit) {
        v4v?.control?.submit(action) { error -> ui.post {
            if (error != null) permissionMessage = error.message?.take(160) ?: "V4V operation not confirmed"
            v4vState = v4v?.control?.view
        } }
    }
    @Composable private fun V4vControls(configurable: Boolean) {
        val owner = v4v
        if (owner == null) { Text("V4V sign-in is unavailable in this build", style = MaterialTheme.typography.bodySmall); return }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Checkbox(useV4v, { manualEdited(); prepareRequest++; useV4v = it }, enabled = configurable)
            Text("Use approved V4V connector", style = MaterialTheme.typography.bodySmall)
        }
        if (!useV4v) return
        Text(v4vState?.message ?: "Loading V4V journal", style = MaterialTheme.typography.bodySmall)
        v4vState?.maxPublishBps?.let { cap ->
            Text("V4V cap $cap bits/s · ${selectedIntent.label} needs ${selectedIntent.minimumPublishBps} bits/s", style = MaterialTheme.typography.bodySmall)
            if (cap < selectedIntent.minimumPublishBps) Text("Select a supported quality before Prepare. The reserved session can still be ended.", style = MaterialTheme.typography.bodySmall)
        }
        if (configurable) {
            val signers = remember(owner) { Nip55Login(this, owner.signerRelay).installed() }
            signers.forEach { (pkg, label) -> OutlinedButton(onClick = {
                owner.beginLogin(pkg, this) { error -> permissionMessage = error?.message?.take(160) }
            }) { Text("Sign in with $label") } }
            if (signers.isEmpty()) Text("Install a NIP-55 signer to sign in; no key import", style = MaterialTheme.typography.bodySmall)
        }
        OutlinedButton(enabled = v4vState?.signedIn == true, onClick = {
            var rows: List<Pair<String, String>> = emptyList()
            owner.control.submit({ rows = listEvents() }) { error -> ui.post {
                if (error == null) v4vEvents = rows else permissionMessage = error.message?.take(160)
            } }
        }) { Text("Load my live events") }
        v4vEvents.forEach { (id, title) -> OutlinedButton(enabled = configurable, onClick = { v4vEvent = id }) { Text(title.take(60)) } }
        Text(if (v4vEvent.isBlank()) "Select an event" else "Event selected", style = MaterialTheme.typography.bodySmall)
        OutlinedButton(enabled = configurable && v4vEvent.isNotBlank() && v4vState?.signedIn == true && v4vState?.pending != true,
            onClick = { val selected = v4vEvent; v4vCommand { prepare(selected) } }) { Text("Prepare V4V session") }
        OutlinedButton(enabled = v4vState?.pending == true, onClick = { v4vCommand { retry() } }) { Text("Retry saved operation") }
        OutlinedButton(enabled = v4vState?.eventId != null, onClick = { prepareRequest++; cancelPreparation(); capture.cancelPendingPrepare(preparedV4vCapture); owner.stopPublisher(); owner.control.endDestination { error -> ui.post { if (error != null) permissionMessage = "V4V End intent not confirmed"; v4vState = owner.control.view } } }) { Text("End V4V destination") }
        OutlinedButton(enabled = v4vState?.eventId != null, onClick = { v4vCommand { status() } }) { Text("Check origin finalisation") }
        if (v4vState?.recoveryOffered == true) {
            Row { Checkbox(acknowledgeLoss, { acknowledgeLoss = it }); Text("Abandon failed automatic recording", style = MaterialTheme.typography.bodySmall) }
            OutlinedButton(enabled = acknowledgeLoss && v4vState?.pending != true, onClick = { v4vCommand { acknowledge() }; acknowledgeLoss = false }) { Text("Acknowledge recording failure") }
        }
        if (v4vState?.recoveryState == "acknowledged") OutlinedButton(enabled = configurable && v4vEvent.isNotBlank(), onClick = { val selected = v4vEvent; v4vCommand { prepare(selected, true) } }) { Text("Explicitly prepare replacement") }
        Text("Playback has not been verified", style = MaterialTheme.typography.bodySmall)
    }

    private fun captureIdleForFiles(): Boolean = !preparationPending && ::capture.isInitialized && capture.snapshot().canPrepare

    private fun loadRecordings() {
        if (preparationPending) { libraryMessage = "Wait for capture preparation to finish before refreshing."; return }
        val screen = WeakReference(this)
        libraryMessage = null
        val ticket = recordingLibrary.scan { result ->
            val current = screen.get() ?: return@scan
            if (current.isDestroyed) return@scan
            current.libraryScan = null; current.libraryBusy = !current.recordingLibrary.idle
            result.fold(onSuccess = { current.libraryEntries = it }, onFailure = { current.libraryMessage = RecordingLibraryOwner.message(it) })
        }
        libraryScan = ticket; libraryBusy = !recordingLibrary.idle
        if (ticket == null) libraryMessage = "A private file operation is still finishing. Wait before refreshing."
    }
    private fun chooseRecordingExport(entry: RecordingCatalogue.Entry, allowUnverified: Boolean) {
        if (!captureIdleForFiles()) { libraryMessage = "Stop and save capture before opening the document picker."; return }
        val selected = recordingLibrary.select(entry, allowUnverified)
        if (selected == null) { libraryMessage = "Another private file operation is still finishing."; return }
        libraryBusy = true; libraryMessage = "Choose where to save a copy."
        try { exportDocument.launch("Sapflow-${entry.segmentId.take(8)}.mp4") }
        catch (_: Exception) { recordingLibrary.cancel(selected); libraryBusy = !recordingLibrary.idle; libraryMessage = "The document picker could not open. Your private original is unchanged." }
    }
    private fun finishDocumentExport(uri: Uri?) {
        if (!::recordingLibrary.isInitialized) return
        val selected = recordingLibrary.pendingSelection ?: return
        if (uri == null || uri.scheme != "content" || !captureIdleForFiles()) {
            recordingLibrary.cancel(selected); libraryBusy = !recordingLibrary.idle
            libraryMessage = "Export cancelled. Your private original is unchanged."
            return
        }
        val resolver = applicationContext.contentResolver
        val screen = WeakReference(this)
        libraryMessage = "Exporting a copy; the private original stays on this device."
        val queued = recordingLibrary.export(selected, { resolver.openOutputStream(uri, "wt") }) { result ->
            val current = screen.get() ?: return@export
            if (current.isDestroyed) return@export
            current.libraryBusy = !current.recordingLibrary.idle
            current.libraryMessage = result.fold(onSuccess = { "Exported $it bytes. Your private original is unchanged." }, onFailure = {
                RecordingLibraryOwner.message(it) + " The selected document may be incomplete."
            })
        }
        libraryBusy = !recordingLibrary.idle
        if (!queued) libraryMessage = "Export could not start. Your private original is unchanged."
    }
    private fun cancelLibraryOperation() {
        if (recordingLibrary.pendingSelection != null) {
            libraryMessage = "Cancel in the document picker to finish that selection."
            return
        }
        libraryScan?.let(recordingLibrary::cancel); libraryScan = null
        recordingLibrary.cancelCopy()
        libraryMessage = "Cancellation requested. The private original is preserved; the selected document may be incomplete."
        libraryBusy = !recordingLibrary.idle
    }

    private fun currentPreparation(intent: PreparationIntent): Boolean = pendingPreparation === intent && intent.request == prepareRequest &&
        poseTracking && !isDestroyed && capture.snapshot().canPrepare && selectedIntent == intent.capture && selectedAudioInputId == intent.audioInputId && (backgroundAudio && !intent.capture.hasVideo) == intent.backgroundAudio && recordLocally == intent.record && useV4v == intent.v4v &&
        (intent.v4v || (useSrt == intent.srt && destinationUrl == intent.endpoint && destinationKey == intent.key && srtPassphrase == intent.passphrase &&
            srtPlaintextLab == intent.srtPlaintext && allowPlaintextDevelopment == intent.rtmpPlaintext))

    private fun cancelPreparation() {
        pendingPreparation = null; preparationPending = false
        recordingPreflight?.let(recordingLibrary::cancel); recordingPreflight = null
    }
    private fun preparationPermissions(intent: PreparationIntent): List<String> =
        intent.capture.requiredPermissions + if (intent.backgroundAudio && android.os.Build.VERSION.SDK_INT >= 33) listOf(Manifest.permission.POST_NOTIFICATIONS) else emptyList()

    private fun prepareWithPermissions() {
        if (preparationPending || permissionReplies.outstanding || !recordingLibrary.idle || !poseTracking || !capture.snapshot().canPrepare || prepareRequest == Long.MAX_VALUE) return
        manualEdited()
        val intent = PreparationIntent(++prepareRequest, selectedIntent, recordLocally, useV4v, useSrt, destinationUrl, destinationKey, srtPassphrase, srtPlaintextLab, allowPlaintextDevelopment, selectedAudioInputId, backgroundAudio && !selectedIntent.hasVideo)
        pendingPreparation = intent; preparationPending = true
        val needed = preparationPermissions(intent).toTypedArray()
        if (needed.all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) {
            permissionMessage = null
            prepareCapture(intent)
        } else {
            check(permissionReplies.reserve(intent))
            permissionReplyOutstanding = true
            try { permissions.launch(needed) } catch (_: Exception) {
                permissionReplies.receive(); permissionReplyOutstanding = false
                cancelPreparation(); permissionMessage = "The permission request could not open. Try Prepare again."
            }
        }
    }

    private fun prepareCapture(intent: PreparationIntent, approvedGrant: V4vCapturePermit? = null, checkedRecording: RecordingLibraryOwner.Preparation? = null) {
        if (!currentPreparation(intent)) { if (pendingPreparation === intent) cancelPreparation(); return }
        if (intent.v4v && approvedGrant == null) {
            val owner = v4v ?: run { cancelPreparation(); return }
            owner.grant(intent.capture) { grant, error ->
                if (!currentPreparation(intent)) { if (pendingPreparation === intent) cancelPreparation(); return@grant }
                if (error != null || grant == null) {
                    cancelPreparation()
                    permissionMessage = error?.message?.take(160) ?: "V4V grant unavailable"
                } else prepareCapture(intent, grant)
            }
            return
        }
        if (intent.record && checkedRecording == null) {
            val screen = WeakReference(this)
            val request = recordingLibrary.prepare(intent.capture) { result ->
                val current = screen.get() ?: return@prepare
                if (!current.currentPreparation(intent)) { if (current.pendingPreparation === intent) current.cancelPreparation(); return@prepare }
                current.recordingPreflight = null
                result.fold(onSuccess = { current.prepareCapture(intent, approvedGrant, it) }, onFailure = {
                    current.cancelPreparation(); current.permissionMessage = RecordingLibraryOwner.message(it)
                })
            }
            recordingPreflight = request
            if (request == null) { cancelPreparation(); permissionMessage = "A private file operation is still finishing. Try Prepare again when it completes." }
            return
        }
        try {
            require(preparationPermissions(intent).all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) { "Capture permissions changed. Prepare again." }
            require(!intent.backgroundAudio || audioOwner.notificationsGranted()) { "Enable microphone-capture notifications before Prepare." }
            val surface = if (intent.capture.hasVideo) preview?.takeIf { it.isValid } ?: run { cancelPreparation(); return } else null
            if (intent.record) {
                val ready = checkNotNull(checkedRecording)
                require(ready.intent == intent.capture && ready.fresh(System.nanoTime())) { "Recording storage check expired. Prepare again." }
            }
            val key = intent.key
            val passphrase = intent.passphrase
            val plaintext = intent.srtPlaintext
            val publisher: () -> EncodedSink? = if (intent.v4v) {
                require(approvedGrant?.intent == intent.capture) { "Capture quality changed; Prepare again" }
                val prepared = checkNotNull(v4v).captureFactory(checkNotNull(approvedGrant))
                preparedV4v = prepared.binding
                prepared.factory
            } else if (intent.endpoint.isBlank()) ({ null }) else if (intent.srt) {
                SrtDestination.validateCredentials(key, passphrase, plaintext)
                val destination = SrtDestination.parse(intent.endpoint.trim())
                val coordinator = ReconnectingPublishingSink(authorityNowNs = AndroidAuthorityClock::nowNs) { events -> SrtPublishingSink(destination, key, passphrase, plaintext, events, intent = intent.capture) }
                ({ coordinator.newEpoch() })
            } else {
                require(key.isNotEmpty()) { "A stream key is required" }
                val destination = RtmpDestination.parse(intent.endpoint.trim(), intent.rtmpPlaintext)
                val coordinator = ReconnectingPublishingSink(authorityNowNs = AndroidAuthorityClock::nowNs) { events -> RtmpPublishingSink(destination, key, events, intent.capture) }
                ({ coordinator.newEpoch() })
            }
            val recorder: () -> EncodedSink? = if (intent.record) ({ checkNotNull(checkedRecording).recorder() }) else ({ null })
            updatePreview()
            // Explicit Prepare restores the stable physical pose even if no
            // further sensor callback followed qualification Stop/CLEAR.
            if (intent.capture.hasVideo) capture.orientationChanged(poseSelector.prepareSession())
            preparedCapture = capture.prepare(surface, publisher, recorder, intent.capture, intent.audioInputId, intent.backgroundAudio)
            if (intent.backgroundAudio) {
                val prepared = checkNotNull(preparedCapture) { "Capture preparation was not admitted" }
                require(audioOwner.arm(prepared, intent.v4v, intent.record)) { "Background audio service could not be admitted. Prepare again." }
            }
            if (intent.v4v) preparedV4vCapture = preparedCapture
            pendingPreparation = null; preparationPending = false
        } catch (error: Exception) {
            cancelPreparation()
            permissionMessage = error.message?.take(240) ?: "Destination configuration failed"
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) { super.onConfigurationChanged(newConfig); updatePreview() }
    override fun onStart() {
        super.onStart()
        audioOwner.enterForeground(runtimeClient)
        poseSelector = PoseInputPolicy(displayPose(), qualificationPoseControlEnabled)
        capture.orientationChanged(poseSelector.pose)
        poseTracking = true
        getSystemService(DisplayManager::class.java).registerDisplayListener(displayChanges, ui)
        updatePreview()
        if (orientation.canDetectOrientation()) orientation.enable()
        ui.post(refresh)
    }
    override fun onResume() {
        super.onResume(); manualForeground = true; resumeManualDocument()
    }
    override fun onPause() {
        manualForeground = false
        if (!manualPicker.expectedDeparture(runtimeClient)) manualEdited()
        super.onPause()
    }
    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("sapflow.manual-picker.discard", manualPicker.outstandingFor(runtimeClient))
        super.onSaveInstanceState(outState)
    }
    override fun onStop() {
        if (!manualPicker.expectedDeparture(runtimeClient)) manualEdited()
        poseTracking = false
        prepareRequest++
        cancelPreparation()
        enableQualificationPoseControl(false)
        orientation.disable()
        ui.removeCallbacks(settlePose); poseSelector.cancelPending()
        getSystemService(DisplayManager::class.java).unregisterDisplayListener(displayChanges)
        ui.removeCallbacks(refresh)
        if (audioOwner.leaveForeground(runtimeClient) == CaptureForegroundClients.Departure.STOP) {
            // An old Activity's delayed onStop must not retire any newer publisher.
            preparedCapture?.let {
                if (capture.stop(it, "Capture interrupted: the app left the foreground. Prepare again to resume.")) v4v?.stopPublisher()
            }
        }
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        super.onStop()
    }
    override fun onDestroy() { manualEdited(); libraryScan?.let(recordingLibrary::cancel); cancelPreparation(); v4v?.signerRelay?.detach(launchSigner); enableQualificationPoseControl(false); super.onDestroy() }
}
