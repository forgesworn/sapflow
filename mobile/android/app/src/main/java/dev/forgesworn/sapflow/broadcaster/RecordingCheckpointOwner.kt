package dev.forgesworn.sapflow.broadcaster

import android.os.Build
import dev.forgesworn.sapflow.DecodedRecordingCheckpoint
import dev.forgesworn.sapflow.DecodedRecordingCheckpointInfo
import dev.forgesworn.sapflow.EncodedRecordingCheckpointInfo
import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeAudioFileReceipt
import dev.forgesworn.sapflow.NativeAudioGenerationReceipt
import dev.forgesworn.sapflow.NativeRecordingCheckpoint
import dev.forgesworn.sapflow.NativeRecordingChunkKind
import dev.forgesworn.sapflow.NativeRecordingCompletion
import dev.forgesworn.sapflow.NativeRecordingMuxer
import dev.forgesworn.sapflow.NativeRecordingPresentationExtent
import dev.forgesworn.sapflow.decodeRecordingCheckpoint
import dev.forgesworn.sapflow.validateRecordingCheckpointSuccessor
import java.nio.file.Path
import java.util.UUID

/** Optional SDK/file compound owner on the EXISTING recording worker/Writer.
 * No capture/default wiring, second reservation, media queue or native facts.
 * Supplied head/generation/file values are trusted caller input, not F0 proof.
 * Even COMPLETE SDK output is submitted-input scope with UNKNOWN app admission. */
