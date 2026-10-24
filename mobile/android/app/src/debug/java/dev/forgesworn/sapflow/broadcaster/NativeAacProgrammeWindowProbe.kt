package dev.forgesworn.sapflow.broadcaster

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import android.os.Build
import dev.forgesworn.sapflow.MediaCodec as SharedCodec
import dev.forgesworn.sapflow.*
import java.io.RandomAccessFile
import java.util.Collections
import java.util.IdentityHashMap
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
 * inferred native trim, PCM crop or native completion capability exists here.
 * SDK_WINDOW explicitly exercises submitted geometry and lab file IO only.
 * The Activity retains this concrete owner if any native/file close is unknown. */
internal class NativeAacProgrammeWindowProbe(private val root: File, private val input: File,
    private val manifestSha256: String, private val cancelled: AtomicBoolean, private val mode: Mode = Mode.REFERENCE) {
    enum class Mode { REFERENCE, SDK_WINDOW, EXPLICIT_READER, NATIVE_PAIR }
    private val retained = mutableListOf<Any>()
    private var cleanupKnown = true
    private val journal = JSONArray()
    private val deadlineNs = System.nanoTime() + (if (mode == Mode.SDK_WINDOW) 240_000_000_000L else 120_000_000_000L)
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
        if (mode == Mode.SDK_WINDOW) return runSdk()
        if (mode == Mode.EXPLICIT_READER) return runReader()
        if (mode == Mode.NATIVE_PAIR) return runNativePair()
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

    private val destructionAttempted = Collections.newSetFromMap(IdentityHashMap<AutoCloseable, Boolean>())
    private var sdkPushes = 0
    private var sdkBegins = 0
    private val sdkCalls = JSONArray()
    private data class UnknownAcquisition(val operation: String, val reason: String)
    /** Only documented typed Rust validation errors imply no new returned owner.
     * A wrapper/lift/cleaner-registration failure can occur after Rust created a
     * handle, so retain this parent and fence work without guessing its pointer. */
    private fun <T> acquire(name: String, block: () -> T): T {
        check(cleanupKnown) { "Previous acquisition/close unknown" }
        try { return block() }
        catch (error: BindingException) { throw error }
        catch (error: NativeRecordingException) { throw error }
        catch (error: RecordingCheckpointException) { throw error }
        catch (error: Throwable) {
            cleanupKnown = false
            retained.add(UnknownAcquisition(name, error.javaClass.simpleName))
            throw error
        }
    }
    private fun remember(value: AutoCloseable): AutoCloseable { retained.add(value); return value }
    private fun destroy(value: AutoCloseable?, name: String) {
        if (value != null && destructionAttempted.add(value) && close("shared.destroy:$name") { value.close() }) retained.remove(value)
    }
    private fun direct(bytes: ByteArray): ByteBuffer = ByteBuffer.allocateDirect(bytes.size).put(bytes).apply { flip() }
    private fun bytes32(value: String) = MessageDigest.getInstance("SHA-256").digest(value.toByteArray(Charsets.UTF_8))

    private fun runSdk(): JSONObject {
        check(Build.HARDWARE in setOf("ranchu", "goldfish") && ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN)
        val supplied = read(input, "manifest.json", 512 * 1024); check(sha(supplied) == manifestSha256)
        val manifest = JSONObject(String(supplied, Charsets.UTF_8))
        check(manifest.getString("schema") == "generated-native-aac-sdk-window-input-v1" && manifest.getBoolean("generatedOnly"))
        check(manifest.getString("pairedNativePresentation") == "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
        val cases = manifest.getJSONArray("cases"); check(cases.length() == 4)
        val results = JSONArray(); var failure: String? = null; var negativesVisited = false
        val missingBytes = read(input, "missing-input-eos.receipt.json", 4 * 1024 * 1024)
        check(sha(missingBytes) == manifest.getJSONObject("extraFiles").getString("missing-input-eos.receipt.json"))
        val missing = JSONObject(String(missingBytes, Charsets.UTF_8))
        val missingBeforeBegins = sdkBegins; val missingBeforePushes = sdkPushes
        val missingEligible = missing.getBoolean("inputEosQueued") && missing.getBoolean("outputEosObserved") && missing.getBoolean("cleanupConfirmed") && !missing.getBoolean("deliberateMissingInputEos")
        check(!missingEligible) { "Actual missing-EOF source unexpectedly eligible" }
        val missingDecision = JSONObject().put("sourceSHA256", sha(missingBytes)).put("inputEosQueued", missing.getBoolean("inputEosQueued"))
            .put("outputEosObserved", missing.getBoolean("outputEosObserved")).put("ownerCleanupConfirmed", missing.getBoolean("cleanupConfirmed"))
            .put("deliberateMissingInputEos", missing.getBoolean("deliberateMissingInputEos")).put("eligibleForCompleteSubmission", missingEligible)
            .put("decision", if (missingEligible) "ELIGIBLE" else "REFUSED_BEFORE_SDK")
            .put("beginCallsBefore", missingBeforeBegins).put("beginCallsAfter", sdkBegins).put("pushCallsBefore", missingBeforePushes).put("pushCallsAfter", sdkPushes)
        try {
            for (i in 0 until cases.length()) {
                active(); val source = cases.getJSONObject(i); val name = source.getString("name")
                check(name.matches(Regex("seed-[0-9]+-programme-(48000|2765)")))
                val original = File(input, name); check(original.canonicalFile.parentFile == input.canonicalFile)
                val hashes = source.getJSONObject("files"); check(hashes.length() in 1..16)
                for (key in hashes.keys()) check(sha(read(original, key, 4 * 1024 * 1024)) == hashes.getString(key))
                val sourceNative = JSONObject(String(read(original, "receipt.json", 4 * 1024 * 1024), Charsets.UTF_8))
                if (source.getBoolean("eligible")) {
                    check(sourceNative.getBoolean("startIssued") && sourceNative.getBoolean("inputEos") && sourceNative.getBoolean("outputEos") && sourceNative.getBoolean("cleanupConfirmed") && sourceNative.isNull("failure"))
                    check(sourceNative.getJSONObject("encoderClosure").getBoolean("stopReturned") && sourceNative.getJSONObject("encoderClosure").getBoolean("releaseReturned"))
                    val queued = source.getJSONObject("queued")
                    check(queued.getLong("preparation") == sourceNative.getLong("successfulPreambleFrames") && queued.getLong("programmeRaw") == sourceNative.getLong("generatedProgrammeRawFrames") && queued.getLong("programmeIntentionalZero") == sourceNative.getLong("intentionalProgrammeZeroFrames"))
                }
                val result = JSONObject().put("name", name).put("sourceManifestCase", source)
                    .put("sourceOutcome", source.getString("sourceOutcome")).put("nativeWindowQualified", false)
                    .put("pairedNativePresentation", "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
                results.put(result)
                if (!source.getBoolean("eligible")) { result.put("classification", "RETAINED_FAILED_SOURCE_NO_START_OR_WHOLE_EOF"); continue }
                val directory = File(root, name); check(directory.mkdir())
                val bytes = read(original, "buffers.bin", 4 * 1024 * 1024)
                val packets = source.getJSONArray("packets"); check(packets.length() in 1..128)
                for (pi in 0 until packets.length()) {
                    val p = packets.getJSONObject(pi); val offset = p.getInt("offset"); val size = p.getInt("size")
                    check(offset >= 0 && size in 1..65536 && offset.toLong() + size <= bytes.size)
                    check(sha(bytes.copyOfRange(offset, offset + size)) == p.getString("sha256"))
                    check(p.getLong("codedPts") == p.getLong("codedDts") && p.getInt("codedDuration") == 1024 && p.getInt("codedTimescale") == 48000)
                }
                val controls = JSONArray(); result.put("controls", controls)
                val healthy = sdkFile(directory, "healthy", source, bytes, i + 1)
                controls.put(healthy)
                if (!cleanupKnown) break
                if (healthy.getBoolean("fileClosed") && !healthy.has("failure")) {
                    healthy.put("decoder", decode(directory, "healthy", signedPresentation = true))
                    if (!cleanupKnown) break
                }
                if (!negativesVisited) {
                    negativesVisited = true
                    for (kind in listOf("incomplete", "drop-au", "shifted-first", "wrong-generation", "wrong-programme", "partial-write", "readback-fault", "cancel-prefix", "legacy")) {
                        active(); val control = sdkFile(directory, kind, source, bytes, i + 1); controls.put(control)
                        if (!cleanupKnown) break
                        if (kind == "legacy" && control.getBoolean("fileClosed") && !control.has("failure"))
                            control.put("decoder", decode(directory, kind, signedPresentation = true))
                    }
                    if (!cleanupKnown) break
                    // Same bytes, fresh decoder, actual deliberately omitted input EOS.
                    controls.put(JSONObject().put("kind", "missing-decoder-eos").put("decoder", decode(directory, "healthy", signedPresentation = true, omitInputEos = true, outputName = "missing-decoder-eos")))
                    if (!cleanupKnown) break
                }
                result.put("cleanupConfirmed", cleanupKnown).put("classification", "SDK_FILE_OBSERVATIONS_NOT_NATIVE_PRESENTATION_ADMISSION")
                writeJson(File(directory, "receipt.json"), result)
            }
        } catch (error: Exception) { failure = error.javaClass.simpleName + ": " + error.message?.take(512) }
        return JSONObject().put("schema", "generated-native-aac-sdk-window-v1").put("generatedOnly", true).put("emulatorOnly", true)
            .put("hardware", Build.HARDWARE).put("sdk", Build.VERSION.SDK_INT).put("fingerprint", Build.FINGERPRINT)
            .put("inputManifestSHA256", manifestSha256).put("sourceProofs", manifest.getJSONObject("sourceProofs"))
            .put("cases", results).put("allCasesVisited", results.length() == 4).put("sdkMethodCalls", sdkCalls)
            .put("sdkBeginCallsReturned", sdkBegins).put("sdkPushCallsReturned", sdkPushes)
            .put("missingProducerEosDecision", missingDecision).put("actualMissingProducerEosRefusedBeforeSdk", !missingEligible && missingDecision.getInt("beginCallsAfter") == missingBeforeBegins && missingDecision.getInt("pushCallsAfter") == missingBeforePushes).put("missingProducerEosSHA256", sha(missingBytes))
            .put("programmeModeExecutedOnDevice", sdkBegins > 0).put("currentRustWindowExercised", sdkPushes > 0)
            .put("currentRustSourceExercised", sdkBegins > 0).put("encoderCreated", false)
            .put("cleanupConfirmed", cleanupKnown && retained.isEmpty()).put("failure", failure ?: JSONObject.NULL)
            .put("operations", journal).put("cancelled", cancelled.get()).put("nativeWindowQualified", false)
            .put("presentationEquivalent", false).put("appAdmission", "UNKNOWN")
            .put("pairedNativePresentation", "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
    }

    /** A real sequential lab file owner. It never returns an app/storage receipt.
     * Checked writes and SDK tokens are independent facts; final geometry cannot
     * turn a source EOF observation or a failed file write into producer truth. */
    private fun sdkFile(directory: File, kind: String, source: JSONObject, original: ByteArray, generation: Int): JSONObject {
        val result = JSONObject().put("kind", kind).put("injectedFault", kind in listOf("partial-write", "readback-fault"))
            .put("fileClosed", false).put("appAdmission", "UNKNOWN").put("nativeWindowQualified", false)
        val records = JSONArray(); val submitted = JSONArray(); val refusals = JSONArray()
        result.put("writes", records).put("submitted", submitted).put("controls", refusals)
        val path = File(directory, "$kind.mp4"); check(!path.exists())
        var file: RandomAccessFile? = null; var mux: NativeRecordingMuxer? = null; var codec: SharedCodec? = null
        var first: NativeProgrammeCheckpoint? = null; var firstBytes: ByteArray? = null; var firstHash: ByteArray? = null
        var previous: DecodedProgrammeCheckpoint? = null
        var initial = ByteArray(0); var fragmentHash = MessageDigest.getInstance("SHA-256")
        var frontier = 0L; var sequence = 0; var finalHeader = false; var complete = false; var fileClosed = false
        val recordingBinding = bytes32(root.name + source.getString("name") + kind)
        val fileBinding = bytes32(root.name + source.getString("name") + kind + ":file")
        val packets = source.getJSONArray("packets")
        val selectedFirst = source.getInt("selectedFirstPacket"); val selectedEnd = source.getInt("selectedEndPacket")
        check(selectedFirst in 0 until selectedEnd && selectedEnd <= packets.length())
        val q = source.getJSONObject("queued")
        val start = NativeProgrammeWindowStart(generation.toULong(), source.getLong("codedOrigin").toULong(),
            q.getLong("preparation").toULong(), source.getLong("measuredDisplacement").toULong(),
            source.getLong("discardPrefixFrames").toULong(), 0uL)
        val end = NativeProgrammeGenerationEnd((if (kind == "wrong-generation") generation + 1 else generation).toULong(),
            NativeProgrammeQueuedFrames(q.getLong("preparation").toULong(),
                (q.getLong("programmeRaw") - if (kind == "wrong-programme") 1 else 0).toULong(),
                q.getLong("programmeIntentionalZero").toULong(), q.getLong("blockPadding").toULong(), q.getLong("codecDrain").toULong()),
            source.getLong("observedCodedFrames").toULong())
        fun checkedWrite(bytes: ByteArray, position: Long, label: String) {
            val owned = checkNotNull(file); check(position >= 0 && bytes.size <= 4 * 1024 * 1024)
            val buffer = ByteBuffer.wrap(bytes); owned.channel.position(position); var loops = 0; var zeros = 0
            while (buffer.hasRemaining()) {
                active(); check(++loops <= 4096)
                if (kind == "partial-write" && position > 0 && buffer.position() >= minOf(8, bytes.size)) error("INJECTED_AFTER_ACTUAL_PARTIAL_FILE_WRITE")
                val oldLimit = buffer.limit()
                if (kind == "partial-write" && position > 0) buffer.limit(minOf(oldLimit, 8))
                val wrote = owned.channel.write(buffer); buffer.limit(oldLimit)
                if (wrote == 0) check(++zeros <= 3) else zeros = 0
            }
            owned.fd.sync()
            if (kind == "readback-fault" && position > 0 && bytes.isNotEmpty()) {
                owned.seek(position); owned.write(bytes[0].toInt() xor 1); owned.fd.sync()
            }
            val actual = ByteArray(bytes.size); owned.seek(position); owned.readFully(actual)
            check(actual.contentEquals(bytes)) { "Actual private file readback mismatch" }
            records.put(JSONObject().put("operation", label).put("offset", position).put("bytes", bytes.size).put("sha256", sha(actual)).put("readbackChecked", true))
        }
        fun drain() {
            val owner = checkNotNull(mux)
            if (kind == "legacy") {
                while (true) {
                    val write = acquire("legacy.output:$kind") { owner.takeOutput() } ?: break
                    checkedWrite(write.bytes, if (write.rewriteHeader) 0 else checkNotNull(file).length(), if (write.rewriteHeader) "LEGACY_FINAL_HEADER" else "LEGACY_APPEND")
                    if (write.rewriteHeader) finalHeader = true
                }
                return
            }
            while (true) {
                val chunk = acquire("programme.output:$kind") { owner.takeProgrammeOutput() } ?: break
                val token = chunk.checkpoint
                if (token != null) remember(token)
                try {
                    when (chunk.kind) {
                        NativeRecordingChunkKind.INITIAL -> {
                            check(initial.isEmpty() && checkNotNull(file).length() == 0L && token == null)
                            initial = chunk.bytes; checkedWrite(initial, 0, "INITIAL"); frontier = initial.size.toLong()
                            val initFile = File(directory, "$kind.initial.bin"); check(!initFile.exists())
                            val initialStream = acquire("initial.stream:$kind") { FileOutputStream(initFile) }; retained.add(initialStream)
                            try { initialStream.write(initial); initialStream.fd.sync() } finally { if (close("initial.close:$kind") { initialStream.close() }) retained.remove(initialStream) }
                            check(cleanupKnown) { "INITIAL close unknown; no readback or progress" }
                            check(initFile.readBytes().contentEquals(initial))
                        }
                        NativeRecordingChunkKind.FRAGMENT -> {
                            check(initial.isNotEmpty()); checkedWrite(chunk.bytes, frontier, "FRAGMENT")
                            frontier += chunk.bytes.size; fragmentHash.update(chunk.bytes)
                            val issued = checkNotNull(token); val hash = (fragmentHash.clone() as MessageDigest).digest()
                            val encoded = owner.encodeProgrammeCheckpoint(issued, recordingBinding, fileBinding, hash)
                            check(encoded.info.committedOffset == frontier.toULong() && encoded.info.fragmentRegionBytes == (frontier - initial.size).toULong())
                            check(encoded.info.initialHeaderSha256.contentEquals(MessageDigest.getInstance("SHA-256").digest(initial)))
                            check(encoded.info.fragmentRegionSha256.contentEquals(hash))
                            val slot = File(directory, "$kind-${++sequence}.sfcp2"); check(!slot.exists())
                            val output = acquire("sidecar.stream:$kind") { FileOutputStream(slot) }; retained.add(output)
                            try { output.write(encoded.bytes); output.fd.sync() } finally { if (close("sidecar.close:$kind:$sequence") { output.close() }) retained.remove(output) }
                            check(cleanupKnown) { "Sidecar close unknown; no readback/JNI/progress" }
                            check(slot.readBytes().contentEquals(encoded.bytes))
                            val decoded = acquire("sidecar.decode:$kind") { decodeProgrammeCheckpoint(slot.readBytes(), recordingBinding, fileBinding) }; remember(decoded)
                            try {
                                check(decoded.info().facts == encoded.info.facts && decoded.info().start == start)
                                check(decoded.header().contentEquals(owner.programmeCheckpointHeader(issued)))
                                previous?.let { validateProgrammeCheckpointSuccessor(it, decoded) }
                                destroy(previous, "previous:$kind"); check(cleanupKnown) { "Previous decoded handle close unknown" }; previous = decoded
                            } catch (error: Exception) { destroy(decoded, "invalid:$kind"); throw error }
                            if (first == null) { first = issued; firstBytes = encoded.bytes; firstHash = hash }
                            if (issued !== first) destroy(issued, "issued:$kind:$sequence")
                            check(cleanupKnown) { "Issued token close unknown; no progress" }
                            complete = encoded.info.facts.windowGeometryComplete
                            records.put(JSONObject().put("operation", "SFCP0002").put("sequence", sequence).put("slot", slot.name)
                                .put("sha256", sha(encoded.bytes)).put("frontier", frontier).put("audioUnits", encoded.info.facts.audioUnits.toLong())
                                .put("audioStart", encoded.info.facts.audioStart.toLong()).put("audioEnd", encoded.info.facts.audioEnd.toLong())
                                .put("containerEnd", encoded.info.facts.audioContainerEnd.toLong()).put("head", encoded.info.facts.leadingFrames.toLong())
                                .put("tail", encoded.info.facts.trailingFrames.toLong()).put("windowGeometryComplete", complete))
                        }
                        NativeRecordingChunkKind.FINAL_HEADER -> {
                            check(chunk.bytes.size == initial.size && token == null)
                            checkedWrite(chunk.bytes, 0, "FINAL_HEADER"); finalHeader = true
                        }
                    }
                } finally {
                    if (token != null && token !== first && retained.any { it === token }) destroy(token, "remaining:$kind")
                }
                check(cleanupKnown) { "Storage/JNI close unconfirmed" }
            }
        }
        try {
            val created = path.createNewFile(); result.put("exclusiveCreateReturned", created); check(created) { "Private lab file already exists" }
            file = acquire("file.open:$kind") { RandomAccessFile(path, "rw") }; retained.add(checkNotNull(file))
            codec = acquire("codec.aac:$kind") { SharedCodec.aac(direct(byteArrayOf(0x11, 0x88.toByte()))) }; remember(checkNotNull(codec))
            mux = acquire("mux.create:$kind") { if (kind == "legacy") NativeRecordingMuxer(false, true, 0uL, 4096uL, 100u)
                else NativeRecordingMuxer.withProgrammeAacTiming(0uL, 4096uL, 100u) }
            remember(checkNotNull(mux))
            if (kind != "legacy") { checkNotNull(mux).beginProgrammeWindow(start); sdkBegins++ }
            sdkCalls.put(JSONObject().put("kind", kind).put("generation", generation).put("beginReturned", kind != "legacy"))
            for (index in selectedFirst until selectedEnd) {
                if (kind == "drop-au" && index == selectedFirst + 1) continue
                val row = packets.getJSONObject(index); val at = row.getInt("offset"); val bytes = original.copyOfRange(at, at + row.getInt("size"))
                val shift = if (kind == "shifted-first" && index == selectedFirst) 1 else 0
                val sample = acquire("sample.create:$kind:$index") { checkNotNull(codec).sample(direct(bytes), row.getLong("codedPts") + shift, row.getLong("codedDts") + shift, 1024u, 48000u, false) }
                remember(sample)
                try { checkNotNull(mux).push(sample); if (kind != "legacy") sdkPushes++ }
                finally { destroy(sample, "sample:$kind:$index") }
                check(cleanupKnown) { "Sample handle close unknown; no submission progress" }
                submitted.put(JSONObject(row.toString()).put("originalPacketIndex", index).put("codedShiftInjected", shift).put("pushReturned", true))
                drain()
                if (kind == "cancel-prefix" && submitted.length() == 4) break
            }
            if (kind == "cancel-prefix") {
                checkNotNull(mux).cancel(); result.put("cancelReturned", true)
            } else if (kind == "legacy") { checkNotNull(mux).finish(); drain() }
            else {
                if (kind != "incomplete") checkNotNull(mux).closeProgrammeWindow(end)
                checkNotNull(mux).finishProgramme(if (kind == "incomplete") NativeProgrammeCompletion.INCOMPLETE_PREFIX else NativeProgrammeCompletion.COMPLETE_SUBMITTED_WINDOW)
                drain()
            }
            if (finalHeader && kind != "legacy") {
                val final = checkNotNull(previous); val header = final.header()
                val readHeader = ByteArray(header.size); checkNotNull(file).seek(0); checkNotNull(file).readFully(readHeader)
                check(readHeader.contentEquals(header)) { "Final physical header differs from sealed checkpoint" }
                result.put("finalHeaderMatchesSealedToken", true)
                if (kind == "healthy") {
                    val facts = final.info().facts
                    check(facts.windowGeometryComplete && facts.audioUnits == (selectedEnd - selectedFirst).toULong())
                    check(facts.audioStart == (source.getLong("codedOrigin") + source.getLong("discardPrefixFrames")).toULong())
                    check(facts.audioEnd == (source.getLong("codedOrigin") + source.getLong("selectedEndFrames")).toULong())
                    check(facts.leadingFrames == source.getInt("derivedHeadFrames").toUInt() && facts.trailingFrames == source.getInt("derivedTailFrames").toUInt())
                    check(final.presentationExtent().durationTicks == ((q.getLong("programmeRaw") + q.getLong("programmeIntentionalZero")) * 15).toULong())
                }
            }
            first?.let { token ->
                val before = checkNotNull(firstBytes)
                checkNotNull(mux).cancel()
                val again = checkNotNull(mux).encodeProgrammeCheckpoint(token, recordingBinding, fileBinding, checkNotNull(firstHash))
                check(again.bytes.contentEquals(before)); result.put("oldTokenImmutableAfterCancel", true)
                var foreign: NativeRecordingMuxer? = null
                try {
                    foreign = acquire("foreign.create:$kind") { NativeRecordingMuxer.withProgrammeAacTiming(0uL, 4096uL, 100u) }; remember(foreign)
                    try { foreign.encodeProgrammeCheckpoint(token, recordingBinding, fileBinding, checkNotNull(firstHash)); error("Foreign token accepted") }
                    catch (_: RecordingCheckpointException.ForeignCheckpoint) { refusals.put("FOREIGN_TOKEN_REFUSED") }
                } finally { destroy(foreign, "foreign:$kind") }
                check(cleanupKnown) { "Foreign handle close unconfirmed" }
                try { val bad = acquire("wrong-binding.decode:$kind") { decodeProgrammeCheckpoint(before, ByteArray(32), fileBinding) }; remember(bad); destroy(bad, "wrong-binding"); check(cleanupKnown) { "Unexpected decoded handle close unknown" }; error("Wrong binding accepted") }
                catch (_: RecordingCheckpointException.Binding) { refusals.put("WRONG_BINDING_REFUSED") }
                previous?.let { last ->
                    try { validateProgrammeCheckpointSuccessor(last, last); error("Stale successor accepted") }
                    catch (_: RecordingCheckpointException.Successor) { refusals.put("STALE_SUCCESSOR_REFUSED") }
                }
            }
            checkNotNull(file).fd.sync()
            result.put("fileReadbackSHA256", sha(path.readBytes())).put("fileBytes", path.length())
        } catch (error: Exception) { result.put("failure", error.javaClass.simpleName + ": " + error.message?.take(512)) }
        finally {
            destroy(previous, "decoded:$kind"); destroy(first, "first:$kind")
            if (mux != null) { try { checkNotNull(mux).cancel() } catch (error: Exception) { result.put("cancelFailure", error.javaClass.simpleName) }; destroy(mux, "mux:$kind") }
            destroy(codec, "codec:$kind")
            file?.let { owned -> fileClosed = close("sdkFile.close:$kind") { owned.close() }; if (fileClosed) retained.remove(owned) }
        }
        result.put("fileClosed", fileClosed).put("finalHeaderRewritten", finalHeader).put("windowGeometryComplete", complete)
            .put("recordingBindingHex", recordingBinding.joinToString("") { "%02x".format(it) }).put("fileBindingHex", fileBinding.joinToString("") { "%02x".format(it) })
            .put("initialBytes", initial.size).put("sealedFrontier", frontier)
            .put("scope", "ACTUAL_SUBMITTED_SDK_AND_LAB_STORAGE_ONLY; NOT_NATIVE_SOURCE_EOF_OR_APP_COMPLETION")
        if (cleanupKnown && path.isFile && path.length() <= 4 * 1024 * 1024) {
            val actual = path.readBytes()
            result.put("fileSHA256", sha(actual)).put("fragmentRegionSHA256", sha(actual.drop(initial.size).toByteArray()))
        } else result.put("physicalReadbackAfterClose", "UNKNOWN_QUARANTINED_NO_FURTHER_IO")

        return result
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

    /** Application reader policy, not native extractor metadata or source truth. */
    private var readerDecoderCreates = 0
    private var readerCheckpointDecodes = 0
    private data class ReaderRequest(val head: Int, val tail: Int, val mediaTime: Long,
        val packets: JSONArray, val decoderName: String, val generation: Long,
        val expectedFrames: Long, val provenance: JSONObject)
    private fun readerGeometry(head: Int, tail: Int, units: Long, programme: Long, first: Long, mediaTime: Long) {
        check(head in 0..8191 && tail in 0..1023 && units in 1..128 && programme in 1..48000 && first >= 0)
        check(Math.multiplyExact(units, 1024L) == Math.addExact(Math.addExact(head.toLong(), programme), tail.toLong()))
        check(Math.addExact(first, head.toLong()) == mediaTime)
    }
    private fun hex32(value: String): ByteArray {
        check(value.matches(Regex("[a-f0-9]{64}")))
        return ByteArray(32) { value.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    }
    private fun checkedReader(sourceDir: File, source: JSONObject, reference: JSONObject, refusals: JSONArray): ReaderRequest {
        val file = read(sourceDir, "healthy.mp4", 4 * 1024 * 1024)
        val initial = read(sourceDir, "healthy.initial.bin", 65536)
        val slot = read(sourceDir, "healthy.final.sfcp2", 66080)
        val recording = hex32(reference.getString("recordingBindingHex")); val binding = hex32(reference.getString("fileBindingHex"))
        val decoderCallsBefore = readerDecoderCreates
        val decoded = acquire("reader.checkpoint.decode") { decodeProgrammeCheckpoint(slot, recording, binding) }; remember(decoded); readerCheckpointDecodes++
        var request: ReaderRequest? = null
        try {
            val info = decoded.info(); val facts = info.facts; val start = info.start; val end = checkNotNull(info.windowEnd)
            val q = source.getJSONObject("queued"); val generation = reference.getLong("sourceGeneration").toULong()
            check(facts.windowGeometryComplete && start.generation == generation && end.generation == generation)
            check(start.codedOrigin == source.getLong("codedOrigin").toULong() && start.preparationFrames == q.getLong("preparation").toULong())
            check(start.measuredDisplacement == source.getLong("measuredDisplacement").toULong() && start.discardPrefixFrames == source.getLong("discardPrefixFrames").toULong() && start.movieDelayTicks == 0uL)
            check(end.queued == NativeProgrammeQueuedFrames(q.getLong("preparation").toULong(), q.getLong("programmeRaw").toULong(), q.getLong("programmeIntentionalZero").toULong(), q.getLong("blockPadding").toULong(), q.getLong("codecDrain").toULong()))
            check(end.codedFrames == source.getLong("observedCodedFrames").toULong())
            check(initial.isNotEmpty() && info.initialHeaderLen.toInt() == initial.size && info.committedOffset == file.size.toULong())
            check(info.initialHeaderSha256.contentEquals(MessageDigest.getInstance("SHA-256").digest(initial)))
            check(info.fragmentRegionBytes == (file.size - initial.size).toULong() && file.size >= initial.size)
            check(info.fragmentRegionSha256.contentEquals(MessageDigest.getInstance("SHA-256").digest(file.copyOfRange(initial.size, file.size))))
            val header = decoded.header(); check(header.size == initial.size && file.copyOfRange(0, initial.size).contentEquals(header))
            check(sha(header) == reference.getString("sealedHeaderSHA256") && sha(file) == reference.getString("fileSHA256"))
            val all = source.getJSONArray("packets"); val first = source.getInt("selectedFirstPacket"); val last = source.getInt("selectedEndPacket")
            check(first in 0 until last && last <= all.length() && last - first in 1..128)
            val selected = JSONArray(); for (i in first until last) selected.put(JSONObject(all.getJSONObject(i).toString()))
            val head = facts.leadingFrames.toInt(); val tail = facts.trailingFrames.toInt()
            val n = Math.addExact(q.getLong("programmeRaw"), q.getLong("programmeIntentionalZero"))
            val firstTicks = selected.getJSONObject(0).getLong("codedDts")
            val mediaTime = Math.addExact(firstTicks, head.toLong())
            readerGeometry(head, tail, selected.length().toLong(), n, firstTicks, mediaTime)
            check(facts.audioUnits == selected.length().toULong() && facts.audioStart == firstTicks.toULong())
            check(facts.audioEnd == Math.addExact(firstTicks, Math.multiplyExact(selected.length().toLong(), 1024L)).toULong())
            check(facts.audioContainerEnd == (facts.audioEnd - facts.trailingFrames.toULong()))
            check(head == source.getInt("derivedHeadFrames") && tail == source.getInt("derivedTailFrames"))
            check(head.toLong() == Math.subtractExact(Math.addExact(q.getLong("preparation"), source.getLong("measuredDisplacement")), source.getLong("discardPrefixFrames")))
            check(decoded.presentationExtent().durationTicks == Math.multiplyExact(n, 15L).toULong())
            // These controls exercise the same pure guard used before configure.
            var badHeadRefused = false
            try { readerGeometry(-1, tail, selected.length().toLong(), n, firstTicks, mediaTime) } catch (_: IllegalStateException) { badHeadRefused = true }
            check(badHeadRefused) { "Invalid head accepted" }
            refusals.put(JSONObject().put("kind", "INVALID_HEAD_REFUSED_BEFORE_DECODER").put("refused", badHeadRefused).put("decoderCallsBefore", decoderCallsBefore).put("decoderCallsAfter", readerDecoderCreates))
            var badTailRefused = false
            try { readerGeometry(head, 1024, selected.length().toLong(), n, firstTicks, mediaTime) } catch (_: IllegalStateException) { badTailRefused = true }
            check(badTailRefused) { "Invalid tail accepted" }
            refusals.put(JSONObject().put("kind", "INVALID_TAIL_REFUSED_BEFORE_DECODER").put("refused", badTailRefused).put("decoderCallsBefore", decoderCallsBefore).put("decoderCallsAfter", readerDecoderCreates))
            try {
                val foreign = acquire("reader.foreign.checkpoint.decode") { decodeProgrammeCheckpoint(slot, ByteArray(32), binding) }; remember(foreign)
                destroy(foreign, "reader.foreign"); check(cleanupKnown); error("Foreign checkpoint accepted")
            } catch (_: RecordingCheckpointException.Binding) { refusals.put(JSONObject().put("kind", "FOREIGN_CHECKPOINT_REFUSED_BEFORE_DECODER").put("refused", true).put("decoderCallsBefore", decoderCallsBefore).put("decoderCallsAfter", readerDecoderCreates)) }
            check(readerDecoderCreates == decoderCallsBefore)
            val name = reference.getString("decoderName"); check(name == "c2.android.aac.decoder")
            val provenance = JSONObject().put("scope", "CHECKED_SFCP2_GENERATED_FILE_PAIRING_NOT_NATIVE_SOURCE_AUTHORITY")
                .put("sourceGeneration", generation.toLong()).put("recordingBindingHex", reference.getString("recordingBindingHex"))
                .put("fileBindingHex", reference.getString("fileBindingHex")).put("fileSHA256", sha(file))
                .put("sealedHeaderSHA256", sha(header)).put("checkpointSHA256", sha(slot))
                .put("initialHeaderSHA256", sha(initial)).put("fragmentRegionSHA256", info.fragmentRegionSha256.hexLower())
                .put("committedOffset", info.committedOffset.toLong()).put("audioUnits", facts.audioUnits.toLong())
                .put("originalCodedStart48k", firstTicks).put("sourceMediaOrigin48k", mediaTime)
                .put("originalCodedEnd48k", facts.audioEnd.toLong()).put("containerEnd48k", facts.audioContainerEnd.toLong())
                .put("leadingFrames", head).put("trailingFrames", tail)
                .put("sourceContextSHA256", sha(source.toString().toByteArray(Charsets.UTF_8)))
                .put("movieDelay720k", 0).put("expectedFrames", n)
            request = ReaderRequest(head, tail, mediaTime, selected, name, generation.toLong(), n, provenance)
        } finally { destroy(decoded, "reader.checkpoint") }
        check(cleanupKnown) { "Reader checkpoint close unknown; decoder fenced" }
        return checkNotNull(request)
    }
    private fun ByteArray.hexLower(): String {
        val chars = "0123456789abcdef"
        return buildString { for (b in this@hexLower) { val v = b.toInt() and 255; append(chars[v ushr 4]); append(chars[v and 15]) } }
    }
    private fun scheduleSnapshot(value: ReturnedPcmClock.Snapshot): JSONObject = JSONObject()
        .put("contract", value.contract).put("scheduleComplete", value.scheduleComplete)
        .put("returnedFrames", value.returnedFrames).put("sinkAcceptedFrames", value.sinkAcceptedFrames)
        .put("returnedRanges", value.returnedRanges).put("releaseAttempts", value.releaseAttempts).put("releasesReturned", value.releasesReturned)
        .put("inputEosQueueReturned", value.inputEosQueueReturned).put("outputEosObserved", value.outputEosObserved)
        .put("ownerCloseAttempted", value.ownerCloseAttempted).put("ownerCloseReturned", value.ownerCloseReturned)
        .put("sinkCloseAttempted", value.sinkCloseAttempted).put("sinkCloseReturned", value.sinkCloseReturned)
        .put("activeSerial", value.activeSerial ?: JSONObject.NULL).put("knownFault", value.knownFault ?: JSONObject.NULL)
        .put("unknownOwnership", value.unknownOwnership).put("sinkAcceptanceUnknown", value.sinkAcceptanceUnknown)
        .put("nativeOwnershipUnknown", value.nativeOwnershipUnknown).put("rawNativeClockGatePassed", value.rawNativeClockGatePassed)
        .put("contentQualified", value.contentQualified).put("nativeSourceQualified", value.nativeSourceQualified)
        .put("presentationEquivalent", value.presentationEquivalent).put("appAdmission", "UNKNOWN")
    private fun schedulePosition(value: ReturnedPcmClock.Position?): Any = value?.let { JSONObject()
        .put("prefixFrames", it.prefixFrames).put("frames", it.frames).put("sourceStart48k", it.sourceStart48k).put("sourceEnd48k", it.sourceEnd48k)
        .put("movieStart720k", it.movieStart720k).put("movieEnd720k", it.movieEnd720k) } ?: JSONObject.NULL
    private fun runReader(): JSONObject {
        check(Build.HARDWARE in setOf("ranchu", "goldfish") && ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN)
        val bytes = read(input, "manifest.json", 512 * 1024); check(sha(bytes) == manifestSha256)
        val manifest = JSONObject(String(bytes, Charsets.UTF_8))
        check(manifest.getString("schema") == "generated-native-aac-explicit-reader-input-v1" && manifest.getBoolean("generatedOnly"))
        check(manifest.getString("pairedNativePresentation") == "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
        val cases = manifest.getJSONArray("cases"); check(cases.length() == 4)
        val results = JSONArray(); var failure: String? = null; var baselineDone = false
        try {
            for (i in 0 until cases.length()) {
                active(); val source = cases.getJSONObject(i); val name = source.getString("name")
                check(name.matches(Regex("seed-[0-9]+-programme-(48000|2765)")))
                val sourceDir = File(input, name); check(sourceDir.canonicalFile.parentFile == input.canonicalFile && !Files.isSymbolicLink(sourceDir.toPath()))
                val files = source.getJSONObject("files"); check(files.length() in 1..16)
                for (key in files.keys()) check(sha(read(sourceDir, key, 4 * 1024 * 1024)) == files.getString(key))
                val row = JSONObject().put("name", name).put("sourceManifestCase", source).put("sourceOutcome", source.getString("sourceOutcome"))
                    .put("nativeWindowQualified", false).put("presentationEquivalent", false).put("appAdmission", false)
                results.put(row)
                if (!source.getBoolean("eligible")) { check(i == 0 && source.getString("sourceOutcome") == "FAILED"); row.put("classification", "RETAINED_FAILED_SOURCE_NO_REPLAY"); continue }
                check(i > 0 && source.getString("sourceOutcome") == "CANDIDATE_QUALIFIED")
                val observed = JSONObject(String(read(sourceDir, "receipt.json", 4 * 1024 * 1024), Charsets.UTF_8))
                check(observed.getBoolean("generatedOnly") && observed.getBoolean("startIssued") && observed.getBoolean("inputEos") && observed.getBoolean("outputEos") && observed.getBoolean("cleanupConfirmed") && observed.isNull("failure"))
                val closure = observed.getJSONObject("encoderClosure"); check(closure.getBoolean("stopReturned") && closure.getBoolean("releaseReturned"))
                val q = source.getJSONObject("queued")
                check(q.getLong("preparation") == observed.getLong("successfulPreambleFrames") && q.getLong("programmeRaw") == observed.getLong("generatedProgrammeRawFrames") && q.getLong("programmeIntentionalZero") == observed.getLong("intentionalProgrammeZeroFrames"))
                check(q.getLong("blockPadding") == observed.getLong("successfulBlockPaddingFrames") && q.getLong("codecDrain") == observed.getLong("successfulCodecDrainFrames"))
                val original = read(sourceDir, "buffers.bin", 4 * 1024 * 1024); val packets = source.getJSONArray("packets"); check(packets.length() in 1..128)
                for (pi in 0 until packets.length()) {
                    val p = packets.getJSONObject(pi); val at = p.getInt("offset"); val size = p.getInt("size")
                    check(at >= 0 && size in 1..65536 && at.toLong() + size <= original.size && sha(original.copyOfRange(at, at + size)) == p.getString("sha256"))
                    check(p.getLong("codedPts") == p.getLong("codedDts") && p.getInt("codedDuration") == 1024 && p.getInt("codedTimescale") == 48000)
                    check(p.getLong("codedDts") == Math.addExact(source.getLong("codedOrigin"), Math.multiplyExact(pi.toLong(), 1024L)))
                    Math.multiplyExact(p.getLong("codedPts"), 1_000_000L) // Refuse unrepresentable conversion before decoder creation.
                    check(p.getInt("flags") and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                }
                val reference = source.getJSONObject("readerFile"); check(reference.getLong("sourceGeneration") == (i + 1).toLong())
                val refusals = JSONArray(); row.put("preDecoderRefusals", refusals)
                val request = checkedReader(sourceDir, source, reference, refusals)
                check(cleanupKnown)
                val directory = File(root, name); check(directory.mkdir())
                // Existing immutable private file, no header/packet/clock rewrite.
                val copied = File(directory, "healthy.mp4"); check(copied.createNewFile())
                val stream = acquire("reader.copy.open") { FileOutputStream(copied) }; retained.add(stream)
                try { stream.write(read(sourceDir, "healthy.mp4", 4 * 1024 * 1024)); stream.fd.sync() }
                finally { if (close("reader.copy.close") { stream.close() }) retained.remove(stream) }
                check(cleanupKnown && sha(read(directory, "healthy.mp4", 4 * 1024 * 1024)) == reference.getString("fileSHA256"))
                if (!baselineDone) {
                    baselineDone = true
                    row.put("unconfiguredControl", decode(directory, "healthy", signedPresentation = true, outputName = "unconfigured-control", expectedDecoderName = request.decoderName))
                    check(cleanupKnown) { "Baseline decoder cleanup unknown" }
                }
                row.put("explicitReader", decode(directory, "healthy", signedPresentation = true, outputName = "explicit-reader", reader = request, expectedDecoderName = request.decoderName))
                check(cleanupKnown) { "Explicit reader cleanup unknown" }
                check(sha(read(directory, "healthy.mp4", 4 * 1024 * 1024)) == reference.getString("fileSHA256"))
                row.put("classification", "EXPLICIT_READER_OBSERVATION_NOT_GENERIC_FILE_OR_PRESENTATION_ADMISSION")
                writeJson(File(directory, "receipt.json"), row); check(cleanupKnown)
            }
        } catch (error: Exception) { failure = error.javaClass.simpleName + ": " + error.message?.take(512) }
        return JSONObject().put("schema", "generated-native-aac-explicit-reader-v1").put("generatedOnly", true).put("emulatorOnly", true)
            .put("hardware", Build.HARDWARE).put("sdk", Build.VERSION.SDK_INT).put("fingerprint", Build.FINGERPRINT)
            .put("inputManifestSHA256", manifestSha256).put("sourceProofs", manifest.getJSONObject("sourceProofs")).put("cases", results)
            .put("operations", journal).put("encoderCreated", false).put("sdkMuxerCreated", false).put("newSdkFileCreated", false)
            .put("programmeModeExecutedOnDevice", false).put("currentRustWindowExercised", false).put("sdkCheckpointDecodeCallsReturned", readerCheckpointDecodes)
            .put("baselineControlVisited", baselineDone).put("allCasesVisited", results.length() == 4)
            .put("decoderCreateAttempts", readerDecoderCreates).put("plannedDecoderObservations", 4).put("cleanupConfirmed", cleanupKnown && retained.isEmpty())
            .put("failure", failure ?: JSONObject.NULL).put("cancelled", cancelled.get()).put("nativeWindowQualified", false)
            .put("presentationEquivalent", false).put("appAdmission", false).put("stockFileInteropRestored", false)
            .put("pairedNativePresentation", "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
    }

    /** Separate qualification action. Native counterparts never enter the SDK
     * checkpoint decoder. Their original AU geometry is bound to the partner's
     * real decoded SFCP2 and the actual retained native file/table bytes. */
    private fun nativeReader(sourceDir: File, source: JSONObject, sdk: ReaderRequest, refusals: JSONArray): ReaderRequest {
        val reference = source.getJSONObject("nativeRecorderFile")
        val expectedKeys = setOf("filename", "fileSHA256", "sdkFileSHA256", "sourceGeneration", "writerReceiptSHA256", "writerAuditSHA256")
        check(reference.keys().asSequence().toSet() == expectedKeys)
        check(reference.getString("filename") == "native-recorder.mp4")
        hex32(reference.getString("writerReceiptSHA256")); hex32(reference.getString("writerAuditSHA256"))
        val bytes = read(sourceDir, "native-recorder.mp4", NativeAacRecorderFileAdmission.MAX_BYTES)
        val packets = (0 until sdk.packets.length()).map { index ->
            val packet = sdk.packets.getJSONObject(index)
            NativeAacRecorderFileAdmission.OriginalPacket(packet.getInt("size"), packet.getLong("codedDts"),
                packet.getLong("codedPts"), packet.getInt("codedDuration"), packet.getString("sha256"))
        }
        val checkedSource = NativeAacRecorderFileAdmission.Source(sdk.provenance.getString("fileSHA256"), sdk.generation,
            sdk.head, sdk.tail, sdk.expectedFrames, sdk.mediaTime, packets)
        val registration = NativeAacRecorderFileAdmission.Registration(reference.getString("fileSHA256"),
            reference.getString("sdkFileSHA256"), reference.getLong("sourceGeneration"))
        val before = readerDecoderCreates
        val actual = NativeAacRecorderFileAdmission.admit(bytes, registration, checkedSource)
        fun refused(kind: String, operation: () -> Unit) {
            var rejected = false
            try { operation() } catch (_: IllegalArgumentException) { rejected = true }
            check(rejected && before == readerDecoderCreates) { "Native file refusal bypassed" }
            refusals.put(JSONObject().put("kind", kind).put("refused", rejected)
                .put("decoderCallsBefore", before).put("decoderCallsAfter", readerDecoderCreates))
        }
        refused("FOREIGN_NATIVE_SOURCE_REFUSED_BEFORE_DECODER") {
            NativeAacRecorderFileAdmission.admit(bytes, registration.copy(sdkFileSHA256 = "00".repeat(32)), checkedSource)
        }
        // This is the actual valid SDK partner token, not a fabricated native
        // token. The native-file admission seam categorically refuses it.
        val foreign = read(sourceDir, "healthy.final.sfcp2", 66080)
        refused("FOREIGN_NATIVE_CHECKPOINT_REFUSED_BEFORE_DECODER") {
            NativeAacRecorderFileAdmission.admit(bytes, registration, checkedSource, foreign)
        }
        val malformed = bytes.copyOf().also { it[0] = 0; it[1] = 0; it[2] = 0; it[3] = 0 }
        refused("MALFORMED_NATIVE_BOX_REFUSED_BEFORE_DECODER") {
            NativeAacRecorderFileAdmission.admit(malformed, registration.copy(fileSHA256 = sha(malformed)), checkedSource)
        }
        check(cleanupKnown && readerDecoderCreates == before)
        val observedPackets = JSONArray()
        for (packet in actual.packets) observedPackets.put(JSONObject().put("offset", packet.offset).put("size", packet.size)
            .put("codedDts", packet.dts).put("codedPts", packet.pts).put("codedDuration", packet.duration)
            .put("codedTimescale", 48000).put("sha256", packet.sha256))
        val provenance = JSONObject().put("scope", "CHECKED_NATIVE_RECORDER_FILE_AND_SEPARATE_SDK_GEOMETRY; NOT_NATIVE_SOURCE_AUTHORITY")
            .put("sdkPartner", JSONObject(sdk.provenance.toString())).put("nativeFileSHA256", actual.sha256)
            .put("nativeFileMovieDuration", actual.movieDuration).put("nativeFileEdits", actual.edits)
            .put("nativeFileHasSFCP2", false).put("nativeCheckpointDecodeCallsReturned", 0)
            .put("sourceGeneration", sdk.generation).put("nativeSourceQualified", false)
            .put("actualNativePackets", observedPackets).put("registeredWriter", reference)
        return sdk.copy(provenance = provenance)
    }

    private fun copyPairFile(sourceDir: File, directory: File, name: String, expectedHash: String) {
        val file = File(directory, name); check(file.createNewFile())
        val stream = acquire("pair.copy.open:$name") { FileOutputStream(file) }; retained.add(stream)
        try { stream.write(read(sourceDir, name, 4 * 1024 * 1024)); stream.fd.sync() }
        finally { if (close("pair.copy.close:$name") { stream.close() }) retained.remove(stream) }
        check(cleanupKnown) { "Pair file close unknown" }
        check(sha(read(directory, name, 4 * 1024 * 1024)) == expectedHash)
    }

    private fun runNativePair(): JSONObject {
        check(Build.HARDWARE in setOf("ranchu", "goldfish") && ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN)
        val bytes = read(input, "manifest.json", 512 * 1024); check(sha(bytes) == manifestSha256)
        val manifest = JSONObject(String(bytes, Charsets.UTF_8))
        check(manifest.getString("schema") == "generated-native-aac-paired-reader-input-v1" && manifest.getBoolean("generatedOnly"))
        check(manifest.getString("pairedNativePresentation") == "UNSUPPORTED_W3_NATIVE_TRIM_AND_CLOCK")
        val names = listOf("seed-79225-programme-48000", "seed-79225-programme-2765", "seed-149120-programme-48000", "seed-149120-programme-2765")
        val cases = manifest.getJSONArray("cases"); check(cases.length() == names.size)
        val results = JSONArray(); var failure: String? = null
        val observations = NativeAacPairObservationStatus(names.drop(1).flatMap { name ->
            listOf("$name:sdkFileReader", "$name:nativeFileReader")
        })
        fun observe(name: String, role: String, value: JSONObject) {
            observations.record("$name:$role", NativeAacPairObservationStatus.Observation(
                acquiredResourcesClosed = value.getBoolean("acquiredResourcesClosed"),
                failure = value.opt("failure") as? String,
                inputEos = value.getBoolean("inputEos"), outputEos = value.getBoolean("outputEos"),
                streamClosed = value.getBoolean("streamClosed"), pcmAvailable = value.has("pcmSHA256"),
            ))
            failure = failure ?: observations.firstFailure
        }
        try {
            for (i in 0 until cases.length()) {
                active(); val source = cases.getJSONObject(i); val name = source.getString("name"); check(name == names[i])
                val sourceDir = File(input, name); check(sourceDir.canonicalFile.parentFile == input.canonicalFile && !Files.isSymbolicLink(sourceDir.toPath()))
                val files = source.getJSONObject("files"); check(files.length() in 1..16)
                for (key in files.keys()) check(sha(read(sourceDir, key, 4 * 1024 * 1024)) == files.getString(key))
                val row = JSONObject().put("name", name).put("sourceManifestCase", source).put("sourceOutcome", source.getString("sourceOutcome"))
                    .put("nativeSourceQualified", false).put("nativeWindowQualified", false).put("presentationEquivalent", false).put("appAdmission", false)
                results.put(row)
                if (!source.getBoolean("eligible")) { check(i == 0 && source.getString("sourceOutcome") == "FAILED"); row.put("classification", "RETAINED_FAILED_SOURCE_NO_REPLAY"); continue }
                check(i > 0 && source.getString("sourceOutcome") == "CANDIDATE_QUALIFIED")
                val observed = JSONObject(String(read(sourceDir, "receipt.json", 4 * 1024 * 1024), Charsets.UTF_8))
                check(observed.getBoolean("generatedOnly") && observed.getBoolean("startIssued") && observed.getBoolean("inputEos") && observed.getBoolean("outputEos") && observed.getBoolean("cleanupConfirmed") && observed.isNull("failure"))
                val closure = observed.getJSONObject("encoderClosure"); check(closure.getBoolean("stopReturned") && closure.getBoolean("releaseReturned"))
                val q = source.getJSONObject("queued")
                check(q.getLong("preparation") == observed.getLong("successfulPreambleFrames") && q.getLong("programmeRaw") == observed.getLong("generatedProgrammeRawFrames") && q.getLong("programmeIntentionalZero") == observed.getLong("intentionalProgrammeZeroFrames"))
                check(q.getLong("blockPadding") == observed.getLong("successfulBlockPaddingFrames") && q.getLong("codecDrain") == observed.getLong("successfulCodecDrainFrames"))
                val original = read(sourceDir, "buffers.bin", 4 * 1024 * 1024); val packets = source.getJSONArray("packets"); check(packets.length() in 1..128)
                for (pi in 0 until packets.length()) {
                    val p = packets.getJSONObject(pi); val at = p.getInt("offset"); val size = p.getInt("size")
                    check(at >= 0 && size in 1..65536 && at.toLong() + size <= original.size && sha(original.copyOfRange(at, at + size)) == p.getString("sha256"))
                    check(p.getLong("codedPts") == p.getLong("codedDts") && p.getInt("codedDuration") == 1024 && p.getInt("codedTimescale") == 48000)
                    check(p.getLong("codedDts") == Math.addExact(source.getLong("codedOrigin"), Math.multiplyExact(pi.toLong(), 1024L)))
                    Math.multiplyExact(p.getLong("codedPts"), 1_000_000L)
                    check(p.getInt("flags") and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                }
                val reference = source.getJSONObject("readerFile"); check(reference.getLong("sourceGeneration") == (i + 1).toLong())
                val sdkRefusals = JSONArray(); val nativeRefusals = JSONArray()
                row.put("sdkPreDecoderRefusals", sdkRefusals).put("nativePreDecoderRefusals", nativeRefusals)
                val beforeSdkCheckpoint = readerCheckpointDecodes
                val sdk = checkedReader(sourceDir, source, reference, sdkRefusals)
                val sdkCheckpointReturns = readerCheckpointDecodes - beforeSdkCheckpoint
                check(sdkCheckpointReturns == 1)
                val beforeNativeCheckpoint = readerCheckpointDecodes
                val native = nativeReader(sourceDir, source, sdk, nativeRefusals)
                val nativeCheckpointReturns = readerCheckpointDecodes - beforeNativeCheckpoint
                check(cleanupKnown && nativeCheckpointReturns == 0)
                val directory = File(root, name); check(directory.mkdir())
                copyPairFile(sourceDir, directory, "healthy.mp4", reference.getString("fileSHA256"))
                copyPairFile(sourceDir, directory, "native-recorder.mp4", source.getJSONObject("nativeRecorderFile").getString("fileSHA256"))
                row.put("sdkCheckpointDecodeCallsReturned", sdkCheckpointReturns).put("nativeCheckpointDecodeCallsReturned", nativeCheckpointReturns)
                val sdkObservation = decode(directory, "healthy", signedPresentation = true, outputName = "sdk-file", reader = sdk, expectedDecoderName = sdk.decoderName)
                row.put("sdkFileReader", sdkObservation); observe(name, "sdkFileReader", sdkObservation)
                check(cleanupKnown && observations.mayContinue) { "SDK pair acquired decoder/file closure unknown" }
                val nativeObservation = decode(directory, "native-recorder", signedPresentation = true, outputName = "native-file", reader = native, expectedDecoderName = sdk.decoderName)
                row.put("nativeFileReader", nativeObservation); observe(name, "nativeFileReader", nativeObservation)
                check(cleanupKnown && observations.mayContinue) { "Native pair acquired decoder/file closure unknown" }
                check(sha(read(directory, "healthy.mp4", 4 * 1024 * 1024)) == reference.getString("fileSHA256"))
                check(sha(read(directory, "native-recorder.mp4", 4 * 1024 * 1024)) == source.getJSONObject("nativeRecorderFile").getString("fileSHA256"))
                row.put("classification", if (sdkObservation.getBoolean("observationCompleted") && nativeObservation.getBoolean("observationCompleted"))
                    "PAIRED_DECODER_OBSERVATIONS_PENDING_EXTERNAL_WHOLE_ARRAY_AND_EXACT_GATES" else "FAILED_DECODER_OBSERVATION")
                writeJson(File(directory, "receipt.json"), row); check(cleanupKnown)
            }
        } catch (error: Exception) { failure = failure ?: (error.javaClass.simpleName + ": " + error.message?.take(512)) }
        return JSONObject().put("schema", "generated-native-aac-paired-reader-v1").put("generatedOnly", true).put("emulatorOnly", true)
            .put("hardware", Build.HARDWARE).put("sdk", Build.VERSION.SDK_INT).put("fingerprint", Build.FINGERPRINT)
            .put("inputManifestSHA256", manifestSha256).put("sourceProofs", manifest.getJSONObject("sourceProofs")).put("cases", results)
            .put("operations", journal).put("encoderCreated", false).put("sdkMuxerCreated", false).put("newSdkFileCreated", false)
            .put("programmeModeExecutedOnDevice", false).put("currentRustWindowExercised", false)
            .put("sdkCheckpointDecodeCallsReturned", readerCheckpointDecodes).put("nativeCheckpointDecodeCallsReturned", 0)
            .put("allCasesVisited", results.length() == 4).put("decoderCreateAttempts", readerDecoderCreates).put("plannedDecoderObservations", 6)
            .put("decoderObservationsRecorded", observations.observationsRecorded).put("completedDecoderObservations", observations.completedObservations)
            .put("allDecoderObservationsComplete", observations.allDecoderObservationsComplete && failure == null)
            .put("cleanupConfirmed", cleanupKnown && retained.isEmpty()).put("failure", failure ?: JSONObject.NULL).put("cancelled", cancelled.get())
            .put("rawNativeClockGatePassed", false).put("nativeSourceQualified", false).put("contentQualified", false)
            .put("nativeWindowQualified", false).put("presentationEquivalent", false).put("appAdmission", false).put("stockFileInteropRestored", false)
            .put("pairedNativePresentation", "UNQUALIFIED_NEW_NATIVE_FILE_PAIR; W3_UNSUPPORTED_RETAINED")
    }

    private fun decode(directory: File, kind: String, signedPresentation: Boolean = false, omitInputEos: Boolean = false, outputName: String = kind,
        reader: ReaderRequest? = null, expectedDecoderName: String? = null): JSONObject {
        val result = JSONObject().put("inputEos", false).put("outputEos", false).put("cleanupConfirmed", false)
        val formats = JSONArray(); result.put("formats", formats)
        val rows = JSONArray(); result.put("buffers", rows)
        var extractor: MediaExtractor? = null; var native: MediaCodec? = null; var stream: FileOutputStream? = null
        var startAttempted = false; var started = false; var inputEos = false; var outputEos = false; var frames = 0
        var inputPackets = 0; var releaseAttempts = 0; var releaseReturned = 0; var formatSeen = false
        var stopReturned = false; var decoderReleased = false; var extractorReleased = false; var streamClosed = false
        var nativeIndexUncertain = false
        var checkedOutputOwner: NativeRecordingReader? = null
        var inputEosSuppressed = false; var suppressedInputIndex: Int? = null
        var suppressedObservationDeadline = Long.MAX_VALUE; var suppressedObservationExpired = false
        var outputDequeuesAfterSuppression = 0
        var lastQueuedPts = 0L; var exactQueueMapping = true
        result.put("readerPolicy", if (reader == null) "UNCHANGED_EXTRACTOR_FORMAT_AND_PTS" else "EXPLICIT_SDK_GEOMETRY_NATIVE_TRIM_AND_MOVIE_MAPPING")
        if (reader != null) result.put("configuredReaderHead", reader.head).put("configuredReaderTail", reader.tail)
            .put("absoluteEditMediaTime", reader.mediaTime).put("queueTimeMapping", "FLOOR((ORIGINAL_CODED_PTS_MINUS_ABSOLUTE_EDIT)*1000000/48000); REMAINDER_RECORDED")
        try {
            cleanupKnown = false
            val source = call("extractor.create:$kind") { MediaExtractor() }; extractor = source; retained.add(source); cleanupKnown = true
            call("extractor.source:$kind") { source.setDataSource(File(directory, "$kind.mp4").path) }
            check(source.trackCount == 1); val extracted = source.getTrackFormat(0)
            check(extracted.getString(MediaFormat.KEY_MIME) == MediaFormat.MIMETYPE_AUDIO_AAC)
            formats.put(format("extractor-original", extracted)); source.selectTrack(0)
            // Original modes request PCM16 only. Explicit reader overrides are
            // declared application policy, never reported native encoder truth.
            extracted.setInteger(MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
            if (reader != null) {
                extracted.setInteger("encoder-delay", reader.head)
                extracted.setInteger("encoder-padding", reader.tail)
            }
            formats.put(format("decoder-requested", extracted))
            cleanupKnown = false
            if (mode == Mode.EXPLICIT_READER || mode == Mode.NATIVE_PAIR) readerDecoderCreates++
            val codec = call("decoder.create:$kind") { MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC) }
            native = codec; retained.add(codec); cleanupKnown = true
            result.put("decoderName", codec.name)
            if (expectedDecoderName != null) check(codec.name == expectedDecoderName) { "Selected native decoder component changed" }
            call("decoder.configure:$kind") { codec.configure(extracted, null, null, 0) }
            formats.put(format("decoder-accepted", codec.inputFormat))
            startAttempted = true; call("decoder.start:$kind") { codec.start() }; started = true
            cleanupKnown = false // A throwing stream acquisition has no confirmed returned owner.
            stream = FileOutputStream(File(directory, "$outputName.s16le")); retained.add(checkNotNull(stream)); cleanupKnown = true
            if (reader != null) {
                val owner = NativeRecordingReader(codec, source, checkNotNull(stream), reader.generation,
                    ReturnedPcmClock.Geometry(reader.mediaTime, 0, reader.expectedFrames), ::event, kind)
                checkedOutputOwner = owner; retained.add(owner)
                result.put("checkedSdkPlaybackClock", JSONObject().put("profile", "checked-sdk-playback-clock-v1")
                    .put("admission", "UNKNOWN").put("provenance", reader.provenance).put("rawNativeClockGatePassed", false)
                    .put("ownershipHandoff", "AFTER_SUCCESSFUL_CODEC_START_AND_STREAM_OPEN_ON_SAME_WORKER")
                    .put("ownerScope", "ACTUAL_INPUT_QUEUE_OUTPUT_AND_TERMINAL_HANDLES; CONFIGURATION_INPUT_PREPARATION_AND_FORMAT_OBSERVATION_REMAIN_CALLER_OWNED")
                    .put("ownerBinding", "STRONG_CODEC_REFERENCE_AND_SOURCE_GENERATION").put("actualDecoderName", result.getString("decoderName")))
            }
            var loops = 0; var lastPts = 0L
            val until = System.nanoTime() + 12_000_000_000L
            while (!outputEos) {
                active()
                if (inputEosSuppressed && System.nanoTime() >= suppressedObservationDeadline) { suppressedObservationExpired = true; break }
                check(System.nanoTime() < until && ++loops <= 1024) { "Native replay decoder deadline" }
                if (!inputEos && !inputEosSuppressed) {
                    val index = call("decoder.dequeueInput:$kind") { codec.dequeueInputBuffer(1000) }
                    if (index >= 0) {
                        val buffer = checkNotNull(codec.getInputBuffer(index)); buffer.clear()
                        val size = source.readSampleData(buffer, 0)
                        check(size <= 65536)
                        val eos = size < 0
                        val pts = if (eos) lastPts else source.sampleTime.also { if (!signedPresentation) check(it >= 0); lastPts = it }
                        val sampleTrack = source.sampleTrackIndex
                        val sampleSize = source.sampleSize
                        val sampleFlags = source.sampleFlags
                        check(eos || (sampleTrack == 0 && sampleSize == size.toLong() && sampleFlags and 6 == 0))
                        if (eos && omitInputEos) {
                            inputEosSuppressed = true; suppressedInputIndex = index
                            suppressedObservationDeadline = System.nanoTime() + 200_000_000L
                            result.put("deliberateMissingInputEos", true)
                            // Hold this actual borrowed index until checked stop.
                            // Continue real output dequeue/release below. EOF is
                            // whatever the native decoder actually reports, never
                            // assigned from this deliberate input omission.
                        }
                        if (!inputEosSuppressed) {
                        check(eos || inputPackets < 128)
                        val payload = if (eos) ByteArray(0) else ByteArray(size).also { buffer.duplicate().apply { position(0); limit(size) }.get(it) }
                        var queuedPts = if (eos && reader != null) lastQueuedPts else pts
                        val inputRow = JSONObject().put("type", "INPUT").put("index", index).put("size", if (eos) 0 else size)
                            .put("extractorPtsUs", pts).put("sampleTrackIndex", sampleTrack).put("sampleSize", sampleSize).put("extractorFlags", sampleFlags)
                            .put("queuedCodecFlags", if (eos) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0).put("sha256", sha(payload)).put("eos", eos)
                        if (reader != null) {
                            if (eos) check(inputPackets == reader.packets.length()) { "Selected native packet set incomplete" }
                            else {
                                check(inputPackets < reader.packets.length())
                                val original = reader.packets.getJSONObject(inputPackets)
                                check(size == original.getInt("size") && sha(payload) == original.getString("sha256")) { "Actual file payload/index differs from checked original" }
                                val ticks = Math.subtractExact(original.getLong("codedPts"), reader.mediaTime)
                                val numerator = Math.multiplyExact(ticks, 1_000_000L)
                                queuedPts = Math.floorDiv(numerator, 48_000L)
                                val remainder = Math.floorMod(numerator, 48_000L); exactQueueMapping = exactQueueMapping && remainder == 0L
                                inputRow.put("originalCodedPts", original.getLong("codedPts")).put("originalCodedDts", original.getLong("codedDts"))
                                    .put("originalCodedTimescale", 48000).put("originalCodedDuration", original.getInt("codedDuration"))
                                    .put("originalPacketSequence", original.getInt("originalSequence")).put("originalEncoderOutputFlags", original.getInt("flags"))
                                    .put("moviePresentationTicks", ticks).put("microsecondNumerator", numerator).put("microsecondDenominator", 48000)
                                    .put("microsecondFloorRemainder", remainder).put("queueMappingExactlyRepresentable", remainder == 0L)
                            }
                        }
                        val owner = checkedOutputOwner
                        if (owner != null) {
                            active()
                            owner.queueInput(index, 0, if (eos) 0 else size, queuedPts, if (eos) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0)
                            lastQueuedPts = queuedPts
                            // The real queue returned before any caller JSON work.
                            if (eos) inputEos = true else inputPackets++
                            rows.put(inputRow.put("nativePtsUs", queuedPts).put("queueReturned", true))
                            if (!eos) source.advance()
                        } else {
                        call("decoder.queueInput:$kind") { codec.queueInputBuffer(index, 0, if (eos) 0 else size, queuedPts, if (eos) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0) }
                        lastQueuedPts = queuedPts
                        rows.put(inputRow.put("nativePtsUs", queuedPts).put("queueReturned", true))
                        if (eos) inputEos = true else { inputPackets++; source.advance() }
                        }
                        }
                    }
                }
                val info = MediaCodec.BufferInfo()
                val index = call("decoder.dequeueOutput:$kind") { codec.dequeueOutputBuffer(info, 1000) }
                if (inputEosSuppressed) outputDequeuesAfterSuppression++
                if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    val output = codec.outputFormat; formats.put(format("decoder-output", output))
                    check(!formatSeen && output.getInteger("sample-rate") == 48000 && output.getInteger("channel-count") == 1)
                    val explicit = output.containsKey("pcm-encoding")
                    check(!explicit || output.getInteger("pcm-encoding") == AudioFormat.ENCODING_PCM_16BIT)
                    result.put("pcmEncodingInterpretation", if (explicit) "PRESENT_PCM16" else "ABSENT_DOCUMENTED_PCM16_DEFAULT")
                    checkedOutputOwner?.acceptOutputFormat(output)
                    formatSeen = true
                } else if (index >= 0) {
                    val owner = checkedOutputOwner
                    if (owner != null) {
                        val output = owner.consumeOutput(index, info)
                        check(rows.length() < 384 && formatSeen)
                        frames += info.size / 2
                        outputEos = output.eos
                        rows.put(JSONObject().put("type", "OUTPUT").put("index", index).put("capacity", output.capacity ?: JSONObject.NULL)
                            .put("offset", info.offset).put("size", info.size).put("nativePtsUs", info.presentationTimeUs).put("flags", info.flags)
                            .put("sha256", output.sha256).put("eos", outputEos).put("cumulativeFrames", frames)
                            .put("checkedSdkPlaybackPosition", schedulePosition(output.position)).put("checkedSdkPlaybackBorrowSerial", output.borrowSerial)
                            .put("sinkBytesReturned", output.sinkBytesReturned).put("sinkWriteReturns", output.sinkWriteReturns))
                    } else {
                    try {
                        check(rows.length() < 384 && formatSeen && info.offset >= 0 && info.size >= 0 && info.size % 2 == 0)
                        check(info.flags and (MediaCodec.BUFFER_FLAG_CODEC_CONFIG or MediaCodec.BUFFER_FLAG_PARTIAL_FRAME) == 0)
                        check(frames.toLong() + info.size / 2 <= 65536)
                        val payload = ByteArray(info.size)
                        val actualOutputBuffer = codec.getOutputBuffer(index)?.duplicate()
                        if (info.size > 0) {
                            val buffer = checkNotNull(actualOutputBuffer)
                            check(info.offset.toLong() + info.size <= buffer.capacity()); buffer.position(info.offset); buffer.limit(info.offset + info.size); buffer.get(payload)
                            checkNotNull(stream).write(payload)
                        }
                        frames += info.size / 2
                        outputEos = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                        rows.put(JSONObject().put("type", "OUTPUT").put("index", index).put("capacity", actualOutputBuffer?.capacity() ?: JSONObject.NULL).put("offset", info.offset).put("size", info.size)
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
            }
        } catch (error: Exception) { result.put("failure", error.javaClass.simpleName + ": " + error.message?.take(512)) }
        finally {
            val owner = checkedOutputOwner
            if (owner != null) {
                owner.closeOwned()
                releaseAttempts = owner.releaseAttempts; releaseReturned = owner.releaseReturned
                stopReturned = owner.stopReturned; decoderReleased = owner.decoderReleased
                extractorReleased = owner.extractorReleased; streamClosed = owner.streamClosed
                nativeIndexUncertain = owner.nativeIndexUncertain
                if (decoderReleased) native?.let { retained.remove(it) }
                if (extractorReleased) extractor?.let { retained.remove(it) }
                if (streamClosed) stream?.let { retained.remove(it) }
                if (owner.cleanupConfirmed && owner.streamSynced && owner.observationFailure == null && owner.clockBookkeepingFailure == null) retained.remove(owner) else cleanupKnown = false
                val profile = result.getJSONObject("checkedSdkPlaybackClock")
                profile.put("schedule", scheduleSnapshot(owner.snapshot()))
                    .put("sinkBytesReturned", owner.sinkBytesReturned).put("sinkWriteReturns", owner.sinkWriteReturns)
                    .put("actualInputEosQueueReturned", owner.inputEosReturned).put("inputQueueUncertain", owner.inputQueueUncertain)
                    .put("actualInputQueueAttempts", owner.inputQueueAttempts).put("actualInputQueueReturns", owner.inputQueueReturns)
                    .put("actualInputMediaQueueReturns", owner.inputMediaQueueReturns)
                    .put("actualNativeOwnershipUnknown", owner.actualNativeOwnershipUnknown)
                    .put("clockBookkeepingFailure", owner.clockBookkeepingFailure?.let { it.javaClass.simpleName + ": " + it.message?.take(256) } ?: JSONObject.NULL)
                    .put("journalFailure", owner.observationFailure?.let { it.javaClass.simpleName + ": " + it.message?.take(256) } ?: JSONObject.NULL)
            } else {
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
        }
        result.put("inputEosSuppressed", inputEosSuppressed).put("suppressedBorrowedInputIndex", suppressedInputIndex ?: JSONObject.NULL)
            .put("suppressedObservationBoundNs", if (omitInputEos) 200_000_000L else JSONObject.NULL)
            .put("suppressedObservationDeadlineExpired", suppressedObservationExpired).put("actualOutputDequeuesAfterSuppression", outputDequeuesAfterSuppression)
            .put("signedPresentationAllowed", signedPresentation).put("deliberateMissingInputEos", omitInputEos).put("pcmFile", "$outputName.s16le")
            .put("allMediaQueueMappingsExactlyRepresentable", if (reader != null) exactQueueMapping else JSONObject.NULL)
            .put("inputEos", inputEos).put("outputEos", outputEos).put("frames", frames).put("inputPackets", inputPackets)
            .put("releaseAttempts", releaseAttempts).put("releaseReturned", releaseReturned).put("stopReturned", stopReturned)
            .put("stopSkippedNotStarted", !started && !startAttempted).put("decoderReleased", decoderReleased).put("extractorReleased", extractorReleased).put("streamClosed", streamClosed)
            .put("cleanupConfirmed", decoderReleased && extractorReleased && streamClosed && !nativeIndexUncertain)
        val pcm = File(directory, "$outputName.s16le")
        val pcmReadbackAllowed = NativeAacPairObservationStatus.pcmReadbackAllowed(
            acquisitionsKnown = cleanupKnown, streamAcquired = stream != null, streamClosed = streamClosed)
        result.put("pcmReadback", if (pcmReadbackAllowed) "ACQUISITION_KNOWN_AND_NO_STREAM_OR_CHECKED_CLOSE" else "NO_FILE_READBACK")
        if (pcmReadbackAllowed) {
            if (pcm.isFile && pcm.length() <= 131072) result.put("pcmBytes", pcm.length()).put("pcmSHA256", sha(pcm.readBytes()))
            if (checkedOutputOwner != null) result.getJSONObject("checkedSdkPlaybackClock").put("pcmReadback", "AFTER_KNOWN_STREAM_CLOSE")
        } else {
            // Unknown acquisition or stream close forbids even stat/read/hash.
            // There need not be a returned stream/output owner to retain.
            if (checkedOutputOwner != null) result.getJSONObject("checkedSdkPlaybackClock").put("pcmReadback", "NO_FILE_READBACK")
        }
        val acquiredResourcesClosed = NativeAacPairObservationStatus.acquiredResourcesClosed(
            acquisitionsKnown = cleanupKnown,
            decoderAcquired = native != null, decoderReleased = decoderReleased,
            extractorAcquired = extractor != null, extractorReleased = extractorReleased,
            streamAcquired = stream != null, streamClosed = streamClosed,
            nativeIndexUncertain = nativeIndexUncertain,
        )
        val observation = NativeAacPairObservationStatus.Observation(acquiredResourcesClosed,
            result.opt("failure") as? String, inputEos, outputEos, streamClosed, result.has("pcmSHA256"))
        result.put("streamAcquired", stream != null).put("acquiredResourcesClosed", acquiredResourcesClosed)
            .put("observationCompleted", observation.completed)
        return result
    }
}
