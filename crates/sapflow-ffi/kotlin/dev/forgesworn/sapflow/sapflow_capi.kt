@file:JvmName("Sapflow_ffiKt")

package dev.forgesworn.sapflow

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.lang.ref.Reference

interface Disposable {
    fun destroy()
    companion object {
        fun destroy(vararg args: Any?) = args.forEach { arg ->
            when (arg) {
                is Disposable -> arg.destroy()
                is Map<*, *> -> arg.values.forEach { (it as? Disposable)?.destroy() }
                is Iterable<*> -> arg.forEach { (it as? Disposable)?.destroy() }
            }
        }
    }
}
inline fun <T : Disposable?, R> T.use(block: (T) -> R) = try { block(this) } finally {
    try { this?.destroy() } catch (_: Throwable) {}
}

internal class NativeException(message: String) : Exception(message)
internal object Native {
    init {
        System.loadLibrary("sapflow_ffi")
        System.loadLibrary("sapflow_jni")
        check(abiVersion() == 1) { "Unsupported Sapflow ABI version: ${abiVersion()}" }
    }
    @JvmStatic external fun abiVersion(): Int
    @JvmStatic external fun call(operation: Int, handle: Long, input: ByteArray): ByteArray
    @JvmStatic external fun objectFree(kind: Int, handle: Long)
    @JvmStatic external fun directBufferAddress(buffer: ByteBuffer): Long
    @JvmStatic external fun encodedBufferNew(address: Long, len: Long): Long
    @JvmStatic external fun encodedBufferScalar(buffer: Long, operation: Int, index: Long): Long
    @JvmStatic external fun mediaSampleScalar(sample: Long, operation: Int): Long
    @JvmStatic external fun mediaCodecSample(codec: Long, address: Long, len: Long, pts: Long, dts: Long, duration: Int, timescale: Int, keyframe: Boolean): Long
    @JvmStatic external fun mediaSampleMetadata(sample: Long): LongArray
    @JvmStatic external fun aacTimingViewsSample(views: Long, publishing: Boolean): LongArray?
    @JvmStatic external fun sampleFanoutPush(fanout: Long, sample: Long, nowNs: Long, aacViews: Boolean): LongArray
    @JvmStatic external fun sampleFanoutPop(fanout: Long, recording: Boolean, nowNs: Long): LongArray?
    @JvmStatic external fun recordingPush(muxer: Long, sample: Long)
    @JvmStatic external fun recordingTakeOutput(muxer: Long, mode: Int, metadata: LongArray): ByteArray?
    @JvmStatic external fun rtmpFeed(publisher: Long, address: Long, len: Long, nowMs: Long)
    @JvmStatic external fun rtmpSend(publisher: Long, sample: Long)
    @JvmStatic external fun rtmpTakeOutput(publisher: Long): ByteArray
    @JvmStatic external fun rtmpSendAndTakeOutput(publisher: Long, sample: Long, nowMs: Long, metadata: LongArray): ByteArray
    @JvmStatic external fun rtmpNextDeadlineMs(publisher: Long): LongArray
    @JvmStatic external fun rtmpQueuedOutputBytes(publisher: Long): Long
    @JvmStatic external fun rtmpState(publisher: Long): Long
    @JvmStatic external fun srtPush(publisher: Long, sample: Long, nowUs: Long)
    @JvmStatic external fun srtFeed(publisher: Long, source: ByteArray, address: Long, len: Long, nowUs: Long)
    @JvmStatic external fun srtPollDatagram(publisher: Long, nowUs: Long): Array<Any>?
    @JvmStatic external fun srtIsReady(publisher: Long): Long
    @JvmStatic external fun srtHasImmediateWork(publisher: Long): Long
    @JvmStatic external fun srtNextDeadlineUs(publisher: Long): LongArray
    @JvmStatic external fun srtState(publisher: Long): Long
}

internal class Writer {
    private val out = ByteArrayOutputStream()
    fun u8(v: Int) = out.write(v and 255)
    fun bool(v: Boolean) = u8(if (v) 1 else 0)
    fun u32(v: UInt) = out.writeBytes(v.toInt().toLittle())
    fun u64(v: ULong) = out.writeBytes(v.toLong().toLittle())
    fun i64(v: Long) = out.writeBytes(v.toLittle())
    fun bytes(v: ByteArray) { u64(v.size.toULong()); out.writeBytes(v) }
    fun string(v: String) = bytes(v.toByteArray(Charsets.UTF_8))
    fun handle(v: HandleObject) = u64(v.rawHandle().toULong())
    fun optional(v: HandleObject?) { bool(v != null); if (v != null) handle(v) }
    fun optional(v: ULong?) { bool(v != null); if (v != null) u64(v) }
    fun optional(v: Long?) { bool(v != null); if (v != null) i64(v) }
    fun result(): ByteArray = out.toByteArray()
    private fun Int.toLittle() = ByteArray(4) { (this ushr (it * 8)).toByte() }
    private fun Long.toLittle() = ByteArray(8) { (this ushr (it * 8)).toByte() }
}
internal fun ByteBuffer.toOwnedArray() = duplicate().let { ByteArray(it.remaining()).also(it::get) }
internal inline fun <T> ByteBuffer.borrowDirect(block: (address: Long, len: Long) -> T): T {
    check(isDirect) { "direct buffer required" }
    val base = Native.directBufferAddress(this)
    val length = remaining().toLong()
    require(length == 0L || base != 0L) { "direct buffer has no native address" }
    val address = Math.addExact(base, position().toLong())
    return try {
        block(address, length)
    } finally {
        Reference.reachabilityFence(this)
    }
}
internal class Reader(bytes: ByteArray) {
    private val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
    fun u8(): Int = buf.get().toInt() and 255
    fun bool(): Boolean = when (u8()) { 0 -> false; 1 -> true; else -> throw IllegalStateException("Invalid native boolean") }
    fun u32(): UInt = buf.int.toUInt()
    fun u64(): ULong = buf.long.toULong()
    fun i64(): Long = buf.long
    fun bytes(): ByteArray {
        val n = u64().toInt()
        if (n < 0 || n > buf.remaining()) throw IllegalStateException("Invalid native byte length")
        return ByteArray(n).also(buf::get)
    }
    fun string() = String(bytes(), Charsets.UTF_8)
    fun handle(): Long = u64().toLong()
    fun optionalU64(): ULong? = if (bool()) u64() else null
    fun optionalHandle(): Long? = if (bool()) handle() else null
    fun done() { check(!buf.hasRemaining()) { "Trailing native response bytes" } }
}
internal fun request(block: Writer.() -> Unit) = Writer().apply(block).result()
internal fun <T> response(bytes: ByteArray, block: Reader.() -> T): T = Reader(bytes).run {
    val value = block(); done(); value
}
internal enum class ErrorKind { BINDING, RECORDING, CHECKPOINT, RTMP, SESSION, SRT }
internal inline fun <T> hot(kind: ErrorKind, block: () -> T): T = try {
    block()
} catch (e: NativeException) {
    throw nativeError(kind, e.message ?: "Panic")
}
internal fun invoke(operation: Int, kind: ErrorKind, handle: Long = 0, input: ByteArray = ByteArray(0)): ByteArray = try {
    Native.call(operation, handle, input)
} catch (e: NativeException) {
    throw nativeError(kind, e.message ?: "Panic")
}
internal fun newHandle(operation: Int, objectKind: Int, error: ErrorKind, input: ByteArray) =
    response(invoke(operation, error, input = input)) { handle() }

object NoHandle
abstract class HandleObject internal constructor(private val kind: Int, private var handle: Long) : Disposable, AutoCloseable {
    private var destroyed = false
    val uniffiIsDestroyed: Boolean get() = destroyed
    internal fun rawHandle(): Long = synchronized(this) {
        if (handle == 0L) throw IllegalStateException("Sapflow object is closed")
        handle
    }
    internal inline fun <T> withHandle(block: (Long) -> T): T = synchronized(this) { block(rawHandle()) }
    @Synchronized override fun destroy() {
        if (destroyed) return
        destroyed = true
        if (handle != 0L) { Native.objectFree(kind, handle); handle = 0 }
    }
    @Synchronized override fun close() = destroy()
    @JvmName("finalize") protected fun finalizeSapflow() = close()
}

