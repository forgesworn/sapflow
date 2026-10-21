import Foundation
@preconcurrency import AVFoundation
import AudioToolbox
import CoreMedia
import CoreVideo
import VideoToolbox

struct OriginDecodedMedia: Equatable {
    let videoOutputs: Int, pcmFrames: Int, pcmOutputs: Int
    let hardwareVideo: Bool
    var sufficient: Bool { hardwareVideo && videoOutputs >= 2 && pcmFrames >= 2048 && pcmOutputs > 0 }
}

/// Counts native PCM frames and their exact temporal extent, not callbacks.
/// AVAssetReader can coalesce several AAC units into a single PCM buffer.
struct OriginPCMProgress {
    private(set) var frames = 0, outputs = 0
    private var rate: Int32?
    private var end: CMTime?
    mutating func accept(frames count: Int, bytes: Int, bytesPerFrame: Int, rate: Int32,
                         first: CMTime, last: CMTime, firstDuration: CMTime, lastDuration: CMTime) throws {
        guard (8000...96000).contains(rate), count > 0, count <= Int(rate), bytesPerFrame == 2 || bytesPerFrame == 4,
              bytes == count * bytesPerFrame, self.rate == nil || self.rate == rate, frames <= 192_000 - count, outputs < 64 else { throw OriginObservationFailure.bounds }
        let frame = CMTime(value: 1, timescale: rate)
        guard first.isNumeric, last.isNumeric, CMTimeCompare(firstDuration, frame) == 0, CMTimeCompare(lastDuration, frame) == 0,
              CMTimeCompare(last, CMTimeAdd(first, CMTime(value: Int64(count - 1), timescale: rate))) == 0,
              end == nil || CMTimeCompare(first, end!) >= 0 else { throw OriginObservationFailure.progress }
        self.rate = rate; end = CMTimeAdd(last, lastDuration); frames += count; outputs += 1
    }
}

protocol OriginMediaDecoding: AnyObject {
    var cleanupComplete: Bool { get }
    func whenCleanupComplete(_ completion: @escaping () -> Void)
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void)
    func close()
}

