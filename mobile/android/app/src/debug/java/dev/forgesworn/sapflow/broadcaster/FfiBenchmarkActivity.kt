package dev.forgesworn.sapflow.broadcaster

import android.app.Activity
import android.os.Bundle
import android.os.SystemClock
import android.widget.ScrollView
import android.widget.TextView
import dev.forgesworn.sapflow.MediaCodec
import dev.forgesworn.sapflow.NativeRtmpPublisher
import dev.forgesworn.sapflow.NativeRtmpState
import dev.forgesworn.sapflow.NativeSampleFanout
import org.json.JSONArray
import org.json.JSONObject
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.ceil

/** Explicit debug-only, offline microbenchmark of the installed real bindings.
 * No camera, microphone, sockets, app storage or media recordings are accessed.
 * Opaque AAC bytes exercise SDK ownership/validation, not decoding or quality. */
class FfiBenchmarkActivity : Activity() {
    private val cancelled = AtomicBoolean(false)
    private var worker: Thread? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val output = TextView(this).apply { text = "Measuring installed native bindings…"; textSize = 12f }
        setContentView(ScrollView(this).apply { addView(output) })
        worker = Thread({
            val result = JSONObject()
                .put("scope", "installed Android bindings; synthetic offline microbenchmark")
                .put("package", packageName)
                .put("media_decoded", false)
                .put("comparative_performance", false)
                .put("results", JSONArray())
            val rows = result.getJSONArray("results")
            val deadline = System.nanoTime() + 12_000_000_000L
            fun active() { check(!cancelled.get() && System.nanoTime() < deadline) { "Benchmark cancelled or deadline reached" } }
            fun measure(label: String, work: () -> Unit) {
                repeat(32) { active(); work() }
                val elapsed = LongArray(200)
                repeat(elapsed.size) { index ->
                    active()
                    val started = System.nanoTime()
                    work()
                    elapsed[index] = System.nanoTime() - started
                }
                elapsed.sort()
                rows.put(JSONObject().put("operation", label).put("iterations", elapsed.size)
                    .put("mean_us", elapsed.sumOf { it.toDouble() } / elapsed.size / 1000)
                    .put("p50_us", elapsed[elapsed.size / 2] / 1000.0)
                    .put("p95_us", elapsed[ceil(elapsed.size * 0.95).toInt() - 1] / 1000.0)
                    .put("max_us", elapsed.last() / 1000.0))
            }
            try {
                val configuration = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).apply { flip() }
                val payload = ByteBuffer.allocateDirect(256).apply { put(ByteArray(256) { 1 }); flip() }
                MediaCodec.aac(configuration).use { codec ->
                    codec.sample(payload, 0, 0, 1024u, 48_000u, false).use { sample ->
                        measure("one native sample length getter") { check(sample.len() == 256uL) }
                        measure("four native sample metadata getters") {
                            check(sample.len() == 256uL && !sample.isVideo() && !sample.isKeyframe() && sample.timescale() == 48_000u)
                        }
                    }
                    var ticks = 0L
                    measure("create and release one compressed sample") {
                        codec.sample(payload, ticks, ticks, 1024u, 48_000u, false).close()
                        ticks += 1024
                    }
                    NativeSampleFanout(4uL * 1024uL * 1024uL, 8uL * 1024uL * 1024uL, 2000u, false).use { queues ->
                        measure("create, fan out, pop both handles and release") {
                            codec.sample(payload, ticks, ticks, 1024u, 48_000u, false).use { sample ->
                                val state = queues.push(sample, SystemClock.elapsedRealtimeNanos().toULong())
                                check(state.publishingQueued && state.recordingQueued)
                                checkNotNull(queues.popPublishing(SystemClock.elapsedRealtimeNanos().toULong())).close()
                                checkNotNull(queues.popRecording(SystemClock.elapsedRealtimeNanos().toULong())).close()
                            }
                            ticks += 1024
                        }
                        queues.cancel()
                    }
                    readyPublisher().use { publisher ->
                        var frame = 0L
                        measure("legacy RTMP sample/tick/send/output/queue/deadline/type") {
                            codec.sample(payload, frame, frame, 1024u, 48_000u, false).use { sample ->
                                publisher.tick(6uL)
                                publisher.send(sample)
                                check(publisher.takeOutput().isNotEmpty())
                                check(publisher.queuedOutputBytes() == 0uL)
                                check(publisher.nextDeadlineMs() == null)
                                check(!sample.isVideo())
                            }
                            frame += 1024
                        }
                        publisher.cancel()
                    }
                    readyPublisher().use { publisher ->
                        var frame = 0L
                        measure("fused RTMP sample/send-and-output-snapshot") {
                            codec.sample(payload, frame, frame, 1024u, 48_000u, false).use { sample ->
                                val sent = publisher.sendAndTakeOutput(sample, 6uL)
                                check(sent.output.isNotEmpty() && sent.queuedOutputBytes == 0uL)
                                check(sent.nextDeadlineMs == null && sent.state == NativeRtmpState.PUBLISHING)
                            }
                            frame += 1024
                        }
                        publisher.cancel()
                    }
                }
                result.put("completed", true)
            } catch (error: Exception) {
                result.put("completed", false).put("failure", error.javaClass.simpleName + ": " + error.message?.take(160))
            }
            runOnUiThread { if (!isFinishing && !isDestroyed) output.text = result.toString(2) }
        }, "sapflow-ffi-qualification").apply { start() }
    }

    override fun onDestroy() {
        cancelled.set(true)
        worker?.interrupt()
        super.onDestroy()
    }

    /** Same bounded in-memory peer vectors as the real SDK consumer check.
     * The .invalid destination is never resolved and no socket is opened. */
    private fun readyPublisher(): NativeRtmpPublisher {
        val publisher = NativeRtmpPublisher("live", "synthetic", "rtmp://origin.invalid/live", 0uL, 0uL)
        try {
            val c0c1 = publisher.takeOutput()
            check(c0c1.size == 1537)
            publisher.feed(ByteBuffer.allocateDirect(3073).put(3.toByte()).put(ByteArray(1536))
                .put(c0c1, 1, 1536).apply { flip() }, 2uL)
            publisher.takeOutput()
            listOf(
                "AwAAAAAAPhQAAAAAAgAHX3Jlc3VsdAA/8AAAAAAAAAUDAARjb2RlAgAdTmV0Q29ubmVjdGlvbi5Db25uZWN0LlN1Y2Nlc3MAAAk=",
                "AwAAAAAAHRQAAAAAAgAHX3Jlc3VsdABAAAAAAAAAAAUAP/AAAAAAAAA=",
                "AwAAAAAAORQBAAAAAgAIb25TdGF0dXMAAAAAAAAAAAAFAwAEY29kZQIAF05ldFN0cmVhbS5QdWJsaXNoLlN0YXJ0AAAJ",
            ).forEachIndexed { index, reply ->
                val bytes = Base64.getDecoder().decode(reply)
                publisher.feed(ByteBuffer.allocateDirect(bytes.size).put(bytes).apply { flip() }, (index + 3).toULong())
                publisher.takeOutput()
            }
            check(publisher.state() == NativeRtmpState.PUBLISHING)
            return publisher
        } catch (error: Exception) { publisher.close(); throw error }
    }
}