sealed class BindingException : Exception() {
    class EmptyBuffer : BindingException(); class BufferTooLarge : BindingException()
    class IndexOutOfBounds : BindingException(); class InvalidMedia : BindingException()
    class InvalidQueueLimits : BindingException(); class ClockWentBackwards : BindingException()
    class Busy : BindingException(); class Closed : BindingException()
    class InvalidSession : BindingException(); class StaleSession : BindingException()
    override val message: String get() = ""
}
sealed class NativeRecordingException : Exception() {
    class InvalidConfig : NativeRecordingException(); class InvalidMedia : NativeRecordingException()
    class Busy : NativeRecordingException(); class Backpressure : NativeRecordingException()
    class Closed : NativeRecordingException(); class MissingTracks : NativeRecordingException()
    class Encoding : NativeRecordingException()
    override val message: String get() = ""
}
sealed class RecordingCheckpointException : Exception() {
    class Busy : RecordingCheckpointException(); class ForeignCheckpoint : RecordingCheckpointException()
    class DigestLength : RecordingCheckpointException(); class Envelope : RecordingCheckpointException()
    class Unsupported : RecordingCheckpointException(); class Checksum : RecordingCheckpointException()
    class Binding : RecordingCheckpointException(); class Facts : RecordingCheckpointException()
    class Successor : RecordingCheckpointException(); class Overflow : RecordingCheckpointException()
    override val message: String get() = ""
}
sealed class NativeRtmpException : Exception() {
    class InvalidConfig : NativeRtmpException(); class Busy : NativeRtmpException()
    class Closed : NativeRtmpException(); class NotPublishing : NativeRtmpException()
    class Backpressure : NativeRtmpException(); class InvalidMedia : NativeRtmpException()
    class ProtocolViolation : NativeRtmpException(); class Refused : NativeRtmpException()
    class Timeout : NativeRtmpException(); class UnsupportedHandshake : NativeRtmpException()
    override val message: String get() = ""
}
sealed class NativeSrtException : Exception() {
    class InvalidConfig : NativeSrtException(); class InvalidMedia : NativeSrtException()
    class ConfigurationChanged : NativeSrtException(); class InvalidDatagram : NativeSrtException()
    class ClockWentBackwards : NativeSrtException(); class ClockOverflow : NativeSrtException()
    class Busy : NativeSrtException(); class Backpressure : NativeSrtException()
    class NotConnected : NativeSrtException(); class Closed : NativeSrtException()
    override val message: String get() = ""
}
private fun nativeError(kind: ErrorKind, code: String): Exception = when (kind) {
    ErrorKind.BINDING, ErrorKind.SESSION -> when (code) {
        "EmptyBuffer" -> BindingException.EmptyBuffer(); "BufferTooLarge" -> BindingException.BufferTooLarge()
        "IndexOutOfBounds" -> BindingException.IndexOutOfBounds(); "InvalidMedia" -> BindingException.InvalidMedia()
        "InvalidQueueLimits" -> BindingException.InvalidQueueLimits(); "ClockWentBackwards" -> BindingException.ClockWentBackwards()
        "Busy" -> BindingException.Busy(); "Closed" -> BindingException.Closed()
        "InvalidSession" -> BindingException.InvalidSession(); "StaleSession" -> BindingException.StaleSession()
        else -> IllegalStateException(code)
    }
    ErrorKind.RECORDING -> when (code) {
        "InvalidConfig" -> NativeRecordingException.InvalidConfig(); "InvalidMedia" -> NativeRecordingException.InvalidMedia()
        "Busy" -> NativeRecordingException.Busy(); "Backpressure" -> NativeRecordingException.Backpressure()
        "Closed" -> NativeRecordingException.Closed(); "MissingTracks" -> NativeRecordingException.MissingTracks()
        "Encoding" -> NativeRecordingException.Encoding(); else -> IllegalStateException(code)
    }
    ErrorKind.CHECKPOINT -> when (code) {
        "Busy" -> RecordingCheckpointException.Busy(); "ForeignCheckpoint" -> RecordingCheckpointException.ForeignCheckpoint()
        "DigestLength" -> RecordingCheckpointException.DigestLength(); "Envelope" -> RecordingCheckpointException.Envelope()
        "Unsupported" -> RecordingCheckpointException.Unsupported(); "Checksum" -> RecordingCheckpointException.Checksum()
        "Binding" -> RecordingCheckpointException.Binding(); "Facts" -> RecordingCheckpointException.Facts()
        "Successor" -> RecordingCheckpointException.Successor(); "Overflow" -> RecordingCheckpointException.Overflow()
        else -> IllegalStateException(code)
    }
    ErrorKind.RTMP -> when (code) {
        "InvalidConfig" -> NativeRtmpException.InvalidConfig(); "Busy" -> NativeRtmpException.Busy()
        "Closed" -> NativeRtmpException.Closed(); "NotPublishing" -> NativeRtmpException.NotPublishing()
        "Backpressure" -> NativeRtmpException.Backpressure(); "InvalidMedia" -> NativeRtmpException.InvalidMedia()
        "ProtocolViolation" -> NativeRtmpException.ProtocolViolation(); "Refused" -> NativeRtmpException.Refused()
        "Timeout" -> NativeRtmpException.Timeout(); "UnsupportedHandshake" -> NativeRtmpException.UnsupportedHandshake()
        else -> IllegalStateException(code)
    }
    ErrorKind.SRT -> when (code) {
        "InvalidConfig" -> NativeSrtException.InvalidConfig(); "InvalidMedia" -> NativeSrtException.InvalidMedia()
        "ConfigurationChanged" -> NativeSrtException.ConfigurationChanged(); "InvalidDatagram" -> NativeSrtException.InvalidDatagram()
        "ClockWentBackwards" -> NativeSrtException.ClockWentBackwards(); "ClockOverflow" -> NativeSrtException.ClockOverflow()
        "Busy" -> NativeSrtException.Busy(); "Backpressure" -> NativeSrtException.Backpressure()
        "NotConnected" -> NativeSrtException.NotConnected(); "Closed" -> NativeSrtException.Closed()
        else -> IllegalStateException(code)
    }
}

