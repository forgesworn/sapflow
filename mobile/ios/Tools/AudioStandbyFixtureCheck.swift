// Generated-only host proof of the actual AAC encoder and recording owners.
// check-audio-standby.sh supplies an empty owned directory, never input media.
import Foundation
import AVFoundation
import CryptoKit
import Sapflow

private func requireStandby(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CaptureFailure(message: message) }
}

@main
enum AudioStandbyFixtureCheck {
    private final class ValidationSink: EncodedPublishingSink {
        private let queues: NativeSampleFanout
        private(set) var configurations = 0
        private(set) var accepted: UInt64 = 0
        var status: String { "Generated local SDK validation only" }
        init() throws { queues = try NativeSampleFanout(publishingBytes: 1_048_576, recordingBytes: 1_048_576, maxMediaMs: 1000, hasVideo: false) }
        func start() throws {}
        func configure(video: MediaCodec?, audio: MediaCodec) throws {
            try requireStandby(video == nil && configurations == 0, "SDK descriptor was configured again across AAC generations")
            configurations += 1
        }
        func offer(_ sample: MediaSample) throws {
            let outcome = try queues.push(sample: sample, nowNs: accepted)
            try requireStandby(outcome.publishingQueued && outcome.recordingQueued && !outcome.recordingFailed, "Actual SDK fanout rejected the persistent descriptor or timeline")
            let publish = try queues.popPublishing(nowNs: accepted)
            let record = try queues.popRecording(nowNs: accepted)
            try requireStandby(publish != nil && record != nil, "Actual accepted SDK branch could not drain")
            accepted += 1
        }
        func stop() { try? queues.cancel() }
    }
    private enum Delivery {
        case timing(AudioRecordingTiming.Event)
        case sample(CompressedUnit)
    }
    private struct Tone {
        let name: String
        let start: CMTime
        let frames: Int
        let rate: Int32
        let kind: Int
    }