internal class RecordingCheckpointOwner private constructor(
    private val writer: RecordingCatalogue.Writer,
    private val epoch: ULong,
) {
    enum class ApplicationAdmission { UNKNOWN }
    enum class Scope { SUBMITTED_SDK_INPUTS_ONLY }
    @ConsistentCopyVisibility
    data class Frontier internal constructor(val owner: UUID, val sequence: Long,
        val fileEnd: Long, val fragmentBytes: Long, val audioUnits: ULong,
        val videoUnits: ULong, val audioContainerEnd: ULong, val videoEnd: ULong?,
        val completeAudioEof: Boolean)
    @ConsistentCopyVisibility
    data class StoredInputsClosure internal constructor(val scope: Scope,
        val applicationAdmission: ApplicationAdmission, val sdkOutcome: NativeRecordingCompletion?,
        val bytes: Long, val headerRewritten: Boolean, val movieTimescale: UInt?,
        val durationTicks: ULong?, val durationMsCeil: ULong?, val completeAudioEof: Boolean,
        val cancelRequestedBeforePublication: Boolean)

    private class NativeCloseUnconfirmed(cause: Exception) : IllegalStateException("Native handle closure is unconfirmed", cause)
    /** A throwing native handle close is retained and never attempted again. */
    private class Handle<T : AutoCloseable>(val value: T) {
        private var attempted = false
        private var closed = false
        private var failure: Exception? = null
        fun closeChecked() {
            failure?.let { throw it }
            if (closed) return
            check(!attempted) { "Native handle closure is unconfirmed" }
            attempted = true
            try { value.close(); closed = true }
            catch (error: Exception) {
                val unknown = NativeCloseUnconfirmed(error)
                failure = unknown; throw unknown
            }
        }
    }
    private enum class Phase { ACTIVE, FAILED, CLOSING, RETAINED, CLOSED }
    private val worker = Thread.currentThread()
    private val identity = UUID.randomUUID()
    private val guard = Any()
    private var phase = Phase.ACTIVE
    private var busy = false
    private var cancelled = false
    private var published: Frontier? = null
    private var result: StoredInputsClosure? = null
    private var closeFailure: Exception? = null
    private var storage: RecordingCheckpointStorage? = null
    private var muxer: Handle<NativeRecordingMuxer>? = null
    private var nativeCancelAttempted = false
    private var nativeCancelled = false
    private var leadingSupplied = false
    private var fileReceiptSupplied = false
    private var finishOutcome: NativeRecordingCompletion? = null
    private var outputDrainedAfterFinish = false
    private var initialLength = 0
    private var deferredHeader: ByteArray? = null
    private var installedReceipt: RecordingCheckpointStorage.InstalledCheckpoint? = null
    private var installedToken: Handle<NativeRecordingCheckpoint>? = null
    private var installedDecoded: Handle<DecodedRecordingCheckpoint>? = null
    // At most the current output token, its prepared metadata and one validator
    // handle. These are ownership references, not another media/staging queue.
    private var pendingToken: Handle<NativeRecordingCheckpoint>? = null
    private var preparedDecoded: Handle<DecodedRecordingCheckpoint>? = null
    private var validatorDecoded: Handle<DecodedRecordingCheckpoint>? = null

    val frontier: Frontier? get() = synchronized(guard) { published }
    val cleanupComplete: Boolean get() = synchronized(guard) { phase == Phase.CLOSED } && writer.cleanupComplete

    /** Nonblocking scalar fence. Actual IO/native cleanup stays on the worker. */
    fun cancel() {
        synchronized(guard) { if (phase == Phase.CLOSED) return; cancelled = true }
        writer.cancel()
    }
    private fun onWorker() { check(Thread.currentThread() === worker) { "SDK recording has another worker" } }
    private fun requireAdmission() {
        check(synchronized(guard) { phase == Phase.ACTIVE && busy && !cancelled }) { "SDK recording admission is closed" }
    }
    private inline fun <T> perform(body: () -> T): T {
        onWorker()
        synchronized(guard) {
            check(phase == Phase.ACTIVE && !cancelled) { "SDK recording is closed" }
            check(!busy) { "SDK recording operation is already active" }; busy = true
        }
        try { return body() }
        catch (error: Exception) {
            synchronized(guard) {
                if (error is NativeCloseUnconfirmed) { phase = Phase.RETAINED; closeFailure = error }
                else phase = Phase.FAILED
            }
            if (error is NativeCloseUnconfirmed) writer.retainUncertainCleanup()
            writer.cancel(); throw error
        } finally { synchronized(guard) { busy = false } }
    }
    fun checkStorage() = perform { checkNotNull(storage).checkCapacity(); requireAdmission() }

    /** Explicit low-level trusted input. Never call with an inferred zero. */
    fun beginAudio(firstLeadingFrames: UInt) = perform {
        check(!leadingSupplied) { "First native leading fact was already supplied" }
        requireAdmission()
        checkNotNull(muxer).value.beginNativeAudio(firstLeadingFrames)
        leadingSupplied = true; drain()
    }
    /** Caller retains/then closes its own validated sample wrapper. Rust shares
     * its existing payload; no new codec/sample construction or AU validation. */
    fun push(sample: MediaSample) = perform {
        check(leadingSupplied) { "Native first-leading fact is UNKNOWN" }
        check(!fileReceiptSupplied && finishOutcome == null)
        requireAdmission()
        checkNotNull(muxer).value.push(sample); drain()
    }
    /** Must be ordered on the SAME FIFO as all original recording samples. */
    fun generationEnded(receipt: NativeAudioGenerationReceipt) = perform {
        check(leadingSupplied && !fileReceiptSupplied && finishOutcome == null)
        requireAdmission()
        checkNotNull(muxer).value.endNativeAudioGeneration(receipt.copy()); drain()
    }
    /** Supplied receipt is not a concrete Android producer-closure authority. */
    fun completeAudio(receipt: NativeAudioFileReceipt) = perform {
        check(leadingSupplied && !fileReceiptSupplied && finishOutcome == null)
        requireAdmission()
        checkNotNull(muxer).value.completeNativeAudio(receipt.copy())
        fileReceiptSupplied = true; drain()
    }

    private fun drain() {
        val native = checkNotNull(muxer).value
        repeat(MAX_OUTPUTS_PER_OPERATION) {
            requireAdmission()
            check(pendingToken == null && preparedDecoded == null && validatorDecoded == null)
            val chunk = native.takeNativeOutput()
            if (chunk == null) {
                if (finishOutcome != null) outputDrainedAfterFinish = true
                return
            }
            // Retain a returned native token before any profile/storage check.
            pendingToken = chunk.checkpoint?.let { token -> Handle(token) }
            when (chunk.kind) {
                NativeRecordingChunkKind.INITIAL -> {
                    check(initialLength == 0 && pendingToken == null && chunk.bytes.size in 1..RecordingCheckpointStorage.MAX_HEADER)
                    checkNotNull(storage).appendInitial(chunk.bytes)
                    initialLength = chunk.bytes.size
                }
                NativeRecordingChunkKind.FRAGMENT -> installFragment(chunk.bytes)
                NativeRecordingChunkKind.FINAL_HEADER -> {
                    check(initialLength > 0 && pendingToken == null && deferredHeader == null && chunk.bytes.size == initialLength)
                    // Bounded container metadata only; never append/hash as media.
                    deferredHeader = chunk.bytes
                }
            }
        }
        error("SDK output exceeded its bounded synchronous drain")
    }
    private fun installFragment(bytes: ByteArray) {
        val native = checkNotNull(muxer).value
        val token = checkNotNull(pendingToken)
        val facts = token.value.facts()
        check(initialLength > 0 && bytes.size in 1..RecordingCheckpointStorage.MAX_FRAGMENT)
        // Write/hash THIS original fragment once. The storage owner then reads
        // its actual immutable frontier; no cached caller offset arithmetic.
        val file = checkNotNull(storage)
        file.appendFragment(bytes)
        val expected = file.prepareCheckpoint(facts.sequence.toLong())
        requireAdmission()
        val encoded = native.encodeCheckpoint(token.value, expected.recordingBinding.hexBytes(),
            expected.sourceFileBinding.hexBytes(), expected.fragmentRegionSha256.hexBytes())
        check(encoded.info.facts == facts)
        checkExpected(encoded.info, expected)
        val prepared = Handle(decodeRecordingCheckpoint(encoded.bytes,
            expected.recordingBinding.hexBytes(), expected.sourceFileBinding.hexBytes()))
        preparedDecoded = prepared
        checkEqual(prepared.value.info(), encoded.info)
        val prior = installedDecoded
        if (prior == null) check(facts.sequence == 1u)
        else validateRecordingCheckpointSuccessor(prior.value, prepared.value)
        requireAdmission()
        val receipt = file.installCheckpoint(facts.sequence.toLong(), encoded.bytes) { actual, frontier ->
            check(frontier == expected)
            checkExpected(encoded.info, frontier)
            val decoded = Handle(decodeRecordingCheckpoint(actual,
                frontier.recordingBinding.hexBytes(), frontier.sourceFileBinding.hexBytes()))
            validatorDecoded = decoded
            checkEqual(decoded.value.info(), encoded.info)
            if (prior != null) validateRecordingCheckpointSuccessor(prior.value, decoded.value)
            decoded.closeChecked(); validatorDecoded = null
            requireAdmission()
        }
        check(receipt.expected == expected)
        // Retire old metadata outside the scalar publication lock. A failed
        // close keeps old/new handles and the actual Writer reservation rooted.
        installedToken?.closeChecked(); installedDecoded?.closeChecked()
        requireAdmission()
        installedToken = token; pendingToken = null
        installedDecoded = prepared; preparedDecoded = null
        installedReceipt = receipt
        val next = Frontier(identity, expected.sequence, expected.committedOffset, expected.fragmentRegionBytes,
            facts.audioUnits, facts.videoUnits, facts.audioContainerEnd, facts.videoEnd, facts.completeAudioEof)
        synchronized(guard) {
            check(phase == Phase.ACTIVE && busy && !cancelled)
            published = next // Only after actual slot readback AND checked close.
        }
    }

    /** Finalise submitted SDK inputs only. No method here can mint complete
     * app admission or a finalised catalogue URL, even with COMPLETE native EOF. */
    fun finishStoredInputs(outcome: NativeRecordingCompletion = NativeRecordingCompletion.INCOMPLETE_PREFIX): StoredInputsClosure {
        onWorker()
        synchronized(guard) {
            result?.let { check(it.sdkOutcome == outcome); return it }
        }
        return perform {
            check(leadingSupplied && finishOutcome == null)
            if (outcome == NativeRecordingCompletion.COMPLETE) check(fileReceiptSupplied) { "Native file receipt is UNKNOWN" }
            val native = checkNotNull(muxer).value
            requireAdmission()
            native.finishNative(outcome); finishOutcome = outcome; drain()
            val token = checkNotNull(installedToken).value
            val decoded = checkNotNull(installedDecoded).value
            val receipt = checkNotNull(installedReceipt)
            val header = checkNotNull(deferredHeader)
            val info = decoded.info(); val facts = token.facts(); val stats = native.stats()
            check(outputDrainedAfterFinish && info.facts == facts && header.size == initialLength &&
                header.contentEquals(decoded.header()) && header.contentEquals(native.checkpointHeader(token)))
            checkExpectedInfo(info, receipt.expected)
            check(facts.audioUnits <= ULong.MAX_VALUE - facts.videoUnits)
            check(stats.finished && stats.bufferedSamples == 0uL && stats.bufferedBytes == 0uL &&
                stats.queuedOutputBytes == 0uL && stats.acceptedSamples == facts.audioUnits + facts.videoUnits &&
                stats.fragments == facts.sequence.toULong())
            val extent = token.presentationExtent()
            check(extent.movieTimescale == 720_000u && extent.completeAudioEof == facts.completeAudioEof)
            check(outcome != NativeRecordingCompletion.COMPLETE || facts.completeAudioEof)
            requireAdmission()
            // All native handle cancellation/destruction precedes the file's
            // potential release of the SAME Writer lease. No IO under guard.
            closeNativeOwners()
            requireAdmission()
            val closed = checkNotNull(storage).finishStoredInputs(header, receipt) { actual, expected ->
                check(expected == receipt.expected && actual.contentEquals(header))
                checkExpectedInfo(info, expected)
                requireAdmission()
            }
            publishClosure(closed, outcome, extent)
        }
    }
    private fun publishClosure(closed: RecordingCheckpointStorage.Closure,
        outcome: NativeRecordingCompletion?, extent: NativeRecordingPresentationExtent?): StoredInputsClosure {
        check(writer.cleanupComplete)
        val value = synchronized(guard) {
            StoredInputsClosure(Scope.SUBMITTED_SDK_INPUTS_ONLY, ApplicationAdmission.UNKNOWN, outcome,
                closed.bytes, closed.headerRewritten, extent?.movieTimescale, extent?.durationTicks,
                extent?.durationMsCeil, extent?.completeAudioEof ?: false, cancelled).also {
                phase = Phase.CLOSED; result = it
            }
        }
        return value
    }
    /** Known closure is idempotent. Unknown close retains actual owners and
     * never repeats a raw descriptor or native handle close attempt. */
    fun closeIncomplete(): StoredInputsClosure {
        onWorker()
        synchronized(guard) {
            result?.let { return it }; closeFailure?.let { throw it }
            check(!busy && phase != Phase.CLOSING && phase != Phase.RETAINED)
            busy = true; phase = Phase.CLOSING; cancelled = true
        }
        writer.cancel()
        try {
            closeNativeOwners()
            val file = storage
            val closed = if (file != null) file.closeIncomplete() else {
                // Failed backend construction owns its own checked cleanup.
                // Never bypass an unconfirmed bootstrap with ownersClosed().
                check(writer.cleanupComplete) { "Storage bootstrap closure is unconfirmed" }
                RecordingCheckpointStorage.Closure(0, 0, false)
            }
            return publishClosure(closed, null, null)
        } catch (error: Exception) {
            writer.retainUncertainCleanup()
            synchronized(guard) { closeFailure = error; phase = Phase.RETAINED }
            throw error
        } finally { synchronized(guard) { busy = false } }
    }
    private fun closeNativeOwners() {
        val native = muxer
        if (native != null && !nativeCancelled) {
            check(!nativeCancelAttempted) { "Native cancel is unconfirmed" }
            nativeCancelAttempted = true
            try { native.value.cancel(); nativeCancelled = true }
            catch (error: Exception) { throw NativeCloseUnconfirmed(error) }
        }
        // A closed object is skipped by its own latch. A throwing close is
        // never retried; keep the exact handle reachable on every error path.
        validatorDecoded?.closeChecked(); preparedDecoded?.closeChecked()
        pendingToken?.closeChecked(); installedDecoded?.closeChecked(); installedToken?.closeChecked()
        native?.closeChecked()
    }
    private fun checkExpected(info: EncodedRecordingCheckpointInfo, expected: RecordingCheckpointStorage.ExpectedCheckpoint) {
        check(info.facts.sequence.toLong() == expected.sequence && info.mediaEpochNs == epoch && info.initialHeaderLen.toLong() == expected.initialHeaderLength.toLong() &&
            info.initialHeaderSha256.contentEquals(expected.initialHeaderSha256.hexBytes()) &&
            info.recordingBinding.contentEquals(expected.recordingBinding.hexBytes()) &&
            info.sourceFileBinding.contentEquals(expected.sourceFileBinding.hexBytes()) &&
            info.fragmentRegionSha256.contentEquals(expected.fragmentRegionSha256.hexBytes()) &&
            info.fragmentRegionBytes == expected.fragmentRegionBytes.toULong() && info.committedOffset == expected.committedOffset.toULong())
    }
    private fun checkExpectedInfo(info: DecodedRecordingCheckpointInfo, expected: RecordingCheckpointStorage.ExpectedCheckpoint) {
        check(info.facts.sequence.toLong() == expected.sequence && info.mediaEpochNs == epoch && info.initialHeaderLen.toLong() == expected.initialHeaderLength.toLong() &&
            info.initialHeaderSha256.contentEquals(expected.initialHeaderSha256.hexBytes()) &&
            info.recordingBinding.contentEquals(expected.recordingBinding.hexBytes()) &&
            info.sourceFileBinding.contentEquals(expected.sourceFileBinding.hexBytes()) &&
            info.fragmentRegionSha256.contentEquals(expected.fragmentRegionSha256.hexBytes()) &&
            info.fragmentRegionBytes == expected.fragmentRegionBytes.toULong() && info.committedOffset == expected.committedOffset.toULong())
    }
    private fun checkEqual(a: DecodedRecordingCheckpointInfo, b: EncodedRecordingCheckpointInfo) {
        check(a.facts == b.facts && a.mediaEpochNs == b.mediaEpochNs && a.initialHeaderLen == b.initialHeaderLen &&
            a.initialHeaderSha256.contentEquals(b.initialHeaderSha256) && a.recordingBinding.contentEquals(b.recordingBinding) &&
            a.sourceFileBinding.contentEquals(b.sourceFileBinding) && a.committedOffset == b.committedOffset &&
            a.fragmentRegionBytes == b.fragmentRegionBytes && a.fragmentRegionSha256.contentEquals(b.fragmentRegionSha256))
    }
    private fun String.hexBytes(): ByteArray {
        check(length == 64 && all { it in '0'..'9' || it in 'a'..'f' })
        return ByteArray(32) { i -> substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    }
    companion object {
        private const val MAX_OUTPUTS_PER_OPERATION = 8
        /** Explicit cold opt-in only. Construction and all sample/timing/close
         * calls belong to the SAME existing recording worker/FIFO. The default
         * Android public-Os backend refuses API29 before descriptor IO. A host
         * test may supply actual filesystem IO, never a production fallback. */
        fun open(catalogue: RecordingCatalogue, identity: RecordingCatalogue.Identity,
            intent: CaptureIntent, mediaEpochNs: ULong, maxFragmentBytes: ULong = 4uL * 1024uL * 1024uL,
            maxFragmentMs: UInt = 500u,
            storageFactory: ((Path, (Any) -> Unit) -> RecordingCheckpointStorage.Root)? = null,
        ): RecordingCheckpointOwner {
            require(identity.profileId == intent.profileId && intent.hasAudio)
            require(maxFragmentBytes in 1uL..RecordingCheckpointStorage.MAX_FRAGMENT.toULong())
            require(maxFragmentMs in 1u..60_000u)
            if (storageFactory == null && Build.VERSION.SDK_INT < 30) {
                throw UnsupportedOperationException("Checked SDK recording requires public API30 descriptor storage")
            }
            val writer = catalogue.begin(identity)
            val owner = RecordingCheckpointOwner(writer, mediaEpochNs)
            try {
                owner.storage = writer.checkpointStorage(retainedOwner = owner,
                    factory = storageFactory ?: { path, retain -> AndroidRecordingStorageIO.open(path, retain) })
                owner.muxer = Handle(NativeRecordingMuxer.withNativeAacTiming(intent.hasVideo, true,
                    mediaEpochNs, maxFragmentBytes, maxFragmentMs))
                return owner
            } catch (error: Exception) {
                try { owner.closeIncomplete() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
                throw error
            }
        }
    }
}