data class DecodedProgrammeCheckpointInfo(var facts: NativeProgrammeCheckpointFacts, var start: NativeProgrammeWindowStart, var windowEnd: NativeProgrammeGenerationEnd?, var mediaEpochNs: ULong, var initialHeaderLen: UInt, var initialHeaderSha256: ByteArray, var recordingBinding: ByteArray, var sourceFileBinding: ByteArray, var committedOffset: ULong, var fragmentRegionBytes: ULong, var fragmentRegionSha256: ByteArray)
data class DecodedRecordingCheckpointInfo(var facts: NativeRecordingCheckpointFacts, var mediaEpochNs: ULong, var initialHeaderLen: UInt, var initialHeaderSha256: ByteArray, var recordingBinding: ByteArray, var sourceFileBinding: ByteArray, var committedOffset: ULong, var fragmentRegionBytes: ULong, var fragmentRegionSha256: ByteArray)
data class EncodedProgrammeCheckpointInfo(var facts: NativeProgrammeCheckpointFacts, var start: NativeProgrammeWindowStart, var windowEnd: NativeProgrammeGenerationEnd?, var mediaEpochNs: ULong, var initialHeaderLen: UInt, var initialHeaderSha256: ByteArray, var recordingBinding: ByteArray, var sourceFileBinding: ByteArray, var committedOffset: ULong, var fragmentRegionBytes: ULong, var fragmentRegionSha256: ByteArray)
data class EncodedRecordingCheckpointInfo(var facts: NativeRecordingCheckpointFacts, var mediaEpochNs: ULong, var initialHeaderLen: UInt, var initialHeaderSha256: ByteArray, var recordingBinding: ByteArray, var sourceFileBinding: ByteArray, var committedOffset: ULong, var fragmentRegionBytes: ULong, var fragmentRegionSha256: ByteArray)
data class FanoutOutcome(var publishingQueued: Boolean, var recordingQueued: Boolean, var publishingWaitingKeyframe: Boolean, var publishingOverflows: ULong, var recordingFailed: Boolean)
data class NativeAacSampleSourceInfo(var producerGeneration: ULong, var nativePtsUs: Long, var originalPts: Long, var originalDts: Long, var originalDuration: UInt, var originalTimescale: UInt)
data class NativeAudioFileReceipt(var generationCount: ULong, var sourceFramesAt48k: ULong, var intentionalStandbyFrames: ULong, var encodedFrames: ULong, var firstLeadingFrames: UInt, var lastTrailingFrames: UInt)
data class NativeAudioGenerationReceipt(var generation: ULong, var inputFrames: ULong, var rawFrames: ULong, var zeroFrames: ULong, var inputRate: UInt, var encodedFrames: ULong, var leadingFrames: UInt, var trailingFrames: UInt, var sourceMapping: NativeRecordingSourceMapping)
data class NativeProgrammeCheckpointEncoding(var bytes: ByteArray, var info: EncodedProgrammeCheckpointInfo)
data class NativeProgrammeCheckpointFacts(var sequence: UInt, var audioUnits: ULong, var audioStart: ULong, var audioEnd: ULong, var audioContainerEnd: ULong, var leadingFrames: UInt, var trailingFrames: UInt, var windowGeometryComplete: Boolean)
data class NativeProgrammeChunk(var bytes: ByteArray, var kind: NativeRecordingChunkKind, var checkpoint: NativeProgrammeCheckpoint?)
data class NativeProgrammeGenerationEnd(var generation: ULong, var queued: NativeProgrammeQueuedFrames, var codedFrames: ULong)
data class NativeProgrammePresentationExtent(var movieTimescale: UInt, var durationTicks: ULong, var durationMsCeil: ULong, var windowGeometryComplete: Boolean)
data class NativeProgrammeQueuedFrames(var preparation: ULong, var programmeRaw: ULong, var programmeZero: ULong, var blockPadding: ULong, var codecDrain: ULong)
data class NativeProgrammeWindowStart(var generation: ULong, var codedOrigin: ULong, var preparationFrames: ULong, var measuredDisplacement: ULong, var discardPrefixFrames: ULong, var movieDelayTicks: ULong)
data class NativeQueueStats(var publishingBytes: ULong, var recordingBytes: ULong, var publishingDrops: ULong, var recordingDrops: ULong, var publishingHighWaterBytes: ULong, var recordingHighWaterBytes: ULong, var publishingMediaMs: ULong, var publishingHighWaterMediaMs: ULong, var publishingOverflows: ULong, var publishingOverflowBytes: ULong, var publishingOverflowSamples: ULong, var publishingOverflowMedia: ULong, var publishingOverflowResidence: ULong, var publishingWaitingKeyframe: Boolean, var recordingFailed: Boolean)
data class NativeRecordingCheckpointEncoding(var bytes: ByteArray, var info: EncodedRecordingCheckpointInfo)
data class NativeRecordingCheckpointFacts(var sequence: UInt, var audioUnits: ULong, var videoUnits: ULong, var audioStart: ULong, var audioEnd: ULong, var audioContainerEnd: ULong, var finalTrailingFrames: UInt, var completeAudioEof: Boolean, var videoStart: ULong?, var videoEnd: ULong?, var firstLeadingFrames: UInt, var completedGenerationCount: ULong)
data class NativeRecordingChunk(var bytes: ByteArray, var kind: NativeRecordingChunkKind, var checkpoint: NativeRecordingCheckpoint?)
data class NativeRecordingPresentationExtent(var movieTimescale: UInt, var durationTicks: ULong, var durationMsCeil: ULong, var completeAudioEof: Boolean)
data class NativeRecordingStats(var bufferedBytes: ULong, var bufferedSamples: ULong, var queuedOutputBytes: ULong, var acceptedSamples: ULong, var fragments: ULong, var durationMs: ULong, var finished: Boolean)
data class NativeRtmpSendOutput(var output: ByteArray, var queuedOutputBytes: ULong, var nextDeadlineMs: ULong?, var state: NativeRtmpState, var startupSubmitted: Boolean, var peerMediaProgress: Boolean)
data class NativeSessionSnapshot(var generation: ULong, var attemptEpoch: ULong, var mediaReadiness: NativeMediaReadiness, var retryBudget: NativeRetryBudgetState, var transportRecoverySource: NativeTransportRecoverySource?, var networkRecoveries: ULong, var outageDeadlineNs: ULong?, var nextDeadlineNs: ULong?, var state: NativeSessionState, var transportConnected: Boolean, var reconnectDeadlineNs: ULong?, var transportFailures: ULong, var retryAttempts: ULong, var mediaConfirmations: ULong, var staleCallbacks: ULong)
data class NativeSrtConfig(var destination: String, var streamId: String, var cipher: NativeSrtCipher?, var latencyMs: UInt, var maxBufferedPackets: UInt, var maxBufferedBytes: UInt, var maxPacketAgeMs: UInt, var maxSampleResidenceMs: UInt, var bandwidthBytesPerSecond: ULong)
data class NativeSrtDatagram(var destination: String, var bytes: ByteArray)
data class NativeSrtStats(var acceptedSamples: ULong, var acceptedSampleBytes: ULong, var bufferedSampleBytes: ULong, var bufferedSampleDurationNs: ULong, var discardedSampleBytes: ULong, var pendingChunkBytes: ULong, var pendingChunkCapacity: ULong, var discardedPendingChunkBytes: ULong, var bufferedPackets: ULong, var bufferedBytes: ULong, var peakBufferedBytes: ULong, var sentPackets: ULong, var sentBytes: ULong, var retransmittedPackets: ULong, var retransmittedBytes: ULong, var acknowledgedBytes: ULong, var abandonedBytes: ULong, var encryptedPackets: ULong, var kmRequests: ULong, var kmResponses: ULong, var rttUs: ULong, var malformedDatagrams: ULong, var strayDatagrams: ULong, var rejectedDatagrams: ULong, var backpressureEvents: ULong, var ffiInputHandleClones: ULong, var sdkInputHandleClones: ULong, var tsOutputAllocations: ULong, var elementaryStagingCopyBytes: ULong, var packetPayloadCopyBytes: ULong, var writerPayloadCopyBytes: ULong, var tsSerialisedBytes: ULong, var returnedDatagrams: ULong, var returnedDatagramBytes: ULong)
data class NativeStopResult(var generation: ULong, var action: NativeSessionAction)
data class RecordingWrite(var bytes: ByteArray, var rewriteHeader: Boolean)

enum class NativeMediaReadiness { UNKNOWN, CONFIRMED }
enum class NativeProgrammeCompletion { COMPLETE_SUBMITTED_WINDOW, INCOMPLETE_PREFIX }
enum class NativeRecordingChunkKind { INITIAL, FRAGMENT, FINAL_HEADER }
enum class NativeRecordingCompletion { COMPLETE, INCOMPLETE_PREFIX }
enum class NativeRecordingSourceMapping { NEAREST, FLOOR }
enum class NativeRetryBudgetState { AVAILABLE, WINDOW_OPEN, EXHAUSTED }
enum class NativeRtmpEvent { SENDING, PEER_MEDIA_PROGRESS }
enum class NativeRtmpState { HANDSHAKING, CONNECTING, CREATING_STREAM, AWAITING_PUBLISH, PUBLISHING, FINISHING, CLOSED, FAILED }
enum class NativeSessionState { IDLE, PREPARING, CONNECTING, SENDING, LIVE, RECONNECTING, INTERRUPTED, STOPPING, STOPPED, FAILED }
enum class NativeSrtCipher { AES128, AES256 }
enum class NativeSrtCloseReason { CANCELLED, SOCKET_SEND_FAILED, CONNECT_TIMEOUT, HANDSHAKE_LIMIT, PEER_IDLE, REFUSED, UNSUPPORTED_PEER, CRYPTO, SAMPLE_EXPIRED, MEDIA_EXPIRED, RETRY_LIMIT, PEER_CLOSED, PEER_ERROR, PACKAGING_FAILED }
enum class NativeSrtState { CONNECTING, CONNECTED, CLOSED }
enum class NativeTransportRecoverySource { SRT_PEER_ACKNOWLEDGED_WATERMARK, RTMP_PEER_ACKNOWLEDGED_WATERMARK }
sealed class NativeSessionAction {
    object None : NativeSessionAction(); object Connect : NativeSessionAction()
    data class RetryAt(val atNs: ULong) : NativeSessionAction()
    data class AwaitGraceDeadline(val atNs: ULong) : NativeSessionAction()
    object EndDestination : NativeSessionAction(); object GraceExpired : NativeSessionAction()
    object RetryBudgetExhausted : NativeSessionAction()
}
sealed class NativeSrtEvent {
    object Sending : NativeSrtEvent(); object PeerMediaProgress : NativeSrtEvent()
    data class Connected(val latencyMs: UInt, val maxPayloadBytes: UInt) : NativeSrtEvent()
    data class Failed(val reason: NativeSrtCloseReason) : NativeSrtEvent()
    data class Closed(val reason: NativeSrtCloseReason, val peerCode: UInt?) : NativeSrtEvent()
}

