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

// Network recovery is distinct from decoded readiness; exercise generated enums.
let recovery = try NativeSession(graceMs: 1)
let recoveryGeneration = try recovery.start(nowNs: 0)
_ = try recovery.prepared(generation: recoveryGeneration)
_ = try recovery.transportFailed(generation: recoveryGeneration, nowNs: 1)
_ = try recovery.replaceTransport(generation: recoveryGeneration, nowNs: 2)
let recoveryAttempt = try recovery.snapshot().attemptEpoch
try recovery.connected(generation: recoveryGeneration, nowNs: 3)
_ = try recovery.sending(generation: recoveryGeneration, attemptEpoch: recoveryAttempt, nowNs: 4)
_ = try recovery.transportRecovered(generation: recoveryGeneration, attemptEpoch: recoveryAttempt, nowNs: 5, source: .srtPeerAcknowledgedWatermark)
let networkOnly = try recovery.snapshot()
precondition(networkOnly.state == .sending && networkOnly.mediaReadiness == .unknown)
precondition(networkOnly.retryBudget == .available && networkOnly.networkRecoveries == 1 && networkOnly.nextDeadlineNs == nil)
_ = try recovery.transportFailed(generation: recoveryGeneration, nowNs: 10)
_ = try recovery.replaceTransport(generation: recoveryGeneration, nowNs: 11)
let uncertainAttempt = try recovery.snapshot().attemptEpoch
try recovery.connected(generation: recoveryGeneration, nowNs: 12)
_ = try recovery.sending(generation: recoveryGeneration, attemptEpoch: uncertainAttempt, nowNs: 13)
let exhaustedAction = try recovery.tick(generation: recoveryGeneration, nowNs: 1_000_010)
let uncertain = try recovery.snapshot()
precondition(exhaustedAction == .retryBudgetExhausted && uncertain.transportConnected)
precondition(uncertain.retryBudget == .exhausted && uncertain.nextDeadlineNs == nil && uncertain.outageDeadlineNs == 1_000_010)
do {
    _ = try recovery.transportRecovered(generation: recoveryGeneration, attemptEpoch: recoveryAttempt, nowNs: 1_000_011, source: .srtPeerAcknowledgedWatermark)
    fatalError("stale attempt recovered")
} catch BindingError.StaleSession {}
do {
    try recovery.mediaConfirmedForAttempt(generation: recoveryGeneration, attemptEpoch: recoveryAttempt, nowNs: UInt64.max)
    fatalError("old decoded observation confirmed successor")
} catch BindingError.StaleSession {}
let beforeDecoded = try recovery.snapshot()
precondition(beforeDecoded.mediaReadiness == .unknown)
// This is a control-only fixture, not an actual media decoder.
try recovery.mediaConfirmedForAttempt(generation: recoveryGeneration, attemptEpoch: uncertainAttempt, nowNs: 1_000_012)
let afterDecoded = try recovery.snapshot()
precondition(afterDecoded.state == .live && afterDecoded.mediaReadiness == .confirmed)
precondition(afterDecoded.retryBudget == .exhausted && afterDecoded.outageDeadlineNs == 1_000_010)
_ = try recovery.cancel(generation: recoveryGeneration)
do {
    try recovery.mediaConfirmedForAttempt(generation: recoveryGeneration, attemptEpoch: uncertainAttempt, nowNs: 1_000_013)
    fatalError("decoded observation resurrected cancelled session")
} catch BindingError.StaleSession {}

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
try rtmp.declareTracks(hasVideo: false, hasAudio: true)
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
let connectWire = try rtmp.takeOutput() // C2 and connect command, never written to a socket
var transferredWire = connectWire.count - 1536
// Independent fmt-0 AMF0 connect/createStream/Publish.Start response vectors.
let replies = [
    "AwAAAAAAPhQAAAAAAgAHX3Jlc3VsdAA/8AAAAAAAAAUDAARjb2RlAgAdTmV0Q29ubmVjdGlvbi5Db25uZWN0LlN1Y2Nlc3MAAAk=",
    "AwAAAAAAHRQAAAAAAgAHX3Jlc3VsdABAAAAAAAAAAAUAP/AAAAAAAAA=",
    "AwAAAAAAORQBAAAAAgAIb25TdGF0dXMAAAAAAAAAAAAFAwAEY29kZQIAF05ldFN0cmVhbS5QdWJsaXNoLlN0YXJ0AAAJ",
]
for (index, reply) in replies.enumerated() {
    try rtmp.feed(bytes: Data(base64Encoded: reply)!, nowMs: UInt64(index + 3))
    transferredWire += try rtmp.takeOutput().count
}
let sent: NativeRtmpSendOutput = try rtmp.sendAndTakeOutput(sample: frame, nowMs: 6)
precondition(!sent.output.isEmpty && sent.output.count <= 8 * 1024 * 1024)
precondition(sent.queuedOutputBytes == 0 && sent.nextDeadlineMs == nil && sent.state == .publishing)
precondition(sent.startupSubmitted && !sent.peerMediaProgress)
let sendingEvent = try rtmp.pollEvent()
precondition(sendingEvent == .sending)
transferredWire += sent.output.count
var acknowledgedWire = UInt32(transferredWire).bigEndian
var acknowledgement = Data([2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0])
withUnsafeBytes(of: &acknowledgedWire) { acknowledgement.append(contentsOf: $0) }
try rtmp.feed(bytes: acknowledgement, nowMs: 6)
let progressEvent = try rtmp.pollEvent()
precondition(progressEvent == nil)
let second = try codec.sample(bytes: Data(repeating: 0x21, count: 4096), pts: 1024, dts: 1024,
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
transferredWire += retainedOutput.count
acknowledgedWire = UInt32(transferredWire).bigEndian
acknowledgement = Data([2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0])
withUnsafeBytes(of: &acknowledgedWire) { acknowledgement.append(contentsOf: $0) }
try rtmp.feed(bytes: acknowledgement, nowMs: 7)
let guardedProgress = try rtmp.pollEvent()
precondition(guardedProgress == .peerMediaProgress)
try rtmp.feed(bytes: Data([2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0]), nowMs: 8)
let third = try codec.sample(bytes: Data([7, 8, 9]), pts: 2048, dts: 2048,
                            duration: 1024, timescale: 48_000, keyframe: false)
let blocked: NativeRtmpSendOutput = try rtmp.sendAndTakeOutput(sample: third, nowMs: 9)
precondition(blocked.output.count == 1 && blocked.queuedOutputBytes > 0)
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
try fanout.closePublishingAdmission()
try fanout.closePublishingAdmission()
let selectiveNext = try codec.sample(bytes: Data([4, 5, 6]), pts: 1024, dts: 1024,
                                    duration: 1024, timescale: 48_000, keyframe: false)
let recordingOnly = try fanout.push(sample: selectiveNext, nowNs: 1)
precondition(!recordingOnly.publishingQueued && recordingOnly.recordingQueued && !recordingOnly.recordingFailed)
let published = try fanout.popPublishing(nowNs: 1)
let recorded = try fanout.popRecording(nowNs: 1)
precondition(published?.len() == 3 && recorded?.timescale() == 48_000)
let publishingTail = try fanout.popPublishing(nowNs: 1)
precondition(publishingTail == nil)
let remainingRecord = try fanout.popRecording(nowNs: 1)
precondition(remainingRecord?.len() == 3)
try fanout.closeRecordingAdmission()
let noBranches = try fanout.push(sample: third, nowNs: 2)
precondition(!noBranches.publishingQueued && !noBranches.recordingQueued && !noBranches.recordingFailed)
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
try checkNativeRecordingTimingBinding()
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

// Synthetic compressed bytes exercise the generated API, not AAC decoding.
func checkNativeRecordingTimingBinding() throws {
    let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
    let mux = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true,
        mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
    try mux.beginNativeAudio(firstLeadingFrames: 2112)
    var firstSize = 0
    var finalSize = 0
    var sealed: NativeRecordingCheckpoint?
    var terminalFacts: NativeRecordingCheckpointFacts?
    func drain() throws {
        while let chunk = try mux.takeNativeOutput() {
            precondition(!chunk.bytes.isEmpty)
            if chunk.kind == .initial { firstSize = chunk.bytes.count }
            if chunk.kind == .finalHeader { finalSize = chunk.bytes.count }
            if let facts = chunk.checkpoint?.facts(), facts.completeAudioEof { terminalFacts = facts }
            if sealed == nil { sealed = chunk.checkpoint }
        }
    }
    for n in 0..<9 {
        let pts = Int64(n * 1024)
        let sample = try codec.sample(bytes: Data([1, 2, 3]), pts: pts, dts: pts,
            duration: 1024, timescale: 48_000, keyframe: false)
        try mux.push(sample: sample)
        try drain()
    }
    guard let checkpoint = sealed else { fatalError("missing sealed binding receipt") }
    let facts = checkpoint.facts()
    precondition(facts.audioUnits == 4 && facts.audioEnd == 4096)
    precondition(facts.completedGenerationCount == 0)
    precondition(facts.audioContainerEnd == facts.audioEnd && facts.finalTrailingFrames == 0 && !facts.completeAudioEof)
    let prefixHeader = try mux.checkpointHeader(checkpoint: checkpoint)
    precondition(firstSize > 0 && prefixHeader.count == firstSize)
    let other = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true,
        mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
    do {
        _ = try other.checkpointHeader(checkpoint: checkpoint)
        fatalError("foreign recording checkpoint accepted")
    } catch NativeRecordingError.InvalidMedia {}
    try mux.endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt(generation: 1,
        inputFrames: 6400, rawFrames: 6400, zeroFrames: 0, inputRate: 48_000,
        encodedFrames: 9216, leadingFrames: 2112, trailingFrames: 704, sourceMapping: .nearest))
    try mux.completeNativeAudio(receipt: NativeAudioFileReceipt(generationCount: 1,
        sourceFramesAt48k: 6400, intentionalStandbyFrames: 0, encodedFrames: 9216,
        firstLeadingFrames: 2112, lastTrailingFrames: 704))
    try mux.finishNative(outcome: .complete)
    try drain()
    precondition(finalSize == firstSize)
    guard let terminal = terminalFacts else { fatalError("missing checked audio EOF checkpoint") }
    precondition(terminal.audioUnits == 9 && terminal.audioEnd == 9216)
    precondition(terminal.audioContainerEnd == 8512 && terminal.finalTrailingFrames == 704)
    precondition(terminal.completeAudioEof && terminal.completedGenerationCount == 1)
    let stats = try mux.stats()
    precondition(stats.finished && stats.acceptedSamples == 9)
    do {
        _ = try mux.takeOutput()
        fatalError("native recording used legacy output API")
    } catch NativeRecordingError.InvalidConfig {}
}
