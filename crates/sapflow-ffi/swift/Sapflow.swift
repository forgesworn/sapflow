import SapflowFFI
import Foundation

private let abiIsCompatible = sapflow_abi_version() == SAPFLOW_ABI_VERSION
private struct CapiFailure: Error { let code: String }
private enum ErrorKind { case binding, recording, checkpoint, rtmp, session, srt }

private func take(_ buffer: SapflowBuffer) -> Data {
    guard let ptr = buffer.ptr, buffer.len > 0 else { sapflow_buffer_free(buffer); return Data() }
    return Data(bytesNoCopy: ptr, count: buffer.len, deallocator: .custom { ptr, len in
        sapflow_buffer_free(SapflowBuffer(ptr: ptr.assumingMemoryBound(to: UInt8.self), len: len))
    })
}
private func withData<R>(_ data: Data, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try data.withUnsafeBytes { try body($0.baseAddress?.assumingMemoryBound(to: UInt8.self), $0.count) }
}
private func invoke(_ operation: UInt32, _ kind: ErrorKind, _ handle: UnsafeRawPointer? = nil, _ input: Data = Data()) throws -> Data {
    precondition(abiIsCompatible, "Sapflow native/library ABI mismatch")
    return try withData(input) { ptr, len in
        var out = SapflowBuffer(), error = SapflowBuffer()
        let status = sapflow_call(operation, handle, ptr, len, &out, &error)
        if status != 0 { throw nativeError(kind, String(decoding: take(error), as: UTF8.self)) }
        return take(out)
    }
}
private func newHandle(_ operation: UInt32, _ error: ErrorKind, _ input: Data) throws -> UnsafeMutableRawPointer {
    let value = try Reader(invoke(operation, error, nil, input)).handle()
    guard let handle = UnsafeMutableRawPointer(bitPattern: UInt(value)) else { throw CapiFailure(code: "Argument") }
    return handle
}
private func hotHandle(_ kind: ErrorKind, _ body: (UnsafeMutablePointer<UnsafeMutableRawPointer?>, UnsafeMutablePointer<SapflowBuffer>) -> Int32) throws -> UnsafeMutableRawPointer {
    var out: UnsafeMutableRawPointer?, error = SapflowBuffer()
    let status = body(&out, &error)
    if status != 0 { throw nativeError(kind, String(decoding: take(error), as: UTF8.self)) }
    guard let out else { throw CapiFailure(code: "Argument") }
    return out
}
private func hotSample(_ kind: ErrorKind, _ body: (UnsafeMutablePointer<SapflowMediaSampleResult>, UnsafeMutablePointer<SapflowBuffer>) -> Int32) throws -> SapflowMediaSampleResult {
    var out = SapflowMediaSampleResult(), error = SapflowBuffer()
    let status = body(&out, &error)
    if status != 0 { throw nativeError(kind, String(decoding: take(error), as: UTF8.self)) }
    return out
}
private func sampleMetadata(_ raw: UnsafeRawPointer?) throws -> SapflowMediaSampleResult {
    try hotSample(.binding) { out, error in sapflow_media_sample_metadata(raw, out, error) }
}
private func check(_ status: Int32, _ kind: ErrorKind, _ error: SapflowBuffer) throws {
    if status != 0 { throw nativeError(kind, String(decoding: take(error), as: UTF8.self)) }
}
private func encodedScalar(_ raw: UnsafeRawPointer?, _ operation: UInt32, _ index: UInt64 = 0) throws -> UInt64 {
    var out: UInt64 = 0, error = SapflowBuffer()
    let status = sapflow_encoded_buffer_scalar(raw, operation, index, &out, &error)
    if status != 0 { throw nativeError(.binding, String(decoding: take(error), as: UTF8.self)) }
    return out
}

private final class Writer {
    private(set) var data = Data()
    func u8(_ value: Int) { data.append(UInt8(value)) }
    func bool(_ value: Bool) { u8(value ? 1 : 0) }
    func u32(_ value: UInt32) { withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) } }
    func u64(_ value: UInt64) { withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) } }
    func i64(_ value: Int64) { u64(UInt64(bitPattern: value)) }
    func bytes(_ value: Data) { u64(UInt64(value.count)); data.append(value) }
    func string(_ value: String) { bytes(Data(value.utf8)) }
    func handle(_ value: NativeHandle) { u64(value.rawValue) }
    func optional(_ value: NativeHandle?) { bool(value != nil); if let value { handle(value) } }
    func optional(_ value: UInt64?) { bool(value != nil); if let value { u64(value) } }
    func optional(_ value: Int64?) { bool(value != nil); if let value { i64(value) } }
}
private func request(_ body: (Writer) -> Void) -> Data { let w = Writer(); body(w); return w.data }
private final class Reader {
    let data: Data; var at = 0
    init(_ data: Data) { self.data = data }
    func take(_ count: Int) throws -> Data {
        guard count >= 0, at + count <= data.count else { throw CapiFailure(code: "Argument") }
        defer { at += count }; return data.subdata(in: at..<at + count)
    }
    func u8() throws -> Int { Int(try take(1)[0]) }
    func bool() throws -> Bool { switch try u8() { case 0: false; case 1: true; default: throw CapiFailure(code: "Argument") } }
    func u32() throws -> UInt32 { try take(4).withUnsafeBytes { $0.loadUnaligned(as: UInt32.self).littleEndian } }
    func u64() throws -> UInt64 { try take(8).withUnsafeBytes { $0.loadUnaligned(as: UInt64.self).littleEndian } }
    func i64() throws -> Int64 { Int64(bitPattern: try u64()) }
    func bytes() throws -> Data { try take(Int(try u64())) }
    func string() throws -> String { String(decoding: try bytes(), as: UTF8.self) }
    func handle() throws -> UInt64 { try u64() }
    func optionalU64() throws -> UInt64? { try bool() ? u64() : nil }
    func optionalHandle() throws -> UInt64? { try bool() ? u64() : nil }
    func done() throws { guard at == data.count else { throw CapiFailure(code: "Argument") } }
}
private func response<T>(_ data: Data, _ body: (Reader) throws -> T) throws -> T {
    let reader = Reader(data); let value = try body(reader); try reader.done(); return value
}

open class NativeHandle: @unchecked Sendable {
    fileprivate let kind: UInt32
    fileprivate let raw: UnsafeMutableRawPointer?
    fileprivate var rawValue: UInt64 { UInt64(UInt(bitPattern: raw)) }
    fileprivate init(kind: UInt32, raw: UnsafeMutableRawPointer?) { self.kind = kind; self.raw = raw }
    deinit { if let raw { sapflow_object_free(kind, raw) } }
}

public enum BindingError: Error { case EmptyBuffer, BufferTooLarge, IndexOutOfBounds, InvalidMedia, InvalidQueueLimits, ClockWentBackwards, Busy, Closed, InvalidSession, StaleSession }
public enum NativeRecordingError: Error { case InvalidConfig, InvalidMedia, Busy, Backpressure, Closed, MissingTracks, Encoding }
public enum RecordingCheckpointError: Error { case Busy, ForeignCheckpoint, DigestLength, Envelope, Unsupported, Checksum, Binding, Facts, Successor, Overflow }
public enum NativeRtmpError: Error { case InvalidConfig, Busy, Closed, NotPublishing, Backpressure, InvalidMedia, ProtocolViolation, Refused, Timeout, UnsupportedHandshake }
public enum NativeSrtError: Error { case InvalidConfig, InvalidMedia, ConfigurationChanged, InvalidDatagram, ClockWentBackwards, ClockOverflow, Busy, Backpressure, NotConnected, Closed }
extension BindingError: Equatable, Hashable, LocalizedError { public var errorDescription: String? { String(reflecting: self) } }
extension NativeRecordingError: Equatable, Hashable, LocalizedError { public var errorDescription: String? { String(reflecting: self) } }
extension RecordingCheckpointError: Equatable, Hashable, LocalizedError { public var errorDescription: String? { String(reflecting: self) } }
extension NativeRtmpError: Equatable, Hashable, LocalizedError { public var errorDescription: String? { String(reflecting: self) } }
extension NativeSrtError: Equatable, Hashable, LocalizedError { public var errorDescription: String? { String(reflecting: self) } }
private func nativeError(_ kind: ErrorKind, _ code: String) -> Error {
    switch kind {
    case .binding, .session:
        switch code { case "EmptyBuffer": BindingError.EmptyBuffer; case "BufferTooLarge": BindingError.BufferTooLarge
        case "IndexOutOfBounds": BindingError.IndexOutOfBounds; case "InvalidMedia": BindingError.InvalidMedia
        case "InvalidQueueLimits": BindingError.InvalidQueueLimits; case "ClockWentBackwards": BindingError.ClockWentBackwards
        case "Busy": BindingError.Busy; case "Closed": BindingError.Closed; case "InvalidSession": BindingError.InvalidSession
        case "StaleSession": BindingError.StaleSession; default: CapiFailure(code: code) }
    case .recording:
        switch code { case "InvalidConfig": NativeRecordingError.InvalidConfig; case "InvalidMedia": NativeRecordingError.InvalidMedia
        case "Busy": NativeRecordingError.Busy; case "Backpressure": NativeRecordingError.Backpressure; case "Closed": NativeRecordingError.Closed
        case "MissingTracks": NativeRecordingError.MissingTracks; case "Encoding": NativeRecordingError.Encoding; default: CapiFailure(code: code) }
    case .checkpoint:
        switch code { case "Busy": RecordingCheckpointError.Busy; case "ForeignCheckpoint": RecordingCheckpointError.ForeignCheckpoint
        case "DigestLength": RecordingCheckpointError.DigestLength; case "Envelope": RecordingCheckpointError.Envelope
        case "Unsupported": RecordingCheckpointError.Unsupported; case "Checksum": RecordingCheckpointError.Checksum
        case "Binding": RecordingCheckpointError.Binding; case "Facts": RecordingCheckpointError.Facts
        case "Successor": RecordingCheckpointError.Successor; case "Overflow": RecordingCheckpointError.Overflow; default: CapiFailure(code: code) }
    case .rtmp:
        switch code { case "InvalidConfig": NativeRtmpError.InvalidConfig; case "Busy": NativeRtmpError.Busy; case "Closed": NativeRtmpError.Closed
        case "NotPublishing": NativeRtmpError.NotPublishing; case "Backpressure": NativeRtmpError.Backpressure; case "InvalidMedia": NativeRtmpError.InvalidMedia
        case "ProtocolViolation": NativeRtmpError.ProtocolViolation; case "Refused": NativeRtmpError.Refused; case "Timeout": NativeRtmpError.Timeout
        case "UnsupportedHandshake": NativeRtmpError.UnsupportedHandshake; default: CapiFailure(code: code) }
    case .srt:
        switch code { case "InvalidConfig": NativeSrtError.InvalidConfig; case "InvalidMedia": NativeSrtError.InvalidMedia
        case "ConfigurationChanged": NativeSrtError.ConfigurationChanged; case "InvalidDatagram": NativeSrtError.InvalidDatagram
        case "ClockWentBackwards": NativeSrtError.ClockWentBackwards; case "ClockOverflow": NativeSrtError.ClockOverflow
        case "Busy": NativeSrtError.Busy; case "Backpressure": NativeSrtError.Backpressure; case "NotConnected": NativeSrtError.NotConnected
        case "Closed": NativeSrtError.Closed; default: CapiFailure(code: code) }
    }
}