    static func main() async {
        do {
            try requireStandby(CommandLine.arguments.count == 2, "Use the generated-only standby script")
            let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
            try requireStandby(root == root.resolvingSymlinksInPath(), "Owned root must not resolve elsewhere")
            let marker = try String(contentsOf: root.appendingPathComponent(".sapflow-audio-standby-fixture"), encoding: .utf8)
            try requireStandby(marker == "generated-native-audio-standby-v1\n", "Missing owned generated fixture marker")
            var cases: [[String: Any]] = []
            for (name, rate, channels) in [("mono48", 48_000, 1), ("stereo441", 44_100, 2)] {
                let directory = root.appendingPathComponent(name, isDirectory: true)
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
                do { cases.append(try await run(root: directory, name: name, rate: Int32(rate), channels: UInt32(channels))) }
                catch { cases.append(["name": name, "failed": true, "error": String(describing: error)]) }
            }
            let summary: [String: Any] = ["generatedOnly": true, "cases": cases,
                "scope": "Actual native AAC, Rust descriptor validation and recorder, generated raw A -> zero -> raw B -> zero only. No microphone, camera, socket, phone or real-time capture-driver proof."]
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: summary, options: [.prettyPrinted, .sortedKeys]))
            FileHandle.standardOutput.write(Data([10]))
            if cases.contains(where: { $0["failed"] as? Bool == true }) { exit(1) }
        } catch {
            FileHandle.standardError.write(Data(("Generated standby check failed: \(error)\n").utf8)); exit(1)
        }
    }

    private static func toneValue(frame: Int, rate: Int32, kind: Int) -> Int16 {
        let t = Double(frame) / Double(rate)
        let value: Double
        if kind == 0 {
            value = 0.28 * sin(2 * .pi * (113 * t + 173 * t * t)) + 0.18 * sin(2 * .pi * (997 * t + 71 * t * t))
        } else {
            value = 0.23 * sin(2 * .pi * (317 * t + 127 * t * t)) + 0.21 * sin(2 * .pi * (1297 * t + 53 * t * t))
        }
        return Int16((value * 32767).rounded())
    }

    private static func pcm(format: AVAudioFormat, start: CMTime, firstFrame: Int, frames: Int, kind: Int) throws -> CMSampleBuffer {
        let channels = Int(format.channelCount)
        var bytes = [Int16](repeating: 0, count: frames * channels)
        for n in 0..<frames {
            let value = toneValue(frame: firstFrame + n, rate: Int32(format.sampleRate), kind: kind)
            for channel in 0..<channels { bytes[n * channels + channel] = value }
        }
        let byteCount = bytes.count * MemoryLayout<Int16>.size
        var block: CMBlockBuffer?
        let allocated = CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil,
            blockLength: byteCount, blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: byteCount, flags: 0, blockBufferOut: &block)
        guard allocated == noErr, let block else { throw CaptureFailure(message: "Generated PCM allocation failed") }
        let copied = bytes.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: byteCount) }
        try requireStandby(copied == noErr, "Generated PCM storage failed")
        var description: CMAudioFormatDescription?
        try requireStandby(CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault,
            asbd: format.streamDescription, layoutSize: 0, layout: nil, magicCookieSize: 0, magicCookie: nil,
            extensions: nil, formatDescriptionOut: &description) == noErr, "Generated PCM description failed")
        guard let description else { throw CaptureFailure(message: "Generated PCM description unavailable") }
        let rate = Int32(format.sampleRate)
        let time = CMTimeAdd(start, CMTime(value: Int64(firstFrame), timescale: rate))
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: rate), presentationTimeStamp: time, decodeTimeStamp: time)
        var sample: CMSampleBuffer?
        var size = channels * MemoryLayout<Int16>.size
        let made = CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block,
            formatDescription: description, sampleCount: frames, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample)
        guard made == noErr, let sample else { throw CaptureFailure(message: "Generated PCM sample failed") }
        return sample
    }

    private static func feed(_ encoder: NativeAudioEncoder, token: NativeAudioEncoder.InputToken,
                             format: AVAudioFormat, start: CMTime, frames: Int, kind: Int) throws {
        for at in stride(from: 0, to: frames, by: 480) {
            let sample = try pcm(format: format, start: start, firstFrame: at, frames: min(480, frames - at), kind: kind)
            try encoder.encode(sample, time: CMSampleBufferGetPresentationTimeStamp(sample), token: token)
        }
    }

    private static func run(root: URL, name: String, rate: Int32, channels: UInt32) async throws -> [String: Any] {
        guard let format = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(rate), channels: channels, interleaved: true) else {
            throw CaptureFailure(message: "Generated input format unavailable")
        }
        let sink = try ValidationSink()
        let ingress = RustEncodedIngress(); ingress.reset(hasVideo: false); ingress.destination = sink
        var deliveries: [Delivery] = [], units: [CompressedUnit] = []
        var outputFailure: Error?
        var generation = 1
        var packets: [[String: Any]] = []
        weak var callbackEncoder: NativeAudioEncoder?
        let encoder = try NativeAudioEncoder(output: { unit in
            do {
                try requireStandby(!unit.isVideo && !unit.keyframe && unit.duration == CMTime(value: 1024, timescale: 48_000), "Unexpected encoded topology/cadence")
                try requireStandby(unit.time == CMTime(value: Int64(units.count * 1024), timescale: 48_000), "Global AAC output cursor reset or skipped")
                try ingress.accept(unit)
                try requireStandby(callbackEncoder?.lastOutputGeneration == UInt64(generation), "Native output generation differed from the declared input boundary")
                units.append(unit); deliveries.append(.sample(unit))
                var packet = try AudioOnlyFixtureCheck.payloadHash(unit.buffer)
                packet["generation"] = generation; packet["pts48k"] = unit.time.value
                packets.append(packet)
                try requireStandby(units.count <= 256 && deliveries.count <= 512, "Generated output retention bound")
            } catch { outputFailure = outputFailure ?? error }
        }, meter: { _ in }, recordingTiming: { deliveries.append(.timing($0)) })
        callbackEncoder = encoder
        defer {
            // Keep bounded scalar facts even when a real native format or EOF
            // fails, without preserving generated PCM or silently retrying it.
            let diagnostics: [String: Any] = ["sourceFormat": NativeAudioEncoder.formatFacts(format),
                "ledger": String(describing: encoder.generationLedger), "units": units.count,
                "configurationChecks": encoder.verifiedConfigurationGenerations]
            if let bytes = try? JSONSerialization.data(withJSONObject: diagnostics, options: [.prettyPrinted, .sortedKeys]) {
                try? bytes.write(to: root.appendingPathComponent("diagnostics.local.json"), options: .withoutOverwriting)
            }
        }
        guard let original = encoder.inputToken else { throw CaptureFailure(message: "Initial raw input token unavailable") }
        let rawA = Int(rate) / 3 + 137
        try feed(encoder, token: original, format: format, start: .zero, frames: rawA, kind: 0)
        if let outputFailure { throw outputFailure }
        var tones = [Tone(name: "A", start: .zero, frames: rawA, rate: rate, kind: 0)]
        var boundaries: [Int] = []
        let firstZero = try encoder.beginStandby(from: original)
        generation += 1; boundaries.append(units.count * 1024)
        let firstTarget = UInt64((Double(rawA) * 48_000 / Double(rate)).rounded()) + 32_017
        let firstPlan = try encoder.standbyPlan(validFramesThrough: firstTarget, token: firstZero)
        var remaining = firstPlan.additionalSourceFrames
        while remaining > 0 {
            let count = UInt32(min(480, remaining))
            try encoder.encodeSilence(frames: count, token: firstZero); remaining -= UInt64(count)
        }
        guard let observedBStart = encoder.nextInputTime else { throw CaptureFailure(message: "Resume source clock unavailable") }
        // Pin the resume marker to the independently declared interval on the
        // source's hardware-rate grid, never to a clock returned by the encoder.
        let rawBSourceFrame = (Int64(firstTarget) * Int64(rate) + 24_000) / 48_000
        let rawBStart = CMTime(value: rawBSourceFrame, timescale: rate)
        try requireStandby(abs(CMTimeSubtract(observedBStart, rawBStart).seconds) <= 1 / Double(rate) + 1 / 48_000.0,
            "Encoder moved the independently declared Resume boundary")
        let resumed = try encoder.resumeInput(from: firstZero, at: rawBStart)
        let beforeStale = units.count
        let staleA = try pcm(format: format, start: rawBStart, firstFrame: 0, frames: 127, kind: 0)
        do {
            try encoder.encode(staleA, time: rawBStart, token: original)
            throw CaptureFailure(message: "Retired raw token was accepted")
        } catch let error as CaptureFailure where error.message == "Retired raw token was accepted" { throw error }
        catch { /* Expected stale-input refusal. The current raw owner must still work. */ }
        try requireStandby(units.count == beforeStale, "Stale raw input altered encoded output")
        do {
            try encoder.encodeSilence(frames: 127, token: firstZero)
            throw CaptureFailure(message: "Retired zero token was accepted")
        } catch let error as CaptureFailure where error.message == "Retired zero token was accepted" { throw error }
        catch { /* A delayed zero producer cannot mutate the resumed owner. */ }
        try requireStandby(units.count == beforeStale, "Retired zero producer altered encoded output")
        let rawB = Int(rate) / 3 + 83
        tones.append(Tone(name: "B", start: rawBStart, frames: rawB, rate: rate, kind: 1))
        try feed(encoder, token: resumed, format: format, start: rawBStart, frames: rawB, kind: 1)
        if let outputFailure { throw outputFailure }
        let secondZero = try encoder.beginStandby(from: resumed)
        generation += 1; boundaries.append(units.count * 1024)
        let beforeDelayedB = units.count
        let staleB = try pcm(format: format, start: rawBStart, firstFrame: 0, frames: 127, kind: 1)
        do {
            try encoder.encode(staleB, time: rawBStart, token: resumed)
            throw CaptureFailure(message: "Delayed resumed raw token was accepted")
        } catch let error as CaptureFailure where error.message == "Delayed resumed raw token was accepted" { throw error }
        catch { /* Second Standby fences the previous raw owner too. */ }
        try requireStandby(units.count == beforeDelayedB, "Second Standby admitted delayed raw B")
        let rawBEnd = CMTimeAdd(rawBStart, CMTime(value: Int64(rawB), timescale: rate))
        let target = UInt64(CMTimeConvertScale(rawBEnd, timescale: 48_000, method: .roundHalfAwayFromZero).value) + 32_119
        let secondPlan = try encoder.standbyPlan(validFramesThrough: target, token: secondZero)
        remaining = secondPlan.additionalSourceFrames
        while remaining > 0 {
            let count = UInt32(min(480, remaining))
            try encoder.encodeSilence(frames: count, token: secondZero); remaining -= UInt64(count)
        }
        try encoder.finishStandbyThrough(validFrames: secondPlan.attainableValidFrames, token: secondZero)
        if let outputFailure { throw outputFailure }
        let finishedCount = units.count, finishedDeliveries = deliveries.count
        try encoder.finish()
        try requireStandby(units.count == finishedCount && deliveries.count == finishedDeliveries, "Repeated finish emitted another tail or metadata event")
        guard let extent = encoder.generationLedger.completed else { throw CaptureFailure(message: "Final actual generation ledger unavailable") }
        try requireStandby(encoder.verifiedConfigurationGenerations == 3 && sink.configurations == 1 && sink.accepted == ingress.accepted,
            "Per-generation native configuration or persistent SDK descriptor proof missing")
        do {
            try encoder.encodeSilence(frames: 64, token: secondZero)
            throw CaptureFailure(message: "Silence admitted after final EOF")
        } catch let error as CaptureFailure where error.message == "Silence admitted after final EOF" { throw error }
        catch { /* Final EOF remains terminal. */ }
        let file = try writeRecording(root: root, deliveries: deliveries)
        let asset = AVURLAsset(url: file)
        let audio = try await asset.loadTracks(withMediaType: .audio)
        let video = try await asset.loadTracks(withMediaType: .video)
        try requireStandby(audio.count == 1 && video.isEmpty, "Standby recording topology changed")
        let nativePackets = try AudioOnlyFixtureCheck.readEncodedPackets(asset: asset, track: audio[0])
        let expectedHashes = packets.map { ["size": $0["size"]!, "sha256": $0["sha256"]!] }
        try requireStandby(NSDictionary(dictionary: ["packets": nativePackets["packetHashes"]!]).isEqual(to: ["packets": expectedHashes]), "Actual recorder changed native AAC sequence")
        let nativePCM = root.appendingPathComponent("native.pcm")
        let frames = try AudioOnlyFixtureCheck.readPCM(asset: asset, track: audio[0], destination: nativePCM)
        let intervals = try nativeIntervals(asset: asset, track: audio[0], expected: secondPlan.attainableValidFrames)
        let nativeBytes = try Data(contentsOf: nativePCM)
        let adts = root.appendingPathComponent("joined.aac")
        var elementary = Data()
        var fresh = Data(), zeroOnly = Data()
        for (index, unit) in units.enumerated() {
            guard let block = CMSampleBufferGetDataBuffer(unit.buffer) else { throw CaptureFailure(message: "Native AAC payload unavailable") }
            let count = CMSampleBufferGetTotalSampleSize(unit.buffer)
            let length = count + 7
            try requireStandby((1..<8192).contains(length), "Generated ADTS bound")
            var framed = Data([0xff, 0xf1, 0x4c, UInt8(0x40 | length >> 11), UInt8((length >> 3) & 255), UInt8((length & 7) << 5 | 0x1f), 0xfc])
            var payload = Data(count: count)
            let copied = payload.withUnsafeMutableBytes { CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: count, destination: $0.baseAddress!) }
            try requireStandby(copied == noErr, "Generated ADTS payload copy failed"); framed.append(payload)
            elementary.append(framed)
            if index >= boundaries[0] / 1024 { fresh.append(framed) }
            if index >= boundaries[1] / 1024 { zeroOnly.append(framed) }
        }
        try elementary.write(to: adts, options: .withoutOverwriting)
        var controls: [[String: Any]] = []
        for (control, bytes, boundary) in [("fresh", fresh, boundaries[0]), ("zero", zeroOnly, boundaries[1])] {
            let source = root.appendingPathComponent(control + ".aac")
            try bytes.write(to: source, options: .withoutOverwriting)
            let controlAsset = AVURLAsset(url: source)
            let tracks = try await controlAsset.loadTracks(withMediaType: .audio)
            try requireStandby(tracks.count == 1, "Generated AAC control track unavailable")
            let decoded = root.appendingPathComponent(control + "-native.pcm")
            let frames = try AudioOnlyFixtureCheck.readPCM(asset: controlAsset, track: tracks[0], destination: decoded)
            controls.append(["name": control, "codedStart48k": boundary, "codedFrames": units.count * 1024 - boundary,
                "nativeFrames": frames, "nativePCMSHA256": SHA256.hash(data: try Data(contentsOf: decoded)).map { String(format: "%02x", $0) }.joined()])
        }
        var avRecordings: [[String: Any]] = []
        var shortRecordings: [[String: Any]] = []
        if name == "mono48" {
            let videoUnits = try await generatedVideo(root: root.deletingLastPathComponent())
            var combined: [Delivery] = [], index = 0
            for delivery in deliveries {
                if case let .sample(audioUnit) = delivery {
                    while index < videoUnits.count && CMTimeCompare(videoUnits[index].time, audioUnit.time) <= 0 {
                        combined.append(.sample(videoUnits[index])); index += 1
                    }
                }
                combined.append(delivery)
            }
            for unit in videoUnits.dropFirst(index) { combined.append(.sample(unit)) }
            let expectedVideo = try videoUnits.map { try AudioOnlyFixtureCheck.payloadHash($0.buffer) }
            for (basename, injectedFailure, audioOffset) in [("av-finalised", false, 0), ("av-native-failure", true, 0), ("av-delayed-audio", false, 1440)] {
                var shifted: [Delivery] = []
                for event in combined {
                    if case let .sample(unit) = event, !unit.isVideo, audioOffset != 0 {
                        let pts = CMTimeAdd(unit.time, CMTime(value: Int64(audioOffset), timescale: 48_000))
                        var timing = CMSampleTimingInfo(duration: unit.duration, presentationTimeStamp: pts, decodeTimeStamp: pts)
                        var copy: CMSampleBuffer?
                        let status = CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault,
                            sampleBuffer: unit.buffer, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &copy)
                        guard status == noErr, let copy else { throw CaptureFailure(message: "Generated delayed AAC timing failed") }
                        shifted.append(.sample(CompressedUnit(buffer: copy, isVideo: false, keyframe: false,
                            time: pts, duration: unit.duration, configuration: unit.configuration)))
                    } else { shifted.append(event) }
                }
                // Delay one track on its original timestamp grid. Each track
                // retains its ordering and every coded byte remains unchanged.
                let saved = try writeRecording(root: root, deliveries: shifted, basename: basename,
                    hasVideo: true, injectedFailure: injectedFailure)
                let recordedAsset = AVURLAsset(url: saved)
                let audioTracks = try await recordedAsset.loadTracks(withMediaType: .audio)
                let videoTracks = try await recordedAsset.loadTracks(withMediaType: .video)
                try requireStandby(audioTracks.count == 1 && videoTracks.count == 1, "Generated A/V topology changed")
                let storedAudio = try AudioOnlyFixtureCheck.readEncodedPackets(asset: recordedAsset, track: audioTracks[0])
                let storedVideo = try AudioOnlyFixtureCheck.readEncodedPackets(asset: recordedAsset, track: videoTracks[0])
                let sameAudio = NSDictionary(dictionary: ["packets": storedAudio["packetHashes"]!]).isEqual(to: ["packets": expectedHashes])
                let sameVideo = NSDictionary(dictionary: ["packets": storedVideo["packetHashes"]!]).isEqual(to: ["packets": expectedVideo])
                let expectedFrames = extent.encodedFrames - UInt64(extent.leadingFrames) - (injectedFailure ? 0 : UInt64(extent.trailingFrames))
                let avPCM = root.appendingPathComponent(basename + ".pcm")
                let decodedFrames = try AudioOnlyFixtureCheck.readPCM(asset: recordedAsset, track: audioTracks[0], destination: avPCM)
                let avIntervals = try nativeIntervals(asset: recordedAsset, track: audioTracks[0], expected: nil)
                avRecordings.append(["recording": saved.lastPathComponent, "finalised": !injectedFailure,
                    "injectedNativeTimingFailure": injectedFailure, "audioPackets": expectedHashes,
                    "videoPackets": expectedVideo, "exactNativeStoredPayloads": sameAudio && sameVideo,
                    "nativeAudioReader": storedAudio, "nativeVideoReader": storedVideo,
                    "nativePCM": avPCM.lastPathComponent, "nativePCMFrames": decodedFrames,
                    "expectedNativePCMFrames": expectedFrames, "audioOffset48k": audioOffset, "nativeIntervals": avIntervals])
            }
            let headCount = try AudioRecordingTiming.headUnits(extent.leadingFrames)
            for count in 0...headCount {
                let selected = Array(units.prefix(count))
                let selectedAudioHashes = try selected.map { try AudioOnlyFixtureCheck.payloadHash($0.buffer) }
                let prefix: [Delivery] = [.timing(.began(leadingFrames: extent.leadingFrames)), .sample(videoUnits[0])] + selected.map(Delivery.sample)
                for injectedFailure in [false, true] {
                    let basename = "av-prefix-\(count)-" + (injectedFailure ? "fault" : "stop")
                    let saved = try writeRecording(root: root, deliveries: prefix, basename: basename,
                        hasVideo: true, injectedFailure: injectedFailure, expectedFinalised: false)
                    let prefixAsset = AVURLAsset(url: saved)
                    let videoTracks = try await prefixAsset.loadTracks(withMediaType: .video)
                    let audioTracks = try await prefixAsset.loadTracks(withMediaType: .audio)
                    try requireStandby(videoTracks.count == 1 && audioTracks.count == (count == 0 ? 0 : 1), "Short recording discarded an accepted track")
                    let storedVideo = try AudioOnlyFixtureCheck.readEncodedPackets(asset: prefixAsset, track: videoTracks[0])
                    try requireStandby(NSDictionary(dictionary: ["packets": storedVideo["packetHashes"]!]).isEqual(to: ["packets": [expectedVideo[0]]]), "Short recording discarded its positive encoded video prefix")
                    shortRecordings.append(["recording": saved.lastPathComponent, "finalised": false,
                        "audioPackets": selectedAudioHashes, "videoPackets": [expectedVideo[0]],
                        "injectedNativeTimingFailure": injectedFailure, "acceptedAudioUnits": count])
                }
            }
        }
        return ["name": name, "sourceRate": rate, "sourceChannels": channels, "actualRustValidatedUnits": ingress.accepted,
            "packets": packets, "freshCodedBoundaries": boundaries, "controls": controls, "requestedFinalValidFrames": target,
            "attainableFinalValidFrames": secondPlan.attainableValidFrames,
            "quantizationOutputFrames": secondPlan.quantizationOutputFrames,
            "fileLeadingFrames": extent.leadingFrames, "fileTrailingFrames": extent.trailingFrames,
            "nativeConfigurationChecks": encoder.verifiedConfigurationGenerations, "sdkConfigurations": sink.configurations,
            "nativePCMFrames": frames, "nativePCMSHA256": SHA256.hash(data: nativeBytes).map { String(format: "%02x", $0) }.joined(),
            "nativeReader": nativePackets, "nativeIntervals": intervals, "recording": file.lastPathComponent, "avRecordings": avRecordings,
            "shortRecordings": shortRecordings, "ledger": String(describing: encoder.generationLedger),
            "tones": tones.map { ["name": $0.name, "startValue": $0.start.value, "startScale": $0.start.timescale,
                "sourceFrames": $0.frames, "sourceRate": $0.rate, "kind": $0.kind] },
            "rawTokenStaleRefused": true, "finishIdempotent": true, "postEOFSilenceRefused": true]
    }

    private static func generatedVideo(root: URL) async throws -> [CompressedUnit] {
        // The launcher creates this H.264 colour fixture from lavfi in its
        // empty owned directory. There is no caller-supplied media input.
        let asset = AVURLAsset(url: root.appendingPathComponent("generated-video.mov"))
        let tracks = try await asset.loadTracks(withMediaType: .video)
        try requireStandby(tracks.count == 1, "Generated video track unavailable")
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: tracks[0], outputSettings: nil)
        output.alwaysCopiesSampleData = false; reader.add(output)
        try requireStandby(reader.startReading(), "Generated video reader could not start")
        defer { if reader.status == .reading { reader.cancelReading() } }
        var units: [CompressedUnit] = [], emptyMarkers = 0
        while let sample = output.copyNextSampleBuffer() {
            if CMSampleBufferGetNumSamples(sample) == 0 {
                emptyMarkers += 1
                try requireStandby(emptyMarkers <= 16 && CMSampleBufferGetTotalSampleSize(sample) == 0 &&
                    CMSampleBufferGetDuration(sample) == .zero, "Generated H.264 empty marker bound")
                continue
            }
            guard CMSampleBufferGetNumSamples(sample) == 1, let format = CMSampleBufferGetFormatDescription(sample),
                  CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_H264 else {
                throw CaptureFailure(message: "Generated H.264 sample unavailable")
            }
            func parameter(_ index: Int) throws -> Data {
                var pointer: UnsafePointer<UInt8>?
                var size = 0, count = 0
                var header: Int32 = 0
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index,
                    parameterSetPointerOut: &pointer, parameterSetSizeOut: &size, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &header)
                guard status == noErr, let pointer, size > 0, header == 4 else {
                    throw CaptureFailure(message: "Generated H.264 parameter set unavailable")
                }
                return Data(bytes: pointer, count: size)
            }
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
            let pts = CMSampleBufferGetPresentationTimeStamp(sample)
            let duration = CMSampleBufferGetDuration(sample)
            try requireStandby(pts.isNumeric && pts == CMTime(value: Int64(units.count), timescale: 30) &&
                duration == CMTime(value: 1, timescale: 30), "Generated H.264 cadence changed")
            units.append(CompressedUnit(buffer: sample, isVideo: true,
                keyframe: (attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true,
                time: pts, duration: duration, configuration: .h264(sps: try parameter(0), pps: try parameter(1))))
            try requireStandby(units.count <= 64, "Generated H.264 retention bound")
        }
        try requireStandby(reader.status == .completed && units.count == 63, "Generated H.264 fixture incomplete")
        return units
    }

    private static func nativeIntervals(asset: AVAsset, track: AVAssetTrack, expected: UInt64?) throws -> [String: Any] {
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: [AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: 48_000, AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false, AVLinearPCMIsNonInterleaved: false])
        reader.add(output); try requireStandby(reader.startReading(), "Native interval reader could not start")
        defer { if reader.status == .reading { reader.cancelReading() } }
        var cursor: Int64 = 0, batches = 0, empty = 0
        var first: Int64?
        let began = ProcessInfo.processInfo.systemUptime
        while let sample = output.copyNextSampleBuffer() {
            try requireStandby(ProcessInfo.processInfo.systemUptime - began < 5, "Native interval reader deadline")
            let count = CMSampleBufferGetNumSamples(sample)
            if count == 0 { empty += 1; try requireStandby(empty <= 16, "Native interval marker bound"); continue }
            let pts = CMTimeConvertScale(CMSampleBufferGetOutputPresentationTimeStamp(sample), timescale: 48_000, method: .roundHalfAwayFromZero)
            let duration = CMTimeConvertScale(CMSampleBufferGetOutputDuration(sample), timescale: 48_000, method: .roundHalfAwayFromZero)
            if first == nil { first = pts.value; cursor = pts.value }
            try requireStandby(pts.isNumeric && duration.isNumeric && pts.value == cursor && duration.value == Int64(count),
                "Native PCM PTS/duration contains a gap, overlap or hidden head offset")
            cursor += Int64(count); batches += 1
            try requireStandby(batches <= 256 && cursor <= 144_000, "Native interval retention bound")
        }
        try requireStandby(reader.status == .completed && cursor >= 0 && first != nil, "Native PCM interval reader incomplete")
        if let expected {
            try requireStandby(first == 0 && UInt64(cursor) == expected, "Native PCM final interval differs from declared presentation")
        }
        return ["firstPts48k": first ?? -1, "finalEnd48k": cursor, "frames": cursor - (first ?? 0), "batches": batches, "contiguous": true]
    }

    private static func writeRecording(root: URL, deliveries: [Delivery], basename: String = "recording",
                                       hasVideo: Bool = false, injectedFailure: Bool = false, expectedFinalised: Bool? = nil) throws -> URL {
        let queue = DispatchQueue(label: "generated.audio-standby.recording")
        let catalogue = try queue.sync { try RecordingCatalogue(root: root.appendingPathComponent(basename + "-catalogue", isDirectory: true)) }
        let show = catalogue.newShow(profile: hasVideo ? .sd480p30 : .audioOnly48kMono)
        let receipt = try queue.sync { try catalogue.begin(identity: show.nextIdentity()) }
        let recorder = try queue.sync { try CompressedRecorder(receipt: receipt, queue: queue, hasVideo: hasVideo, onFailure: { _ in }) }
        let start = DispatchTime.now().uptimeNanoseconds
        for event in deliveries {
            switch event {
            case let .timing(value): recorder.audioTiming(value)
            case let .sample(unit):
                let target = start + UInt64(unit.time.seconds * 1_000_000_000)
                let now = DispatchTime.now().uptimeNanoseconds
                if target > now { Thread.sleep(forTimeInterval: Double(target - now) / 1_000_000_000) }
                recorder.offer(unit)
            }
        }
        if injectedFailure { recorder.audioTiming(.failed) }
        let finished = DispatchSemaphore(value: 0)
        var result: CompressedRecorder.Completion?
        recorder.finish { result = $0; finished.signal() }
        try requireStandby(finished.wait(timeout: .now() + 10) == .success, "Actual recorder finish timeout")
        let finalised = expectedFinalised ?? !injectedFailure
        guard let result, result.finalised == finalised, result.cleanupComplete,
              (result.failure != nil) == !finalised, let file = result.fileURL else {
            throw CaptureFailure(message: "Actual standby recording incomplete: \(String(describing: result?.failure))")
        }
        let entry = try queue.sync { try catalogue.scan().first { $0.segmentID == receipt.identity.segmentID } }
        try requireStandby(entry?.state == (finalised ? .finalised : .incomplete), "Native timing failure did not persist a truthful catalogue state")
        try queue.sync { try catalogue.close() }
        try requireStandby(catalogue.cleanupComplete, "Actual catalogue cleanup incomplete")
        let export = root.appendingPathComponent(basename + (hasVideo ? ".mov" : ".m4a"))
        try FileManager.default.copyItem(at: file, to: export)
        return export
    }
}
