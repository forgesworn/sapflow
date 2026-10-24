import Foundation
import CryptoKit

precondition(sdkAbiVersion() == 1)
try checkSharedAacTimingViews()
try checkProgrammeWindowCheckpoints()
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
func checkProgrammeWindowCheckpoints() throws {
    // Synthetic compressed bytes and supplied scalars prove SDK container calls
    // only. Native source/EOF, a file and application admission remain external.
    let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
    let mux = try NativeRecordingMuxer.withProgrammeAacTiming(mediaEpochNs: 0,
        maxFragmentBytes: 4096, maxFragmentMs: 100)
    let start = NativeProgrammeWindowStart(generation: 7, codedOrigin: 48_000,
        preparationFrames: 8192, measuredDisplacement: 2048, discardPrefixFrames: 8192, movieDelayTicks: 0)
    let end = NativeProgrammeGenerationEnd(generation: 7,
        queued: NativeProgrammeQueuedFrames(preparation: 8192, programmeRaw: 1517,
            programmeZero: 1248, blockPadding: 307, codecDrain: 4096), codedFrames: 15_360)
    try mux.beginProgrammeWindow(start: start)
    let recording = Data(repeating: 7, count: 32)
    let file = Data(repeating: 9, count: 32)
    var first: NativeProgrammeCheckpoint?
    var firstSlot: Data?
    var firstRegion: Data?
    var previous: DecodedProgrammeCheckpoint?
    var initialSize = 0
    var initialHash = Data()
    var regionBytes: UInt64 = 0
    var hash = SHA256()
    var terminal = false
    func drain() throws {
        while let chunk = try mux.takeProgrammeOutput() {
            precondition(!chunk.bytes.isEmpty)
            if chunk.kind == .initial {
                precondition(initialSize == 0 && chunk.checkpoint == nil)
                initialSize = chunk.bytes.count
                initialHash = Data(SHA256.hash(data: chunk.bytes))
            }
            if chunk.kind == .finalHeader { precondition(chunk.bytes.count == initialSize) }
            if let issued = chunk.checkpoint {
                if first == nil { first = issued }
                precondition(chunk.kind == .fragment)
                regionBytes += UInt64(chunk.bytes.count); hash.update(data: chunk.bytes)
                let regionHash = Data(hash.finalize())
                let encoded = try mux.encodeProgrammeCheckpoint(token: issued, recordingBinding: recording,
                    sourceFileBinding: file, fragmentRegionSha256: regionHash)
                precondition(encoded.bytes.prefix(8) == Data("SFCP0002".utf8))
                precondition(encoded.info.facts == issued.facts() && encoded.info.start == start)
                precondition(encoded.info.initialHeaderLen == UInt32(initialSize))
                precondition(encoded.info.initialHeaderSha256 == initialHash)
                precondition(encoded.info.fragmentRegionBytes == regionBytes)
                precondition(encoded.info.committedOffset == UInt64(initialSize) + regionBytes)
                precondition(encoded.info.fragmentRegionSha256 == regionHash)
                let decoded = try decodeProgrammeCheckpoint(bytes: encoded.bytes,
                    expectedRecordingBinding: recording, expectedSourceFileBinding: file)
                precondition(decoded.info().facts == encoded.info.facts && decoded.info().start == start)
                let header = try mux.programmeCheckpointHeader(checkpoint: issued)
                precondition(decoded.header() == header)
                let decodedExtent = try decoded.presentationExtent()
                let issuedExtent = try issued.presentationExtent()
                precondition(decodedExtent == issuedExtent)
                if let previous { try validateProgrammeCheckpointSuccessor(prior: previous, next: decoded) }
                previous = decoded
                if firstSlot == nil { firstSlot = encoded.bytes; firstRegion = regionHash }
                terminal = issued.facts().windowGeometryComplete
                if terminal {
                    let facts = issued.facts()
                    precondition(facts.audioUnits == 5 && facts.audioStart == 56_192)
                    precondition(facts.audioEnd == 61_312 && facts.audioContainerEnd == 61_005)
                    precondition(facts.trailingFrames == 307 && issued.windowEnd() == end)
                    precondition(issuedExtent.durationTicks == 41_475)
                }
            }
        }
    }
    for n in 0..<5 {
        let tick = Int64(56_192 + n * 1024)
        let sample = try codec.sample(bytes: Data([0x21, UInt8(n)]), pts: tick, dts: tick,
            duration: 1024, timescale: 48_000, keyframe: false)
        try mux.push(sample: sample); try drain()
    }
    guard let issued = first, let savedSlot = firstSlot, let savedRegion = firstRegion else {
        fatalError("programme fragment did not issue exact token")
    }
    precondition(issued.facts().audioUnits == 4 && !issued.facts().windowGeometryComplete)
    let prefixExtent = try issued.presentationExtent()
    precondition(prefixExtent.durationTicks == 30_720)
    do { _ = try mux.takeNativeOutput(); fatalError("programme used converter output API") }
    catch NativeRecordingError.InvalidConfig {}
    let other = try NativeRecordingMuxer.withProgrammeAacTiming(mediaEpochNs: 0,
        maxFragmentBytes: 4096, maxFragmentMs: 100)
    try other.beginProgrammeWindow(start: start)
    for n in 0..<5 {
        let tick = Int64(56_192 + n * 1024)
        let sample = try codec.sample(bytes: Data([1]), pts: tick, dts: tick,
            duration: 1024, timescale: 48_000, keyframe: false)
        try other.push(sample: sample)
        while let _ = try other.takeProgrammeOutput() {}
    }
    do {
        _ = try other.encodeProgrammeCheckpoint(token: issued, recordingBinding: recording,
            sourceFileBinding: file, fragmentRegionSha256: savedRegion)
        fatalError("foreign programme token accepted")
    } catch RecordingCheckpointError.ForeignCheckpoint {}
    do {
        _ = try decodeRecordingCheckpoint(bytes: savedSlot,
            expectedRecordingBinding: recording, expectedSourceFileBinding: file)
        fatalError("V2 reinterpreted as V1")
    } catch RecordingCheckpointError.Unsupported {}
    try mux.closeProgrammeWindow(end: end)
    try mux.finishProgramme(outcome: .completeSubmittedWindow)
    try drain(); precondition(terminal)
    let stats = try mux.stats()
    precondition(stats.durationMs == 58 && stats.acceptedSamples == 5)
    try mux.cancel()
    let old = try mux.encodeProgrammeCheckpoint(token: issued, recordingBinding: recording,
        sourceFileBinding: file, fragmentRegionSha256: savedRegion)
    let oldExtent = try issued.presentationExtent()
    precondition(old.bytes == savedSlot && oldExtent == prefixExtent)

    let incomplete = try NativeRecordingMuxer.withProgrammeAacTiming(mediaEpochNs: 0,
        maxFragmentBytes: 4096, maxFragmentMs: 100)
    try incomplete.beginProgrammeWindow(start: start)
    for n in 0..<5 {
        let tick = Int64(56_192 + n * 1024)
        let sample = try codec.sample(bytes: Data([1]), pts: tick, dts: tick,
            duration: 1024, timescale: 48_000, keyframe: false)
        try incomplete.push(sample: sample)
        while let _ = try incomplete.takeProgrammeOutput() {}
    }
    try incomplete.closeProgrammeWindow(end: end)
    try incomplete.finishProgramme(outcome: .incompletePrefix)
    var incompleteChecked = false
    while let chunk = try incomplete.takeProgrammeOutput() {
        if let token = chunk.checkpoint {
            precondition(!token.facts().windowGeometryComplete && token.facts().trailingFrames == 0)
            incompleteChecked = true
        }
    }
    precondition(incompleteChecked)
    print("Sapflow Swift programme window checkpoints passed")
}