private fun Reader.recordingFacts() = NativeRecordingCheckpointFacts(u32(), u64(), u64(), u64(), u64(), u64(), u32(), bool(), optionalU64(), optionalU64(), u32(), u64())
private fun Reader.programmeFacts() = NativeProgrammeCheckpointFacts(u32(), u64(), u64(), u64(), u64(), u32(), u32(), bool())
private fun Reader.programmeStart() = NativeProgrammeWindowStart(u64(), u64(), u64(), u64(), u64(), u64())
private fun Reader.programmeEnd() = NativeProgrammeGenerationEnd(u64(), NativeProgrammeQueuedFrames(u64(), u64(), u64(), u64(), u64()), u64())
private fun Reader.recordingInfoFacts() = listOf(recordingFacts(), u64(), u32(), bytes(), bytes(), bytes(), u64(), u64(), bytes())
private fun Reader.programmeInfoFacts() = listOf(programmeFacts(), programmeStart(), if (bool()) programmeEnd() else null, u64(), u32(), bytes(), bytes(), bytes(), u64(), u64(), bytes())
@Suppress("UNCHECKED_CAST")
private fun Reader.encodedRecordingInfo() = recordingInfoFacts().let { EncodedRecordingCheckpointInfo(it[0] as NativeRecordingCheckpointFacts, it[1] as ULong, it[2] as UInt, it[3] as ByteArray, it[4] as ByteArray, it[5] as ByteArray, it[6] as ULong, it[7] as ULong, it[8] as ByteArray) }
@Suppress("UNCHECKED_CAST")
private fun Reader.decodedRecordingInfo() = recordingInfoFacts().let { DecodedRecordingCheckpointInfo(it[0] as NativeRecordingCheckpointFacts, it[1] as ULong, it[2] as UInt, it[3] as ByteArray, it[4] as ByteArray, it[5] as ByteArray, it[6] as ULong, it[7] as ULong, it[8] as ByteArray) }
@Suppress("UNCHECKED_CAST")
private fun Reader.encodedProgrammeInfo() = programmeInfoFacts().let { EncodedProgrammeCheckpointInfo(it[0] as NativeProgrammeCheckpointFacts, it[1] as NativeProgrammeWindowStart, it[2] as NativeProgrammeGenerationEnd?, it[3] as ULong, it[4] as UInt, it[5] as ByteArray, it[6] as ByteArray, it[7] as ByteArray, it[8] as ULong, it[9] as ULong, it[10] as ByteArray) }
@Suppress("UNCHECKED_CAST")
private fun Reader.decodedProgrammeInfo() = programmeInfoFacts().let { DecodedProgrammeCheckpointInfo(it[0] as NativeProgrammeCheckpointFacts, it[1] as NativeProgrammeWindowStart, it[2] as NativeProgrammeGenerationEnd?, it[3] as ULong, it[4] as UInt, it[5] as ByteArray, it[6] as ByteArray, it[7] as ByteArray, it[8] as ULong, it[9] as ULong, it[10] as ByteArray) }
private fun Reader.sessionAction(): NativeSessionAction = when (u8()) {
    0 -> NativeSessionAction.None; 1 -> NativeSessionAction.Connect; 2 -> NativeSessionAction.RetryAt(u64())
    3 -> NativeSessionAction.AwaitGraceDeadline(u64()); 4 -> NativeSessionAction.EndDestination
    5 -> NativeSessionAction.GraceExpired; 6 -> NativeSessionAction.RetryBudgetExhausted
    else -> throw IllegalStateException("Unknown session action")
}
private fun Writer.programmeStart(v: NativeProgrammeWindowStart) { u64(v.generation); u64(v.codedOrigin); u64(v.preparationFrames); u64(v.measuredDisplacement); u64(v.discardPrefixFrames); u64(v.movieDelayTicks) }
private fun Writer.programmeEnd(v: NativeProgrammeGenerationEnd) { u64(v.generation); u64(v.queued.preparation); u64(v.queued.programmeRaw); u64(v.queued.programmeZero); u64(v.queued.blockPadding); u64(v.queued.codecDrain); u64(v.codedFrames) }
private fun Writer.generation(v: NativeAudioGenerationReceipt) { u64(v.generation); u64(v.inputFrames); u64(v.rawFrames); u64(v.zeroFrames); u32(v.inputRate); u64(v.encodedFrames); u32(v.leadingFrames); u32(v.trailingFrames); u8(v.sourceMapping.ordinal) }
private fun Writer.fileReceipt(v: NativeAudioFileReceipt) { u64(v.generationCount); u64(v.sourceFramesAt48k); u64(v.intentionalStandbyFrames); u64(v.encodedFrames); u32(v.firstLeadingFrames); u32(v.lastTrailingFrames) }
private fun Writer.srtConfig(v: NativeSrtConfig) { string(v.destination); string(v.streamId); val cipher = v.cipher; bool(cipher != null); if (cipher != null) u8(cipher.ordinal); u32(v.latencyMs); u32(v.maxBufferedPackets); u32(v.maxBufferedBytes); u32(v.maxPacketAgeMs); u32(v.maxSampleResidenceMs); u64(v.bandwidthBytesPerSecond) }

public interface EncodedBufferInterface { fun len(): ULong; fun isEmpty(): Boolean; fun byteAt(index: ULong): UByte; companion object }
open class EncodedBuffer internal constructor(handle: Long, private val length: ULong) : HandleObject(1, handle), EncodedBufferInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L, 0uL)
    constructor(bytes: ByteBuffer) : this(
        if (bytes.isDirect) hot(ErrorKind.BINDING) { bytes.borrowDirect(Native::encodedBufferNew) }
        else newHandle(2, 1, ErrorKind.BINDING, request { bytes(bytes.toOwnedArray()) }),
        bytes.remaining().toULong(),
    )
    override fun len() = withHandle { length }
    override fun isEmpty() = withHandle { length == 0uL }
    @Throws(BindingException::class) override fun byteAt(index: ULong) = withHandle { hot(ErrorKind.BINDING) { Native.encodedBufferScalar(it, 5, index.toLong()).toUByte() } }
}
fun sdkAbiVersion(): UInt = response(invoke(1, ErrorKind.BINDING)) { u32() }

