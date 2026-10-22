// Generated-only macOS proof. check-audio-only.sh creates the sole input.
import Foundation
import AVFoundation
import CryptoKit

private func requireAudioFixture(_ value: @autoclosure () -> Bool, _ message: String) throws {
    if !value() { throw CaptureFailure(message: message) }
}

@main
enum AudioOnlyFixtureCheck {
    static func main() async {
        do { try await perform() }
        catch {
            FileHandle.standardError.write(Data(("Generated audio-only check failed: \(error)\n").utf8))
            exit(1)
        }
    }
    static func perform() async throws {
        try requireAudioFixture(CommandLine.arguments.count == 2, "Use check-audio-only.sh with no media arguments")
        let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        try requireAudioFixture(root == root.resolvingSymlinksInPath(), "Owned scratch cannot be a symlink")
        let marker = root.appendingPathComponent(".sapflow-audio-only-fixture")
        let markerText = try String(contentsOf: marker, encoding: .utf8)
        try requireAudioFixture(markerText == "generated-lavfi-audio-only-v1\n", "Missing generated fixture ownership")
        let input = root.appendingPathComponent("generated.wav")
        let info = try input.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
        try requireAudioFixture(info.isRegularFile == true && info.isSymbolicLink != true && (info.fileSize ?? 0) > 0 && (info.fileSize ?? Int.max) < 512 * 1024, "Generated PCM input bound")
        let asset = AVURLAsset(url: input)
        let duration = try await asset.load(.duration)
        let video = try await asset.loadTracks(withMediaType: .video)
        let audio = try await asset.loadTracks(withMediaType: .audio)
        try requireAudioFixture(video.isEmpty && audio.count == 1 && duration.seconds > 0 && duration.seconds <= 3.01, "Expected at most three seconds of generated audio only")
        var summary = try run(root: root, asset: asset, track: audio[0])
        guard let rows = summary["cases"] as? [[String: Any]] else { throw CaptureFailure(message: "Generated cases unavailable") }
        var checked: [[String: Any]] = []
        for var row in rows {
            if let name = row["export"] as? String {
                let recorded = AVURLAsset(url: root.appendingPathComponent("exports").appendingPathComponent(name))
                let audios = try await recorded.loadTracks(withMediaType: .audio)
                let videos = try await recorded.loadTracks(withMediaType: .video)
                try requireAudioFixture(audios.count == 1 && videos.isEmpty, "Native reader found unexpected recording topology")
                row["nativeReader"] = try readEncodedPackets(asset: recorded, track: audios[0])
            }
            checked.append(row)
        }
        summary["cases"] = checked
        FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: summary, options: [.sortedKeys, .prettyPrinted]))
        FileHandle.standardOutput.write(Data([10]))
    }
    static func readEncodedPackets(asset: AVAsset, track: AVAssetTrack) throws -> [String: Any] {
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: nil)
        output.alwaysCopiesSampleData = false; reader.add(output)
        try requireAudioFixture(reader.startReading(), "Native recording reader start")
        defer { if reader.status == .reading { reader.cancelReading() } }
        var packets: [[String: Any]] = [], empty = 0
        var firstPts: Int64?, lastPts: Int64?
        let began = ProcessInfo.processInfo.systemUptime
        while let batch = output.copyNextSampleBuffer() {
            try requireAudioFixture(ProcessInfo.processInfo.systemUptime - began < 5, "Native recording reader deadline")
            let count = CMSampleBufferGetNumSamples(batch)
            if count == 0 { empty += 1; try requireAudioFixture(empty <= 16, "Native reader marker bound"); continue }
            try requireAudioFixture(count > 0 && count <= 176, "Native reader compressed batch bound")
            for index in 0..<count {
                var sample: CMSampleBuffer?
                let status = CMSampleBufferCopySampleBufferForRange(allocator: kCFAllocatorDefault, sampleBuffer: batch,
                    sampleRange: CFRange(location: index, length: 1), sampleBufferOut: &sample)
                guard status == noErr, let sample else { throw CaptureFailure(message: "Native reader AAC split failed") }
                let pts = CMTimeConvertScale(CMSampleBufferGetPresentationTimeStamp(sample), timescale: 48_000, method: .roundHalfAwayFromZero).value
                if firstPts == nil { firstPts = pts }; lastPts = pts
                packets.append(try payloadHash(sample))
                try requireAudioFixture(packets.count <= 176, "Native reader packet bound")
            }
        }
        try requireAudioFixture(reader.status == .completed && !packets.isEmpty, "Native reader incomplete")
        return ["packetHashes": packets, "emptyMarkers": empty, "firstPts48k": firstPts ?? -1, "lastPts48k": lastPts ?? -1]
    }
    static func payloadHash(_ sample: CMSampleBuffer) throws -> [String: Any] {
        guard let block = CMSampleBufferGetDataBuffer(sample) else { throw CaptureFailure(message: "Encoded hash buffer unavailable") }
        var contiguous = 0, total = 0
        var pointer: UnsafeMutablePointer<Int8>?
        let status = CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &contiguous, totalLengthOut: &total, dataPointerOut: &pointer)
        guard status == noErr, contiguous == total, let pointer, total > 0, total <= 64 * 1024 else { throw CaptureFailure(message: "Encoded hash payload unavailable") }
        let bytes = Data(bytesNoCopy: pointer, count: total, deallocator: .none)
        return ["size": total, "sha256": SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()]
    }
    static func run(root: URL, asset: AVAsset, track: AVAssetTrack) throws -> [String: Any] {
        let reader = try AVAssetReader(asset: asset)
        // Request a deterministic native PCM representation, rather than
        // inheriting a file container's implicit decoding/output defaults.
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false])
        output.alwaysCopiesSampleData = false
        reader.add(output)
        try requireAudioFixture(reader.startReading(), "Generated PCM reader start")
        defer { if reader.status == .reading { reader.cancelReading() } }
        let ingress = RustEncodedIngress()
        ingress.reset(hasVideo: false) // Actual packaged Rust descriptor validation; no destination/socket.
        var units: [CompressedUnit] = []
        var compressedBytes = 0
        var encodeFailure: Error?
        var meters = 0
        let encoder = try NativeAudioEncoder(output: { unit in
            do {
                try requireAudioFixture(!unit.isVideo && !unit.keyframe, "Unexpected video/IDR in audio-only output")
                guard case let .aac(asc) = unit.configuration, asc == Data([0x11, 0x88]) else {
                    throw CaptureFailure(message: "Expected AAC-LC 48 kHz mono configuration")
                }
                try requireAudioFixture(unit.duration == CMTime(value: 1024, timescale: 48_000), "Expected 1024-frame AAC units")
                try ingress.accept(unit)
                units.append(unit); compressedBytes += unit.byteCount
                try requireAudioFixture(units.count <= 160 && compressedBytes <= 128 * 1024, "Encoded fixture bound")
            } catch { if encodeFailure == nil { encodeFailure = error } }
        }, meter: { _ in meters += 1 })
        var inputFrames = 0
        var callbacks = 0
        var emptyMarkers = 0
        var firstTime: CMTime?
        let began = ProcessInfo.processInfo.systemUptime
        while let batch = output.copyNextSampleBuffer() {
            try requireAudioFixture(ProcessInfo.processInfo.systemUptime - began < 10, "Native AAC encoding deadline")
            let count = CMSampleBufferGetNumSamples(batch)
            if count == 0 {
                emptyMarkers += 1
                try requireAudioFixture(emptyMarkers <= 16 && CMSampleBufferGetTotalSampleSize(batch) == 0, "Generated empty reader marker bound")
                continue
            }
            try requireAudioFixture(count > 0 && count <= 144_000, "Generated PCM batch bound: \(count)")
            guard let format = CMSampleBufferGetFormatDescription(batch), let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format) else {
                throw CaptureFailure(message: "PCM format unavailable")
            }
            let pcmFormat = asbd.pointee
            try requireAudioFixture(pcmFormat.mFormatID == kAudioFormatLinearPCM && pcmFormat.mSampleRate == 48_000 && pcmFormat.mChannelsPerFrame == 1 &&
                pcmFormat.mBitsPerChannel == 16 && pcmFormat.mFramesPerPacket == 1 && pcmFormat.mBytesPerFrame == 2 && pcmFormat.mBytesPerPacket == 2 &&
                pcmFormat.mFormatFlags == (kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked), "Expected explicit generated signed-16 packed/interleaved 48 kHz mono PCM")
            // Match bounded microphone-sized input rather than passing an
            // entire file batch into the native converter in one callback.
            for offset in stride(from: 0, to: count, by: 1024) {
                var sample: CMSampleBuffer?
                let status = CMSampleBufferCopySampleBufferForRange(allocator: kCFAllocatorDefault, sampleBuffer: batch,
                    sampleRange: CFRange(location: offset, length: min(1024, count - offset)), sampleBufferOut: &sample)
                guard status == noErr, let sample else { throw CaptureFailure(message: "Generated PCM slicing failed") }
                let pts = CMSampleBufferGetPresentationTimeStamp(sample)
                if firstTime == nil { firstTime = pts }
                try encoder.encode(sample, time: CMTimeSubtract(pts, firstTime!))
                if let encodeFailure { throw encodeFailure }
                callbacks += 1; inputFrames += CMSampleBufferGetNumSamples(sample)
                try requireAudioFixture(callbacks <= 160 && inputFrames <= 144_000, "PCM callback/frame bound")
            }
        }
        try requireAudioFixture(reader.status == .completed && inputFrames == 144_000, "Generated PCM extraction incomplete")
        try encoder.finish()
        if let encodeFailure { throw encodeFailure }
        try requireAudioFixture(units.count >= 125 && ingress.accepted == UInt64(units.count) && meters == callbacks, "Actual native AAC/SDK output missing")
        for (index, unit) in units.enumerated() {
            try requireAudioFixture(unit.time == CMTime(value: Int64(index * 1024), timescale: 48_000), "AAC timestamp cadence")
        }
        let queue = DispatchQueue(label: "generated.audio-only.recording")
        let catalogue = try queue.sync { try RecordingCatalogue(root: root.appendingPathComponent("recordings", isDirectory: true)) }
        let exports = root.appendingPathComponent("exports", isDirectory: true)
        try FileManager.default.createDirectory(at: exports, withIntermediateDirectories: false)
        var results: [[String: Any]] = []
        for name in ["finalised", "prior_loss", "recording_only_stop", "empty"] {
            let show = catalogue.newShow(profile: .audioOnly48kMono)
            let receipt = try queue.sync { try catalogue.begin(identity: show.nextIdentity()) }
            let recorder = try queue.sync { try CompressedRecorder(receipt: receipt, queue: queue, hasVideo: false, onFailure: { _ in }) }
            let selected = name == "empty" ? [] : units.filter { name == "finalised" || $0.time.seconds < 1.25 }
            let start = DispatchTime.now().uptimeNanoseconds
            for unit in selected {
                let target = start + UInt64(unit.time.seconds * 1_000_000_000)
                let now = DispatchTime.now().uptimeNanoseconds
                if target > now { Thread.sleep(forTimeInterval: Double(target - now) / 1_000_000_000) }
                recorder.offer(unit) // The very same native AAC output validated above, never re-encoded.
            }
            queue.sync {}
            if name == "prior_loss" { recorder.markIncomplete(CaptureFailure(message: "Generated prior encoded loss")) }
            let hold = DispatchSemaphore(value: 0)
            queue.async { _ = hold.wait(timeout: .now() + 5) }
            let listeners = DispatchGroup(), lock = NSLock()
            var completions: [CompressedRecorder.Completion] = []
            for _ in 0..<2 {
                listeners.enter()
                recorder.finish(incomplete: name == "recording_only_stop") { result in
                    lock.lock(); completions.append(result); lock.unlock(); listeners.leave()
                }
            }
            hold.signal()
            try requireAudioFixture(listeners.wait(timeout: .now() + 10) == .success, "Actual recorder closure deadline")
            let finalised = name == "finalised"
            try requireAudioFixture(completions.count == 2 && completions.allSatisfy { $0.cleanupComplete && $0.finalised == finalised } && receipt.cleanupComplete, "Finish listeners did not join truthful actual closure")
            let repeatDone = DispatchSemaphore(value: 0)
            recorder.finish { value in if value.cleanupComplete && value.finalised == finalised { repeatDone.signal() } }
            try requireAudioFixture(repeatDone.wait(timeout: .now() + 3) == .success, "Repeated finish changed result")
            guard let entry = try queue.sync(execute: { try catalogue.scan().first { $0.segmentID == receipt.identity.segmentID } }) else {
                throw CaptureFailure(message: "Recording receipt missing")
            }
            if name == "empty" {
                try requireAudioFixture(entry.bytes == 0 && entry.state == .orphanUnverified && !FileManager.default.fileExists(atPath: receipt.fileURL.path), "Empty encoder run invented media")
                results.append(["case": name, "state": entry.state.rawValue, "bytes": 0, "actualCloseListeners": 2, "cachedRepeatStable": true])
                continue
            }
            try requireAudioFixture(entry.bytes > 0 && entry.state == (finalised ? .finalised : .incomplete), "Sticky recording state mismatch")
            let destination = exports.appendingPathComponent(name + ".mov")
            try requireAudioFixture(FileManager.default.createFile(atPath: destination.path, contents: nil), "Owned export creation")
            let file = try FileHandle(forWritingTo: destination)
            var maximumChunk = 0
            let copied = try queue.sync { () throws -> UInt64 in
                let lease = try catalogue.openExport(entry: entry, allowUnverified: !finalised)
                do {
                    let count = try lease.copy { data in maximumChunk = max(maximumChunk, data.count); try file.write(contentsOf: data) }
                    try file.synchronize(); try file.close(); try lease.close()
                    try requireAudioFixture(lease.cleanupComplete, "Actual export close missing")
                    return count
                } catch { try? file.close(); try? lease.close(); throw error }
            }
            try requireAudioFixture(copied == entry.bytes && maximumChunk <= 65_536, "Export size/chunk bound")
            let hashes = try selected.map { try payloadHash($0.buffer) }
            results.append(["case": name, "state": entry.state.rawValue, "bytes": entry.bytes, "durationMs": entry.durationMs ?? -1,
                            "encodedOutputsUsed": selected.count, "encodedPayloadHashes": hashes, "actualCloseListeners": 2, "cachedRepeatStable": true,
                            "maxExportChunk": maximumChunk, "original": receipt.fileURL.lastPathComponent, "export": destination.lastPathComponent])
        }
        try queue.sync { try catalogue.close() }
        try requireAudioFixture(catalogue.cleanupComplete, "Catalogue actual close missing")
        let summary: [String: Any] = ["generatedPCMFrames": inputFrames, "emptyReaderMarkersSkipped": emptyMarkers, "nativeEncodeCalls": callbacks, "nativeAACUnits": units.count,
            "encodedBytes": compressedBytes, "rustValidatedSameAACUnits": ingress.accepted, "encodePasses": 1, "videoTracks": 0,
            "profile": CaptureIntent.audioOnly48kMono.rawValue, "actualCatalogueClosed": true, "cases": results,
            "scope": "Generated macOS native AAC + recording + SDK validation only; no mic, camera, sockets or device",
            "lowDiskCase": "Explicit production incomplete-finish seam; no actual disk pressure" ]
        return summary
    }
}
