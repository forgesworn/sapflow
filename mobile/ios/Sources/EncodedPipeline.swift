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
    func start() throws
    func configure(video: MediaCodec, audio: MediaCodec) throws
    func offer(_ sample: MediaSample) throws
    func stop()
    func stop(completion: @escaping () -> Void)
}

extension EncodedPublishingSink {
    func configure(video: MediaCodec, audio: MediaCodec) throws {}
    func stop(completion: @escaping () -> Void) { stop(); completion() }
}

/// Connects native compressed data to the actual Rust descriptor validator.
/// No attached destination means recording/SDK validation only, visibly so.
final class RustEncodedIngress {
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
    func prepare(video: CodecDescription, audio: CodecDescription) throws {
        _ = try codec(for: video); _ = try codec(for: audio)
        try configureDestination()
    }
    private func configureDestination() throws {
        if !configuredDestination, let video, let audio {
            // This capture profile always has both tracks. Freeze each shared
            // codec once before SRT constructs its TS/transport epoch.
            try destination?.configure(video: video, audio: audio)
            configuredDestination = true
        }
    }
    func accept(_ unit: CompressedUnit) throws {
        let codec = try codec(for: unit.configuration)
        try configureDestination()
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
        try withExtendedLifetime(unit.buffer) {
            let sample = try codec.sample(bytes: view, pts: pts, dts: pts, duration: duration, timescale: UInt32(scale), keyframe: unit.keyframe)
            // Immutable sample goes through the same seam with or without a
            // network adapter; validation is real, network status is separate.
            try destination?.offer(sample)
        }
        accepted += 1
    }

    func reset() {
        destination?.stop()
        video = nil; audio = nil
        previousVideoConfig = nil; previousAudioConfig = nil
        accepted = 0; configuredDestination = false
    }
}

