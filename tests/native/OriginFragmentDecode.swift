// Device-free feasibility only. The caller must supply generated, non-camera media.
// No rendering, playback, audio session, export, frame storage or hardware claim.
import Foundation
@preconcurrency import AVFoundation
import AudioToolbox
import CoreMedia
import CoreVideo
import Darwin

private enum ProbeFailure: String, Error {
    case arguments, inputBounds, sourceTracks, sourceCodec, sourceDimensions
    case readerSetup, readerStart, readerFailed, sampleLimit, timestamps
    case decodedFormat, insufficientProgress, timeout
}

// The reader may coalesce many decoded PCM frames into one output buffer.
// Validate native per-frame timing, not the number of reader callbacks.
private struct PCMWindow {
    let frames: Int, bytes: Int, bytesPerFrame: Int
    let rate: Int32
    let first: CMTime, last: CMTime, firstDuration: CMTime, lastDuration: CMTime
}
private struct PCMProgress {
    private(set) var frames = 0
    private var rate: Int32?
    private var end: CMTime?
    mutating func accept(_ value: PCMWindow) throws {
        guard value.rate >= 8000, value.rate <= 96000,
              value.frames > 0, value.frames <= Int(value.rate),
              value.bytesPerFrame == 2 || value.bytesPerFrame == 4,
              value.bytes == value.frames * value.bytesPerFrame,
              rate == nil || rate == value.rate else { throw ProbeFailure.decodedFormat }
        let frame = CMTime(value: 1, timescale: value.rate)
        let span = CMTime(value: Int64(value.frames), timescale: value.rate)
        guard value.first.isNumeric, value.last.isNumeric,
              CMTimeCompare(value.firstDuration, frame) == 0,
              CMTimeCompare(value.lastDuration, frame) == 0,
              CMTimeCompare(value.last, CMTimeAdd(value.first, CMTime(value: Int64(value.frames - 1), timescale: value.rate))) == 0,
              CMTimeCompare(CMTimeAdd(value.last, value.lastDuration), CMTimeAdd(value.first, span)) == 0,
              end == nil || CMTimeCompare(value.first, end!) >= 0 else { throw ProbeFailure.timestamps }
        rate = value.rate; end = CMTimeAdd(value.last, value.lastDuration)
        frames += value.frames
    }
    // This fixture uses AAC-LC's 1024 PCM frames per compressed audio unit.
    var enough: Bool { frames >= 2048 }
}
private func checkPCMProgress() throws {
    func window(_ count: Int, _ start: Int64 = 0, bytes: Int? = nil, last: Int64? = nil, duration: Int64 = 1) -> PCMWindow {
        PCMWindow(frames: count, bytes: bytes ?? count * 2, bytesPerFrame: 2, rate: 48000,
                  first: CMTime(value: start, timescale: 48000), last: CMTime(value: last ?? start + Int64(count) - 1, timescale: 48000),
                  firstDuration: CMTime(value: duration, timescale: 48000), lastDuration: CMTime(value: duration, timescale: 48000))
    }
    func refused(_ value: PCMWindow) -> Bool { var proof = PCMProgress(); do { try proof.accept(value); return false } catch { return true } }
    precondition(refused(window(0)))
    precondition(refused(window(2048, bytes: 2)))
    precondition(refused(window(2048, last: 4095)))
    precondition(refused(window(2048, duration: 2)))
    var one = PCMProgress(); try one.accept(window(1024)); precondition(!one.enough)
    try one.accept(window(1024, 1024)); precondition(one.enough && one.frames == 2048)
    var duplicateRefused = false
    do { try one.accept(window(1024, 1024)) } catch { duplicateRefused = true }
    precondition(duplicateRefused)
    var coalesced = PCMProgress(); try coalesced.accept(window(7104)); precondition(coalesced.enough && coalesced.frames == 7104)
}

private final class Run: @unchecked Sendable {
    private let lock = NSLock()
    private var readers: [AVAssetReader] = []
    private var stage = "arguments"
    private let started = DispatchTime.now().uptimeNanoseconds
    func mark(_ value: String) { lock.lock(); stage = value; lock.unlock() }
    private var finished = false
    private var timedOut = false
    private var outputs = [0, 0]
    private var audioWindows: [[String: Double]] = []
    private var pcmProgress = PCMProgress()
    private var seen = [0, 0]
    private var lastPTS: [CMTime?] = [nil, nil]