public enum NativeMediaReadiness { case unknown, confirmed }
public enum NativeProgrammeCompletion { case completeSubmittedWindow, incompletePrefix }
public enum NativeRecordingChunkKind { case initial, fragment, finalHeader }
public enum NativeRecordingCompletion { case complete, incompletePrefix }
public enum NativeRecordingSourceMapping { case nearest, floor }
public enum NativeRetryBudgetState { case available, windowOpen, exhausted }
public enum NativeRtmpEvent { case sending, peerMediaProgress }
public enum NativeRtmpState { case handshaking, connecting, creatingStream, awaitingPublish, publishing, finishing, closed, failed }
public enum NativeSessionState { case idle, preparing, connecting, sending, live, reconnecting, interrupted, stopping, stopped, failed }
public enum NativeSrtCipher { case aes128, aes256 }
public enum NativeSrtCloseReason { case cancelled, socketSendFailed, connectTimeout, handshakeLimit, peerIdle, refused, unsupportedPeer, crypto, sampleExpired, mediaExpired, retryLimit, peerClosed, peerError, packagingFailed }
public enum NativeSrtState { case connecting, connected, closed }
public enum NativeTransportRecoverySource { case srtPeerAcknowledgedWatermark, rtmpPeerAcknowledgedWatermark }
public enum NativeSessionAction { case none, connect, retryAt(atNs: UInt64), awaitGraceDeadline(atNs: UInt64), endDestination, graceExpired, retryBudgetExhausted }
public enum NativeSrtEvent { case sending, peerMediaProgress, connected(latencyMs: UInt32, maxPayloadBytes: UInt32), failed(reason: NativeSrtCloseReason), closed(reason: NativeSrtCloseReason, peerCode: UInt32?) }
extension NativeMediaReadiness: Sendable, Equatable, Hashable {}; extension NativeProgrammeCompletion: Sendable, Equatable, Hashable {}
extension NativeRecordingChunkKind: Sendable, Equatable, Hashable {}; extension NativeRecordingCompletion: Sendable, Equatable, Hashable {}
extension NativeRecordingSourceMapping: Sendable, Equatable, Hashable {}; extension NativeRetryBudgetState: Sendable, Equatable, Hashable {}
extension NativeRtmpEvent: Sendable, Equatable, Hashable {}; extension NativeRtmpState: Sendable, Equatable, Hashable {}
extension NativeSessionState: Sendable, Equatable, Hashable {}; extension NativeSrtCipher: Sendable, Equatable, Hashable {}
extension NativeSrtCloseReason: Sendable, Equatable, Hashable {}; extension NativeSrtState: Sendable, Equatable, Hashable {}
extension NativeTransportRecoverySource: Sendable, Equatable, Hashable {}; extension NativeSessionAction: Sendable, Equatable, Hashable {}
extension NativeSrtEvent: Sendable, Equatable, Hashable {}

private let mediaReadiness: [NativeMediaReadiness] = [.unknown, .confirmed]
private let retryStates: [NativeRetryBudgetState] = [.available, .windowOpen, .exhausted]
private let rtmpStates: [NativeRtmpState] = [.handshaking, .connecting, .creatingStream, .awaitingPublish, .publishing, .finishing, .closed, .failed]
private let sessionStates: [NativeSessionState] = [.idle, .preparing, .connecting, .sending, .live, .reconnecting, .interrupted, .stopping, .stopped, .failed]
private let recoverySources: [NativeTransportRecoverySource] = [.srtPeerAcknowledgedWatermark, .rtmpPeerAcknowledgedWatermark]
private let srtStates: [NativeSrtState] = [.connecting, .connected, .closed]
private let srtCloseReasons: [NativeSrtCloseReason] = [.cancelled, .socketSendFailed, .connectTimeout, .handshakeLimit, .peerIdle, .refused, .unsupportedPeer, .crypto, .sampleExpired, .mediaExpired, .retryLimit, .peerClosed, .peerError, .packagingFailed]
private let chunkKinds: [NativeRecordingChunkKind] = [.initial, .fragment, .finalHeader]
private func indexed<T>(_ values: [T], _ index: Int) throws -> T { guard values.indices.contains(index) else { throw CapiFailure(code: "Argument") }; return values[index] }

public struct DecodedProgrammeCheckpointInfo: Sendable, Equatable, Hashable { public var facts: NativeProgrammeCheckpointFacts, start: NativeProgrammeWindowStart, windowEnd: NativeProgrammeGenerationEnd?, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data; public init(facts: NativeProgrammeCheckpointFacts, start: NativeProgrammeWindowStart, windowEnd: NativeProgrammeGenerationEnd?, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data) { self.facts=facts;self.start=start;self.windowEnd=windowEnd;self.mediaEpochNs=mediaEpochNs;self.initialHeaderLen=initialHeaderLen;self.initialHeaderSha256=initialHeaderSha256;self.recordingBinding=recordingBinding;self.sourceFileBinding=sourceFileBinding;self.committedOffset=committedOffset;self.fragmentRegionBytes=fragmentRegionBytes;self.fragmentRegionSha256=fragmentRegionSha256 } }
public struct DecodedRecordingCheckpointInfo: Sendable, Equatable, Hashable { public var facts: NativeRecordingCheckpointFacts, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data; public init(facts: NativeRecordingCheckpointFacts, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data) { self.facts=facts;self.mediaEpochNs=mediaEpochNs;self.initialHeaderLen=initialHeaderLen;self.initialHeaderSha256=initialHeaderSha256;self.recordingBinding=recordingBinding;self.sourceFileBinding=sourceFileBinding;self.committedOffset=committedOffset;self.fragmentRegionBytes=fragmentRegionBytes;self.fragmentRegionSha256=fragmentRegionSha256 } }
public struct EncodedProgrammeCheckpointInfo: Sendable, Equatable, Hashable { public var facts: NativeProgrammeCheckpointFacts, start: NativeProgrammeWindowStart, windowEnd: NativeProgrammeGenerationEnd?, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data; public init(facts: NativeProgrammeCheckpointFacts, start: NativeProgrammeWindowStart, windowEnd: NativeProgrammeGenerationEnd?, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data) { self.facts=facts;self.start=start;self.windowEnd=windowEnd;self.mediaEpochNs=mediaEpochNs;self.initialHeaderLen=initialHeaderLen;self.initialHeaderSha256=initialHeaderSha256;self.recordingBinding=recordingBinding;self.sourceFileBinding=sourceFileBinding;self.committedOffset=committedOffset;self.fragmentRegionBytes=fragmentRegionBytes;self.fragmentRegionSha256=fragmentRegionSha256 } }
public struct EncodedRecordingCheckpointInfo: Sendable, Equatable, Hashable { public var facts: NativeRecordingCheckpointFacts, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data; public init(facts: NativeRecordingCheckpointFacts, mediaEpochNs: UInt64, initialHeaderLen: UInt32, initialHeaderSha256: Data, recordingBinding: Data, sourceFileBinding: Data, committedOffset: UInt64, fragmentRegionBytes: UInt64, fragmentRegionSha256: Data) { self.facts=facts;self.mediaEpochNs=mediaEpochNs;self.initialHeaderLen=initialHeaderLen;self.initialHeaderSha256=initialHeaderSha256;self.recordingBinding=recordingBinding;self.sourceFileBinding=sourceFileBinding;self.committedOffset=committedOffset;self.fragmentRegionBytes=fragmentRegionBytes;self.fragmentRegionSha256=fragmentRegionSha256 } }