/// One finite native worker. Timeout fences its result immediately but does
/// not free admission until native calls, readers and owned scratch clean up.
/// A stuck platform call therefore retains one owner, never starts replacements
/// or stops the app/capture process. No existing file URL is accepted.
final class OriginMediaDecoder: OriginMediaDecoding, @unchecked Sendable {
    private let lock = NSLock()
    private var job: Job?
    private var closed = false
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return job == nil }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) {
        lock.lock()
        guard !closed, job == nil else { let error: OriginObservationFailure = closed ? .cancelled : .busy; lock.unlock(); completion(.failure(error)); return }
        let now = DispatchTime.now().uptimeNanoseconds
        guard !data.isEmpty, data.count <= OriginMediaBody.totalLimit, deadlineNs > now, deadlineNs - now <= 3_000_000_000 else { lock.unlock(); completion(.failure(.bounds)); return }
        guard #available(iOS 17, macOS 12, *) else { lock.unlock(); completion(.failure(.unsupported)); return }
        let current = Job(deadline: deadlineNs, completion: completion); job = current; lock.unlock()
        current.startDeadline()
        Task.detached(priority: .utility) { [self, current] in
            let result: Result<OriginDecodedMedia, OriginObservationFailure>
            do {
                let proof = try await current.run(data)
                try current.cleanup()
                try current.check()
                result = .success(proof)
            } catch {
                do { try current.cleanup(); result = .failure((error as? OriginObservationFailure) ?? .decode) }
                catch { current.report(.failure(.cleanup)); return } // Retain the uncertain owner.
            }
            let fullyClosed = lock.withLock { () -> Bool in
                if job === current { job = nil }
                return closed && job == nil
            }
            if fullyClosed { cleanup.complete() }
            current.report(result)
        }
    }
    func close() {
        lock.lock(); closed = true; let old = job; lock.unlock()
        old?.cancel(.cancelled)
        if old == nil { cleanup.complete() }
    }

    private final class Job: @unchecked Sendable {
        private let lock = NSLock()
        private let deadline: UInt64
        private var callback: ((Result<OriginDecodedMedia, OriginObservationFailure>) -> Void)?
        private var timer: DispatchSourceTimer?
        private var failure: OriginObservationFailure?
        private var asset: AVURLAsset?
        private var readers: [AVAssetReader] = []
        private var videoSession: VTDecompressionSession?
        private var scratch: URL?
        private var cleaning = false
        private let cancellations = DispatchGroup()
        private var videoCount = 0
        private var videoPTS: CMTime?
        private var pcm = OriginPCMProgress()
        init(deadline: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) { self.deadline = deadline; callback = completion }
        func startDeadline() {
            let next = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "sapflow.origin.decode.deadline"))
            next.schedule(deadline: DispatchTime(uptimeNanoseconds: deadline))
            next.setEventHandler { [weak self] in self?.cancel(.deadline) }
            lock.withLock { timer = next }; next.resume()
        }
        func check() throws {
            try lock.withLock {
                if let failure { throw failure }
                guard DispatchTime.now().uptimeNanoseconds < deadline else { throw OriginObservationFailure.deadline }
            }
        }
        func report(_ result: Result<OriginDecodedMedia, OriginObservationFailure>) {
            let pair = lock.withLock { () -> (((Result<OriginDecodedMedia, OriginObservationFailure>) -> Void)?, DispatchSourceTimer?) in
                let saved = callback; callback = nil; let old = timer; timer = nil; return (saved, old)
            }
            pair.1?.setEventHandler {}; pair.1?.cancel(); pair.0?(result)
        }
        func cancel(_ reason: OriginObservationFailure) {
            let resources = lock.withLock { () -> (AVURLAsset?, [AVAssetReader], Bool) in
                guard failure == nil else { return (nil, [], false) }; failure = reason
                if cleaning { return (nil, [], true) }
                if asset != nil { cancellations.enter() }
                for _ in readers { cancellations.enter() }
                return (asset, readers, true)
            }
            guard resources.2 else { return }
            report(.failure(reason))
            // At most one cancellation per resource. A blocked cancel call
            // cannot stop the independent cancellation of the other reader.
            if let asset = resources.0 { DispatchQueue.global(qos: .utility).async { defer { self.cancellations.leave() }; asset.cancelLoading() } }
            for reader in resources.1 { DispatchQueue.global(qos: .utility).async { defer { self.cancellations.leave() }; reader.cancelReading() } }
        }
        func cleanup() throws {
            // Keep admission while any platform cleanup call has not returned.
            let owned = lock.withLock { cleaning = true; return (asset, readers, videoSession, scratch) }
            guard cancellations.wait(timeout: .now() + .seconds(2)) == .success else { throw OriginObservationFailure.cleanup }
            owned.0?.cancelLoading()
            for reader in owned.1 { reader.cancelReading() }
            if let video = owned.2 { VTDecompressionSessionInvalidate(video) }
            if let directory = owned.3 { try FileManager.default.removeItem(at: directory) }
            lock.withLock { asset = nil; readers = []; videoSession = nil; scratch = nil }
        }
        @available(iOS 17, macOS 12, *)
        func run(_ data: Data) async throws -> OriginDecodedMedia {
            try check()
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-origin-probe-" + UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            lock.withLock { scratch = directory }
            var values = URLResourceValues(); values.isExcludedFromBackup = true
            var privateDirectory = directory; try privateDirectory.setResourceValues(values)
            let input = directory.appendingPathComponent("owned-origin-part.mp4")
            guard FileManager.default.createFile(atPath: input.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw OriginObservationFailure.cleanup }
            try check()
            let asset = AVURLAsset(url: input); lock.withLock { self.asset = asset }; try check()
            let tracks = try await asset.load(.tracks)
            let videos = tracks.filter { $0.mediaType == .video }, audios = tracks.filter { $0.mediaType == .audio }
            guard tracks.count == 2, videos.count == 1, audios.count == 1 else { throw OriginObservationFailure.decode }
            let videoFormats = try await videos[0].load(.formatDescriptions), audioFormats = try await audios[0].load(.formatDescriptions)
            try check()
            guard videoFormats.count == 1, audioFormats.count == 1,
                  CMFormatDescriptionGetMediaSubType(videoFormats[0]) == kCMVideoCodecType_H264,
                  CMFormatDescriptionGetMediaSubType(audioFormats[0]) == kAudioFormatMPEG4AAC,
                  let audio = CMAudioFormatDescriptionGetStreamBasicDescription(audioFormats[0]),
                  audio.pointee.mFramesPerPacket == 1024, (1...2).contains(audio.pointee.mChannelsPerFrame),
                  audio.pointee.mSampleRate >= 8000, audio.pointee.mSampleRate <= 96000 else { throw OriginObservationFailure.unsupported }
            let size = CMVideoFormatDescriptionGetDimensions(videoFormats[0])
            guard size.width > 0, size.height > 0, size.width <= 1920, size.height <= 1920 else { throw OriginObservationFailure.bounds }
            var video: VTDecompressionSession?
            let specification = [kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: true] as CFDictionary
            let destination = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange] as CFDictionary
            guard VTDecompressionSessionCreate(allocator: nil, formatDescription: videoFormats[0], decoderSpecification: specification, imageBufferAttributes: destination, outputCallback: nil, decompressionSessionOut: &video) == noErr, let video else { throw OriginObservationFailure.unsupported }
            lock.withLock { videoSession = video }; try check()
            var hardware: Unmanaged<CFTypeRef>?
            guard VTSessionCopyProperty(video, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, allocator: nil, valueOut: &hardware) == noErr,
                  (hardware?.takeRetainedValue() as? NSNumber)?.boolValue == true else { throw OriginObservationFailure.unsupported }
            var outputs: [AVAssetReaderTrackOutput] = []
            let audioSettings: [String: Any] = [AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 16, AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false, AVLinearPCMIsNonInterleaved: false]
            for (index, track) in [videos[0], audios[0]].enumerated() {
                try check()
                let reader = try AVAssetReader(asset: asset)
                lock.withLock { readers.append(reader) }; try check()
                let output = AVAssetReaderTrackOutput(track: track, outputSettings: index == 0 ? nil : audioSettings)
                output.alwaysCopiesSampleData = false
                guard reader.canAdd(output) else { throw OriginObservationFailure.decode }
                reader.add(output)
                guard reader.startReading() else { throw OriginObservationFailure.decode }; outputs.append(output)
            }
            var ended = [false, false], examined = [0, 0]
            while !ended.allSatisfy({ $0 }) {
                for index in 0..<2 where !ended[index] {
                    try check(); examined[index] += 1
                    guard examined[index] <= 128 else { throw OriginObservationFailure.bounds }
                    let available = try autoreleasepool { () throws -> Bool in
                        guard let sample = outputs[index].copyNextSampleBuffer() else { return false }
                        guard CMSampleBufferGetNumSamples(sample) > 0 else { return true }
                        if index == 0 {
                            var callbackFailure: OriginObservationFailure?
                            let result = VTDecompressionSessionDecodeFrame(video, sampleBuffer: sample, flags: [], infoFlagsOut: nil) { status, flags, image, pts, _ in
                                do {
                                    try self.check()
                                    guard status == noErr, !flags.contains(.frameDropped), let image,
                                          CVPixelBufferGetWidth(image) == Int(size.width), CVPixelBufferGetHeight(image) == Int(size.height),
                                          pts.isNumeric, self.videoPTS == nil || CMTimeCompare(pts, self.videoPTS!) > 0, self.videoCount < 64 else { throw OriginObservationFailure.progress }
                                    self.videoPTS = pts; self.videoCount += 1
                                } catch { callbackFailure = (error as? OriginObservationFailure) ?? .decode }
                            }
                            if let callbackFailure { throw callbackFailure }
                            guard result == noErr else { throw OriginObservationFailure.decode }
                        } else { try observePCM(sample, sourceRate: audio.pointee.mSampleRate, sourceChannels: audio.pointee.mChannelsPerFrame) }
                        return true
                    }
                    if !available {
                        try check()
                        let reader = lock.withLock { readers[index] }
                        guard reader.status == .completed else { throw OriginObservationFailure.decode }; ended[index] = true
                    }
                }
            }
            let result = OriginDecodedMedia(videoOutputs: videoCount, pcmFrames: pcm.frames, pcmOutputs: pcm.outputs, hardwareVideo: true)
            guard result.sufficient else { throw OriginObservationFailure.progress }; return result
        }
        private func observePCM(_ sample: CMSampleBuffer, sourceRate: Double, sourceChannels: UInt32) throws {
            guard let format = CMSampleBufferGetFormatDescription(sample), let raw = CMAudioFormatDescriptionGetStreamBasicDescription(format), raw.pointee.mFormatID == kAudioFormatLinearPCM else { throw OriginObservationFailure.decode }
            let value = raw.pointee, frames = CMSampleBufferGetNumSamples(sample)
            guard value.mSampleRate.isFinite, value.mSampleRate.rounded() == value.mSampleRate,
                  value.mSampleRate >= 8000, value.mSampleRate <= 96000, value.mBitsPerChannel == 16,
                  value.mSampleRate == sourceRate, value.mChannelsPerFrame == sourceChannels, value.mFramesPerPacket == 1,
                  (1...2).contains(value.mChannelsPerFrame), value.mBytesPerFrame == value.mChannelsPerFrame * 2,
                  value.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0,
                  value.mFormatFlags & (kAudioFormatFlagIsFloat | kAudioFormatFlagIsBigEndian | kAudioFormatFlagIsNonInterleaved) == 0 else { throw OriginObservationFailure.decode }
            var first = CMSampleTimingInfo(), last = CMSampleTimingInfo()
            guard frames > 0, CMSampleBufferGetSampleTimingInfo(sample, at: 0, timingInfoOut: &first) == noErr,
                  CMSampleBufferGetSampleTimingInfo(sample, at: frames - 1, timingInfoOut: &last) == noErr else { throw OriginObservationFailure.progress }
            try pcm.accept(frames: frames, bytes: CMSampleBufferGetTotalSampleSize(sample), bytesPerFrame: Int(value.mBytesPerFrame), rate: Int32(value.mSampleRate), first: first.presentationTimeStamp, last: last.presentationTimeStamp, firstDuration: first.duration, lastDuration: last.duration)
        }
    }
}