    func install(_ reader: AVAssetReader) throws {
        lock.lock(); defer { lock.unlock() }
        guard !timedOut && !finished else { throw ProbeFailure.timeout }
        readers.append(reader)
    }
    func check() throws {
        lock.lock(); defer { lock.unlock() }
        if timedOut { throw ProbeFailure.timeout }
    }
    func observe(_ track: Int, _ sample: CMSampleBuffer) throws {
        lock.lock(); defer { lock.unlock() }
        guard !timedOut else { throw ProbeFailure.timeout }
        seen[track] += 1
        guard seen[track] <= 256 else { throw ProbeFailure.sampleLimit }
        guard CMSampleBufferGetNumSamples(sample) > 0 else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        guard pts.isNumeric else { throw ProbeFailure.timestamps }
        if track == 0 {
            guard let image = CMSampleBufferGetImageBuffer(sample),
                  CVPixelBufferGetWidth(image) > 0, CVPixelBufferGetHeight(image) > 0 else {
                throw ProbeFailure.decodedFormat
            }
        } else {
            guard let format = CMSampleBufferGetFormatDescription(sample),
                  let pcm = CMAudioFormatDescriptionGetStreamBasicDescription(format),
                  pcm.pointee.mFormatID == kAudioFormatLinearPCM,
                  CMSampleBufferGetTotalSampleSize(sample) > 0 else {
                throw ProbeFailure.decodedFormat
            }
            let rate = pcm.pointee.mSampleRate
            guard rate.isFinite && rate > 0 else { throw ProbeFailure.decodedFormat }
            let count = CMSampleBufferGetNumSamples(sample)
            guard rate.rounded() == rate, (8000...96000).contains(rate), pcm.pointee.mFramesPerPacket == 1,
                  pcm.pointee.mBitsPerChannel == 16, pcm.pointee.mBytesPerFrame == pcm.pointee.mChannelsPerFrame * 2,
                  pcm.pointee.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0,
                  pcm.pointee.mFormatFlags & kAudioFormatFlagIsFloat == 0,
                  pcm.pointee.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0 else { throw ProbeFailure.decodedFormat }
            var first = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: .invalid, decodeTimeStamp: .invalid)
            var last = first
            guard count > 0, count <= Int(rate),
                  CMSampleBufferGetSampleTimingInfo(sample, at: 0, timingInfoOut: &first) == noErr,
                  CMSampleBufferGetSampleTimingInfo(sample, at: count - 1, timingInfoOut: &last) == noErr else { throw ProbeFailure.timestamps }
            try pcmProgress.accept(PCMWindow(frames: count, bytes: CMSampleBufferGetTotalSampleSize(sample), bytesPerFrame: Int(pcm.pointee.mBytesPerFrame),
                rate: Int32(rate), first: first.presentationTimeStamp, last: last.presentationTimeStamp, firstDuration: first.duration, lastDuration: last.duration))
            if audioWindows.count < 8 {
                let frames = Double(CMSampleBufferGetNumSamples(sample))
                audioWindows.append(["pcmFrames": frames, "pcmBytes": Double(CMSampleBufferGetTotalSampleSize(sample)),
                    "sampleRate": rate, "ptsMs": first.presentationTimeStamp.seconds * 1000,
                    "lastFramePtsMs": last.presentationTimeStamp.seconds * 1000, "lastFrameDurationMs": last.duration.seconds * 1000,
                    "decodedSpanMs": frames / rate * 1000])
            }
        }
        if let prior = lastPTS[track], CMTimeCompare(pts, prior) <= 0 {
            throw ProbeFailure.timestamps
        }
        lastPTS[track] = pts
        outputs[track] += 1
        // Neither pixel-buffer contents nor PCM bytes are read or retained.
    }
    func enough() -> Bool {
        lock.lock(); defer { lock.unlock() }
        return outputs[0] >= 2 && pcmProgress.enough
    }
    func cancelAll() {
        lock.lock(); let copy = readers; lock.unlock()
        for reader in copy { reader.cancelReading() }
    }
    func requestTimeout() {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        timedOut = true; let copy = readers
        lock.unlock()
        // Independent cancellation requests prevent one stuck cancel call from
        // suppressing the other reader's cancellation request.
        for reader in copy {
            DispatchQueue.global(qos: .userInitiated).async { reader.cancelReading() }
        }
    }
    func finish(_ failure: ProbeFailure?) {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        finished = true
        let reason = timedOut ? ProbeFailure.timeout : failure
        let summary: [String: Any] = [
            "ok": reason == nil, "status": reason?.rawValue ?? "pass",
            "lastStage": stage, "elapsedMs": Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000,
            "videoOutputs": outputs[0], "audioPCMOutputs": outputs[1], "audioWindows": audioWindows,
            "validatedPCMFrames": pcmProgress.frames,
            "videoSamplesExamined": seen[0], "audioSamplesExamined": seen[1]
        ]
        lock.unlock()
        if let json = try? JSONSerialization.data(withJSONObject: summary, options: [.sortedKeys]) {
            FileHandle.standardOutput.write(json)
            FileHandle.standardOutput.write(Data([10]))
        }
        Darwin._exit(reason == nil ? 0 : reason == .timeout ? 124 : 1)
    }
}