public struct FanoutOutcome: Sendable, Equatable, Hashable { public var publishingQueued: Bool, recordingQueued: Bool, publishingWaitingKeyframe: Bool, publishingOverflows: UInt64, recordingFailed: Bool; public init(publishingQueued: Bool, recordingQueued: Bool, publishingWaitingKeyframe: Bool, publishingOverflows: UInt64, recordingFailed: Bool) { self.publishingQueued=publishingQueued;self.recordingQueued=recordingQueued;self.publishingWaitingKeyframe=publishingWaitingKeyframe;self.publishingOverflows=publishingOverflows;self.recordingFailed=recordingFailed } }
public struct NativeAacSampleSourceInfo: Sendable, Equatable, Hashable { public var producerGeneration: UInt64, nativePtsUs: Int64, originalPts: Int64, originalDts: Int64, originalDuration: UInt32, originalTimescale: UInt32; public init(producerGeneration: UInt64, nativePtsUs: Int64, originalPts: Int64, originalDts: Int64, originalDuration: UInt32, originalTimescale: UInt32) { self.producerGeneration=producerGeneration;self.nativePtsUs=nativePtsUs;self.originalPts=originalPts;self.originalDts=originalDts;self.originalDuration=originalDuration;self.originalTimescale=originalTimescale } }
public struct NativeAudioFileReceipt: Sendable, Equatable, Hashable { public var generationCount: UInt64, sourceFramesAt48k: UInt64, intentionalStandbyFrames: UInt64, encodedFrames: UInt64, firstLeadingFrames: UInt32, lastTrailingFrames: UInt32; public init(generationCount: UInt64, sourceFramesAt48k: UInt64, intentionalStandbyFrames: UInt64, encodedFrames: UInt64, firstLeadingFrames: UInt32, lastTrailingFrames: UInt32) { self.generationCount=generationCount;self.sourceFramesAt48k=sourceFramesAt48k;self.intentionalStandbyFrames=intentionalStandbyFrames;self.encodedFrames=encodedFrames;self.firstLeadingFrames=firstLeadingFrames;self.lastTrailingFrames=lastTrailingFrames } }
public struct NativeAudioGenerationReceipt: Sendable, Equatable, Hashable { public var generation: UInt64, inputFrames: UInt64, rawFrames: UInt64, zeroFrames: UInt64, inputRate: UInt32, encodedFrames: UInt64, leadingFrames: UInt32, trailingFrames: UInt32, sourceMapping: NativeRecordingSourceMapping; public init(generation: UInt64, inputFrames: UInt64, rawFrames: UInt64, zeroFrames: UInt64, inputRate: UInt32, encodedFrames: UInt64, leadingFrames: UInt32, trailingFrames: UInt32, sourceMapping: NativeRecordingSourceMapping) { self.generation=generation;self.inputFrames=inputFrames;self.rawFrames=rawFrames;self.zeroFrames=zeroFrames;self.inputRate=inputRate;self.encodedFrames=encodedFrames;self.leadingFrames=leadingFrames;self.trailingFrames=trailingFrames;self.sourceMapping=sourceMapping } }
public struct NativeProgrammeCheckpointEncoding: Sendable, Equatable, Hashable { public var bytes: Data, info: EncodedProgrammeCheckpointInfo; public init(bytes: Data, info: EncodedProgrammeCheckpointInfo) { self.bytes=bytes;self.info=info } }
public struct NativeProgrammeCheckpointFacts: Sendable, Equatable, Hashable { public var sequence: UInt32, audioUnits: UInt64, audioStart: UInt64, audioEnd: UInt64, audioContainerEnd: UInt64, leadingFrames: UInt32, trailingFrames: UInt32, windowGeometryComplete: Bool; public init(sequence: UInt32, audioUnits: UInt64, audioStart: UInt64, audioEnd: UInt64, audioContainerEnd: UInt64, leadingFrames: UInt32, trailingFrames: UInt32, windowGeometryComplete: Bool) { self.sequence=sequence;self.audioUnits=audioUnits;self.audioStart=audioStart;self.audioEnd=audioEnd;self.audioContainerEnd=audioContainerEnd;self.leadingFrames=leadingFrames;self.trailingFrames=trailingFrames;self.windowGeometryComplete=windowGeometryComplete } }
public struct NativeProgrammeChunk: Sendable { public var bytes: Data, kind: NativeRecordingChunkKind, checkpoint: NativeProgrammeCheckpoint?; public init(bytes: Data, kind: NativeRecordingChunkKind, checkpoint: NativeProgrammeCheckpoint?) { self.bytes=bytes;self.kind=kind;self.checkpoint=checkpoint } }
public struct NativeProgrammeGenerationEnd: Sendable, Equatable, Hashable { public var generation: UInt64, queued: NativeProgrammeQueuedFrames, codedFrames: UInt64; public init(generation: UInt64, queued: NativeProgrammeQueuedFrames, codedFrames: UInt64) { self.generation=generation;self.queued=queued;self.codedFrames=codedFrames } }
public struct NativeProgrammePresentationExtent: Sendable, Equatable, Hashable { public var movieTimescale: UInt32, durationTicks: UInt64, durationMsCeil: UInt64, windowGeometryComplete: Bool; public init(movieTimescale: UInt32, durationTicks: UInt64, durationMsCeil: UInt64, windowGeometryComplete: Bool) { self.movieTimescale=movieTimescale;self.durationTicks=durationTicks;self.durationMsCeil=durationMsCeil;self.windowGeometryComplete=windowGeometryComplete } }
public struct NativeProgrammeQueuedFrames: Sendable, Equatable, Hashable { public var preparation: UInt64, programmeRaw: UInt64, programmeZero: UInt64, blockPadding: UInt64, codecDrain: UInt64; public init(preparation: UInt64, programmeRaw: UInt64, programmeZero: UInt64, blockPadding: UInt64, codecDrain: UInt64) { self.preparation=preparation;self.programmeRaw=programmeRaw;self.programmeZero=programmeZero;self.blockPadding=blockPadding;self.codecDrain=codecDrain } }
public struct NativeProgrammeWindowStart: Sendable, Equatable, Hashable { public var generation: UInt64, codedOrigin: UInt64, preparationFrames: UInt64, measuredDisplacement: UInt64, discardPrefixFrames: UInt64, movieDelayTicks: UInt64; public init(generation: UInt64, codedOrigin: UInt64, preparationFrames: UInt64, measuredDisplacement: UInt64, discardPrefixFrames: UInt64, movieDelayTicks: UInt64) { self.generation=generation;self.codedOrigin=codedOrigin;self.preparationFrames=preparationFrames;self.measuredDisplacement=measuredDisplacement;self.discardPrefixFrames=discardPrefixFrames;self.movieDelayTicks=movieDelayTicks } }
public struct NativeQueueStats: Sendable, Equatable, Hashable { public var publishingBytes: UInt64, recordingBytes: UInt64, publishingDrops: UInt64, recordingDrops: UInt64, publishingHighWaterBytes: UInt64, recordingHighWaterBytes: UInt64, publishingMediaMs: UInt64, publishingHighWaterMediaMs: UInt64, publishingOverflows: UInt64, publishingOverflowBytes: UInt64, publishingOverflowSamples: UInt64, publishingOverflowMedia: UInt64, publishingOverflowResidence: UInt64, publishingWaitingKeyframe: Bool, recordingFailed: Bool; public init(publishingBytes: UInt64, recordingBytes: UInt64, publishingDrops: UInt64, recordingDrops: UInt64, publishingHighWaterBytes: UInt64, recordingHighWaterBytes: UInt64, publishingMediaMs: UInt64, publishingHighWaterMediaMs: UInt64, publishingOverflows: UInt64, publishingOverflowBytes: UInt64, publishingOverflowSamples: UInt64, publishingOverflowMedia: UInt64, publishingOverflowResidence: UInt64, publishingWaitingKeyframe: Bool, recordingFailed: Bool) { self.publishingBytes=publishingBytes;self.recordingBytes=recordingBytes;self.publishingDrops=publishingDrops;self.recordingDrops=recordingDrops;self.publishingHighWaterBytes=publishingHighWaterBytes;self.recordingHighWaterBytes=recordingHighWaterBytes;self.publishingMediaMs=publishingMediaMs;self.publishingHighWaterMediaMs=publishingHighWaterMediaMs;self.publishingOverflows=publishingOverflows;self.publishingOverflowBytes=publishingOverflowBytes;self.publishingOverflowSamples=publishingOverflowSamples;self.publishingOverflowMedia=publishingOverflowMedia;self.publishingOverflowResidence=publishingOverflowResidence;self.publishingWaitingKeyframe=publishingWaitingKeyframe;self.recordingFailed=recordingFailed } }
public struct NativeRecordingCheckpointEncoding: Sendable, Equatable, Hashable { public var bytes: Data, info: EncodedRecordingCheckpointInfo; public init(bytes: Data, info: EncodedRecordingCheckpointInfo) { self.bytes=bytes;self.info=info } }
public struct NativeRecordingCheckpointFacts: Sendable, Equatable, Hashable { public var sequence: UInt32, audioUnits: UInt64, videoUnits: UInt64, audioStart: UInt64, audioEnd: UInt64, audioContainerEnd: UInt64, finalTrailingFrames: UInt32, completeAudioEof: Bool, videoStart: UInt64?, videoEnd: UInt64?, firstLeadingFrames: UInt32, completedGenerationCount: UInt64; public init(sequence: UInt32, audioUnits: UInt64, videoUnits: UInt64, audioStart: UInt64, audioEnd: UInt64, audioContainerEnd: UInt64, finalTrailingFrames: UInt32, completeAudioEof: Bool, videoStart: UInt64?, videoEnd: UInt64?, firstLeadingFrames: UInt32, completedGenerationCount: UInt64) { self.sequence=sequence;self.audioUnits=audioUnits;self.videoUnits=videoUnits;self.audioStart=audioStart;self.audioEnd=audioEnd;self.audioContainerEnd=audioContainerEnd;self.finalTrailingFrames=finalTrailingFrames;self.completeAudioEof=completeAudioEof;self.videoStart=videoStart;self.videoEnd=videoEnd;self.firstLeadingFrames=firstLeadingFrames;self.completedGenerationCount=completedGenerationCount } }
public struct NativeRecordingChunk: Sendable { public var bytes: Data, kind: NativeRecordingChunkKind, checkpoint: NativeRecordingCheckpoint?; public init(bytes: Data, kind: NativeRecordingChunkKind, checkpoint: NativeRecordingCheckpoint?) { self.bytes=bytes;self.kind=kind;self.checkpoint=checkpoint } }
public struct NativeRecordingPresentationExtent: Sendable, Equatable, Hashable { public var movieTimescale: UInt32, durationTicks: UInt64, durationMsCeil: UInt64, completeAudioEof: Bool; public init(movieTimescale: UInt32, durationTicks: UInt64, durationMsCeil: UInt64, completeAudioEof: Bool) { self.movieTimescale=movieTimescale;self.durationTicks=durationTicks;self.durationMsCeil=durationMsCeil;self.completeAudioEof=completeAudioEof } }
public struct NativeRecordingStats: Sendable, Equatable, Hashable { public var bufferedBytes: UInt64, bufferedSamples: UInt64, queuedOutputBytes: UInt64, acceptedSamples: UInt64, fragments: UInt64, durationMs: UInt64, finished: Bool; public init(bufferedBytes: UInt64, bufferedSamples: UInt64, queuedOutputBytes: UInt64, acceptedSamples: UInt64, fragments: UInt64, durationMs: UInt64, finished: Bool) { self.bufferedBytes=bufferedBytes;self.bufferedSamples=bufferedSamples;self.queuedOutputBytes=queuedOutputBytes;self.acceptedSamples=acceptedSamples;self.fragments=fragments;self.durationMs=durationMs;self.finished=finished } }
public struct NativeRtmpSendOutput: Sendable, Equatable, Hashable { public var output: Data, queuedOutputBytes: UInt64, nextDeadlineMs: UInt64?, state: NativeRtmpState, startupSubmitted: Bool, peerMediaProgress: Bool; public init(output: Data, queuedOutputBytes: UInt64, nextDeadlineMs: UInt64?, state: NativeRtmpState, startupSubmitted: Bool, peerMediaProgress: Bool) { self.output=output;self.queuedOutputBytes=queuedOutputBytes;self.nextDeadlineMs=nextDeadlineMs;self.state=state;self.startupSubmitted=startupSubmitted;self.peerMediaProgress=peerMediaProgress } }
public struct NativeSessionSnapshot: Sendable, Equatable, Hashable { public var generation: UInt64, attemptEpoch: UInt64, mediaReadiness: NativeMediaReadiness, retryBudget: NativeRetryBudgetState, transportRecoverySource: NativeTransportRecoverySource?, networkRecoveries: UInt64, outageDeadlineNs: UInt64?, nextDeadlineNs: UInt64?, state: NativeSessionState, transportConnected: Bool, reconnectDeadlineNs: UInt64?, transportFailures: UInt64, retryAttempts: UInt64, mediaConfirmations: UInt64, staleCallbacks: UInt64; public init(generation: UInt64, attemptEpoch: UInt64, mediaReadiness: NativeMediaReadiness, retryBudget: NativeRetryBudgetState, transportRecoverySource: NativeTransportRecoverySource?, networkRecoveries: UInt64, outageDeadlineNs: UInt64?, nextDeadlineNs: UInt64?, state: NativeSessionState, transportConnected: Bool, reconnectDeadlineNs: UInt64?, transportFailures: UInt64, retryAttempts: UInt64, mediaConfirmations: UInt64, staleCallbacks: UInt64) { self.generation=generation;self.attemptEpoch=attemptEpoch;self.mediaReadiness=mediaReadiness;self.retryBudget=retryBudget;self.transportRecoverySource=transportRecoverySource;self.networkRecoveries=networkRecoveries;self.outageDeadlineNs=outageDeadlineNs;self.nextDeadlineNs=nextDeadlineNs;self.state=state;self.transportConnected=transportConnected;self.reconnectDeadlineNs=reconnectDeadlineNs;self.transportFailures=transportFailures;self.retryAttempts=retryAttempts;self.mediaConfirmations=mediaConfirmations;self.staleCallbacks=staleCallbacks } }
public struct NativeSrtConfig: Sendable, Equatable, Hashable { public var destination: String, streamId: String, cipher: NativeSrtCipher?, latencyMs: UInt32, maxBufferedPackets: UInt32, maxBufferedBytes: UInt32, maxPacketAgeMs: UInt32, maxSampleResidenceMs: UInt32, bandwidthBytesPerSecond: UInt64; public init(destination: String, streamId: String, cipher: NativeSrtCipher?, latencyMs: UInt32, maxBufferedPackets: UInt32, maxBufferedBytes: UInt32, maxPacketAgeMs: UInt32, maxSampleResidenceMs: UInt32, bandwidthBytesPerSecond: UInt64) { self.destination=destination;self.streamId=streamId;self.cipher=cipher;self.latencyMs=latencyMs;self.maxBufferedPackets=maxBufferedPackets;self.maxBufferedBytes=maxBufferedBytes;self.maxPacketAgeMs=maxPacketAgeMs;self.maxSampleResidenceMs=maxSampleResidenceMs;self.bandwidthBytesPerSecond=bandwidthBytesPerSecond } }
public struct NativeSrtDatagram: Sendable, Equatable, Hashable { public var destination: String, bytes: Data; public init(destination: String, bytes: Data) { self.destination=destination;self.bytes=bytes } }
public struct NativeSrtStats: Sendable, Equatable, Hashable { public var acceptedSamples: UInt64, acceptedSampleBytes: UInt64, bufferedSampleBytes: UInt64, bufferedSampleDurationNs: UInt64, discardedSampleBytes: UInt64, pendingChunkBytes: UInt64, pendingChunkCapacity: UInt64, discardedPendingChunkBytes: UInt64, bufferedPackets: UInt64, bufferedBytes: UInt64, peakBufferedBytes: UInt64, sentPackets: UInt64, sentBytes: UInt64, retransmittedPackets: UInt64, retransmittedBytes: UInt64, acknowledgedBytes: UInt64, abandonedBytes: UInt64, encryptedPackets: UInt64, kmRequests: UInt64, kmResponses: UInt64, rttUs: UInt64, malformedDatagrams: UInt64, strayDatagrams: UInt64, rejectedDatagrams: UInt64, backpressureEvents: UInt64, ffiInputHandleClones: UInt64, sdkInputHandleClones: UInt64, tsOutputAllocations: UInt64, elementaryStagingCopyBytes: UInt64, packetPayloadCopyBytes: UInt64, writerPayloadCopyBytes: UInt64, tsSerialisedBytes: UInt64, returnedDatagrams: UInt64, returnedDatagramBytes: UInt64; public init(acceptedSamples: UInt64, acceptedSampleBytes: UInt64, bufferedSampleBytes: UInt64, bufferedSampleDurationNs: UInt64, discardedSampleBytes: UInt64, pendingChunkBytes: UInt64, pendingChunkCapacity: UInt64, discardedPendingChunkBytes: UInt64, bufferedPackets: UInt64, bufferedBytes: UInt64, peakBufferedBytes: UInt64, sentPackets: UInt64, sentBytes: UInt64, retransmittedPackets: UInt64, retransmittedBytes: UInt64, acknowledgedBytes: UInt64, abandonedBytes: UInt64, encryptedPackets: UInt64, kmRequests: UInt64, kmResponses: UInt64, rttUs: UInt64, malformedDatagrams: UInt64, strayDatagrams: UInt64, rejectedDatagrams: UInt64, backpressureEvents: UInt64, ffiInputHandleClones: UInt64, sdkInputHandleClones: UInt64, tsOutputAllocations: UInt64, elementaryStagingCopyBytes: UInt64, packetPayloadCopyBytes: UInt64, writerPayloadCopyBytes: UInt64, tsSerialisedBytes: UInt64, returnedDatagrams: UInt64, returnedDatagramBytes: UInt64) { self.acceptedSamples=acceptedSamples;self.acceptedSampleBytes=acceptedSampleBytes;self.bufferedSampleBytes=bufferedSampleBytes;self.bufferedSampleDurationNs=bufferedSampleDurationNs;self.discardedSampleBytes=discardedSampleBytes;self.pendingChunkBytes=pendingChunkBytes;self.pendingChunkCapacity=pendingChunkCapacity;self.discardedPendingChunkBytes=discardedPendingChunkBytes;self.bufferedPackets=bufferedPackets;self.bufferedBytes=bufferedBytes;self.peakBufferedBytes=peakBufferedBytes;self.sentPackets=sentPackets;self.sentBytes=sentBytes;self.retransmittedPackets=retransmittedPackets;self.retransmittedBytes=retransmittedBytes;self.acknowledgedBytes=acknowledgedBytes;self.abandonedBytes=abandonedBytes;self.encryptedPackets=encryptedPackets;self.kmRequests=kmRequests;self.kmResponses=kmResponses;self.rttUs=rttUs;self.malformedDatagrams=malformedDatagrams;self.strayDatagrams=strayDatagrams;self.rejectedDatagrams=rejectedDatagrams;self.backpressureEvents=backpressureEvents;self.ffiInputHandleClones=ffiInputHandleClones;self.sdkInputHandleClones=sdkInputHandleClones;self.tsOutputAllocations=tsOutputAllocations;self.elementaryStagingCopyBytes=elementaryStagingCopyBytes;self.packetPayloadCopyBytes=packetPayloadCopyBytes;self.writerPayloadCopyBytes=writerPayloadCopyBytes;self.tsSerialisedBytes=tsSerialisedBytes;self.returnedDatagrams=returnedDatagrams;self.returnedDatagramBytes=returnedDatagramBytes } }
public struct NativeStopResult: Sendable, Equatable, Hashable { public var generation: UInt64, action: NativeSessionAction; public init(generation: UInt64, action: NativeSessionAction) { self.generation=generation;self.action=action } }
public struct RecordingWrite: Sendable, Equatable, Hashable { public var bytes: Data, rewriteHeader: Bool; public init(bytes: Data, rewriteHeader: Bool) { self.bytes=bytes;self.rewriteHeader=rewriteHeader } }

