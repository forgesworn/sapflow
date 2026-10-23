package dev.forgesworn.sapflow.broadcaster

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean

/** Qualification-only replay of retained generated AAC. No encoder, capture,
 * Rust muxer, inferred native trim, PCM crop or completion capability exists here.
 * The Activity retains this concrete owner if any native/file close is unknown. */
internal class NativeAacProgrammeWindowProbe(private val root: File, private val input: File,
    private val manifestSha256: String, private val cancelled: AtomicBoolean) {
    private val retained = mutableListOf<Any>()
    private var cleanupKnown = true
    private val journal = JSONArray()
    private val deadlineNs = System.nanoTime() + 120_000_000_000L
    private fun active() { check(!cancelled.get() && System.nanoTime() < deadlineNs) { "Replay cancelled or deadline reached" } }
    private fun sha(bytes: ByteArray): String {
        val chars = "0123456789abcdef"
        return buildString { for (b in MessageDigest.getInstance("SHA-256").digest(bytes)) {
            val v = b.toInt() and 255; append(chars[v ushr 4]); append(chars[v and 15])
        } }
    }
    private fun read(base: File, name: String, max: Int): ByteArray {
        check(name.matches(Regex("[a-zA-Z0-9_.-]+")))
        check(!Files.isSymbolicLink(base.toPath()) && base.isDirectory)
        val file = File(base, name)
        check(!Files.isSymbolicLink(file.toPath()) && file.isFile && file.length() in 0..max.toLong())
        check(file.canonicalFile.parentFile == base.canonicalFile)
        return file.readBytes().also { check(it.size <= max) }
    }
    private fun event(name: String, returned: Boolean, error: Exception? = null) {
        check(journal.length() < 32768)
        journal.put(JSONObject().put("operation", name).put("returned", returned)
            .put("error", error?.let { it.javaClass.simpleName + ": " + it.message?.take(256) } ?: JSONObject.NULL))
    }
    private fun <T> call(name: String, block: () -> T): T {
        active()
        try { return block().also { event(name, true) } }
        catch (error: Exception) { event(name, false, error); throw error }
    }
    // Cleanup itself is bounded by the parent's owned-VM timeout. A throwing
    // native close is recorded once and never retried through another close API.
    private fun close(name: String, block: () -> Unit): Boolean = try { block(); event(name, true); true }
        catch (error: Exception) { cleanupKnown = false; event(name, false, error); false }
    private fun format(phase: String, value: MediaFormat): JSONObject {
        val result = JSONObject().put("phase", phase).put("reported", value.toString().take(4096))
        for (key in listOf("sample-rate", "channel-count", "aac-profile", "pcm-encoding", "encoder-delay", "encoder-padding")) {
            val state = JSONObject()
            try { if (!value.containsKey(key)) state.put("state", "ABSENT") else
                state.put("state", "PRESENT_INT").put("value", value.getInteger(key)) }
            catch (error: Exception) { state.put("state", "READ_FAILED").put("reason", error.javaClass.simpleName) }
            result.put(key, state)
        }
        val csd = value.getByteBuffer("csd-0")?.duplicate()
        if (csd == null) result.put("csd-0", JSONObject().put("state", "ABSENT"))
        else {
            check(csd.remaining() <= 1024)
            val bytes = ByteArray(csd.remaining()); csd.get(bytes)
            result.put("csd-0", JSONObject().put("state", "PRESENT_BYTES").put("sha256", sha(bytes)))
        }
        return result
    }
    private fun newFormat(source: JSONObject, head: Int?, tail: Int?): MediaFormat {
        check(source.getString("ascHex") == "1188")
        val result = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, 48000, 1)
        result.setByteBuffer("csd-0", ByteBuffer.wrap(byteArrayOf(0x11, 0x88.toByte())))
        result.setInteger(MediaFormat.KEY_AAC_PROFILE, 2)
        // Native optional-field presence is reproduced, never filled with zero.
        for (key in listOf("encoder-delay", "encoder-padding")) {
            val field = source.getJSONObject("format").getJSONObject(key)
            when (field.getString("state")) {
                "ABSENT" -> Unit
                "PRESENT_INT" -> result.setInteger(key, field.getInt("value"))
                else -> error("Unsupported original optional format field")
            }
        }
        if (head != null) result.setInteger("encoder-delay", head)
        if (tail != null) result.setInteger("encoder-padding", tail)
        return result
    }
    private fun writeJson(file: File, value: JSONObject) {
        check(!file.exists())
        val stream = FileOutputStream(file); retained.add(stream)
        try { stream.write((value.toString(2) + "\n").toByteArray()); stream.fd.sync() }
        finally { if (close("file.close:" + file.name) { stream.close() }) retained.remove(stream) }
    }

    fun run(): JSONObject {
        check(Build.HARDWARE in setOf("ranchu", "goldfish") && ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN)
        check(manifestSha256.matches(Regex("[a-f0-9]{64}")))
        val bytes = read(input, "manifest.json", 512 * 1024)
        check(sha(bytes) == manifestSha256)
        val manifest = JSONObject(String(bytes, Charsets.UTF_8))
        check(manifest.getString("schema") == "generated-native-aac-window-input-v1" && manifest.getBoolean("generatedOnly"))
        val cases = manifest.getJSONArray("cases"); check(cases.length() == 4)
        val results = JSONArray()
        var failure: String? = null
        try {
            for (index in 0 until cases.length()) {
                active(); val source = cases.getJSONObject(index)
                val name = source.getString("name"); check(name.matches(Regex("seed-[0-9]+-programme-(48000|2765)")))
                val sourceDir = File(input, name)
                check(sourceDir.canonicalFile.parentFile == input.canonicalFile && !Files.isSymbolicLink(sourceDir.toPath()))
                val hashes = source.getJSONObject("files")
                check(hashes.length() in 1..16)
                for (key in hashes.keys()) check(sha(read(sourceDir, key, 4 * 1024 * 1024)) == hashes.getString(key))
                val result = JSONObject().put("name", name).put("sourceOutcome", source.getString("sourceOutcome"))
                    .put("sourceReceiptSHA256", hashes.getString("receipt.json"))
                    .put("sourceEligibleForReference", source.getBoolean("eligible"))
                    .put("sourceManifestCase", source).put("programmeModeExecutedOnDevice", false)
                    .put("currentRustWindowExercised", false).put("nativeWindowQualified", false)
                    .put("formatReconstructionScope", source.optString("formatReconstructionScope", "SOURCE_NOT_ELIGIBLE"))
                results.put(result)
                if (!source.getBoolean("eligible")) {
                    result.put("classification", "RETAINED_SOURCE_FAILURE_NO_START_OR_WHOLE_EOF")
                    continue
                }
                val directory = File(root, name); check(directory.mkdir())
                val buffers = read(sourceDir, "buffers.bin", 4 * 1024 * 1024)
                val rows = source.getJSONArray("packets"); check(rows.length() in 1..128)
                val selectedStart = source.getInt("selectedFirstPacket")
                val selectedEnd = source.getInt("selectedEndPacket")
                check(selectedStart in 0 until selectedEnd && selectedEnd <= rows.length())
                val head = source.getInt("derivedHeadFrames"); val tail = source.getInt("derivedTailFrames")
                check(head in 0..8191 && tail in 0..1023)
                for (i in 0 until rows.length()) {
                    val row = rows.getJSONObject(i); val offset = row.getInt("offset"); val size = row.getInt("size")
                    check(offset >= 0 && size in 1..65536 && offset.toLong() + size <= buffers.size)
                    check(sha(buffers.copyOfRange(offset, offset + size)) == row.getString("sha256"))
                    check(row.getLong("nativePtsUs") >= 0 && row.getInt("flags") and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                }
                val controls = JSONArray(); result.put("controls", controls)
                for (kind in listOf("full-reference", "selected-baseline", "selected-trim-request")) {
                    active()
                    val first = if (kind == "full-reference") 0 else selectedStart
                    val end = if (kind == "full-reference") rows.length() else selectedEnd
                    val request = kind == "selected-trim-request"
                    val control = replay(directory, kind, buffers, rows, first, end,
                        newFormat(source, if (request) head else null, if (request) tail else null))
                    controls.put(control.put("derivedTrimRequest", request).put("requestedHeadFrames", if (request) head else JSONObject.NULL)
                        .put("requestedTailFrames", if (request) tail else JSONObject.NULL))
                    if (!cleanupKnown) break
                    if (control.getBoolean("fileClosed")) control.put("decoder", decode(directory, kind))
                    if (!cleanupKnown) break
                }
                result.put("classification", "NATIVE_REFERENCE_OBSERVATIONS_PENDING_HOST_CLOCK_AND_CAPABILITY_CHECKS")
                    .put("cleanupConfirmed", cleanupKnown)
                writeJson(File(directory, "receipt.json"), result)
                if (!cleanupKnown) break
            }
        } catch (error: Exception) { failure = error.javaClass.simpleName + ": " + error.message?.take(512) }
        return JSONObject().put("schema", "generated-native-aac-window-reference-v1")
            .put("generatedOnly", true).put("emulatorOnly", true).put("hardware", Build.HARDWARE)
            .put("sdk", Build.VERSION.SDK_INT).put("fingerprint", Build.FINGERPRINT)
            .put("inputManifestSHA256", manifestSha256).put("sourceProofs", manifest.getJSONObject("sourceProofs"))
            .put("cases", results).put("allCasesVisited", results.length() == 4)
            .put("cleanupConfirmed", cleanupKnown && retained.isEmpty()).put("failure", failure ?: JSONObject.NULL)
            .put("operations", journal).put("cancelled", cancelled.get())
            .put("programmeModeExecutedOnDevice", false).put("currentRustWindowExercised", false)
            .put("encoderCreated", false).put("nativeClockPolicyScope", "POST_EOF_LAB_REFERENCE_ONLY_NOT_LIVE_PACKET_CLOCK_OR_CARDINALITY_POLICY").put("nativeWindowQualified", false).put("presentationEquivalent", false)
    }

    private fun replay(directory: File, kind: String, bytes: ByteArray, rows: JSONArray,
                       first: Int, end: Int, value: MediaFormat): JSONObject {
        val writes = JSONArray()
        val result = JSONObject().put("kind", kind).put("formatRequested", format("muxer-addTrack", value)).put("writes", writes)
            .put("fileClosed", false).put("firstOriginalPacket", first).put("endOriginalPacket", end)
        var native: MediaMuxer? = null; var startAttempted = false; var started = false
        var positiveWrites = 0; var stopReturned = false; var releaseReturned = false
        try {
            check(!File(directory, "$kind.mp4").exists())
            cleanupKnown = false // Constructor failure has no returned closeable owner.
            val created = call("muxer.create:$kind") { MediaMuxer(File(directory, "$kind.mp4").path, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4) }
            native = created; retained.add(created); cleanupKnown = true
            val track = call("muxer.addTrack:$kind") { created.addTrack(value) }
            startAttempted = true; call("muxer.start:$kind") { created.start() }; started = true
            for (index in first until end) {
                active(); val row = rows.getJSONObject(index)
                val offset = row.getInt("offset"); val size = row.getInt("size")
                val info = MediaCodec.BufferInfo().apply { set(offset, size, row.getLong("nativePtsUs"), row.getInt("flags")) }
                call("muxer.write:$kind:$index") { created.writeSampleData(track, ByteBuffer.wrap(bytes), info) }
                positiveWrites++
                writes.put(JSONObject(row.toString()).put("originalPacketIndex", index).put("writeReturned", true))
            }
        } catch (error: Exception) { result.put("failure", error.javaClass.simpleName + ": " + error.message?.take(512)) }
        finally {
            val owned = native
            if (owned != null) {
                if (startAttempted && (!started || positiveWrites == 0)) {
                    cleanupKnown = false; result.put("quarantine", "EMPTY_OR_UNKNOWN_STARTED_MUXER_NO_STOP_OR_RELEASE")
                } else {
                    stopReturned = started && close("muxer.stop:$kind") { owned.stop() }
                    if (!started || stopReturned) releaseReturned = close("muxer.release:$kind") { owned.release() }
                    if (releaseReturned) retained.remove(owned)
                }
            }
        }
        val file = File(directory, "$kind.mp4")
        result.put("startAttempted", startAttempted).put("startReturned", started).put("positiveWrites", positiveWrites)
            .put("stopReturned", stopReturned).put("stopSkippedNotStarted", !started && !startAttempted).put("releaseReturned", releaseReturned)
            .put("fileClosed", started && stopReturned && releaseReturned && positiveWrites == end - first && !result.has("failure"))
        if (file.isFile && file.length() <= 4 * 1024 * 1024) result.put("fileBytes", file.length()).put("fileSHA256", sha(file.readBytes()))
        return result
    }

    private fun decode(directory: File, kind: String): JSONObject {
        val result = JSONObject().put("inputEos", false).put("outputEos", false).put("cleanupConfirmed", false)
        val formats = JSONArray(); result.put("formats", formats)
        val rows = JSONArray(); result.put("buffers", rows)
        var extractor: MediaExtractor? = null; var native: MediaCodec? = null; var stream: FileOutputStream? = null
        var startAttempted = false; var started = false; var inputEos = false; var outputEos = false; var frames = 0
        var inputPackets = 0; var releaseAttempts = 0; var releaseReturned = 0; var formatSeen = false
        var stopReturned = false; var decoderReleased = false; var extractorReleased = false; var streamClosed = false
        var nativeIndexUncertain = false
        try {
            cleanupKnown = false
            val source = call("extractor.create:$kind") { MediaExtractor() }; extractor = source; retained.add(source); cleanupKnown = true
            call("extractor.source:$kind") { source.setDataSource(File(directory, "$kind.mp4").path) }
            check(source.trackCount == 1); val extracted = source.getTrackFormat(0)
            check(extracted.getString(MediaFormat.KEY_MIME) == MediaFormat.MIMETYPE_AUDIO_AAC)
            formats.put(format("extractor-original", extracted)); source.selectTrack(0)
            // Only request PCM16. No delay/padding/PTS edits are injected.
            extracted.setInteger(MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
            formats.put(format("decoder-requested", extracted))
            cleanupKnown = false
            val codec = call("decoder.create:$kind") { MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC) }
            native = codec; retained.add(codec); cleanupKnown = true
            result.put("decoderName", codec.name)
            call("decoder.configure:$kind") { codec.configure(extracted, null, null, 0) }
            formats.put(format("decoder-accepted", codec.inputFormat))
            startAttempted = true; call("decoder.start:$kind") { codec.start() }; started = true
            stream = FileOutputStream(File(directory, "$kind.s16le")); retained.add(checkNotNull(stream))
            var loops = 0; var lastPts = 0L
            val until = System.nanoTime() + 12_000_000_000L
            while (!outputEos) {
                active(); check(System.nanoTime() < until && ++loops <= 1024) { "Native replay decoder deadline" }
                if (!inputEos) {
                    val index = call("decoder.dequeueInput:$kind") { codec.dequeueInputBuffer(1000) }
                    if (index >= 0) {
                        val buffer = checkNotNull(codec.getInputBuffer(index)); buffer.clear()
                        val size = source.readSampleData(buffer, 0)
                        check(size <= 65536)
                        val eos = size < 0
                        val pts = if (eos) lastPts else source.sampleTime.also { check(it >= 0); lastPts = it }
                        check(eos || inputPackets < 128)
                        val payload = if (eos) ByteArray(0) else ByteArray(size).also { buffer.duplicate().apply { position(0); limit(size) }.get(it) }
                        call("decoder.queueInput:$kind") { codec.queueInputBuffer(index, 0, if (eos) 0 else size, pts, if (eos) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0) }
                        rows.put(JSONObject().put("type", "INPUT").put("index", index).put("size", if (eos) 0 else size)
                            .put("nativePtsUs", pts).put("sha256", sha(payload)).put("eos", eos).put("queueReturned", true))
                        if (eos) inputEos = true else { inputPackets++; source.advance() }
                    }
                }
                val info = MediaCodec.BufferInfo()
                val index = call("decoder.dequeueOutput:$kind") { codec.dequeueOutputBuffer(info, 1000) }
                if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    val output = codec.outputFormat; formats.put(format("decoder-output", output))
                    check(!formatSeen && output.getInteger("sample-rate") == 48000 && output.getInteger("channel-count") == 1)
                    val explicit = output.containsKey("pcm-encoding")
                    check(!explicit || output.getInteger("pcm-encoding") == AudioFormat.ENCODING_PCM_16BIT)
                    result.put("pcmEncodingInterpretation", if (explicit) "PRESENT_PCM16" else "ABSENT_DOCUMENTED_PCM16_DEFAULT")
                    formatSeen = true
                } else if (index >= 0) {
                    try {
                        check(rows.length() < 384 && formatSeen && info.offset >= 0 && info.size >= 0 && info.size % 2 == 0)
                        check(info.flags and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                        check(frames.toLong() + info.size / 2 <= 65536)
                        val payload = ByteArray(info.size)
                        if (info.size > 0) {
                            val buffer = checkNotNull(codec.getOutputBuffer(index)).duplicate()
                            check(info.offset.toLong() + info.size <= buffer.capacity()); buffer.position(info.offset); buffer.limit(info.offset + info.size); buffer.get(payload)
                            checkNotNull(stream).write(payload)
                        }
                        frames += info.size / 2
                        outputEos = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                        rows.put(JSONObject().put("type", "OUTPUT").put("index", index).put("size", info.size)
                            .put("nativePtsUs", info.presentationTimeUs).put("flags", info.flags).put("sha256", sha(payload))
                            .put("eos", outputEos).put("cumulativeFrames", frames))
                    } finally {
                        releaseAttempts++
                        if (close("decoder.releaseOutput:$kind:$index") { codec.releaseOutputBuffer(index, false) }) releaseReturned++
                        else nativeIndexUncertain = true
                    }
                    check(!nativeIndexUncertain) { "Native output index release unknown; owner retained" }
                }
            }
        } catch (error: Exception) { result.put("failure", error.javaClass.simpleName + ": " + error.message?.take(512)) }
        finally {
            val codec = native
            if (codec != null && !nativeIndexUncertain && !(startAttempted && !started)) {
                stopReturned = started && close("decoder.stop:$kind") { codec.stop() }
                if (!started || stopReturned) decoderReleased = close("decoder.release:$kind") { codec.release() }
                if (decoderReleased) retained.remove(codec)
            }
            if (startAttempted && !started) { cleanupKnown = false; result.put("quarantine", "DECODER_START_UNCONFIRMED_NO_STOP_OR_RELEASE") }
            extractor?.let { owned -> extractorReleased = close("extractor.release:$kind") { owned.release() }; if (extractorReleased) retained.remove(owned) }
            stream?.let { owned ->
                val synced = close("pcm.sync:$kind") { owned.fd.sync() }
                streamClosed = close("pcm.close:$kind") { owned.close() }
                if (streamClosed) retained.remove(owned)
                if (!synced) cleanupKnown = false
            }
        }
        result.put("inputEos", inputEos).put("outputEos", outputEos).put("frames", frames).put("inputPackets", inputPackets)
            .put("releaseAttempts", releaseAttempts).put("releaseReturned", releaseReturned).put("stopReturned", stopReturned)
            .put("stopSkippedNotStarted", !started && !startAttempted).put("decoderReleased", decoderReleased).put("extractorReleased", extractorReleased).put("streamClosed", streamClosed)
            .put("cleanupConfirmed", decoderReleased && extractorReleased && streamClosed && !nativeIndexUncertain)
        val pcm = File(directory, "$kind.s16le")
        if (pcm.isFile && pcm.length() <= 131072) result.put("pcmBytes", pcm.length()).put("pcmSHA256", sha(pcm.readBytes()))
        return result
    }
}
