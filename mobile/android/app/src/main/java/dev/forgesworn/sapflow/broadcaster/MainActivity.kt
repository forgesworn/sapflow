package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.content.Intent
import android.app.Activity
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.hardware.display.DisplayManager
import android.view.OrientationEventListener
import android.os.Bundle
import android.os.Handler
import android.os.Looper
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
    private var v4v: V4vAppOwner? = null
    private var useV4v by mutableStateOf(false)
    private var prepareRequest = 0L
    private var preparedV4v: V4vCaptureAdmission.Prepared? = null
    private var preparedCapture: CaptureStartAdmission.Preparation? = null
    private var preparedV4vCapture: CaptureStartAdmission.Preparation? = null
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
    private var recordLocally by mutableStateOf(true)
    private var captureProfile by mutableStateOf(CaptureProfile.DEFAULT)
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
            ui.postDelayed(this, 200)
        }
    }
    private val permissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        if (result.values.all { it }) {
            permissionMessage = null
            prepareCapture()
        } else {
            permissionMessage = "Camera and microphone permission are required. Enable them in system settings if permission was permanently denied."
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        capture = CaptureController(applicationContext)
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
            }
        }
    }

    @Composable
    private fun PreviewPane(modifier: Modifier) {
        Box(modifier, contentAlignment = Alignment.Center) {
            AndroidView(factory = { context -> SurfaceView(context).apply {
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
                        Text(permissionMessage ?: current?.message ?: "Prepare the camera and microphone")
                        Text(current?.orientation ?: "${captureProfile.label}30 · H264 · AAC 48 kHz", style = MaterialTheme.typography.bodySmall)
                        val qualityConfigurable = current?.canPrepare != false && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR)
                        Text("Capture quality · choose before Prepare", style = MaterialTheme.typography.labelMedium)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            CaptureProfile.entries.forEach { quality ->
                                OutlinedButton(enabled = qualityConfigurable, onClick = {
                                    if (capture.snapshot().canPrepare && captureProfile != quality) {
                                        prepareRequest++; preparedV4v = null; v4v?.stopPublisher()
                                        captureProfile = quality
                                    }
                                }) { Text(if (captureProfile == quality) "${quality.label} selected" else quality.label) }
                            }
                        }
                        OutlinedButton(
                            onClick = { prepareWithPermissions() },
                            enabled = surfaceReady && current?.canPrepare != false && current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR),
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Prepare") }
                        Button(
                            onClick = { startWithAuth() },
                            enabled = current?.stage == CaptureStage.PREPARED,
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Start capture") }
                        Button(
                            onClick = {
                                ui.removeCallbacks(settlePose)
                                poseSelector.stopCapture()
                                prepareRequest++
                                v4v?.stopPublisher()
                                capture.stop()
                                schedulePoseSettlement()
                                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                            },
                            enabled = current?.stage in listOf(CaptureStage.PREPARING, CaptureStage.PREPARED, CaptureStage.CAPTURING, CaptureStage.STOPPING),
                            colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF833D38)),
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Stop") }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = recordLocally, onCheckedChange = { recordLocally = it },
                                enabled = current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR))
                            Text("Record private MP4 on this device", style = MaterialTheme.typography.bodySmall)
                        }
                        Text("Microphone", style = MaterialTheme.typography.labelMedium)
                        LinearProgressIndicator(progress = { current?.audioLevel ?: 0f }, modifier = Modifier.fillMaxWidth())
                        Text("Video ${current?.videoSamples ?: 0} · Audio ${current?.audioSamples ?: 0} · Ingress rejects ${current?.drops ?: 0}", style = MaterialTheme.typography.bodySmall)
                        Text("Video timing/IDR ${current?.videoRejects ?: 0} · AAC priming ${current?.audioPrimingRejects ?: 0} · Wait IDR ${current?.waitingKeyframeRejects ?: 0} · Other ingress rejects ${current?.queueRejects ?: 0}", style = MaterialTheme.typography.bodySmall)
                        Text(current?.queueDiagnostics ?: "Queue diagnostics not sampled", style = MaterialTheme.typography.bodySmall)
                        Text(current?.cleanupDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text(current?.audioDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text(current?.renderer ?: "GPU not prepared", style = MaterialTheme.typography.bodySmall)
                        Text(current?.sinkDiagnostics ?: "", style = MaterialTheme.typography.bodySmall)
                        Text("Encoded video capture age ${current?.videoCaptureAgeMs?.let { "${it}ms" } ?: "Not measured"}", style = MaterialTheme.typography.bodySmall)
                        Text(current?.publishingStatus ?: "No publisher attached", style = MaterialTheme.typography.bodySmall)
                        Text(current?.recordingStatus ?: "No recorder attached", style = MaterialTheme.typography.bodySmall)
                        val configurable = current?.stage in listOf(null, CaptureStage.IDLE, CaptureStage.ERROR)
                        V4vControls(configurable)
                        Text("Manual destination · bounded reconnect", style = MaterialTheme.typography.labelMedium)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = useSrt, onCheckedChange = { useSrt = it }, enabled = configurable)
                            Text("Use SRT over UDP", style = MaterialTheme.typography.bodySmall)
                        }
                        OutlinedTextField(
                            value = destinationUrl, onValueChange = { destinationUrl = it.take(4096) },
                            label = { Text(if (useSrt) "SRT numeric address and port" else "RTMPS application URL") },
                            placeholder = { Text(if (useSrt) "srt://192.0.2.1:8890" else "rtmps://origin.invalid/live") },
                            enabled = configurable, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        OutlinedTextField(
                            value = destinationKey, onValueChange = { destinationKey = it.take(4096) },
                            label = { Text(if (useSrt) "SRT stream ID (optional)" else "Stream key") }, visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                            enabled = configurable, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        if (useSrt) {
                            OutlinedTextField(
                                value = srtPassphrase, onValueChange = { srtPassphrase = it.take(79) },
                                label = { Text("AES-256 passphrase · 10 to 79 bytes") },
                                visualTransformation = PasswordVisualTransformation(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                                enabled = configurable, singleLine = true, modifier = Modifier.fillMaxWidth(),
                            )
                            Row {
                                Checkbox(checked = srtPlaintextLab, onCheckedChange = { srtPlaintextLab = it }, enabled = configurable)
                                Text("Plaintext SRT lab only · clear passphrase", style = MaterialTheme.typography.bodySmall)
                            }
                        } else {
                            Row {
                                Checkbox(checked = allowPlaintextDevelopment, onCheckedChange = { allowPlaintextDevelopment = it }, enabled = configurable)
                                Text("Allow plaintext RTMP for development", style = MaterialTheme.typography.bodySmall)
                            }
                        }
                        Text("Leave blank for SDK validation only. No transport downgrade.", style = MaterialTheme.typography.bodySmall)
                        Text("Video: ${current?.videoEncoder ?: "Not selected"}", style = MaterialTheme.typography.bodySmall)
                        Text("Audio: ${current?.audioEncoder ?: "Not selected"}", style = MaterialTheme.typography.bodySmall)
                        Text("Capture stops when this screen leaves the foreground.", style = MaterialTheme.typography.bodySmall)
        }
    }


    private fun startWithAuth() {
        val captureIntent = preparedCapture ?: return
        fun start() { capture.start(captureIntent); window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) }
        if (!useV4v) start()
        else {
            val request = prepareRequest
            val binding = preparedV4v ?: return
            v4v?.grant(captureProfile) { permit, error ->
                if (request != prepareRequest || !useV4v || preparedV4v != binding || preparedCapture != captureIntent) return@grant
                if (error == null && permit != null && v4v?.canStart(binding, permit, captureProfile) == true && poseTracking && capture.snapshot().stage == CaptureStage.PREPARED) start()
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
            Checkbox(useV4v, { prepareRequest++; useV4v = it }, enabled = configurable)
            Text("Use approved V4V connector", style = MaterialTheme.typography.bodySmall)
        }
        if (!useV4v) return
        Text(v4vState?.message ?: "Loading V4V journal", style = MaterialTheme.typography.bodySmall)
        v4vState?.maxPublishBps?.let { cap ->
            Text("V4V cap $cap bits/s · ${captureProfile.label} needs ${captureProfile.minimumPublishBps} bits/s", style = MaterialTheme.typography.bodySmall)
            if (cap < captureProfile.minimumPublishBps) Text("Select a supported quality before Prepare. The reserved session can still be ended.", style = MaterialTheme.typography.bodySmall)
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
        OutlinedButton(enabled = v4vState?.eventId != null, onClick = { prepareRequest++; capture.cancelPendingPrepare(preparedV4vCapture); owner.stopPublisher(); owner.control.endDestination { error -> ui.post { if (error != null) permissionMessage = "V4V End intent not confirmed"; v4vState = owner.control.view } } }) { Text("End V4V destination") }
        OutlinedButton(enabled = v4vState?.eventId != null, onClick = { v4vCommand { status() } }) { Text("Check origin finalisation") }
        if (v4vState?.recoveryOffered == true) {
            Row { Checkbox(acknowledgeLoss, { acknowledgeLoss = it }); Text("Abandon failed automatic recording", style = MaterialTheme.typography.bodySmall) }
            OutlinedButton(enabled = acknowledgeLoss && v4vState?.pending != true, onClick = { v4vCommand { acknowledge() }; acknowledgeLoss = false }) { Text("Acknowledge recording failure") }
        }
        if (v4vState?.recoveryState == "acknowledged") OutlinedButton(enabled = configurable && v4vEvent.isNotBlank(), onClick = { val selected = v4vEvent; v4vCommand { prepare(selected, true) } }) { Text("Explicitly prepare replacement") }
        Text("Playback has not been verified", style = MaterialTheme.typography.bodySmall)
    }

    private fun prepareWithPermissions() {
        val needed = arrayOf(Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO)
        if (needed.all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }) {
            permissionMessage = null
            prepareCapture()
        } else permissions.launch(needed)
    }

    private fun prepareCapture(approvedGrant: V4vCapturePermit? = null) {
        if (!capture.snapshot().canPrepare) return
        if (useV4v && approvedGrant == null) {
            val owner = v4v ?: return
            val request = ++prepareRequest
            val requestedProfile = captureProfile
            owner.grant(requestedProfile) { grant, error ->
                if (request != prepareRequest || !useV4v || captureProfile != requestedProfile) return@grant
                if (error != null) permissionMessage = error.message?.take(160) ?: "V4V grant unavailable"
                else if (poseTracking && capture.snapshot().canPrepare) prepareCapture(checkNotNull(grant))
            }
            return
        }
        try {
            val surface = preview?.takeIf { it.isValid } ?: return
            val key = destinationKey
            val passphrase = srtPassphrase
            val plaintext = srtPlaintextLab
            val publisher: () -> EncodedSink? = if (useV4v) {
                require(approvedGrant?.profile == captureProfile) { "Capture quality changed; Prepare again" }
                val prepared = checkNotNull(v4v).captureFactory(checkNotNull(approvedGrant))
                preparedV4v = prepared.binding
                prepared.factory
            } else if (destinationUrl.isBlank()) ({ null }) else if (useSrt) {
                SrtDestination.validateCredentials(key, passphrase, plaintext)
                val destination = SrtDestination.parse(destinationUrl.trim())
                val coordinator = ReconnectingPublishingSink { events -> SrtPublishingSink(destination, key, passphrase, plaintext, events) }
                ({ coordinator.newEpoch() })
            } else {
                require(key.isNotEmpty()) { "A stream key is required" }
                val destination = RtmpDestination.parse(destinationUrl.trim(), allowPlaintextDevelopment)
                val coordinator = ReconnectingPublishingSink { events -> RtmpPublishingSink(destination, key, events) }
                ({ coordinator.newEpoch() })
            }
            val directory = filesDir.toPath().resolve("recordings")
            val recorder: () -> EncodedSink? = if (recordLocally) ({ PrivateRecordingSink(directory) }) else ({ null })
            updatePreview()
            // Explicit Prepare restores the stable physical pose even if no
            // further sensor callback followed qualification Stop/CLEAR.
            capture.orientationChanged(poseSelector.prepareSession())
            preparedCapture = capture.prepare(surface, publisher, recorder, captureProfile)
            if (useV4v) preparedV4vCapture = preparedCapture
        } catch (error: Exception) {
            permissionMessage = error.message?.take(240) ?: "Destination configuration failed"
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) { super.onConfigurationChanged(newConfig); updatePreview() }
    override fun onStart() {
        super.onStart()
        poseSelector = PoseInputPolicy(displayPose(), qualificationPoseControlEnabled)
        capture.orientationChanged(poseSelector.pose)
        poseTracking = true
        getSystemService(DisplayManager::class.java).registerDisplayListener(displayChanges, ui)
        updatePreview()
        if (orientation.canDetectOrientation()) orientation.enable()
        ui.post(refresh)
    }
    override fun onStop() {
        poseTracking = false
        prepareRequest++
        enableQualificationPoseControl(false)
        orientation.disable()
        ui.removeCallbacks(settlePose); poseSelector.cancelPending()
        getSystemService(DisplayManager::class.java).unregisterDisplayListener(displayChanges)
        ui.removeCallbacks(refresh)
        v4v?.stopPublisher()
        capture.stop("Capture interrupted: the app left the foreground. Prepare again to resume.")
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        super.onStop()
    }
    override fun onDestroy() { v4v?.signerRelay?.detach(launchSigner); enableQualificationPoseControl(false); capture.close(); super.onDestroy() }
}