private extension Reader {
    func recordingFacts() throws -> NativeRecordingCheckpointFacts { try .init(sequence: u32(), audioUnits: u64(), videoUnits: u64(), audioStart: u64(), audioEnd: u64(), audioContainerEnd: u64(), finalTrailingFrames: u32(), completeAudioEof: bool(), videoStart: optionalU64(), videoEnd: optionalU64(), firstLeadingFrames: u32(), completedGenerationCount: u64()) }
    func programmeFacts() throws -> NativeProgrammeCheckpointFacts { try .init(sequence: u32(), audioUnits: u64(), audioStart: u64(), audioEnd: u64(), audioContainerEnd: u64(), leadingFrames: u32(), trailingFrames: u32(), windowGeometryComplete: bool()) }
    func programmeStart() throws -> NativeProgrammeWindowStart { try .init(generation: u64(), codedOrigin: u64(), preparationFrames: u64(), measuredDisplacement: u64(), discardPrefixFrames: u64(), movieDelayTicks: u64()) }
    func programmeEnd() throws -> NativeProgrammeGenerationEnd { try .init(generation: u64(), queued: .init(preparation: u64(), programmeRaw: u64(), programmeZero: u64(), blockPadding: u64(), codecDrain: u64()), codedFrames: u64()) }
    func encodedRecordingInfo() throws -> EncodedRecordingCheckpointInfo { try .init(facts: recordingFacts(), mediaEpochNs: u64(), initialHeaderLen: u32(), initialHeaderSha256: bytes(), recordingBinding: bytes(), sourceFileBinding: bytes(), committedOffset: u64(), fragmentRegionBytes: u64(), fragmentRegionSha256: bytes()) }
    func decodedRecordingInfo() throws -> DecodedRecordingCheckpointInfo { try .init(facts: recordingFacts(), mediaEpochNs: u64(), initialHeaderLen: u32(), initialHeaderSha256: bytes(), recordingBinding: bytes(), sourceFileBinding: bytes(), committedOffset: u64(), fragmentRegionBytes: u64(), fragmentRegionSha256: bytes()) }
    func encodedProgrammeInfo() throws -> EncodedProgrammeCheckpointInfo { try .init(facts: programmeFacts(), start: programmeStart(), windowEnd: bool() ? programmeEnd() : nil, mediaEpochNs: u64(), initialHeaderLen: u32(), initialHeaderSha256: bytes(), recordingBinding: bytes(), sourceFileBinding: bytes(), committedOffset: u64(), fragmentRegionBytes: u64(), fragmentRegionSha256: bytes()) }
    func decodedProgrammeInfo() throws -> DecodedProgrammeCheckpointInfo { try .init(facts: programmeFacts(), start: programmeStart(), windowEnd: bool() ? programmeEnd() : nil, mediaEpochNs: u64(), initialHeaderLen: u32(), initialHeaderSha256: bytes(), recordingBinding: bytes(), sourceFileBinding: bytes(), committedOffset: u64(), fragmentRegionBytes: u64(), fragmentRegionSha256: bytes()) }
    func sessionAction() throws -> NativeSessionAction { switch try u8() { case 0: .none;case 1: .connect;case 2: try .retryAt(atNs: u64());case 3: try .awaitGraceDeadline(atNs: u64());case 4: .endDestination;case 5: .graceExpired;case 6: .retryBudgetExhausted;default: throw CapiFailure(code: "Argument") } }
}
private extension Writer {
    func programmeStart(_ v: NativeProgrammeWindowStart) { u64(v.generation);u64(v.codedOrigin);u64(v.preparationFrames);u64(v.measuredDisplacement);u64(v.discardPrefixFrames);u64(v.movieDelayTicks) }
    func programmeEnd(_ v: NativeProgrammeGenerationEnd) { u64(v.generation);u64(v.queued.preparation);u64(v.queued.programmeRaw);u64(v.queued.programmeZero);u64(v.queued.blockPadding);u64(v.queued.codecDrain);u64(v.codedFrames) }
    func generation(_ v: NativeAudioGenerationReceipt) { u64(v.generation);u64(v.inputFrames);u64(v.rawFrames);u64(v.zeroFrames);u32(v.inputRate);u64(v.encodedFrames);u32(v.leadingFrames);u32(v.trailingFrames);u8(v.sourceMapping == .nearest ? 0 : 1) }
    func fileReceipt(_ v: NativeAudioFileReceipt) { u64(v.generationCount);u64(v.sourceFramesAt48k);u64(v.intentionalStandbyFrames);u64(v.encodedFrames);u32(v.firstLeadingFrames);u32(v.lastTrailingFrames) }
    func srtConfig(_ v: NativeSrtConfig) { string(v.destination);string(v.streamId);bool(v.cipher != nil);if let cipher=v.cipher { u8(cipher == .aes128 ? 0 : 1) };u32(v.latencyMs);u32(v.maxBufferedPackets);u32(v.maxBufferedBytes);u32(v.maxPacketAgeMs);u32(v.maxSampleResidenceMs);u64(v.bandwidthBytesPerSecond) }
}