public interface MediaCodecInterface { fun sample(bytes: ByteBuffer, pts: Long, dts: Long, duration: UInt, timescale: UInt, keyframe: Boolean): MediaSample; companion object }
open class MediaCodec internal constructor(handle: Long, private val video: Boolean? = null) : HandleObject(2, handle), MediaCodecInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    @Throws(BindingException::class) override fun sample(bytes: ByteBuffer, pts: Long, dts: Long, duration: UInt, timescale: UInt, keyframe: Boolean) = withHandle { codec ->
        val handle = if (bytes.isDirect) hot(ErrorKind.BINDING) { bytes.borrowDirect { address, len -> Native.mediaCodecSample(codec, address, len, pts, dts, duration.toInt(), timescale.toInt(), keyframe) } }
        else response(invoke(8, ErrorKind.BINDING, codec, request { bytes(bytes.toOwnedArray()); i64(pts); i64(dts); u32(duration); u32(timescale); bool(keyframe) })) { handle() }
        if (video == null) MediaSample(handle)
        else MediaSample(handle, bytes.remaining().toULong(), pts, dts, duration, timescale, video, keyframe)
    }
    companion object {
        @JvmStatic fun h264(sps: ByteBuffer, pps: ByteBuffer) = MediaCodec(newHandle(6, 2, ErrorKind.BINDING, request { bytes(sps.toOwnedArray()); bytes(pps.toOwnedArray()) }), true)
        @JvmStatic fun aac(asc: ByteBuffer) = MediaCodec(newHandle(7, 2, ErrorKind.BINDING, request { bytes(asc.toOwnedArray()) }), false)
    }
}
public interface MediaSampleInterface {
    fun aacTimingViews(producerGeneration: ULong, nativePtsUs: Long, publishingPts: Long?): NativeAacTimingViews
    fun aacSourceInfo(): NativeAacSampleSourceInfo?; fun len(): ULong; fun isEmpty(): Boolean
    fun isVideo(): Boolean; fun isKeyframe(): Boolean; fun pts(): Long; fun dts(): Long
    fun duration(): UInt; fun timescale(): UInt; companion object
}
open class MediaSample internal constructor(
    handle: Long,
    private val length: ULong,
    private val presentation: Long,
    private val decode: Long,
    private val sampleDuration: UInt,
    private val sampleTimescale: UInt,
    private val video: Boolean,
    private val keyframe: Boolean,
) : HandleObject(3, handle), MediaSampleInterface {
    internal constructor(handle: Long) : this(hot(ErrorKind.BINDING) { Native.mediaSampleMetadata(handle) })
    internal constructor(values: LongArray) : this(values[0], values[1].toULong(), values[2], values[3], values[4].toUInt(), values[5].toUInt(), values[6] != 0L, values[7] != 0L)
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L, 0uL, 0L, 0L, 0u, 0u, false, false)
    @Throws(BindingException::class) override fun aacTimingViews(producerGeneration: ULong, nativePtsUs: Long, publishingPts: Long?) = withHandle {
        NativeAacTimingViews(response(invoke(9, ErrorKind.BINDING, it, request { u64(producerGeneration); i64(nativePtsUs); optional(publishingPts) })) { handle() })
    }
    override fun aacSourceInfo() = withHandle { response(invoke(10, ErrorKind.BINDING, it)) { if (bool()) NativeAacSampleSourceInfo(u64(), i64(), i64(), i64(), u32(), u32()) else null } }
    override fun len() = withHandle { length }
    override fun isEmpty() = withHandle { length == 0uL }
    override fun isVideo() = withHandle { video }
    override fun isKeyframe() = withHandle { keyframe }
    override fun pts() = withHandle { presentation }
    override fun dts() = withHandle { decode }
    override fun duration() = withHandle { sampleDuration }
    override fun timescale() = withHandle { sampleTimescale }
}
public interface NativeAacTimingViewsInterface { fun sourceInfo(): NativeAacSampleSourceInfo; fun recordingSample(): MediaSample; fun publishingSample(): MediaSample?; companion object }
open class NativeAacTimingViews internal constructor(handle: Long) : HandleObject(4, handle), NativeAacTimingViewsInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    override fun sourceInfo() = withHandle { response(invoke(19, ErrorKind.BINDING, it)) { NativeAacSampleSourceInfo(u64(), i64(), i64(), i64(), u32(), u32()) } }
    override fun recordingSample() = withHandle { MediaSample(hot(ErrorKind.BINDING) { Native.aacTimingViewsSample(it, false) }!!) }
    override fun publishingSample() = withHandle { hot(ErrorKind.BINDING) { Native.aacTimingViewsSample(it, true) }?.let(::MediaSample) }
}
public interface NativeSampleFanoutInterface {
    fun pushAacTimingViews(views: NativeAacTimingViews, nowNs: ULong): FanoutOutcome
    fun push(sample: MediaSample, nowNs: ULong): FanoutOutcome
    fun popPublishing(nowNs: ULong): MediaSample?; fun popRecording(nowNs: ULong): MediaSample?
    fun stats(): NativeQueueStats; fun closePublishingAdmission(); fun closeRecordingAdmission(); fun cancel(); companion object
}
open class NativeSampleFanout internal constructor(handle: Long) : HandleObject(5, handle), NativeSampleFanoutInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    constructor(publishingBytes: ULong, recordingBytes: ULong, maxMediaMs: UInt, hasVideo: Boolean) : this(newHandle(22, 5, ErrorKind.BINDING, request { u64(publishingBytes); u64(recordingBytes); u32(maxMediaMs); bool(hasVideo) }))
    private fun outcome(values: LongArray) = FanoutOutcome(values[0] != 0L, values[1] != 0L, values[2] != 0L, values[3].toULong(), values[4] != 0L)
    @Throws(BindingException::class) override fun pushAacTimingViews(views: NativeAacTimingViews, nowNs: ULong) = withHandle { h -> views.withHandle { outcome(hot(ErrorKind.BINDING) { Native.sampleFanoutPush(h, it, nowNs.toLong(), true) }) } }
    @Throws(BindingException::class) override fun push(sample: MediaSample, nowNs: ULong) = withHandle { h -> sample.withHandle { outcome(hot(ErrorKind.BINDING) { Native.sampleFanoutPush(h, it, nowNs.toLong(), false) }) } }
    @Throws(BindingException::class) override fun popPublishing(nowNs: ULong) = withHandle { hot(ErrorKind.BINDING) { Native.sampleFanoutPop(it, false, nowNs.toLong()) }?.let(::MediaSample) }
    @Throws(BindingException::class) override fun popRecording(nowNs: ULong) = withHandle { hot(ErrorKind.BINDING) { Native.sampleFanoutPop(it, true, nowNs.toLong()) }?.let(::MediaSample) }
    @Throws(BindingException::class) override fun stats() = withHandle { response(invoke(28, ErrorKind.BINDING, it)) { NativeQueueStats(u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), bool(), bool()) } }
    @Throws(BindingException::class) override fun closePublishingAdmission() = withHandle { invoke(29, ErrorKind.BINDING, it); Unit }
    @Throws(BindingException::class) override fun closeRecordingAdmission() = withHandle { invoke(30, ErrorKind.BINDING, it); Unit }
    @Throws(BindingException::class) override fun cancel() = withHandle { invoke(31, ErrorKind.BINDING, it); Unit }
    companion object {
        @JvmStatic fun withAacTimingViews(publishingBytes: ULong, recordingBytes: ULong, maxMediaMs: UInt, hasVideo: Boolean) = NativeSampleFanout(newHandle(23, 5, ErrorKind.BINDING, request { u64(publishingBytes); u64(recordingBytes); u32(maxMediaMs); bool(hasVideo) }))
    }
}

public interface NativeRecordingCheckpointInterface { fun facts(): NativeRecordingCheckpointFacts; fun presentationExtent(): NativeRecordingPresentationExtent; companion object }
open class NativeRecordingCheckpoint internal constructor(handle: Long) : HandleObject(6, handle), NativeRecordingCheckpointInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    override fun facts() = withHandle { response(invoke(32, ErrorKind.RECORDING, it)) { recordingFacts() } }
    @Throws(NativeRecordingException::class) override fun presentationExtent() = withHandle { response(invoke(33, ErrorKind.RECORDING, it)) { NativeRecordingPresentationExtent(u32(), u64(), u64(), bool()) } }
}
public interface DecodedRecordingCheckpointInterface { fun info(): DecodedRecordingCheckpointInfo; fun header(): ByteArray; fun fragmentRegionSha256(): ByteArray; companion object }
open class DecodedRecordingCheckpoint internal constructor(handle: Long) : HandleObject(7, handle), DecodedRecordingCheckpointInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    override fun info() = withHandle { response(invoke(34, ErrorKind.CHECKPOINT, it)) { decodedRecordingInfo() } }
    override fun header() = withHandle { invoke(35, ErrorKind.CHECKPOINT, it) }
    override fun fragmentRegionSha256() = withHandle { invoke(36, ErrorKind.CHECKPOINT, it) }
}
public interface NativeProgrammeCheckpointInterface { fun facts(): NativeProgrammeCheckpointFacts; fun start(): NativeProgrammeWindowStart; fun windowEnd(): NativeProgrammeGenerationEnd?; fun presentationExtent(): NativeProgrammePresentationExtent; companion object }
open class NativeProgrammeCheckpoint internal constructor(handle: Long) : HandleObject(8, handle), NativeProgrammeCheckpointInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    override fun facts() = withHandle { response(invoke(37, ErrorKind.RECORDING, it)) { programmeFacts() } }
    override fun start() = withHandle { response(invoke(38, ErrorKind.RECORDING, it)) { programmeStart() } }
    override fun windowEnd() = withHandle { response(invoke(39, ErrorKind.RECORDING, it)) { if (bool()) programmeEnd() else null } }
    @Throws(NativeRecordingException::class) override fun presentationExtent() = withHandle { response(invoke(40, ErrorKind.RECORDING, it)) { NativeProgrammePresentationExtent(u32(), u64(), u64(), bool()) } }
}
public interface DecodedProgrammeCheckpointInterface { fun info(): DecodedProgrammeCheckpointInfo; fun header(): ByteArray; fun fragmentRegionSha256(): ByteArray; fun presentationExtent(): NativeProgrammePresentationExtent; companion object }
open class DecodedProgrammeCheckpoint internal constructor(handle: Long) : HandleObject(9, handle), DecodedProgrammeCheckpointInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    override fun info() = withHandle { response(invoke(41, ErrorKind.CHECKPOINT, it)) { decodedProgrammeInfo() } }
    override fun header() = withHandle { invoke(42, ErrorKind.CHECKPOINT, it) }
    override fun fragmentRegionSha256() = withHandle { invoke(43, ErrorKind.CHECKPOINT, it) }
    @Throws(NativeRecordingException::class) override fun presentationExtent() = withHandle { response(invoke(44, ErrorKind.RECORDING, it)) { NativeProgrammePresentationExtent(u32(), u64(), u64(), bool()) } }
}

