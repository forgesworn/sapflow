package dev.forgesworn.sapflow.broadcaster

import android.app.Activity
import android.os.Build
import android.os.Bundle
import android.widget.TextView
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.nio.file.Files

/** Explicit generated-only emulator fixture, isolated from release/default UI.
 * An interrupted/uncertain owner remains retained and blocks another run. */
class NativeAacQualificationActivity : Activity() {
    private var runOwner: NativeAacQualificationRunOwner? = null
    private var handedOff: NativeAacQualificationRunOwner? = null
    private var statusView: TextView? = null
    private var statusUpdate: Runnable? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val view = TextView(this).apply { text = "Generated native AAC probe is dormant" }
        setContentView(view)
        val retained = lastNonConfigurationInstance as? NativeAacQualificationRunOwner
        val action = intent.action
        val runId = intent.getStringExtra("runId")
        val drainMode = action == DRAIN_ACTION
        val leadingMode = action == LEADING_ACTION
        val referenceMode = action == WINDOW_REFERENCE_ACTION
        val sdkWindowMode = action == SDK_WINDOW_ACTION
        val explicitReaderMode = action == EXPLICIT_READER_ACTION
        val nativePairMode = action == NATIVE_PAIR_ACTION
        val savedInputMode = referenceMode || sdkWindowMode || explicitReaderMode || nativePairMode
        val inputId = intent.getStringExtra("inputId")
        val inputSha = intent.getStringExtra("inputManifestSHA256")
        if (action !in setOf(ACTION, DRAIN_ACTION, LEADING_ACTION, WINDOW_REFERENCE_ACTION, SDK_WINDOW_ACTION, EXPLICIT_READER_ACTION, NATIVE_PAIR_ACTION) || intent.data != null || intent.clipData != null ||
            intent.extras?.keySet() != (if (savedInputMode) setOf("runId", "generatedOnly", "inputId", "inputManifestSHA256") else setOf("runId", "generatedOnly")) ||
            (savedInputMode && (inputId == null || !inputId.matches(Regex("[a-f0-9]{32}")) || inputSha == null || !inputSha.matches(Regex("[a-f0-9]{64}")))) ||
            !intent.getBooleanExtra("generatedOnly", false) || runId == null ||
            !runId.matches(Regex("[a-f0-9]{32}")) || Build.HARDWARE !in setOf("ranchu", "goldfish")) {
            retained?.let { slot.cancelRetained(it); lifecycle("INVOCATION_REFUSED", it) }
            view.text = "Explicit generated-only Android Emulator invocation required"
            return
        }
        val invocation = NativeAacQualificationRunOwner.Invocation(checkNotNull(action), runId, inputId, inputSha)
        if (retained != null) {
            val exact = slot.reattach(retained, invocation)
            if (exact == null) {
                lifecycle("REATTACH_REFUSED", retained)
                view.text = "Retained invocation changed or was cancelled; original owner stays fenced"
                return
            }
            runOwner = exact
            lifecycle("REATTACHED", exact)
            showStatus(view, exact)
            return // No directory creation, checkpoint decode or second worker.
        }
        if (slot.hasOwner()) {
            view.text = "Previous native owner is retained or still running"
            return
        }
        val parent = File(filesDir, if (nativePairMode) "native-aac-native-pair" else if (explicitReaderMode) "native-aac-explicit-reader" else if (sdkWindowMode) "native-aac-sdk-window" else if (referenceMode) "native-aac-window-reference" else if (leadingMode) "native-aac-leading" else if (drainMode) "native-aac-drain" else "native-aac-qualification")
        if (!parent.exists()) check(parent.mkdir())
        check(parent.isDirectory && !Files.isSymbolicLink(parent.toPath()) && parent.canonicalFile.parentFile == filesDir.canonicalFile)
        val directory = File(parent, runId)
        if (!directory.mkdir()) {
            view.text = "Preserve an earlier run; use a fresh runId"
            return
        }
        val exact = slot.acquire(invocation)
        if (exact == null) {
            check(directory.delete()) // Only this newly created, still empty directory.
            view.text = "Previous native owner is retained or still running"
            return
        }
        runOwner = exact
        val cancelled = exact.cancelled
        val probe: Any = if (savedInputMode) {
            val inputs = File(filesDir, "native-aac-window-inputs")
            check(inputs.isDirectory && !Files.isSymbolicLink(inputs.toPath()) && inputs.canonicalFile.parentFile == filesDir.canonicalFile)
            val source = File(inputs, checkNotNull(inputId))
            check(source.isDirectory && !Files.isSymbolicLink(source.toPath()) && source.canonicalFile.parentFile == inputs.canonicalFile)
            NativeAacProgrammeWindowProbe(directory, source, checkNotNull(inputSha), cancelled,
                if (nativePairMode) NativeAacProgrammeWindowProbe.Mode.NATIVE_PAIR else if (explicitReaderMode) NativeAacProgrammeWindowProbe.Mode.EXPLICIT_READER else if (sdkWindowMode) NativeAacProgrammeWindowProbe.Mode.SDK_WINDOW else NativeAacProgrammeWindowProbe.Mode.REFERENCE)
        } else if (leadingMode) NativeAacLeadingProbe(directory, cancelled)
            else NativeAacQualificationProbe(directory, cancelled, if (drainMode) NativeAacQualificationProbe.Mode.DRAIN_EXPERIMENTS else NativeAacQualificationProbe.Mode.ORIGINAL)
        showStatus(view, exact)
        lifecycle("STARTING_WORKER", exact)
        slot.start(exact, probe) { Companion.execute(probe, exact, directory) }
    }

    private fun showStatus(view: TextView, owner: NativeAacQualificationRunOwner) {
        statusView = view
        val update = object : Runnable {
            override fun run() {
                if (isDestroyed || runOwner !== owner) return
                view.text = owner.status.get()
                if (!owner.finished) view.postDelayed(this, 250)
            }
        }
        statusUpdate = update
        view.post(update)
    }

    override fun onRetainNonConfigurationInstance(): Any? {
        val owner = runOwner ?: return null
        if (owner.cancelled.get()) return null
        handedOff = owner // Set only by the actual framework callback.
        lifecycle("CONFIGURATION_HANDOFF", owner)
        return owner
    }

    override fun onDestroy() {
        statusUpdate?.let { statusView?.removeCallbacks(it) }
        statusUpdate = null; statusView = null
        runOwner?.let { owner ->
            val transferred = handedOff === owner
            owner.destroyed(isChangingConfigurations, transferred)
            lifecycle(if (isChangingConfigurations && transferred) "CONFIGURATION_DETACH" else "DESTROY_CANCEL", owner)
        }
        runOwner = null; handedOff = null
        super.onDestroy()
    }

    companion object {
        const val ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC"
        const val DRAIN_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_DRAIN"
        const val NATIVE_PAIR_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_NATIVE_PAIR"
        const val EXPLICIT_READER_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_EXPLICIT_READER"
        const val SDK_WINDOW_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_SDK_WINDOW"
        const val WINDOW_REFERENCE_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_WINDOW_REFERENCE"
        const val LEADING_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_LEADING"
        private val slot = NativeAacQualificationRunOwner.Slot()
        private fun lifecycle(event: String, owner: NativeAacQualificationRunOwner) {
            Log.i("SapflowAacLifecycle", "event=$event runId=${owner.invocation.runId} owner=${System.identityHashCode(owner)} finished=${owner.finished} cancelled=${owner.cancelled.get()}")
        }
        /** Static execution has no reference to the creating Activity/view. */
        private fun execute(probe: Any, owner: NativeAacQualificationRunOwner, directory: File): Boolean {
            var confirmed = false
            lifecycle("WORKER_ENTERED", owner)
            try {
                val result = when (probe) {
                    is NativeAacProgrammeWindowProbe -> probe.run()
                    is NativeAacLeadingProbe -> probe.run()
                    is NativeAacQualificationProbe -> probe.run()
                    else -> error("Unsupported generated probe owner")
                }.put("runId", owner.invocation.runId)
                confirmed = result.getBoolean("cleanupConfirmed")
                File(directory, "receipt.json").writeText(result.toString(2) + "\n")
                owner.status.set(result.toString(2))
            } catch (error: Throwable) {
                confirmed = false // An unconfirmed receipt/owner boundary stays retained.
                val result = JSONObject().put("generatedOnly", true).put("runId", owner.invocation.runId)
                    .put("cleanupConfirmed", false).put("failure", error.javaClass.simpleName + ": " + error.message?.take(512))
                    .put("nativeAacQualified", false)
                runCatching { File(directory, "receipt.json").writeText(result.toString(2) + "\n") }
                owner.status.set(result.toString(2))
            } finally {
                lifecycle("PROBE_RETURNING_cleanupConfirmed=$confirmed", owner)
                // No evidence deletion. Unknown native cleanup retains probe
                // and slot; genuine cancellation is never numerical success.
            }
            return confirmed
        }
    }
}