private func decode(_ run: Run) async throws {
    if CommandLine.arguments == [CommandLine.arguments[0], "--check-pcm-timing"] {
        try checkPCMProgress(); return
    }
    guard CommandLine.arguments.count == 3,
          CommandLine.arguments[1] == "--synthetic-mp4" else { throw ProbeFailure.arguments }
    let input = URL(fileURLWithPath: CommandLine.arguments[2]).standardizedFileURL
    let properties = try input.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
    guard properties.isRegularFile == true, properties.isSymbolicLink != true,
          let bytes = properties.fileSize, (1...8 * 1024 * 1024).contains(bytes) else {
        throw ProbeFailure.inputBounds
    }
    try run.check()
    let asset = AVURLAsset(url: input)
    run.mark("metadata-tracks")
    let tracks = try await asset.load(.tracks)
    let videos = tracks.filter { $0.mediaType == .video }
    let audios = tracks.filter { $0.mediaType == .audio }
    guard videos.count == 1, audios.count == 1, tracks.count == 2 else {
        throw ProbeFailure.sourceTracks
    }
    run.mark("metadata-formats")
    let videoFormats = try await videos[0].load(.formatDescriptions)
    let audioFormats = try await audios[0].load(.formatDescriptions)
    guard videoFormats.count == 1, audioFormats.count == 1,
          CMFormatDescriptionGetMediaSubType(videoFormats[0]) == kCMVideoCodecType_H264,
          CMFormatDescriptionGetMediaSubType(audioFormats[0]) == kAudioFormatMPEG4AAC else {
        throw ProbeFailure.sourceCodec
    }
    let size = CMVideoFormatDescriptionGetDimensions(videoFormats[0])
    guard size.width > 0, size.height > 0,
          Int64(size.width) * Int64(size.height) <= 1920 * 1920,
          let audio = CMAudioFormatDescriptionGetStreamBasicDescription(audioFormats[0]),
          audio.pointee.mChannelsPerFrame >= 1, audio.pointee.mChannelsPerFrame <= 2,
          audio.pointee.mSampleRate >= 8000, audio.pointee.mSampleRate <= 96000 else {
        throw ProbeFailure.sourceDimensions
    }
    let settings: [[String: Any]] = [
        [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange],
        [AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 16,
         AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
         AVLinearPCMIsNonInterleaved: false]
    ]
    var readers: [AVAssetReader] = []
    var outputs: [AVAssetReaderTrackOutput] = []
    defer { run.mark("cancel-readers"); run.cancelAll() }
    for (index, track) in [videos[0], audios[0]].enumerated() {
        try run.check()
        let reader = try AVAssetReader(asset: asset)
        do { try run.install(reader) } catch { reader.cancelReading(); throw error }
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: settings[index])
        output.alwaysCopiesSampleData = false
        guard reader.canAdd(output) else { throw ProbeFailure.readerSetup }
        reader.add(output)
        run.mark("reader-start-\(index)")
        guard reader.startReading() else { throw ProbeFailure.readerStart }
        readers.append(reader); outputs.append(output)
    }
    run.mark("decoded-output")
    var ended = [false, false]
    while !ended.allSatisfy({ $0 }) {
        for index in 0..<2 where !ended[index] {
            try run.check()
            let available = try autoreleasepool { () throws -> Bool in
                guard let sample = outputs[index].copyNextSampleBuffer() else { return false }
                try run.observe(index, sample)
                return true
            }
            if !available {
                try run.check()
                guard readers[index].status == .completed else { throw ProbeFailure.readerFailed }
                ended[index] = true
            }
        }
    }
    guard run.enough() else { throw ProbeFailure.insufficientProgress }
}

private let run = Run()
// The soft deadline requests cancellation. The independent hard deadline also
// bounds a platform decode/cancel call that fails to return. Normal/error paths
// cancel both readers before reporting; timeout never claims successful cleanup.
DispatchQueue(label: "sapflow.probe.cancel").asyncAfter(deadline: .now() + .milliseconds(2750)) {
    run.requestTimeout()
}
DispatchQueue(label: "sapflow.probe.deadline").asyncAfter(deadline: .now() + .seconds(3)) {
    run.requestTimeout()
    run.finish(.timeout)
}
Task.detached {
    do {
        try await decode(run)
        run.finish(nil)
    } catch let failure as ProbeFailure {
        run.cancelAll()
        run.finish(failure)
    } catch {
        run.cancelAll()
        run.finish(.readerFailed)
    }
}
dispatchMain()