public interface NativeRecordingMuxerInterface {
    fun beginNativeAudio(firstLeadingFrames: UInt); fun beginProgrammeWindow(start: NativeProgrammeWindowStart)
    fun cancel(); fun checkpointHeader(checkpoint: NativeRecordingCheckpoint): ByteArray
    fun closeProgrammeWindow(end: NativeProgrammeGenerationEnd); fun completeNativeAudio(receipt: NativeAudioFileReceipt)
    fun encodeCheckpoint(token: NativeRecordingCheckpoint, recordingBinding: ByteArray, sourceFileBinding: ByteArray, fragmentRegionSha256: ByteArray): NativeRecordingCheckpointEncoding
    fun encodeProgrammeCheckpoint(token: NativeProgrammeCheckpoint, recordingBinding: ByteArray, sourceFileBinding: ByteArray, fragmentRegionSha256: ByteArray): NativeProgrammeCheckpointEncoding
    fun endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt); fun finish(); fun finishNative(outcome: NativeRecordingCompletion)
    fun finishProgramme(outcome: NativeProgrammeCompletion); fun programmeCheckpointHeader(checkpoint: NativeProgrammeCheckpoint): ByteArray
    fun push(sample: MediaSample); fun stats(): NativeRecordingStats; fun takeNativeOutput(): NativeRecordingChunk?
    fun takeOutput(): RecordingWrite?; fun takeProgrammeOutput(): NativeProgrammeChunk?; companion object
}
open class NativeRecordingMuxer internal constructor(handle: Long) : HandleObject(10, handle), NativeRecordingMuxerInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    constructor(hasVideo: Boolean, hasAudio: Boolean, mediaEpochNs: ULong, maxFragmentBytes: ULong, maxFragmentMs: UInt) : this(newHandle(45, 10, ErrorKind.RECORDING, request { bool(hasVideo); bool(hasAudio); u64(mediaEpochNs); u64(maxFragmentBytes); u32(maxFragmentMs) }))
    @Throws(NativeRecordingException::class) override fun beginNativeAudio(firstLeadingFrames: UInt) = withHandle { invoke(54, ErrorKind.RECORDING, it, request { u32(firstLeadingFrames) }); Unit }
    @Throws(NativeRecordingException::class) override fun beginProgrammeWindow(start: NativeProgrammeWindowStart) = withHandle { invoke(48, ErrorKind.RECORDING, it, request { programmeStart(start) }); Unit }
    @Throws(NativeRecordingException::class) override fun cancel() = withHandle { invoke(64, ErrorKind.RECORDING, it); Unit }
    @Throws(NativeRecordingException::class) override fun checkpointHeader(checkpoint: NativeRecordingCheckpoint) = withHandle { h -> checkpoint.withHandle { invoke(60, ErrorKind.RECORDING, h, request { u64(it.toULong()) }) } }
    @Throws(NativeRecordingException::class) override fun closeProgrammeWindow(end: NativeProgrammeGenerationEnd) = withHandle { invoke(49, ErrorKind.RECORDING, it, request { programmeEnd(end) }); Unit }
    @Throws(NativeRecordingException::class) override fun completeNativeAudio(receipt: NativeAudioFileReceipt) = withHandle { invoke(56, ErrorKind.RECORDING, it, request { fileReceipt(receipt) }); Unit }
    @Throws(RecordingCheckpointException::class) override fun encodeCheckpoint(token: NativeRecordingCheckpoint, recordingBinding: ByteArray, sourceFileBinding: ByteArray, fragmentRegionSha256: ByteArray) = withHandle { h -> token.withHandle {
        response(invoke(61, ErrorKind.CHECKPOINT, h, request { u64(it.toULong()); bytes(recordingBinding); bytes(sourceFileBinding); bytes(fragmentRegionSha256) })) { NativeRecordingCheckpointEncoding(bytes(), encodedRecordingInfo()) }
    } }
    @Throws(RecordingCheckpointException::class) override fun encodeProgrammeCheckpoint(token: NativeProgrammeCheckpoint, recordingBinding: ByteArray, sourceFileBinding: ByteArray, fragmentRegionSha256: ByteArray) = withHandle { h -> token.withHandle {
        response(invoke(52, ErrorKind.CHECKPOINT, h, request { u64(it.toULong()); bytes(recordingBinding); bytes(sourceFileBinding); bytes(fragmentRegionSha256) })) { NativeProgrammeCheckpointEncoding(bytes(), encodedProgrammeInfo()) }
    } }
    @Throws(NativeRecordingException::class) override fun endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt) = withHandle { invoke(55, ErrorKind.RECORDING, it, request { generation(receipt) }); Unit }
    @Throws(NativeRecordingException::class) override fun finish() = withHandle { invoke(62, ErrorKind.RECORDING, it); Unit }
    @Throws(NativeRecordingException::class) override fun finishNative(outcome: NativeRecordingCompletion) = withHandle { invoke(63, ErrorKind.RECORDING, it, request { u8(outcome.ordinal) }); Unit }
    @Throws(NativeRecordingException::class) override fun finishProgramme(outcome: NativeProgrammeCompletion) = withHandle { invoke(53, ErrorKind.RECORDING, it, request { u8(outcome.ordinal) }); Unit }
    @Throws(NativeRecordingException::class) override fun programmeCheckpointHeader(checkpoint: NativeProgrammeCheckpoint) = withHandle { h -> checkpoint.withHandle { invoke(51, ErrorKind.RECORDING, h, request { u64(it.toULong()) }) } }
    @Throws(NativeRecordingException::class) override fun push(sample: MediaSample) = withHandle { h -> sample.withHandle { hot(ErrorKind.RECORDING) { Native.recordingPush(h, it) } } }
    @Throws(NativeRecordingException::class) override fun stats() = withHandle { response(invoke(65, ErrorKind.RECORDING, it)) { NativeRecordingStats(u64(), u64(), u64(), u64(), u64(), u64(), bool()) } }
    @Throws(NativeRecordingException::class) override fun takeNativeOutput() = withHandle {
        val metadata = LongArray(3)
        hot(ErrorKind.RECORDING) { Native.recordingTakeOutput(it, 1, metadata) }?.let { bytes -> NativeRecordingChunk(bytes, NativeRecordingChunkKind.entries[metadata[1].toInt()], if (metadata[0] == 0L) null else NativeRecordingCheckpoint(metadata[0])) }
    }
    @Throws(NativeRecordingException::class) override fun takeOutput() = withHandle {
        val metadata = LongArray(3)
        hot(ErrorKind.RECORDING) { Native.recordingTakeOutput(it, 0, metadata) }?.let { bytes -> RecordingWrite(bytes, metadata[2] != 0L) }
    }
    @Throws(NativeRecordingException::class) override fun takeProgrammeOutput() = withHandle {
        val metadata = LongArray(3)
        hot(ErrorKind.RECORDING) { Native.recordingTakeOutput(it, 2, metadata) }?.let { bytes -> NativeProgrammeChunk(bytes, NativeRecordingChunkKind.entries[metadata[1].toInt()], if (metadata[0] == 0L) null else NativeProgrammeCheckpoint(metadata[0])) }
    }
    companion object {
        @JvmStatic fun withNativeAacTiming(hasVideo: Boolean, hasAudio: Boolean, mediaEpochNs: ULong, maxFragmentBytes: ULong, maxFragmentMs: UInt) = NativeRecordingMuxer(newHandle(46, 10, ErrorKind.RECORDING, request { bool(hasVideo); bool(hasAudio); u64(mediaEpochNs); u64(maxFragmentBytes); u32(maxFragmentMs) }))
        @JvmStatic fun withProgrammeAacTiming(mediaEpochNs: ULong, maxFragmentBytes: ULong, maxFragmentMs: UInt) = NativeRecordingMuxer(newHandle(47, 10, ErrorKind.RECORDING, request { u64(mediaEpochNs); u64(maxFragmentBytes); u32(maxFragmentMs) }))
    }
}
@Throws(RecordingCheckpointException::class) fun decodeRecordingCheckpoint(bytes: ByteArray, expectedRecordingBinding: ByteArray, expectedSourceFileBinding: ByteArray) = DecodedRecordingCheckpoint(newHandle(66, 7, ErrorKind.CHECKPOINT, request { bytes(bytes); bytes(expectedRecordingBinding); bytes(expectedSourceFileBinding) }))
@Throws(RecordingCheckpointException::class) fun validateRecordingCheckpointSuccessor(prior: DecodedRecordingCheckpoint, next: DecodedRecordingCheckpoint) = prior.withHandle { p -> next.withHandle { invoke(67, ErrorKind.CHECKPOINT, input = request { u64(p.toULong()); u64(it.toULong()) }); Unit } }
@Throws(RecordingCheckpointException::class) fun decodeProgrammeCheckpoint(bytes: ByteArray, expectedRecordingBinding: ByteArray, expectedSourceFileBinding: ByteArray) = DecodedProgrammeCheckpoint(newHandle(68, 9, ErrorKind.CHECKPOINT, request { bytes(bytes); bytes(expectedRecordingBinding); bytes(expectedSourceFileBinding) }))
@Throws(RecordingCheckpointException::class) fun validateProgrammeCheckpointSuccessor(prior: DecodedProgrammeCheckpoint, next: DecodedProgrammeCheckpoint) = prior.withHandle { p -> next.withHandle { invoke(69, ErrorKind.CHECKPOINT, input = request { u64(p.toULong()); u64(it.toULong()) }); Unit } }

