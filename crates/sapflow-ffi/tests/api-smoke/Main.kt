import dev.forgesworn.sapflow.*
import java.nio.ByteBuffer

fun main() {
    val codec = MediaCodec.aac(ByteBuffer.wrap(byteArrayOf(0x11, 0x88.toByte())))
    val sample = codec.sample(ByteBuffer.wrap(byteArrayOf(1, 2, 3)), 0, 0, 1024u, 48_000u, false)
    val fanout = NativeSampleFanout(1024u, 1024u, 1000u, false)
    val outcome = fanout.push(sample, 0u)
    val publishing = fanout.popPublishing(1u)
    val recording = fanout.popRecording(1u)
    val muxer = NativeRecordingMuxer(false, true, 0u, 4096u, 100u)
    muxer.push(sample)
    val initial = muxer.takeOutput()
    muxer.finish()
    val finalLengths = mutableListOf<Int>()
    while (true) finalLengths.add(muxer.takeOutput()?.bytes?.size ?: break)
    val views = sample.aacTimingViews(1u, -10, 0)
    val paired = NativeSampleFanout.withAacTimingViews(1024u, 1024u, 1000u, false)
    val pairedOutcome = paired.pushAacTimingViews(views, 0u)
    val pairedPublishing = paired.popPublishing(1u)
    val pairedRecording = paired.popRecording(1u)
    val native = NativeRecordingMuxer.withNativeAacTiming(false, true, 0u, 4096u, 100u)
    native.beginNativeAudio(0u)
    native.push(sample)
    val nativeInitial = native.takeNativeOutput()
    native.finishNative(NativeRecordingCompletion.INCOMPLETE_PREFIX)
    var token: NativeRecordingCheckpoint? = null
    val nativeKinds = mutableListOf<NativeRecordingChunkKind>()
    while (true) {
        val chunk = native.takeNativeOutput() ?: break
        nativeKinds.add(chunk.kind)
        if (chunk.checkpoint != null) token = chunk.checkpoint
    }
    val encoded = native.encodeCheckpoint(token!!, ByteArray(32) { 7 }, ByteArray(32) { 9 }, ByteArray(32) { 13 })
    val decoded = decodeRecordingCheckpoint(encoded.bytes, ByteArray(32) { 7 }, ByteArray(32) { 9 })
    val extent = token.presentationExtent()
    val programme = NativeRecordingMuxer.withProgrammeAacTiming(0u, 4096u, 100u)
    programme.beginProgrammeWindow(NativeProgrammeWindowStart(7u, 48_000u, 8192u, 2048u, 8192u, 0u))
    val programmeSample = codec.sample(ByteBuffer.wrap(byteArrayOf(0x21, 0)), 56_192, 56_192, 1024u, 48_000u, false)
    programme.push(programmeSample)
    val programmeInitial = programme.takeProgrammeOutput()
    programme.finishProgramme(NativeProgrammeCompletion.INCOMPLETE_PREFIX)
    var programmeToken: NativeProgrammeCheckpoint? = null
    while (true) {
        val chunk = programme.takeProgrammeOutput() ?: break
        if (chunk.checkpoint != null) programmeToken = chunk.checkpoint
    }
    val encodedProgramme = programme.encodeProgrammeCheckpoint(programmeToken!!, ByteArray(32) { 7 }, ByteArray(32) { 9 }, ByteArray(32) { 13 })
    val decodedProgramme = decodeProgrammeCheckpoint(encodedProgramme.bytes, ByteArray(32) { 7 }, ByteArray(32) { 9 })
    val programmeExtent = programmeToken.presentationExtent()
    val session = NativeSession(90_000u)
    val generation = session.start(0u)
    val action = session.prepared(generation)
    val snapshot = session.snapshot()
    val rtmp = NativeRtmpPublisher("live", "fixture", "rtmp://origin.invalid/live", 0u, 0u)
    val rtmpState = rtmp.state()
    rtmp.cancel()
    val srt = NativeSrtPublisher(NativeSrtConfig("127.0.0.1:9000", "publish:test", null, 120u, 4096u, 4u * 1024u * 1024u, 3000u, 1000u, 1_250_000u), null, codec, null, 1_000_000u)
    val srtState = srt.state()
    srt.stop(1_000_000u)
    println("abi=${sdkAbiVersion()} sample=${sample.len()}:${sample.pts()}:${sample.duration()} fanout=${outcome.publishingQueued}:${outcome.recordingQueued} popped=${publishing?.pts()}:${recording?.pts()} initial=${initial?.bytes?.size} final=$finalLengths paired=${pairedOutcome.publishingQueued}:${pairedPublishing?.pts()}:${pairedRecording?.pts()}:${views.sourceInfo().nativePtsUs} native=${nativeInitial?.bytes?.size}:$nativeKinds:${encoded.bytes.size}:${decoded.info().facts.audioUnits}:${extent.durationTicks} programme=${programmeInitial?.bytes?.size}:${encodedProgramme.bytes.size}:${decodedProgramme.info().facts.audioUnits}:${programmeExtent.durationTicks} session=$action:${snapshot.state} rtmp=$rtmpState srt=$srtState")
}