public protocol EncodedBufferProtocol: AnyObject, Sendable { func len() -> UInt64; func isEmpty() -> Bool; func byteAt(index: UInt64) throws -> UInt8 }
open class EncodedBuffer: NativeHandle, EncodedBufferProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    private let length: UInt64
    required public init(unsafeFromHandle handle: UInt64) {
        let raw = UnsafeMutableRawPointer(bitPattern: UInt(handle))
        length = raw == nil ? 0 : try! encodedScalar(raw, 3)
        super.init(kind: 1, raw: raw)
    }
    public init(noHandle: NoHandle) { length = 0; super.init(kind: 1, raw: nil) }
    private init(handle: UnsafeMutableRawPointer, length: UInt64) { self.length = length; super.init(kind: 1, raw: handle) }
    public convenience init(bytes: Data) throws {
        let handle = try withData(bytes) { ptr, len in try hotHandle(.binding) { out, error in sapflow_encoded_buffer_new(ptr, len, out, error) } }
        self.init(handle: handle, length: UInt64(bytes.count))
    }
    open func len() -> UInt64 { length }
    open func isEmpty() -> Bool { length == 0 }
    open func byteAt(index: UInt64) throws -> UInt8 { try UInt8(encodedScalar(raw, 5, index)) }
}
public func sdkAbiVersion() -> UInt32 { try! response(try! invoke(1, .binding)) { try $0.u32() } }

public protocol MediaCodecProtocol: AnyObject, Sendable { func sample(bytes: Data, pts: Int64, dts: Int64, duration: UInt32, timescale: UInt32, keyframe: Bool) throws -> MediaSample }
open class MediaCodec: NativeHandle, MediaCodecProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    private let video: Bool?
    required public init(unsafeFromHandle handle: UInt64) { video = nil; super.init(kind: 2, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { video = nil; super.init(kind: 2, raw: nil) }
    private init(handle: UnsafeMutableRawPointer, video: Bool) { self.video = video; super.init(kind: 2, raw: handle) }
    public static func h264(sps: Data, pps: Data) throws -> MediaCodec { MediaCodec(handle: try newHandle(6, .binding, request { $0.bytes(sps);$0.bytes(pps) }), video: true) }
    public static func aac(asc: Data) throws -> MediaCodec { MediaCodec(handle: try newHandle(7, .binding, request { $0.bytes(asc) }), video: false) }
    open func sample(bytes: Data, pts: Int64, dts: Int64, duration: UInt32, timescale: UInt32, keyframe: Bool) throws -> MediaSample {
        let handle = try withData(bytes) { ptr, len in try hotHandle(.binding) { out, error in sapflow_media_codec_sample(raw, ptr, len, pts, dts, duration, timescale, keyframe, out, error) } }
        guard let video else { return MediaSample(unsafeFromHandle: UInt64(UInt(bitPattern: handle))) }
        return MediaSample(handle: handle, length: UInt64(bytes.count), pts: pts, dts: dts, duration: duration, timescale: timescale, isVideo: video, isKeyframe: keyframe)
    }
}
public protocol MediaSampleProtocol: AnyObject, Sendable {
    func aacTimingViews(producerGeneration: UInt64, nativePtsUs: Int64, publishingPts: Int64?) throws -> NativeAacTimingViews
    func aacSourceInfo() -> NativeAacSampleSourceInfo?; func len() -> UInt64; func isEmpty() -> Bool
    func isVideo() -> Bool; func isKeyframe() -> Bool; func pts() -> Int64; func dts() -> Int64; func duration() -> UInt32; func timescale() -> UInt32
}
open class MediaSample: NativeHandle, MediaSampleProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    private let length: UInt64
    private let presentation: Int64
    private let decode: Int64
    private let sampleDuration: UInt32
    private let sampleTimescale: UInt32
    private let video: Bool
    private let keyframe: Bool
    required public convenience init(unsafeFromHandle handle: UInt64) {
        let raw = UnsafeMutableRawPointer(bitPattern: UInt(handle))
        self.init(result: raw == nil ? SapflowMediaSampleResult() : try! sampleMetadata(raw))
    }
    public convenience init(noHandle: NoHandle) { self.init(result: SapflowMediaSampleResult()) }
    fileprivate init(result: SapflowMediaSampleResult) {
        length = result.len
        presentation = result.pts
        decode = result.dts
        sampleDuration = result.duration
        sampleTimescale = result.timescale
        video = result.is_video
        keyframe = result.is_keyframe
        super.init(kind: 3, raw: result.handle)
    }
    fileprivate init(handle: UnsafeMutableRawPointer, length: UInt64, pts: Int64, dts: Int64, duration: UInt32, timescale: UInt32, isVideo: Bool, isKeyframe: Bool) {
        self.length = length
        presentation = pts
        decode = dts
        sampleDuration = duration
        sampleTimescale = timescale
        video = isVideo
        keyframe = isKeyframe
        super.init(kind: 3, raw: handle)
    }
    open func aacTimingViews(producerGeneration: UInt64, nativePtsUs: Int64, publishingPts: Int64?) throws -> NativeAacTimingViews {
        NativeAacTimingViews(unsafeFromHandle: try response(try invoke(9, .binding, raw, request { $0.u64(producerGeneration);$0.i64(nativePtsUs);$0.optional(publishingPts) })) { try $0.handle() })
    }
    open func aacSourceInfo() -> NativeAacSampleSourceInfo? { try! response(try! invoke(10, .binding, raw)) { try $0.bool() ? .init(producerGeneration: $0.u64(), nativePtsUs: $0.i64(), originalPts: $0.i64(), originalDts: $0.i64(), originalDuration: $0.u32(), originalTimescale: $0.u32()) : nil } }
    open func len() -> UInt64 { length }
    open func isEmpty() -> Bool { length == 0 }
    open func isVideo() -> Bool { video }
    open func isKeyframe() -> Bool { keyframe }
    open func pts() -> Int64 { presentation }
    open func dts() -> Int64 { decode }
    open func duration() -> UInt32 { sampleDuration }
    open func timescale() -> UInt32 { sampleTimescale }
}
public protocol NativeAacTimingViewsProtocol: AnyObject, Sendable { func sourceInfo() -> NativeAacSampleSourceInfo; func recordingSample() -> MediaSample; func publishingSample() -> MediaSample? }
open class NativeAacTimingViews: NativeHandle, NativeAacTimingViewsProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 4, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 4, raw: nil) }
    open func sourceInfo() -> NativeAacSampleSourceInfo { try! response(try! invoke(19, .binding, raw)) { try .init(producerGeneration: $0.u64(), nativePtsUs: $0.i64(), originalPts: $0.i64(), originalDts: $0.i64(), originalDuration: $0.u32(), originalTimescale: $0.u32()) } }
    open func recordingSample() -> MediaSample { MediaSample(result: try! hotSample(.binding) { out, error in sapflow_aac_timing_views_sample(raw, false, out, error) }) }
    open func publishingSample() -> MediaSample? {
        let result = try! hotSample(.binding) { out, error in sapflow_aac_timing_views_sample(raw, true, out, error) }
        return result.handle.map { _ in MediaSample(result: result) }
    }
}
public protocol NativeSampleFanoutProtocol: AnyObject, Sendable {
    func pushAacTimingViews(views: NativeAacTimingViews, nowNs: UInt64) throws -> FanoutOutcome
    func push(sample: MediaSample, nowNs: UInt64) throws -> FanoutOutcome
    func popPublishing(nowNs: UInt64) throws -> MediaSample?; func popRecording(nowNs: UInt64) throws -> MediaSample?
    func stats() throws -> NativeQueueStats; func closePublishingAdmission() throws; func closeRecordingAdmission() throws; func cancel() throws
}
open class NativeSampleFanout: NativeHandle, NativeSampleFanoutProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 5, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 5, raw: nil) }
    public init(publishingBytes: UInt64, recordingBytes: UInt64, maxMediaMs: UInt32, hasVideo: Bool) throws { super.init(kind: 5, raw: try newHandle(22, .binding, request { $0.u64(publishingBytes);$0.u64(recordingBytes);$0.u32(maxMediaMs);$0.bool(hasVideo) })) }
    public static func withAacTimingViews(publishingBytes: UInt64, recordingBytes: UInt64, maxMediaMs: UInt32, hasVideo: Bool) throws -> NativeSampleFanout { NativeSampleFanout(unsafeFromHandle: UInt64(UInt(bitPattern: try newHandle(23, .binding, request { $0.u64(publishingBytes);$0.u64(recordingBytes);$0.u32(maxMediaMs);$0.bool(hasVideo) })))) }
    private func outcome(_ other: UnsafeRawPointer?, _ nowNs: UInt64, aacViews: Bool) throws -> FanoutOutcome {
        var out = SapflowFanoutOutcome(), error = SapflowBuffer()
        let status = aacViews ? sapflow_sample_fanout_push_aac_timing_views(raw, other, nowNs, &out, &error) : sapflow_sample_fanout_push(raw, other, nowNs, &out, &error)
        if status != 0 { throw nativeError(.binding, String(decoding: take(error), as: UTF8.self)) }
        return FanoutOutcome(publishingQueued: out.publishing_queued, recordingQueued: out.recording_queued, publishingWaitingKeyframe: out.publishing_waiting_keyframe, publishingOverflows: out.publishing_overflows, recordingFailed: out.recording_failed)
    }
    private func pop(recording: Bool, nowNs: UInt64) throws -> MediaSample? {
        let result = try hotSample(.binding) { out, error in sapflow_sample_fanout_pop(raw, recording, nowNs, out, error) }
        return result.handle.map { _ in MediaSample(result: result) }
    }
    open func pushAacTimingViews(views: NativeAacTimingViews, nowNs: UInt64) throws -> FanoutOutcome { try outcome(views.raw, nowNs, aacViews: true) }
    open func push(sample: MediaSample, nowNs: UInt64) throws -> FanoutOutcome { try outcome(sample.raw, nowNs, aacViews: false) }
    open func popPublishing(nowNs: UInt64) throws -> MediaSample? { try pop(recording: false, nowNs: nowNs) }
    open func popRecording(nowNs: UInt64) throws -> MediaSample? { try pop(recording: true, nowNs: nowNs) }
    open func stats() throws -> NativeQueueStats { try response(try invoke(28, .binding, raw)) { try .init(publishingBytes: $0.u64(), recordingBytes: $0.u64(), publishingDrops: $0.u64(), recordingDrops: $0.u64(), publishingHighWaterBytes: $0.u64(), recordingHighWaterBytes: $0.u64(), publishingMediaMs: $0.u64(), publishingHighWaterMediaMs: $0.u64(), publishingOverflows: $0.u64(), publishingOverflowBytes: $0.u64(), publishingOverflowSamples: $0.u64(), publishingOverflowMedia: $0.u64(), publishingOverflowResidence: $0.u64(), publishingWaitingKeyframe: $0.bool(), recordingFailed: $0.bool()) } }
    open func closePublishingAdmission() throws { _ = try invoke(29, .binding, raw) }
    open func closeRecordingAdmission() throws { _ = try invoke(30, .binding, raw) }
    open func cancel() throws { _ = try invoke(31, .binding, raw) }
}