public interface NativeRtmpPublisherInterface {
    fun declareTracks(hasVideo: Boolean, hasAudio: Boolean); fun pollEvent(): NativeRtmpEvent?
    fun feed(bytes: ByteBuffer, nowMs: ULong); fun tick(nowMs: ULong); fun nextDeadlineMs(): ULong?
    fun takeOutput(): ByteArray; fun queuedOutputBytes(): ULong; fun state(): NativeRtmpState
    fun send(sample: MediaSample); fun sendAndTakeOutput(sample: MediaSample, nowMs: ULong): NativeRtmpSendOutput
    fun finish(); fun cancel(); companion object
}
open class NativeRtmpPublisher internal constructor(handle: Long) : HandleObject(11, handle), NativeRtmpPublisherInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    constructor(app: String, streamKey: String, tcUrl: String, mediaEpochNs: ULong, nowMs: ULong) : this(newHandle(70, 11, ErrorKind.RTMP, request { string(app); string(streamKey); string(tcUrl); u64(mediaEpochNs); u64(nowMs) }))
    @Throws(NativeRtmpException::class) override fun declareTracks(hasVideo: Boolean, hasAudio: Boolean) = withHandle { invoke(71, ErrorKind.RTMP, it, request { bool(hasVideo); bool(hasAudio) }); Unit }
    @Throws(NativeRtmpException::class) override fun pollEvent() = withHandle { response(invoke(72, ErrorKind.RTMP, it)) { if (bool()) NativeRtmpEvent.entries[u8()] else null } }
    @Throws(NativeRtmpException::class) override fun feed(bytes: ByteBuffer, nowMs: ULong) = withHandle {
        if (bytes.isDirect) hot(ErrorKind.RTMP) { bytes.borrowDirect { address, len -> Native.rtmpFeed(it, address, len, nowMs.toLong()) } }
        else invoke(73, ErrorKind.RTMP, it, request { bytes(bytes.toOwnedArray()); u64(nowMs) }); Unit
    }
    @Throws(NativeRtmpException::class) override fun tick(nowMs: ULong) = withHandle { invoke(74, ErrorKind.RTMP, it, request { u64(nowMs) }); Unit }
    @Throws(NativeRtmpException::class) override fun nextDeadlineMs() = withHandle { hot(ErrorKind.RTMP) { Native.rtmpNextDeadlineMs(it) }.let { value -> if (value[1] != 0L) value[0].toULong() else null } }
    @Throws(NativeRtmpException::class) override fun takeOutput() = withHandle { hot(ErrorKind.RTMP) { Native.rtmpTakeOutput(it) } }
    @Throws(NativeRtmpException::class) override fun queuedOutputBytes() = withHandle { hot(ErrorKind.RTMP) { Native.rtmpQueuedOutputBytes(it).toULong() } }
    @Throws(NativeRtmpException::class) override fun state() = withHandle { hot(ErrorKind.RTMP) { NativeRtmpState.entries[Native.rtmpState(it).toInt()] } }
    @Throws(NativeRtmpException::class) override fun send(sample: MediaSample) = withHandle { h -> sample.withHandle { hot(ErrorKind.RTMP) { Native.rtmpSend(h, it) } } }
    @Throws(NativeRtmpException::class) override fun sendAndTakeOutput(sample: MediaSample, nowMs: ULong) = withHandle { h -> sample.withHandle {
        val metadata = LongArray(6)
        val output = hot(ErrorKind.RTMP) { Native.rtmpSendAndTakeOutput(h, it, nowMs.toLong(), metadata) }
        NativeRtmpSendOutput(output, metadata[0].toULong(), if (metadata[2] != 0L) metadata[1].toULong() else null, NativeRtmpState.entries[metadata[3].toInt()], metadata[4] != 0L, metadata[5] != 0L)
    } }
    @Throws(NativeRtmpException::class) override fun finish() = withHandle { invoke(81, ErrorKind.RTMP, it); Unit }
    @Throws(NativeRtmpException::class) override fun cancel() = withHandle { invoke(82, ErrorKind.RTMP, it); Unit }
}

