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
        if (intent.action != ACTION || intent.data != null || intent.clipData != null ||
            intent.extras?.keySet() != setOf("runId", "generatedOnly") ||
            !intent.getBooleanExtra("generatedOnly", false) || runId == null ||
            !runId.matches(Regex("[a-f0-9]{32}")) || Build.HARDWARE !in setOf("ranchu", "goldfish")) {
            view.text = "Explicit generated-only Android Emulator invocation required"
            return
        }
        if (active.get() != null) {
            view.text = "Previous native owner is retained or still running"
            return
        }
        val parent = File(filesDir, "native-aac-qualification")
        if (!parent.exists()) check(parent.mkdir())
        check(parent.isDirectory && !Files.isSymbolicLink(parent.toPath()) && parent.canonicalFile.parentFile == filesDir.canonicalFile)
        val directory = File(parent, runId)
        if (!directory.mkdir()) {
            view.text = "Preserve an earlier run; use a fresh runId"
            return
        }
        val probe = NativeAacQualificationProbe(directory, cancelled)
        if (!active.compareAndSet(null, probe)) {
            check(directory.delete()) // Only this newly created, still empty directory.
            view.text = "Previous native owner is retained or still running"
            return
        }
        view.text = "Running generated native AAC observations"
        worker = Thread({
            var confirmed = false
            try {
                val result = probe.run().put("runId", runId)
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
        private val active = AtomicReference<NativeAacQualificationProbe?>()
    }
}