public protocol NativeRecordingCheckpointProtocol: AnyObject, Sendable { func facts() -> NativeRecordingCheckpointFacts; func presentationExtent() throws -> NativeRecordingPresentationExtent }
open class NativeRecordingCheckpoint: NativeHandle, NativeRecordingCheckpointProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 6, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 6, raw: nil) }
    open func facts() -> NativeRecordingCheckpointFacts { try! response(try! invoke(32, .recording, raw)) { try $0.recordingFacts() } }
    open func presentationExtent() throws -> NativeRecordingPresentationExtent { try response(try invoke(33, .recording, raw)) { try .init(movieTimescale: $0.u32(), durationTicks: $0.u64(), durationMsCeil: $0.u64(), completeAudioEof: $0.bool()) } }
}
public protocol DecodedRecordingCheckpointProtocol: AnyObject, Sendable { func info() -> DecodedRecordingCheckpointInfo; func header() -> Data; func fragmentRegionSha256() -> Data }
open class DecodedRecordingCheckpoint: NativeHandle, DecodedRecordingCheckpointProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 7, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 7, raw: nil) }
    open func info() -> DecodedRecordingCheckpointInfo { try! response(try! invoke(34, .checkpoint, raw)) { try $0.decodedRecordingInfo() } }
    open func header() -> Data { try! invoke(35, .checkpoint, raw) }
    open func fragmentRegionSha256() -> Data { try! invoke(36, .checkpoint, raw) }
}
public protocol NativeProgrammeCheckpointProtocol: AnyObject, Sendable { func facts() -> NativeProgrammeCheckpointFacts; func start() -> NativeProgrammeWindowStart; func windowEnd() -> NativeProgrammeGenerationEnd?; func presentationExtent() throws -> NativeProgrammePresentationExtent }
open class NativeProgrammeCheckpoint: NativeHandle, NativeProgrammeCheckpointProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 8, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 8, raw: nil) }
    open func facts() -> NativeProgrammeCheckpointFacts { try! response(try! invoke(37, .recording, raw)) { try $0.programmeFacts() } }
    open func start() -> NativeProgrammeWindowStart { try! response(try! invoke(38, .recording, raw)) { try $0.programmeStart() } }
    open func windowEnd() -> NativeProgrammeGenerationEnd? { try! response(try! invoke(39, .recording, raw)) { try $0.bool() ? $0.programmeEnd() : nil } }
    open func presentationExtent() throws -> NativeProgrammePresentationExtent { try response(try invoke(40, .recording, raw)) { try .init(movieTimescale: $0.u32(), durationTicks: $0.u64(), durationMsCeil: $0.u64(), windowGeometryComplete: $0.bool()) } }
}
public protocol DecodedProgrammeCheckpointProtocol: AnyObject, Sendable { func info() -> DecodedProgrammeCheckpointInfo; func header() -> Data; func fragmentRegionSha256() -> Data; func presentationExtent() throws -> NativeProgrammePresentationExtent }
open class DecodedProgrammeCheckpoint: NativeHandle, DecodedProgrammeCheckpointProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 9, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 9, raw: nil) }
    open func info() -> DecodedProgrammeCheckpointInfo { try! response(try! invoke(41, .checkpoint, raw)) { try $0.decodedProgrammeInfo() } }
    open func header() -> Data { try! invoke(42, .checkpoint, raw) }
    open func fragmentRegionSha256() -> Data { try! invoke(43, .checkpoint, raw) }
    open func presentationExtent() throws -> NativeProgrammePresentationExtent { try response(try invoke(44, .recording, raw)) { try .init(movieTimescale: $0.u32(), durationTicks: $0.u64(), durationMsCeil: $0.u64(), windowGeometryComplete: $0.bool()) } }
}

