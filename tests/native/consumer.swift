import Foundation

precondition(sdkAbiVersion() == 1)
// Exercise actual generated lifecycle control; no network/capture is attached.
let lifecycle = try NativeSession(graceMs: 90_000)
let lifecycleGeneration = try lifecycle.start(nowNs: 0)
let preparedAction = try lifecycle.prepared(generation: lifecycleGeneration)
precondition(preparedAction == .connect)
try lifecycle.connected(generation: lifecycleGeneration, nowNs: 1)
try lifecycle.mediaConfirmed(generation: lifecycleGeneration, nowNs: 2)
let replaceHealthy = try lifecycle.replaceTransport(generation: lifecycleGeneration, nowNs: 200_000_000_000)
let healthySnapshot = try lifecycle.snapshot()
precondition(replaceHealthy == .connect && healthySnapshot.generation == lifecycleGeneration)
precondition(healthySnapshot.state == .connecting && !healthySnapshot.transportConnected)
precondition(healthySnapshot.reconnectDeadlineNs == nil && healthySnapshot.transportFailures == 0)
_ = try lifecycle.transportFailed(generation: lifecycleGeneration, nowNs: 200_000_000_010)
let outageBefore = try lifecycle.snapshot()
let outageDeadline = outageBefore.reconnectDeadlineNs!
precondition(outageDeadline == 290_000_000_010)
let replaceOutage = try lifecycle.replaceTransport(generation: lifecycleGeneration, nowNs: 200_000_000_020)
let outageAfter = try lifecycle.snapshot()
precondition(replaceOutage == .connect && outageAfter.state == .reconnecting)
precondition(outageAfter.reconnectDeadlineNs == outageDeadline && !outageAfter.transportConnected)
precondition(outageAfter.transportFailures == 1)
let expiredReplacement = try lifecycle.replaceTransport(generation: lifecycleGeneration, nowNs: outageDeadline)
precondition(expiredReplacement == .graceExpired)
let lifecycleCancelled = try lifecycle.cancel(generation: lifecycleGeneration)
do {
    _ = try lifecycle.replaceTransport(generation: lifecycleGeneration, nowNs: outageDeadline)
    fatalError("stale replacement accepted")
} catch BindingError.StaleSession {}
let lifecycleStopped = try lifecycle.snapshot()
precondition(lifecycleStopped.generation == lifecycleCancelled && lifecycleStopped.state == .stopped)

var input = Data([1, 2, 3, 4])
let sample = try EncodedBuffer(bytes: input)
input.resetBytes(in: 0..<input.count)
precondition(sample.len() == 4)
let first = try sample.byteAt(index: 0)
precondition(first == 1)
do {
    _ = try EncodedBuffer(bytes: Data())
    fatalError("empty input accepted")
} catch BindingError.EmptyBuffer {
    // The expected native error, not a process crash.
}
do {
    _ = try sample.byteAt(index: UInt64.max)
    fatalError("invalid index accepted")
} catch BindingError.IndexOutOfBounds {}
let codec = try MediaCodec.aac(asc: Data([0x11, 0x90]))
let frame = try codec.sample(bytes: Data([1, 2, 3]), pts: 0, dts: 0,
                             duration: 1024, timescale: 48_000, keyframe: false)

// Socket-free peer fixture: exercise generated return-record lifting as well
// as errors. No destination or networking implementation is attached here.
let rtmp = try NativeRtmpPublisher(app: "live", streamKey: "synthetic",
    tcUrl: "rtmp://origin.invalid/live", mediaEpochNs: 0, nowMs: 0)
let pendingHandshake = try rtmp.queuedOutputBytes()
precondition(pendingHandshake == 1537)
do {
    _ = try rtmp.sendAndTakeOutput(sample: frame, nowMs: 1)
    fatalError("prepublication fused send accepted media")
} catch NativeRtmpError.NotPublishing {}
let preservedHandshake = try rtmp.queuedOutputBytes()
let startupDeadline = try rtmp.nextDeadlineMs()
precondition(preservedHandshake == pendingHandshake && startupDeadline == 15_000)
let c0c1 = try rtmp.takeOutput()
precondition(c0c1.count == 1537 && c0c1.first == 3)
var response = Data([3])
response.append(Data(repeating: 0, count: 1536)) // original S1, no digest version
response.append(c0c1.dropFirst()) // S2 echoes C1
try rtmp.feed(bytes: response, nowMs: 2)
_ = try rtmp.takeOutput() // C2 and connect command, never written to a socket
// Independent fmt-0 AMF0 connect/createStream/Publish.Start response vectors.
let replies = [
    "AwAAAAAAPhQAAAAAAgAHX3Jlc3VsdAA/8AAAAAAAAAUDAARjb2RlAgAdTmV0Q29ubmVjdGlvbi5Db25uZWN0LlN1Y2Nlc3MAAAk=",
    "AwAAAAAAHRQAAAAAAgAHX3Jlc3VsdABAAAAAAAAAAAUAP/AAAAAAAAA=",
    "AwAAAAAAORQBAAAAAgAIb25TdGF0dXMAAAAAAAAAAAAFAwAEY29kZQIAF05ldFN0cmVhbS5QdWJsaXNoLlN0YXJ0AAAJ",
]
for (index, reply) in replies.enumerated() {
    try rtmp.feed(bytes: Data(base64Encoded: reply)!, nowMs: UInt64(index + 3))
    _ = try rtmp.takeOutput()
}
let sent: NativeRtmpSendOutput = try rtmp.sendAndTakeOutput(sample: frame, nowMs: 6)
precondition(!sent.output.isEmpty && sent.output.count <= 8 * 1024 * 1024)
precondition(sent.queuedOutputBytes == 0 && sent.nextDeadlineMs == nil && sent.state == .publishing)
let second = try codec.sample(bytes: Data([4, 5, 6]), pts: 1024, dts: 1024,
                             duration: 1024, timescale: 48_000, keyframe: false)
