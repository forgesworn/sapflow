import AVFoundation
import Sapflow

struct CaptureFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

struct CompressedUnit {
    let buffer: CMSampleBuffer
    let isVideo: Bool
    let keyframe: Bool
    let time: CMTime
    let duration: CMTime
    let configuration: CodecDescription
    var byteCount: Int { CMSampleBufferGetTotalSampleSize(buffer) }
}

enum CodecDescription {
    case h264(sps: Data, pps: Data)
    case aac(asc: Data)
}

/// A destination must synchronously take ownership of an immutable Rust sample,
/// perform only bounded enqueue work and expose observed network/media status.
/// It must never perform socket or disk I/O in this callback.
protocol EncodedPublishingSink: AnyObject {
    var status: String { get }
    var cleanupComplete: Bool { get }
    func endDestination()
    func closeCodecEpoch()
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void)
    func start() throws
    func configure(video: MediaCodec?, audio: MediaCodec) throws
    func offer(_ sample: MediaSample) throws
    func stop()
    func stop(completion: @escaping () -> Void)
    func readSocketOutbound(completion: @escaping (SocketOutboundSample?) -> Void)
    var finalSocketOutbound: SocketOutboundSample? { get }
    func observeBroadcastEnd(_ completion: @escaping (UInt64) -> Void)
    /// Cold app-activation hint. Implementations recheck original authority,
    /// never create a fresh user Start or reset a reconnect/grant deadline.
    func revalidateAuthority()
}

extension EncodedPublishingSink {
    var cleanupComplete: Bool { true }
    func endDestination() { stop() }
    func closeCodecEpoch() {}
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void) { stop { completion(.success(())) } }
    func configure(video: MediaCodec?, audio: MediaCodec) throws {}
    func stop(completion: @escaping () -> Void) { stop(); completion() }
    func readSocketOutbound(completion: @escaping (SocketOutboundSample?) -> Void) { completion(nil) }
    var finalSocketOutbound: SocketOutboundSample? { nil }
    func observeBroadcastEnd(_ completion: @escaping (UInt64) -> Void) {}
    func revalidateAuthority() {}
}

/// Connects native compressed data to the actual Rust descriptor validator.
/// No attached destination means recording/SDK validation only, visibly so.
final class RustEncodedIngress {
    struct ValidatedUnit {
        let sample: MediaSample
        /// Actual contiguous validated payload size, no second FFI length query.
        let payloadBytes: Int
        let pts: Int64
        let duration: UInt32
        /// MediaCodec.sample rejects a flag that differs from coded AVCC IDR.
        let codedIdr: Bool
    }
    private var expectsVideo = true
    private var video: MediaCodec?
    private var audio: MediaCodec?
    private var previousVideoConfig: CodecDescription?
    private var previousAudioConfig: CodecDescription?
    private var configuredDestination = false
    private(set) var accepted: UInt64 = 0
    var destination: EncodedPublishingSink?
    var status: String { destination?.status ?? "Publisher not attached · local recording only" }