public protocol NativeRecordingMuxerProtocol: AnyObject, Sendable {
    func beginNativeAudio(firstLeadingFrames: UInt32) throws; func beginProgrammeWindow(start: NativeProgrammeWindowStart) throws
    func cancel() throws; func checkpointHeader(checkpoint: NativeRecordingCheckpoint) throws -> Data
    func closeProgrammeWindow(end: NativeProgrammeGenerationEnd) throws; func completeNativeAudio(receipt: NativeAudioFileReceipt) throws
    func encodeCheckpoint(token: NativeRecordingCheckpoint, recordingBinding: Data, sourceFileBinding: Data, fragmentRegionSha256: Data) throws -> NativeRecordingCheckpointEncoding
    func encodeProgrammeCheckpoint(token: NativeProgrammeCheckpoint, recordingBinding: Data, sourceFileBinding: Data, fragmentRegionSha256: Data) throws -> NativeProgrammeCheckpointEncoding
    func endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt) throws; func finish() throws; func finishNative(outcome: NativeRecordingCompletion) throws
    func finishProgramme(outcome: NativeProgrammeCompletion) throws; func programmeCheckpointHeader(checkpoint: NativeProgrammeCheckpoint) throws -> Data
    func push(sample: MediaSample) throws; func stats() throws -> NativeRecordingStats; func takeNativeOutput() throws -> NativeRecordingChunk?
    func takeOutput() throws -> RecordingWrite?; func takeProgrammeOutput() throws -> NativeProgrammeChunk?
}
open class NativeRecordingMuxer: NativeHandle, NativeRecordingMuxerProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 10, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 10, raw: nil) }
    public init(hasVideo: Bool, hasAudio: Bool, mediaEpochNs: UInt64, maxFragmentBytes: UInt64, maxFragmentMs: UInt32) throws { super.init(kind: 10, raw: try newHandle(45, .recording, request { $0.bool(hasVideo);$0.bool(hasAudio);$0.u64(mediaEpochNs);$0.u64(maxFragmentBytes);$0.u32(maxFragmentMs) })) }
    public static func withNativeAacTiming(hasVideo: Bool, hasAudio: Bool, mediaEpochNs: UInt64, maxFragmentBytes: UInt64, maxFragmentMs: UInt32) throws -> NativeRecordingMuxer { NativeRecordingMuxer(unsafeFromHandle: UInt64(UInt(bitPattern: try newHandle(46, .recording, request { $0.bool(hasVideo);$0.bool(hasAudio);$0.u64(mediaEpochNs);$0.u64(maxFragmentBytes);$0.u32(maxFragmentMs) })))) }
    public static func withProgrammeAacTiming(mediaEpochNs: UInt64, maxFragmentBytes: UInt64, maxFragmentMs: UInt32) throws -> NativeRecordingMuxer { NativeRecordingMuxer(unsafeFromHandle: UInt64(UInt(bitPattern: try newHandle(47, .recording, request { $0.u64(mediaEpochNs);$0.u64(maxFragmentBytes);$0.u32(maxFragmentMs) })))) }
    open func beginNativeAudio(firstLeadingFrames: UInt32) throws { _ = try invoke(54, .recording, raw, request { $0.u32(firstLeadingFrames) }) }
    open func beginProgrammeWindow(start: NativeProgrammeWindowStart) throws { _ = try invoke(48, .recording, raw, request { $0.programmeStart(start) }) }
    open func cancel() throws { _ = try invoke(64, .recording, raw) }
    open func checkpointHeader(checkpoint: NativeRecordingCheckpoint) throws -> Data { try invoke(60, .recording, raw, request { $0.handle(checkpoint) }) }
    open func closeProgrammeWindow(end: NativeProgrammeGenerationEnd) throws { _ = try invoke(49, .recording, raw, request { $0.programmeEnd(end) }) }
    open func completeNativeAudio(receipt: NativeAudioFileReceipt) throws { _ = try invoke(56, .recording, raw, request { $0.fileReceipt(receipt) }) }
    open func encodeCheckpoint(token: NativeRecordingCheckpoint, recordingBinding: Data, sourceFileBinding: Data, fragmentRegionSha256: Data) throws -> NativeRecordingCheckpointEncoding { try response(try invoke(61, .checkpoint, raw, request { $0.handle(token);$0.bytes(recordingBinding);$0.bytes(sourceFileBinding);$0.bytes(fragmentRegionSha256) })) { try .init(bytes: $0.bytes(), info: $0.encodedRecordingInfo()) } }
    open func encodeProgrammeCheckpoint(token: NativeProgrammeCheckpoint, recordingBinding: Data, sourceFileBinding: Data, fragmentRegionSha256: Data) throws -> NativeProgrammeCheckpointEncoding { try response(try invoke(52, .checkpoint, raw, request { $0.handle(token);$0.bytes(recordingBinding);$0.bytes(sourceFileBinding);$0.bytes(fragmentRegionSha256) })) { try .init(bytes: $0.bytes(), info: $0.encodedProgrammeInfo()) } }
    open func endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt) throws { _ = try invoke(55, .recording, raw, request { $0.generation(receipt) }) }
    open func finish() throws { _ = try invoke(62, .recording, raw) }
    open func finishNative(outcome: NativeRecordingCompletion) throws { _ = try invoke(63, .recording, raw, request { $0.u8(outcome == .complete ? 0 : 1) }) }
    open func finishProgramme(outcome: NativeProgrammeCompletion) throws { _ = try invoke(53, .recording, raw, request { $0.u8(outcome == .completeSubmittedWindow ? 0 : 1) }) }
    open func programmeCheckpointHeader(checkpoint: NativeProgrammeCheckpoint) throws -> Data { try invoke(51, .recording, raw, request { $0.handle(checkpoint) }) }
    open func push(sample: MediaSample) throws {
        var error = SapflowBuffer()
        try check(sapflow_recording_muxer_push(raw, sample.raw, &error), .recording, error)
    }
    open func stats() throws -> NativeRecordingStats { try response(try invoke(65, .recording, raw)) { try .init(bufferedBytes: $0.u64(), bufferedSamples: $0.u64(), queuedOutputBytes: $0.u64(), acceptedSamples: $0.u64(), fragments: $0.u64(), durationMs: $0.u64(), finished: $0.bool()) } }
    private func takeChunk(_ mode: Int) throws -> (Data, SapflowRecordingOutput)? {
        var out = SapflowRecordingOutput(), error = SapflowBuffer()
        let status = switch mode {
        case 0: sapflow_recording_muxer_take_output(raw, &out, &error)
        case 1: sapflow_recording_muxer_take_native_output(raw, &out, &error)
        default: sapflow_recording_muxer_take_programme_output(raw, &out, &error)
        }
        try check(status, .recording, error)
        guard out.present else { return nil }
        return (take(out.bytes), out)
    }
    open func takeNativeOutput() throws -> NativeRecordingChunk? {
        guard let (bytes, out) = try takeChunk(1) else { return nil }
        return NativeRecordingChunk(bytes: bytes, kind: try indexed(chunkKinds, Int(out.kind)), checkpoint: out.checkpoint.map { NativeRecordingCheckpoint(unsafeFromHandle: UInt64(UInt(bitPattern: $0))) })
    }
    open func takeOutput() throws -> RecordingWrite? {
        guard let (bytes, out) = try takeChunk(0) else { return nil }
        return RecordingWrite(bytes: bytes, rewriteHeader: out.rewrite_header)
    }
    open func takeProgrammeOutput() throws -> NativeProgrammeChunk? {
        guard let (bytes, out) = try takeChunk(2) else { return nil }
        return NativeProgrammeChunk(bytes: bytes, kind: try indexed(chunkKinds, Int(out.kind)), checkpoint: out.checkpoint.map { NativeProgrammeCheckpoint(unsafeFromHandle: UInt64(UInt(bitPattern: $0))) })
    }
}
public func decodeRecordingCheckpoint(bytes: Data, expectedRecordingBinding: Data, expectedSourceFileBinding: Data) throws -> DecodedRecordingCheckpoint { DecodedRecordingCheckpoint(unsafeFromHandle: UInt64(UInt(bitPattern: try newHandle(66, .checkpoint, request { $0.bytes(bytes);$0.bytes(expectedRecordingBinding);$0.bytes(expectedSourceFileBinding) })))) }
public func validateRecordingCheckpointSuccessor(prior: DecodedRecordingCheckpoint, next: DecodedRecordingCheckpoint) throws { _ = try invoke(67, .checkpoint, nil, request { $0.handle(prior);$0.handle(next) }) }
public func decodeProgrammeCheckpoint(bytes: Data, expectedRecordingBinding: Data, expectedSourceFileBinding: Data) throws -> DecodedProgrammeCheckpoint { DecodedProgrammeCheckpoint(unsafeFromHandle: UInt64(UInt(bitPattern: try newHandle(68, .checkpoint, request { $0.bytes(bytes);$0.bytes(expectedRecordingBinding);$0.bytes(expectedSourceFileBinding) })))) }
public func validateProgrammeCheckpointSuccessor(prior: DecodedProgrammeCheckpoint, next: DecodedProgrammeCheckpoint) throws { _ = try invoke(69, .checkpoint, nil, request { $0.handle(prior);$0.handle(next) }) }

public protocol NativeRtmpPublisherProtocol: AnyObject, Sendable {
    func declareTracks(hasVideo: Bool, hasAudio: Bool) throws; func pollEvent() throws -> NativeRtmpEvent?
    func feed(bytes: Data, nowMs: UInt64) throws; func tick(nowMs: UInt64) throws; func nextDeadlineMs() throws -> UInt64?
    func takeOutput() throws -> Data; func queuedOutputBytes() throws -> UInt64; func state() throws -> NativeRtmpState
    func send(sample: MediaSample) throws; func sendAndTakeOutput(sample: MediaSample, nowMs: UInt64) throws -> NativeRtmpSendOutput
    func finish() throws; func cancel() throws
}
open class NativeRtmpPublisher: NativeHandle, NativeRtmpPublisherProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 11, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 11, raw: nil) }
    public init(app: String, streamKey: String, tcUrl: String, mediaEpochNs: UInt64, nowMs: UInt64) throws { super.init(kind: 11, raw: try newHandle(70, .rtmp, request { $0.string(app);$0.string(streamKey);$0.string(tcUrl);$0.u64(mediaEpochNs);$0.u64(nowMs) })) }
    open func declareTracks(hasVideo: Bool, hasAudio: Bool) throws { _ = try invoke(71, .rtmp, raw, request { $0.bool(hasVideo);$0.bool(hasAudio) }) }
    open func pollEvent() throws -> NativeRtmpEvent? { try response(try invoke(72, .rtmp, raw)) { try $0.bool() ? (try indexed([NativeRtmpEvent.sending, .peerMediaProgress], $0.u8())) : nil } }
    open func feed(bytes: Data, nowMs: UInt64) throws {
        var error = SapflowBuffer()
        try withData(bytes) { ptr, len in try check(sapflow_rtmp_publisher_feed(raw, ptr, len, nowMs, &error), .rtmp, error) }
    }
    open func tick(nowMs: UInt64) throws { _ = try invoke(74, .rtmp, raw, request { $0.u64(nowMs) }) }
    open func nextDeadlineMs() throws -> UInt64? {
        var out = SapflowOptionalU64(), error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_next_deadline_ms(raw, &out, &error), .rtmp, error)
        return out.present ? out.value : nil
    }
    open func takeOutput() throws -> Data {
        var out = SapflowBuffer(), error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_take_output(raw, &out, &error), .rtmp, error)
        return take(out)
    }
    open func queuedOutputBytes() throws -> UInt64 {
        var out: UInt64 = 0, error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_queued_output_bytes(raw, &out, &error), .rtmp, error)
        return out
    }
    open func state() throws -> NativeRtmpState {
        var out: UInt64 = 0, error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_state(raw, &out, &error), .rtmp, error)
        return try indexed(rtmpStates, Int(out))
    }
    open func send(sample: MediaSample) throws {
        var error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_send(raw, sample.raw, &error), .rtmp, error)
    }
    open func sendAndTakeOutput(sample: MediaSample, nowMs: UInt64) throws -> NativeRtmpSendOutput {
        var out = SapflowRtmpSendOutput(), error = SapflowBuffer()
        try check(sapflow_rtmp_publisher_send_and_take_output(raw, sample.raw, nowMs, &out, &error), .rtmp, error)
        return NativeRtmpSendOutput(output: take(out.output), queuedOutputBytes: out.queued_output_bytes, nextDeadlineMs: out.has_next_deadline ? out.next_deadline_ms : nil, state: try indexed(rtmpStates, Int(out.state)), startupSubmitted: out.startup_submitted, peerMediaProgress: out.peer_media_progress)
    }
    open func finish() throws { _ = try invoke(81, .rtmp, raw) }
    open func cancel() throws { _ = try invoke(82, .rtmp, raw) }
}