try rtmp.send(sample: second)
let queuedBeforeError = try rtmp.queuedOutputBytes()
do {
    _ = try rtmp.sendAndTakeOutput(sample: second, nowMs: 7)
    fatalError("duplicate fused sample was accepted")
} catch NativeRtmpError.InvalidMedia {}
let queuedAfterError = try rtmp.queuedOutputBytes()
let retainedOutput = try rtmp.takeOutput()
precondition(queuedBeforeError > 0 && queuedAfterError == queuedBeforeError && !retainedOutput.isEmpty)
try rtmp.feed(bytes: Data([2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0]), nowMs: 8)
let third = try codec.sample(bytes: Data([7, 8, 9]), pts: 2048, dts: 2048,
                            duration: 1024, timescale: 48_000, keyframe: false)
let blocked: NativeRtmpSendOutput = try rtmp.sendAndTakeOutput(sample: third, nowMs: 9)
precondition(blocked.output.isEmpty && blocked.queuedOutputBytes > 0)
precondition(blocked.nextDeadlineMs == 1009 && blocked.state == .publishing)
let fourth = try codec.sample(bytes: Data([10, 11, 12]), pts: 3072, dts: 3072,
                             duration: 1024, timescale: 48_000, keyframe: false)
do {
    _ = try rtmp.sendAndTakeOutput(sample: fourth, nowMs: 1009)
    fatalError("stale queued wire media was accepted")
} catch NativeRtmpError.Timeout {}
try rtmp.cancel()
do {
    _ = try rtmp.sendAndTakeOutput(sample: fourth, nowMs: 1010)
    fatalError("cancelled fused publisher accepted media")
} catch NativeRtmpError.Closed {}

let fanout = try NativeSampleFanout(publishingBytes: 1024, recordingBytes: 1024,
                                  maxMediaMs: 1000, hasVideo: false)
let queued = try fanout.push(sample: frame, nowNs: 0)
precondition(queued.publishingQueued && queued.recordingQueued)
let published = try fanout.popPublishing(nowNs: 1)
let recorded = try fanout.popRecording(nowNs: 1)
precondition(published?.len() == 3 && recorded?.timescale() == 48_000)
try fanout.cancel()
do {
    _ = try fanout.push(sample: frame, nowNs: 2)
    fatalError("cancelled sample fanout accepted media")
} catch BindingError.Closed {}
let recorder = try NativeRecordingMuxer(hasVideo: false, hasAudio: true,
                                      mediaEpochNs: 0, maxFragmentBytes: 4096,
                                      maxFragmentMs: 1000)
try recorder.push(sample: frame)
let header = try recorder.takeOutput()
precondition(header != nil && !header!.bytes.isEmpty && !header!.rewriteHeader)
try recorder.finish()
var finalHeader = false
while let write = try recorder.takeOutput() {
    precondition(!write.bytes.isEmpty)
    finalHeader = finalHeader || write.rewriteHeader
}
precondition(finalHeader)
let srtConfig = NativeSrtConfig(destination: "127.0.0.1:9000", streamId: "synthetic",
    cipher: nil, latencyMs: 120, maxBufferedPackets: 64, maxBufferedBytes: 84_224,
    maxPacketAgeMs: 3000, maxSampleResidenceMs: 1000, bandwidthBytesPerSecond: 1_250_000)
let srt = try NativeSrtPublisher(config: srtConfig, video: nil, audio: codec,
                                 passphrase: nil, nowUs: 0)
let handshake = try srt.pollDatagram(nowUs: 0)
precondition(handshake != nil && handshake!.bytes.count <= 1500)
let nextDeadline = try srt.nextDeadlineUs()
precondition(nextDeadline != nil)
try srt.stop(nowUs: 0)
let finalState = try srt.state()
let finalStats = try srt.stats()
precondition(finalState == .closed)
precondition(finalStats.bufferedBytes == 0)
print("Sapflow Swift ownership consumer passed")