    private func codec(for configuration: CodecDescription) throws -> MediaCodec {
        let codec: MediaCodec
        switch configuration {
        case let .h264(sps, pps):
            guard expectsVideo else { throw CaptureFailure(message: "Video is forbidden in this audio-only capture") }
            if case let .h264(oldSps, oldPps) = previousVideoConfig, oldSps != sps || oldPps != pps {
                throw CaptureFailure(message: "Video format changed. Stop and prepare a new recording.")
            }
            if video == nil { video = try MediaCodec.h264(sps: sps, pps: pps); previousVideoConfig = configuration }
            guard let video else { throw CaptureFailure(message: "Video configuration unavailable") }
            codec = video
        case let .aac(asc):
            if case let .aac(old) = previousAudioConfig, old != asc {
                throw CaptureFailure(message: "Audio format changed. Stop and prepare again.")
            }
            if audio == nil { audio = try MediaCodec.aac(asc: asc); previousAudioConfig = configuration }
            guard let audio else { throw CaptureFailure(message: "Audio configuration unavailable") }
            codec = audio
        }
        return codec
    }
    /// Allows a fixture or a platform with advance CSD to share the same codec
    /// handles before media arrives; ordinary capture discovers them in accept.
    func prepare(video: CodecDescription?, audio: CodecDescription) throws {
        guard expectsVideo == (video != nil) else { throw CaptureFailure(message: "Codec tracks do not match the prepared capture") }
        if let video { _ = try codec(for: video) }
        _ = try codec(for: audio)
        try configureDestination()
    }
    private func configureDestination() throws {
        if !configuredDestination, let audio, !expectsVideo || video != nil {
            // Track intent is frozen before capture, independently of which
            // encoder happens to deliver configuration first.
            try destination?.configure(video: video, audio: audio)
            configuredDestination = true
        }
    }
    func validate(_ unit: CompressedUnit) throws -> ValidatedUnit {
        guard !unit.isVideo || expectsVideo else { throw CaptureFailure(message: "Video is forbidden in this audio-only capture") }
        let codec = try codec(for: unit.configuration)
        guard let block = CMSampleBufferGetDataBuffer(unit.buffer) else { throw CaptureFailure(message: "Compressed buffer has no payload") }
        var pointer: UnsafeMutablePointer<Int8>?
        var contiguous = 0
        var total = 0
        let result = CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &contiguous, totalLengthOut: &total, dataPointerOut: &pointer)
        guard result == kCMBlockBufferNoErr, contiguous == total, let pointer else {
            throw CaptureFailure(message: "Encoder output is not contiguous; zero-copy ingress unavailable")
        }
        // UniFFI's borrowed Data call completes synchronously. The only payload
        // ownership copy is Rust's Arc allocation, not a preliminary Data copy.
        let view = Data(bytesNoCopy: pointer, count: total, deallocator: .none)
        let scale: Int32 = unit.isVideo ? 90_000 : 48_000
        let pts = CMTimeConvertScale(unit.time, timescale: scale, method: .roundHalfAwayFromZero).value
        let durationTicks = CMTimeConvertScale(unit.duration, timescale: scale, method: .roundHalfAwayFromZero).value
        guard durationTicks > 0, durationTicks <= Int64(UInt32.max) else { throw CaptureFailure(message: "Compressed sample duration is invalid") }
        let duration = UInt32(durationTicks)
        guard pts >= 0 else { throw CaptureFailure(message: "Encoder timestamp precedes the recording epoch") }
        guard accepted < UInt64.max else { throw CaptureFailure(message: "Encoded validation counter is exhausted") }
        let sample = try withExtendedLifetime(unit.buffer) {
            let sample = try codec.sample(bytes: view, pts: pts, dts: pts, duration: duration, timescale: UInt32(scale), keyframe: unit.keyframe)
            return sample
        }
        accepted += 1
        return ValidatedUnit(sample: sample, payloadBytes: total, pts: pts, duration: duration, codedIdr: unit.isVideo && unit.keyframe)
    }

    /// Validation remains available after a terminal publishing End. Recording
    /// may retain this same immutable Rust sample (or the original native buffer
    /// for legacy); transport needs no second payload validation or copy.
    func publish(_ validated: ValidatedUnit, sample: MediaSample? = nil) throws {
        try configureDestination()
        try destination?.offer(sample ?? validated.sample)
    }

    func accept(_ unit: CompressedUnit) throws {
        try publish(validate(unit))
    }

    func reset(hasVideo: Bool = true) {
        // The old epoch barrier is owned by finishEpoch; resetting codec
        // discovery must not cancel the logical reconnect session.
        video = nil; audio = nil
        expectsVideo = hasVideo
        previousVideoConfig = nil; previousAudioConfig = nil
        accepted = 0; configuredDestination = false
    }
}