public protocol NativeSessionProtocol: AnyObject, Sendable {
    func start(nowNs: UInt64) throws -> UInt64; func prepared(generation: UInt64) throws -> NativeSessionAction
    func connected(generation: UInt64, nowNs: UInt64) throws; func sending(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64) throws -> NativeSessionAction
    func transportRecovered(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64, source: NativeTransportRecoverySource) throws -> NativeSessionAction
    func mediaConfirmed(generation: UInt64, nowNs: UInt64) throws; func mediaConfirmedForAttempt(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64) throws
    func replaceTransport(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction; func transportFailed(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction
    func tick(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction; func interrupt(generation: UInt64) throws -> UInt64
    func stop(generation: UInt64) throws -> NativeStopResult; func finishStop(generation: UInt64) throws; func cancel(generation: UInt64) throws -> UInt64
    func snapshot() throws -> NativeSessionSnapshot
}
open class NativeSession: NativeHandle, NativeSessionProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 12, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 12, raw: nil) }
    public init(graceMs: UInt64) throws { super.init(kind: 12, raw: try newHandle(83, .session, request { $0.u64(graceMs) })) }
    open func start(nowNs: UInt64) throws -> UInt64 { try response(try invoke(84, .session, raw, request { $0.u64(nowNs) })) { try $0.u64() } }
    open func prepared(generation: UInt64) throws -> NativeSessionAction { try response(try invoke(85, .session, raw, request { $0.u64(generation) })) { try $0.sessionAction() } }
    open func connected(generation: UInt64, nowNs: UInt64) throws { _ = try invoke(86, .session, raw, request { $0.u64(generation);$0.u64(nowNs) }) }
    open func sending(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64) throws -> NativeSessionAction { try response(try invoke(87, .session, raw, request { $0.u64(generation);$0.u64(attemptEpoch);$0.u64(nowNs) })) { try $0.sessionAction() } }
    open func transportRecovered(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64, source: NativeTransportRecoverySource) throws -> NativeSessionAction { try response(try invoke(88, .session, raw, request { $0.u64(generation);$0.u64(attemptEpoch);$0.u64(nowNs);$0.u8(source == .srtPeerAcknowledgedWatermark ? 0 : 1) })) { try $0.sessionAction() } }
    open func mediaConfirmed(generation: UInt64, nowNs: UInt64) throws { _ = try invoke(89, .session, raw, request { $0.u64(generation);$0.u64(nowNs) }) }
    open func mediaConfirmedForAttempt(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64) throws { _ = try invoke(90, .session, raw, request { $0.u64(generation);$0.u64(attemptEpoch);$0.u64(nowNs) }) }
    open func replaceTransport(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction { try response(try invoke(91, .session, raw, request { $0.u64(generation);$0.u64(nowNs) })) { try $0.sessionAction() } }
    open func transportFailed(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction { try response(try invoke(92, .session, raw, request { $0.u64(generation);$0.u64(nowNs) })) { try $0.sessionAction() } }
    open func tick(generation: UInt64, nowNs: UInt64) throws -> NativeSessionAction { try response(try invoke(93, .session, raw, request { $0.u64(generation);$0.u64(nowNs) })) { try $0.sessionAction() } }
    open func interrupt(generation: UInt64) throws -> UInt64 { try response(try invoke(94, .session, raw, request { $0.u64(generation) })) { try $0.u64() } }
    open func stop(generation: UInt64) throws -> NativeStopResult { try response(try invoke(95, .session, raw, request { $0.u64(generation) })) { try .init(generation: $0.u64(), action: $0.sessionAction()) } }
    open func finishStop(generation: UInt64) throws { _ = try invoke(96, .session, raw, request { $0.u64(generation) }) }
    open func cancel(generation: UInt64) throws -> UInt64 { try response(try invoke(97, .session, raw, request { $0.u64(generation) })) { try $0.u64() } }
    open func snapshot() throws -> NativeSessionSnapshot { try response(try invoke(98, .session, raw)) { try .init(generation: $0.u64(), attemptEpoch: $0.u64(), mediaReadiness: indexed(mediaReadiness, $0.u8()), retryBudget: indexed(retryStates, $0.u8()), transportRecoverySource: $0.bool() ? indexed(recoverySources, $0.u8()) : nil, networkRecoveries: $0.u64(), outageDeadlineNs: $0.optionalU64(), nextDeadlineNs: $0.optionalU64(), state: indexed(sessionStates, $0.u8()), transportConnected: $0.bool(), reconnectDeadlineNs: $0.optionalU64(), transportFailures: $0.u64(), retryAttempts: $0.u64(), mediaConfirmations: $0.u64(), staleCallbacks: $0.u64()) } }
}

public protocol NativeSrtPublisherProtocol: AnyObject, Sendable {
    func push(sample: MediaSample, nowUs: UInt64) throws; func feed(source: String, bytes: Data, nowUs: UInt64) throws
    func tick(nowUs: UInt64) throws; func pollDatagram(nowUs: UInt64) throws -> NativeSrtDatagram?; func isReady() throws -> Bool
    func hasImmediateWork() throws -> Bool; func nextDeadlineUs() throws -> UInt64?; func state() throws -> NativeSrtState
    func pollEvent() throws -> NativeSrtEvent?; func stop(nowUs: UInt64) throws; func socketSendFailed(nowUs: UInt64) throws
    func stats() throws -> NativeSrtStats
}
open class NativeSrtPublisher: NativeHandle, NativeSrtPublisherProtocol, @unchecked Sendable {
    public struct NoHandle { public init() {} }
    required public init(unsafeFromHandle handle: UInt64) { super.init(kind: 13, raw: UnsafeMutableRawPointer(bitPattern: UInt(handle))) }
    public init(noHandle: NoHandle) { super.init(kind: 13, raw: nil) }
    public init(config: NativeSrtConfig, video: MediaCodec?, audio: MediaCodec?, passphrase: Data?, nowUs: UInt64) throws { super.init(kind: 13, raw: try newHandle(99, .srt, request { $0.srtConfig(config);$0.optional(video);$0.optional(audio);$0.bool(passphrase != nil);if let passphrase { $0.bytes(passphrase) };$0.u64(nowUs) })) }
    open func push(sample: MediaSample, nowUs: UInt64) throws {
        var error = SapflowBuffer()
        try check(sapflow_srt_publisher_push(raw, sample.raw, nowUs, &error), .srt, error)
    }
    open func feed(source: String, bytes: Data, nowUs: UInt64) throws {
        var error = SapflowBuffer()
        let source = Data(source.utf8)
        try withData(source) { sourcePtr, sourceLen in
            try withData(bytes) { ptr, len in
                try check(sapflow_srt_publisher_feed(raw, sourcePtr, sourceLen, ptr, len, nowUs, &error), .srt, error)
            }
        }
    }
    open func tick(nowUs: UInt64) throws { _ = try invoke(102, .srt, raw, request { $0.u64(nowUs) }) }
    open func pollDatagram(nowUs: UInt64) throws -> NativeSrtDatagram? {
        var out = SapflowSrtDatagram(), error = SapflowBuffer()
        try check(sapflow_srt_publisher_poll_datagram(raw, nowUs, &out, &error), .srt, error)
        guard out.present else { return nil }
        return NativeSrtDatagram(destination: String(decoding: take(out.destination), as: UTF8.self), bytes: take(out.bytes))
    }
    open func isReady() throws -> Bool {
        var out: UInt64 = 0, error = SapflowBuffer()
        try check(sapflow_srt_publisher_is_ready(raw, &out, &error), .srt, error)
        return out != 0
    }
    open func hasImmediateWork() throws -> Bool {
        var out: UInt64 = 0, error = SapflowBuffer()
        try check(sapflow_srt_publisher_has_immediate_work(raw, &out, &error), .srt, error)
        return out != 0
    }
    open func nextDeadlineUs() throws -> UInt64? {
        var out = SapflowOptionalU64(), error = SapflowBuffer()
        try check(sapflow_srt_publisher_next_deadline_us(raw, &out, &error), .srt, error)
        return out.present ? out.value : nil
    }
    open func state() throws -> NativeSrtState {
        var out: UInt64 = 0, error = SapflowBuffer()
        try check(sapflow_srt_publisher_state(raw, &out, &error), .srt, error)
        return try indexed(srtStates, Int(out))
    }
    open func pollEvent() throws -> NativeSrtEvent? { try response(try invoke(108, .srt, raw)) { r -> NativeSrtEvent? in
        if try !r.bool() { return nil }
        return switch try r.u8() { case 0: NativeSrtEvent.sending;case 1: NativeSrtEvent.peerMediaProgress
        case 2: try NativeSrtEvent.connected(latencyMs: r.u32(), maxPayloadBytes: r.u32())
        case 3: try NativeSrtEvent.failed(reason: indexed(srtCloseReasons, r.u8()))
        case 4: try NativeSrtEvent.closed(reason: indexed(srtCloseReasons, r.u8()), peerCode: r.bool() ? r.u32() : nil)
        default: throw CapiFailure(code: "Argument") }
    } }
    open func stop(nowUs: UInt64) throws { _ = try invoke(109, .srt, raw, request { $0.u64(nowUs) }) }
    open func socketSendFailed(nowUs: UInt64) throws { _ = try invoke(110, .srt, raw, request { $0.u64(nowUs) }) }
    open func stats() throws -> NativeSrtStats { try response(try invoke(111, .srt, raw)) { r in try .init(acceptedSamples: r.u64(), acceptedSampleBytes: r.u64(), bufferedSampleBytes: r.u64(), bufferedSampleDurationNs: r.u64(), discardedSampleBytes: r.u64(), pendingChunkBytes: r.u64(), pendingChunkCapacity: r.u64(), discardedPendingChunkBytes: r.u64(), bufferedPackets: r.u64(), bufferedBytes: r.u64(), peakBufferedBytes: r.u64(), sentPackets: r.u64(), sentBytes: r.u64(), retransmittedPackets: r.u64(), retransmittedBytes: r.u64(), acknowledgedBytes: r.u64(), abandonedBytes: r.u64(), encryptedPackets: r.u64(), kmRequests: r.u64(), kmResponses: r.u64(), rttUs: r.u64(), malformedDatagrams: r.u64(), strayDatagrams: r.u64(), rejectedDatagrams: r.u64(), backpressureEvents: r.u64(), ffiInputHandleClones: r.u64(), sdkInputHandleClones: r.u64(), tsOutputAllocations: r.u64(), elementaryStagingCopyBytes: r.u64(), packetPayloadCopyBytes: r.u64(), writerPayloadCopyBytes: r.u64(), tsSerialisedBytes: r.u64(), returnedDatagrams: r.u64(), returnedDatagramBytes: r.u64()) } }
}