public interface NativeSessionInterface {
    fun start(nowNs: ULong): ULong; fun prepared(generation: ULong): NativeSessionAction
    fun connected(generation: ULong, nowNs: ULong); fun sending(generation: ULong, attemptEpoch: ULong, nowNs: ULong): NativeSessionAction
    fun transportRecovered(generation: ULong, attemptEpoch: ULong, nowNs: ULong, source: NativeTransportRecoverySource): NativeSessionAction
    fun mediaConfirmed(generation: ULong, nowNs: ULong); fun mediaConfirmedForAttempt(generation: ULong, attemptEpoch: ULong, nowNs: ULong)
    fun replaceTransport(generation: ULong, nowNs: ULong): NativeSessionAction
    fun transportFailed(generation: ULong, nowNs: ULong): NativeSessionAction
    fun tick(generation: ULong, nowNs: ULong): NativeSessionAction; fun interrupt(generation: ULong): ULong
    fun stop(generation: ULong): NativeStopResult; fun finishStop(generation: ULong); fun cancel(generation: ULong): ULong
    fun snapshot(): NativeSessionSnapshot; companion object
}
open class NativeSession internal constructor(handle: Long, @Suppress("UNUSED_PARAMETER") marker: Boolean) : HandleObject(12, handle), NativeSessionInterface {
    constructor(graceMs: ULong) : this(newHandle(83, 12, ErrorKind.SESSION, request { u64(graceMs) }), true)
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L, true)
    @Throws(BindingException::class) override fun start(nowNs: ULong) = withHandle { response(invoke(84, ErrorKind.SESSION, it, request { u64(nowNs) })) { u64() } }
    @Throws(BindingException::class) override fun prepared(generation: ULong) = withHandle { response(invoke(85, ErrorKind.SESSION, it, request { u64(generation) })) { sessionAction() } }
    @Throws(BindingException::class) override fun connected(generation: ULong, nowNs: ULong) = withHandle { invoke(86, ErrorKind.SESSION, it, request { u64(generation); u64(nowNs) }); Unit }
    @Throws(BindingException::class) override fun sending(generation: ULong, attemptEpoch: ULong, nowNs: ULong) = withHandle { response(invoke(87, ErrorKind.SESSION, it, request { u64(generation); u64(attemptEpoch); u64(nowNs) })) { sessionAction() } }
    @Throws(BindingException::class) override fun transportRecovered(generation: ULong, attemptEpoch: ULong, nowNs: ULong, source: NativeTransportRecoverySource) = withHandle { response(invoke(88, ErrorKind.SESSION, it, request { u64(generation); u64(attemptEpoch); u64(nowNs); u8(source.ordinal) })) { sessionAction() } }
    @Throws(BindingException::class) override fun mediaConfirmed(generation: ULong, nowNs: ULong) = withHandle { invoke(89, ErrorKind.SESSION, it, request { u64(generation); u64(nowNs) }); Unit }
    @Throws(BindingException::class) override fun mediaConfirmedForAttempt(generation: ULong, attemptEpoch: ULong, nowNs: ULong) = withHandle { invoke(90, ErrorKind.SESSION, it, request { u64(generation); u64(attemptEpoch); u64(nowNs) }); Unit }
    @Throws(BindingException::class) override fun replaceTransport(generation: ULong, nowNs: ULong) = withHandle { response(invoke(91, ErrorKind.SESSION, it, request { u64(generation); u64(nowNs) })) { sessionAction() } }
    @Throws(BindingException::class) override fun transportFailed(generation: ULong, nowNs: ULong) = withHandle { response(invoke(92, ErrorKind.SESSION, it, request { u64(generation); u64(nowNs) })) { sessionAction() } }
    @Throws(BindingException::class) override fun tick(generation: ULong, nowNs: ULong) = withHandle { response(invoke(93, ErrorKind.SESSION, it, request { u64(generation); u64(nowNs) })) { sessionAction() } }
    @Throws(BindingException::class) override fun interrupt(generation: ULong) = withHandle { response(invoke(94, ErrorKind.SESSION, it, request { u64(generation) })) { u64() } }
    @Throws(BindingException::class) override fun stop(generation: ULong) = withHandle { response(invoke(95, ErrorKind.SESSION, it, request { u64(generation) })) { NativeStopResult(u64(), sessionAction()) } }
    @Throws(BindingException::class) override fun finishStop(generation: ULong) = withHandle { invoke(96, ErrorKind.SESSION, it, request { u64(generation) }); Unit }
    @Throws(BindingException::class) override fun cancel(generation: ULong) = withHandle { response(invoke(97, ErrorKind.SESSION, it, request { u64(generation) })) { u64() } }
    @Throws(BindingException::class) override fun snapshot() = withHandle { response(invoke(98, ErrorKind.SESSION, it)) {
        NativeSessionSnapshot(u64(), u64(), NativeMediaReadiness.entries[u8()], NativeRetryBudgetState.entries[u8()], if (bool()) NativeTransportRecoverySource.entries[u8()] else null, u64(), optionalU64(), optionalU64(), NativeSessionState.entries[u8()], bool(), optionalU64(), u64(), u64(), u64(), u64())
    } }
}

public interface NativeSrtPublisherInterface {
    fun push(sample: MediaSample, nowUs: ULong); fun feed(source: String, bytes: ByteBuffer, nowUs: ULong)
    fun tick(nowUs: ULong); fun pollDatagram(nowUs: ULong): NativeSrtDatagram?; fun isReady(): Boolean
    fun hasImmediateWork(): Boolean; fun nextDeadlineUs(): ULong?; fun state(): NativeSrtState
    fun pollEvent(): NativeSrtEvent?; fun stop(nowUs: ULong); fun socketSendFailed(nowUs: ULong)
    fun stats(): NativeSrtStats; companion object
}
open class NativeSrtPublisher internal constructor(handle: Long) : HandleObject(13, handle), NativeSrtPublisherInterface {
    @Suppress("UNUSED_PARAMETER") constructor(noHandle: NoHandle) : this(0L)
    constructor(config: NativeSrtConfig, video: MediaCodec?, audio: MediaCodec?, passphrase: ByteArray?, nowUs: ULong) : this(newHandle(99, 13, ErrorKind.SRT, request {
        srtConfig(config); optional(video); optional(audio); bool(passphrase != null); if (passphrase != null) bytes(passphrase); u64(nowUs)
    }))
    @Throws(NativeSrtException::class) override fun push(sample: MediaSample, nowUs: ULong) = withHandle { h -> sample.withHandle { hot(ErrorKind.SRT) { Native.srtPush(h, it, nowUs.toLong()) } } }
    @Throws(NativeSrtException::class) override fun feed(source: String, bytes: ByteBuffer, nowUs: ULong) = withHandle {
        if (bytes.isDirect) hot(ErrorKind.SRT) { bytes.borrowDirect { address, len -> Native.srtFeed(it, source.toByteArray(Charsets.UTF_8), address, len, nowUs.toLong()) } }
        else invoke(101, ErrorKind.SRT, it, request { string(source); bytes(bytes.toOwnedArray()); u64(nowUs) }); Unit
    }
    @Throws(NativeSrtException::class) override fun tick(nowUs: ULong) = withHandle { invoke(102, ErrorKind.SRT, it, request { u64(nowUs) }); Unit }
    @Throws(NativeSrtException::class) override fun pollDatagram(nowUs: ULong) = withHandle { hot(ErrorKind.SRT) { Native.srtPollDatagram(it, nowUs.toLong()) }?.let { value -> NativeSrtDatagram(value[0] as String, value[1] as ByteArray) } }
    @Throws(NativeSrtException::class) override fun isReady() = withHandle { hot(ErrorKind.SRT) { Native.srtIsReady(it) != 0L } }
    @Throws(NativeSrtException::class) override fun hasImmediateWork() = withHandle { hot(ErrorKind.SRT) { Native.srtHasImmediateWork(it) != 0L } }
    @Throws(NativeSrtException::class) override fun nextDeadlineUs() = withHandle { hot(ErrorKind.SRT) { Native.srtNextDeadlineUs(it) }.let { value -> if (value[1] != 0L) value[0].toULong() else null } }
    @Throws(NativeSrtException::class) override fun state() = withHandle { hot(ErrorKind.SRT) { NativeSrtState.entries[Native.srtState(it).toInt()] } }
    @Throws(NativeSrtException::class) override fun pollEvent() = withHandle { response(invoke(108, ErrorKind.SRT, it)) {
        if (!bool()) null else when (u8()) {
            0 -> NativeSrtEvent.Sending; 1 -> NativeSrtEvent.PeerMediaProgress
            2 -> NativeSrtEvent.Connected(u32(), u32()); 3 -> NativeSrtEvent.Failed(NativeSrtCloseReason.entries[u8()])
            4 -> NativeSrtEvent.Closed(NativeSrtCloseReason.entries[u8()], if (bool()) u32() else null)
            else -> throw IllegalStateException("Unknown SRT event")
        }
    } }
    @Throws(NativeSrtException::class) override fun stop(nowUs: ULong) = withHandle { invoke(109, ErrorKind.SRT, it, request { u64(nowUs) }); Unit }
    @Throws(NativeSrtException::class) override fun socketSendFailed(nowUs: ULong) = withHandle { invoke(110, ErrorKind.SRT, it, request { u64(nowUs) }); Unit }
    @Throws(NativeSrtException::class) override fun stats() = withHandle { response(invoke(111, ErrorKind.SRT, it)) {
        NativeSrtStats(u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64(), u64())
    } }
}