func checkNativeRecordingTimingBinding() throws {
    let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
    let mux = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true,
        mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
    try mux.beginNativeAudio(firstLeadingFrames: 2112)
    var firstSize = 0
    var finalSize = 0
    var sealed: NativeRecordingCheckpoint?
    var terminalFacts: NativeRecordingCheckpointFacts?
    var terminalExtent: NativeRecordingPresentationExtent?
    let recordingBinding = Data(repeating: 7, count: 32)
    let sourceFileBinding = Data(repeating: 9, count: 32)
    var initialHash = Data()
    var fragmentHash = SHA256()
    var regionBytes: UInt64 = 0
    var serializedEnd: UInt64 = 0
    var firstSlot: Data?
    var firstRegionHash: Data?
    var firstSealedEnd: UInt64?
    var previousDecoded: DecodedRecordingCheckpoint?
    var fragmentCount = 0
    func drain() throws {
        while let chunk = try mux.takeNativeOutput() {
            precondition(!chunk.bytes.isEmpty)
            if chunk.kind == .initial {
                precondition(firstSize == 0 && serializedEnd == 0 && chunk.checkpoint == nil)
                firstSize = chunk.bytes.count
                serializedEnd = UInt64(firstSize)
                initialHash = Data(SHA256.hash(data: chunk.bytes))
            }
            if chunk.kind == .finalHeader {
                // Offset-zero replacement contributes no fragment bytes/hash.
                finalSize = chunk.bytes.count
                precondition(previousDecoded?.info().committedOffset == serializedEnd)
            }
            if let token = chunk.checkpoint {
                let extent = try token.presentationExtent()
                precondition(extent.movieTimescale == 720_000)
                if token.facts().completeAudioEof {
                    terminalFacts = token.facts(); terminalExtent = extent
                }
            }
            if sealed == nil { sealed = chunk.checkpoint }
            if chunk.kind == .fragment {
                guard let token = chunk.checkpoint else { fatalError("fragment missing exact token") }
                precondition(firstSize > 0)
                fragmentCount += 1
                regionBytes += UInt64(chunk.bytes.count)
                serializedEnd += UInt64(chunk.bytes.count)
                fragmentHash.update(data: chunk.bytes)
                // CryptoKit.finalize is nonmutating; update the retained hash
                // only with original Fragment bytes, never the final rewrite.
                let regionHash = Data(fragmentHash.finalize())
                let encoded = try mux.encodeCheckpoint(token: token,
                    recordingBinding: recordingBinding, sourceFileBinding: sourceFileBinding,
                    fragmentRegionSha256: regionHash)
                precondition((545...66080).contains(encoded.bytes.count))
                let info = encoded.info
                precondition(info.facts == token.facts())
                precondition(info.initialHeaderLen == UInt32(firstSize) && info.mediaEpochNs == 0)
                precondition(info.initialHeaderSha256 == initialHash)
                precondition(info.recordingBinding == recordingBinding && info.sourceFileBinding == sourceFileBinding)
                precondition(info.committedOffset == serializedEnd && info.fragmentRegionBytes == regionBytes)
                precondition(info.fragmentRegionSha256 == regionHash)
                let decoded = try decodeRecordingCheckpoint(bytes: encoded.bytes,
                    expectedRecordingBinding: recordingBinding, expectedSourceFileBinding: sourceFileBinding)
                let parsed = decoded.info()
                precondition(parsed.facts == info.facts && parsed.mediaEpochNs == info.mediaEpochNs)
                precondition(parsed.initialHeaderLen == info.initialHeaderLen && parsed.initialHeaderSha256 == initialHash)
                precondition(parsed.recordingBinding == recordingBinding && parsed.sourceFileBinding == sourceFileBinding)
                precondition(parsed.committedOffset == serializedEnd && parsed.fragmentRegionBytes == regionBytes)
                precondition(parsed.fragmentRegionSha256 == regionHash && decoded.fragmentRegionSha256() == regionHash)
                let exactHeader = try mux.checkpointHeader(checkpoint: token)
                precondition(decoded.header() == exactHeader)
                if let prior = previousDecoded {
                    try validateRecordingCheckpointSuccessor(prior: prior, next: decoded)
                }
                previousDecoded = decoded
                if firstSlot == nil {
                    firstSlot = encoded.bytes
                    firstRegionHash = regionHash
                    firstSealedEnd = info.committedOffset
                }
            }
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
    let prefixExtent = try checkpoint.presentationExtent()
    precondition(prefixExtent.movieTimescale == 720_000 && prefixExtent.durationTicks == 29_760)
    precondition(prefixExtent.durationMsCeil == 42 && !prefixExtent.completeAudioEof)
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
    guard let savedSlot = firstSlot, let savedRegionHash = firstRegionHash,
          let savedEnd = firstSealedEnd else { fatalError("missing portable snapshot") }
    precondition(fragmentCount >= 2 && serializedEnd > savedEnd)
    let oldAfterAdvance = try mux.encodeCheckpoint(token: checkpoint,
        recordingBinding: recordingBinding, sourceFileBinding: sourceFileBinding,
        fragmentRegionSha256: savedRegionHash)
    precondition(oldAfterAdvance.bytes == savedSlot && oldAfterAdvance.info.committedOffset == savedEnd)
    let extentAfterAdvance = try checkpoint.presentationExtent()
    precondition(extentAfterAdvance == prefixExtent)
    let beforeRejection = try mux.stats()
    do {
        _ = try other.encodeCheckpoint(token: checkpoint, recordingBinding: recordingBinding,
            sourceFileBinding: sourceFileBinding, fragmentRegionSha256: savedRegionHash)
        fatalError("foreign live checkpoint encoded")
    } catch RecordingCheckpointError.ForeignCheckpoint {}
    do {
        _ = try decodeRecordingCheckpoint(bytes: savedSlot,
            expectedRecordingBinding: Data(repeating: 8, count: 32), expectedSourceFileBinding: sourceFileBinding)
        fatalError("foreign persisted recording binding accepted")
    } catch RecordingCheckpointError.Binding {}
    do {
        _ = try decodeRecordingCheckpoint(bytes: savedSlot,
            expectedRecordingBinding: recordingBinding, expectedSourceFileBinding: Data(repeating: 8, count: 32))
        fatalError("foreign persisted file binding accepted")
    } catch RecordingCheckpointError.Binding {}
    let afterRejection = try mux.stats()
    precondition(beforeRejection.acceptedSamples == afterRejection.acceptedSamples)
    precondition(beforeRejection.queuedOutputBytes == afterRejection.queuedOutputBytes)
    try mux.endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt(generation: 1,
        inputFrames: 6400, rawFrames: 6400, zeroFrames: 0, inputRate: 48_000,
        encodedFrames: 9216, leadingFrames: 2112, trailingFrames: 704, sourceMapping: .nearest))
    try mux.completeNativeAudio(receipt: NativeAudioFileReceipt(generationCount: 1,
        sourceFramesAt48k: 6400, intentionalStandbyFrames: 0, encodedFrames: 9216,
        firstLeadingFrames: 2112, lastTrailingFrames: 704))
    try mux.finishNative(outcome: .complete)
    try drain()
    precondition(finalSize == firstSize)
    guard let terminal = terminalFacts, let measured = terminalExtent else { fatalError("missing checked audio EOF checkpoint") }
    precondition(measured.movieTimescale == 720_000 && measured.durationTicks == 96_000)
    precondition(measured.durationMsCeil == 134 && measured.completeAudioEof)
    precondition(terminal.audioUnits == 9 && terminal.audioEnd == 9216)
    precondition(terminal.audioContainerEnd == 8512 && terminal.finalTrailingFrames == 704)
    precondition(terminal.completeAudioEof && terminal.completedGenerationCount == 1)
    let stats = try mux.stats()
    precondition(stats.finished && stats.acceptedSamples == 9)
    precondition(fragmentCount == 3 && serializedEnd == UInt64(firstSize) + regionBytes)
    precondition(previousDecoded?.info().committedOffset == serializedEnd)
    let oldAfterFinish = try mux.encodeCheckpoint(token: checkpoint,
        recordingBinding: recordingBinding, sourceFileBinding: sourceFileBinding,
        fragmentRegionSha256: savedRegionHash)
    precondition(oldAfterFinish.bytes == savedSlot && oldAfterFinish.info.committedOffset == savedEnd)
    let extentAfterFinish = try checkpoint.presentationExtent()
    precondition(extentAfterFinish == prefixExtent)
    do {
        _ = try mux.takeOutput()
        fatalError("native recording used legacy output API")
    } catch NativeRecordingError.InvalidConfig {}
    try mux.cancel()
    let extentAfterCancel = try checkpoint.presentationExtent()
    precondition(extentAfterCancel == prefixExtent)
}

// Generated API/ownership controls with synthetic bytes, never codec/AU proof.
func checkSharedAacTimingViews() throws {
    let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
    var original: MediaSample? = try codec.sample(bytes: Data([1, 2, 3]), pts: 4096, dts: 4096,
        duration: 1024, timescale: 48_000, keyframe: false)
    precondition(original!.aacSourceInfo() == nil)
    let views = try original!.aacTimingViews(producerGeneration: 17, nativePtsUs: -2112, publishingPts: 0)
    let info = views.sourceInfo()
    precondition(info.producerGeneration == 17 && info.nativePtsUs == -2112)
    precondition(info.originalPts == 4096 && info.originalDts == 4096)
    precondition(info.originalDuration == 1024 && info.originalTimescale == 48_000)
    original = nil // Native views retain their original independently.
    let queues = try NativeSampleFanout.withAacTimingViews(publishingBytes: 1024,
        recordingBytes: 1024, maxMediaMs: 1000, hasVideo: false)
    let first = try queues.pushAacTimingViews(views: views, nowNs: 0)
    precondition(first.publishingQueued && first.recordingQueued)
    let next = try codec.sample(bytes: Data([4, 5, 6]), pts: 5120, dts: 5120,
        duration: 1024, timescale: 48_000, keyframe: false)
    let withheld = try next.aacTimingViews(producerGeneration: 17, nativePtsUs: -1, publishingPts: nil)
    precondition(withheld.publishingSample() == nil)
    let second = try queues.pushAacTimingViews(views: withheld, nowNs: 1)
    precondition(!second.publishingQueued && second.recordingQueued)
    let stats = try queues.stats()
    precondition(stats.publishingDrops == 0)
    let publishing = try queues.popPublishing(nowNs: 2)!
    let recording = try queues.popRecording(nowNs: 2)!
    precondition(publishing.pts() == 0 && recording.pts() == 4096)
    precondition(publishing.aacSourceInfo() == info && recording.aacSourceInfo() == info)
    do {
        _ = try recording.aacTimingViews(producerGeneration: 17, nativePtsUs: 0, publishingPts: 0)
        fatalError("view-of-view accepted")
    } catch BindingError.InvalidMedia {}
    let tail = try queues.popRecording(nowNs: 2)!
    precondition(tail.pts() == 5120 && tail.aacSourceInfo()?.originalPts == 5120)
    let noPublishing = try queues.popPublishing(nowNs: 2)
    let noRecording = try queues.popRecording(nowNs: 2)
    precondition(noPublishing == nil && noRecording == nil)
    let stereo = try MediaCodec.aac(asc: Data([0x11, 0x90]))
    let unsupported = try stereo.sample(bytes: Data([1]), pts: 0, dts: 0,
        duration: 1024, timescale: 48_000, keyframe: false)
    do {
        _ = try unsupported.aacTimingViews(producerGeneration: 1, nativePtsUs: 0, publishingPts: 0)
        fatalError("stereo view accepted")
    } catch BindingError.InvalidMedia {}
    print("Sapflow Swift shared AAC timing views passed")
}
