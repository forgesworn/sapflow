package dev.forgesworn.sapflow.broadcaster

import android.app.Activity
import android.os.Build
import android.os.Bundle
import android.widget.TextView
import org.json.JSONObject
import java.io.File
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Explicit generated-only emulator fixture, isolated from release/default UI.
 * An interrupted/uncertain owner remains retained and blocks another run. */
class NativeAacQualificationActivity : Activity() {
    private val cancelled = AtomicBoolean(false)
    private var worker: Thread? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val view = TextView(this).apply { text = "Generated native AAC probe is dormant" }
        setContentView(view)
        val runId = intent.getStringExtra("runId")
        val drainMode = intent.action == DRAIN_ACTION
        val leadingMode = intent.action == LEADING_ACTION
        val referenceMode = intent.action == WINDOW_REFERENCE_ACTION
        val sdkWindowMode = intent.action == SDK_WINDOW_ACTION
        val explicitReaderMode = intent.action == EXPLICIT_READER_ACTION
        val savedInputMode = referenceMode || sdkWindowMode || explicitReaderMode
        val inputId = intent.getStringExtra("inputId")
        val inputSha = intent.getStringExtra("inputManifestSHA256")
        if (intent.action !in setOf(ACTION, DRAIN_ACTION, LEADING_ACTION, WINDOW_REFERENCE_ACTION, SDK_WINDOW_ACTION, EXPLICIT_READER_ACTION) || intent.data != null || intent.clipData != null ||
            intent.extras?.keySet() != (if (savedInputMode) setOf("runId", "generatedOnly", "inputId", "inputManifestSHA256") else setOf("runId", "generatedOnly")) ||
            (savedInputMode && (inputId == null || !inputId.matches(Regex("[a-f0-9]{32}")) || inputSha == null || !inputSha.matches(Regex("[a-f0-9]{64}")))) ||
            !intent.getBooleanExtra("generatedOnly", false) || runId == null ||
            !runId.matches(Regex("[a-f0-9]{32}")) || Build.HARDWARE !in setOf("ranchu", "goldfish")) {
            view.text = "Explicit generated-only Android Emulator invocation required"
            return
        }
        if (active.get() != null) {
            view.text = "Previous native owner is retained or still running"
            return
        }
        val parent = File(filesDir, if (explicitReaderMode) "native-aac-explicit-reader" else if (sdkWindowMode) "native-aac-sdk-window" else if (referenceMode) "native-aac-window-reference" else if (leadingMode) "native-aac-leading" else if (drainMode) "native-aac-drain" else "native-aac-qualification")
        if (!parent.exists()) check(parent.mkdir())
        check(parent.isDirectory && !Files.isSymbolicLink(parent.toPath()) && parent.canonicalFile.parentFile == filesDir.canonicalFile)
        val directory = File(parent, runId)
        if (!directory.mkdir()) {
            view.text = "Preserve an earlier run; use a fresh runId"
            return
        }
        val probe: Any = if (savedInputMode) {
            val inputs = File(filesDir, "native-aac-window-inputs")
            check(inputs.isDirectory && !Files.isSymbolicLink(inputs.toPath()) && inputs.canonicalFile.parentFile == filesDir.canonicalFile)
            val source = File(inputs, checkNotNull(inputId))
            check(source.isDirectory && !Files.isSymbolicLink(source.toPath()) && source.canonicalFile.parentFile == inputs.canonicalFile)
            NativeAacProgrammeWindowProbe(directory, source, checkNotNull(inputSha), cancelled,
                if (explicitReaderMode) NativeAacProgrammeWindowProbe.Mode.EXPLICIT_READER else if (sdkWindowMode) NativeAacProgrammeWindowProbe.Mode.SDK_WINDOW else NativeAacProgrammeWindowProbe.Mode.REFERENCE)
        } else if (leadingMode) NativeAacLeadingProbe(directory, cancelled)
            else NativeAacQualificationProbe(directory, cancelled, if (drainMode) NativeAacQualificationProbe.Mode.DRAIN_EXPERIMENTS else NativeAacQualificationProbe.Mode.ORIGINAL)
        if (!active.compareAndSet(null, probe)) {
            check(directory.delete()) // Only this newly created, still empty directory.
            view.text = "Previous native owner is retained or still running"
            return
        }
        view.text = "Running generated native AAC observations"
        worker = Thread({
            var confirmed = false
            try {
                val result = when (probe) {
                    is NativeAacProgrammeWindowProbe -> probe.run()
                    is NativeAacLeadingProbe -> probe.run()
                    is NativeAacQualificationProbe -> probe.run()
                    else -> error("Unsupported generated probe owner")
                }.put("runId", runId)
                confirmed = result.getBoolean("cleanupConfirmed")
                File(directory, "receipt.json").writeText(result.toString(2) + "\n")
                runOnUiThread { if (!isDestroyed) view.text = result.toString(2) }
            } catch (error: Exception) {
                val result = JSONObject().put("generatedOnly", true).put("runId", runId)
                    .put("cleanupConfirmed", false).put("failure", error.javaClass.simpleName + ": " + error.message?.take(512))
                    .put("nativeAacQualified", false)
                runCatching { File(directory, "receipt.json").writeText(result.toString(2) + "\n") }
                runOnUiThread { if (!isDestroyed) view.text = result.toString(2) }
            } finally {
                if (confirmed) active.compareAndSet(probe, null)
                // No automatic deletion: root extracts named generated evidence,
                // then removes only this run after confirming process cleanup.
            }
        }, "sapflow-aac-qualification").apply { start() }
    }

    override fun onDestroy() {
        cancelled.set(true); worker?.interrupt()
        super.onDestroy()
    }

    companion object {
        const val ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC"
        const val DRAIN_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_DRAIN"
        const val EXPLICIT_READER_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_EXPLICIT_READER"
        const val SDK_WINDOW_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_SDK_WINDOW"
        const val WINDOW_REFERENCE_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_WINDOW_REFERENCE"
        const val LEADING_ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_LEADING"
        private val active = AtomicReference<Any?>()
    }
}
