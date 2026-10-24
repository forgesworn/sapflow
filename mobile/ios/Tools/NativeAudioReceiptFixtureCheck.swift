// Generated PCM -> actual native AAC -> verbatim real SDK receipts only.
// No capture/session, files, native decoder or presentation acceptance.
import Foundation
import AVFoundation
import CryptoKit
import Sapflow

private func requireReceipts(_ value: @autoclosure () -> Bool, _ message: String) throws {
    guard value() else { throw CaptureFailure(message: message) }
}

@main
enum NativeAudioReceiptFixtureCheck {
    private final class Probe {
        let full: Bool
        let codec: MediaCodec
        let muxer: NativeRecordingMuxer?
        weak var encoder: NativeAudioEncoder?
        var failure: Error?
        var events: [String] = []
        var perGeneration: [UInt64: UInt64] = [:]
        var submitted: [UInt64: (raw: UInt64, zero: UInt64)] = [:]
        var receipts: [AudioRecordingTiming.GenerationExtent] = []
        var ledger = AudioRecordingTiming.Ledger()
        var file: AudioRecordingTiming.FileExtent?
        var legacy: AudioRecordingTiming.Extent?
        var began: UInt32?
        var units: UInt64 = 0
        var failedEvents = 0
        var initials = 0, fragments = 0, finals = 0
        var latestSealedUnits: UInt64 = 0
        var latestSealedEOF = false
        var packetHashes: [String] = []
        init(full: Bool) throws {
            self.full = full
            codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
            muxer = full ? try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true,
                mediaEpochNs: 0, maxFragmentBytes: 1_048_576, maxFragmentMs: 100) : nil
        }
        func append(_ event: String) throws {
            try requireReceipts(events.count < 1024, "Event retention bound")
            events.append(event)
        }
        func output(_ unit: CompressedUnit) {
            guard failure == nil else { return }
            do {
                guard let generation = encoder?.lastOutputGeneration else { throw CaptureFailure(message: "Missing actual output generation") }
                try requireReceipts(began != nil && file == nil && legacy == nil && failedEvents == 0 &&
                    generation == UInt64(receipts.count) + 1, "AAC outside began/generation/EOF interval")
                try requireReceipts(units < 512 && !unit.isVideo && !unit.keyframe &&
                    CMTimeCompare(unit.time, CMTime(value: Int64(units * 1024), timescale: 48_000)) == 0 &&
                    CMTimeCompare(unit.duration, CMTime(value: 1024, timescale: 48_000)) == 0 &&
                    CMTimeCompare(CMSampleBufferGetPresentationTimeStamp(unit.buffer), unit.time) == 0 &&
                    CMTimeCompare(CMSampleBufferGetDuration(unit.buffer), unit.duration) == 0 &&
                    CMSampleBufferGetNumSamples(unit.buffer) == 1, "AAC order/cadence/topology changed")
                guard case let .aac(asc) = unit.configuration, asc == Data([0x11, 0x88]),
                      let block = CMSampleBufferGetDataBuffer(unit.buffer) else { throw CaptureFailure(message: "Unexpected AAC configuration") }
                var contiguous = 0, total = 0
                var pointer: UnsafeMutablePointer<Int8>?
                let status = CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &contiguous,
                    totalLengthOut: &total, dataPointerOut: &pointer)
                guard status == noErr, let pointer, contiguous == total, (1...65_536).contains(total) else { throw CaptureFailure(message: "AAC payload shape") }
                let bytes = Data(bytesNoCopy: pointer, count: total, deallocator: .none)
                let sample = try codec.sample(bytes: bytes, pts: Int64(units * 1024), dts: Int64(units * 1024),
                    duration: 1024, timescale: 48_000, keyframe: false) // Sole actual SDK sample construction.
                try muxer?.push(sample: sample)
                try append("au:\(generation):\(units)")
                units += 1; perGeneration[generation, default: 0] += 1
                packetHashes.append(SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined())
                try drain()
            } catch { failure = error }
        }
        func timing(_ event: AudioRecordingTiming.Event) {
            guard failure == nil else { return }
            do {
                switch event {
                case let .began(leading):
                    try requireReceipts(began == nil && units == 0 && events.isEmpty, "Duplicate/late began")
                    began = leading; try append("began")
                    try muxer?.beginNativeAudio(firstLeadingFrames: leading)
                case let .generationEnded(value):
                    try requireReceipts(failedEvents == 0 && file == nil && legacy == nil, "Generation EOF after terminal event")
                    let accepted = submitted[value.generation] ?? (raw: 0, zero: 0)
                    try requireReceipts(value.rawFrames == accepted.raw && value.zeroFrames == accepted.zero &&
                        value.inputFrames == accepted.raw + accepted.zero && value.encodedFrames == perGeneration[value.generation, default: 0] * 1024,
                        "Actual generation input/AU accounting differs from submissions")
                    try ledger.append(value)
                    try requireReceipts(ledger.extent == encoder?.generationLedger.completed, "Delivered EOF differs from actual stored native ledger")
                    try muxer?.endNativeAudioGeneration(receipt: NativeAudioGenerationReceipt(generation: value.generation,
                        inputFrames: value.inputFrames, rawFrames: value.rawFrames, zeroFrames: value.zeroFrames,
                        inputRate: value.inputRate, encodedFrames: value.encodedFrames, leadingFrames: value.leadingFrames,
                        trailingFrames: value.trailingFrames, sourceMapping: value.sourceMapping == .floor ? .floor : .nearest))
                    receipts.append(value); try append("gen:\(value.generation)")
                case let .endedGenerations(value):
                    try requireReceipts(failedEvents == 0 && file == nil && legacy == nil && ledger.extent == value,
                        "File EOF does not equal delivered generation ledger")
                    try muxer?.completeNativeAudio(receipt: NativeAudioFileReceipt(generationCount: value.generationCount,
                        sourceFramesAt48k: value.sourceFramesAt48k, intentionalStandbyFrames: value.intentionalStandbyFrames,
                        encodedFrames: value.encodedFrames, firstLeadingFrames: value.leadingFrames, lastTrailingFrames: value.trailingFrames))
                    file = value; try append("file:\(value.generationCount)")
                case let .ended(value):
                    try requireReceipts(!full && receipts.isEmpty && file == nil && legacy == nil && failedEvents == 0,
                        "Full mode emitted legacy or duplicate EOF")
                    legacy = value; try append("legacy-ended")
                case .failed:
                    failedEvents += 1
                    try requireReceipts(failedEvents == 1 && file == nil && legacy == nil, "Duplicate failure or failure after accepted final EOF")
                    try append("failed")
                }
                try drain()
            } catch { failure = error }
        }
        func drain() throws {
            guard let muxer else { return }
            for _ in 0..<1024 {
                guard let chunk = try muxer.takeNativeOutput() else { return }
                try requireReceipts(!chunk.bytes.isEmpty && chunk.bytes.count <= 1_179_648, "SDK output bound")
                switch chunk.kind {
                case .initial: initials += 1; try requireReceipts(initials == 1 && chunk.checkpoint == nil, "SDK INITIAL identity")
                case .fragment:
                    fragments += 1; guard let token = chunk.checkpoint else { throw CaptureFailure(message: "SDK fragment has no actual token") }
                    let facts = token.facts()
                    try requireReceipts(facts.audioUnits > latestSealedUnits && facts.audioUnits <= units && facts.videoUnits == 0,
                        "SDK sealed coverage exceeds actual native output")
                    latestSealedUnits = facts.audioUnits; latestSealedEOF = facts.completeAudioEof
                case .finalHeader: finals += 1; try requireReceipts(finals == 1 && chunk.checkpoint == nil, "SDK final metadata identity")
                }
            }
            throw CaptureFailure(message: "SDK output drain exceeded bound")
        }
        func check() throws { if let failure { throw failure } }
        func acceptedInput(_ token: NativeAudioEncoder.InputToken, frames: Int) {
            var value = submitted[token.generation] ?? (raw: 0, zero: 0)
            if token.source == .raw { value.raw += UInt64(frames) } else { value.zero += UInt64(frames) }
            submitted[token.generation] = value
        }
        func finishSDK() throws {
            try check()
            guard let muxer else { return }
            try requireReceipts(file != nil, "No real file receipt for SDK complete")
            try muxer.finishNative(outcome: .complete); try drain()
            try requireReceipts(initials == 1 && fragments > 0 && finals == 1 && latestSealedUnits == units && latestSealedEOF,
                "SDK final sealed facts do not cover actual native EOF")
        }
        func summary(_ name: String, _ encoder: NativeAudioEncoder) -> [String: Any] {
            ["name": name, "passed": true, "fullMode": full, "units": units, "events": events,
             "generationReceipts": receipts.map { ["generation": $0.generation, "inputFrames": $0.inputFrames,
                "rawFrames": $0.rawFrames, "zeroFrames": $0.zeroFrames, "inputRate": UInt64($0.inputRate),
                "encodedFrames": $0.encodedFrames, "leading": UInt64($0.leadingFrames), "trailing": UInt64($0.trailingFrames)] },
             "packetHashes": packetHashes, "file": file.map { String(describing: $0) } ?? "absent",
             "snapshot": String(describing: encoder.generationLedger), "failedEvents": failedEvents,
             "sdkFragments": fragments, "sdkSealedUnits": latestSealedUnits, "sdkSealedEOF": latestSealedEOF]
        }
    }
    private static func make(full: Bool, useDefault: Bool = false) throws -> (NativeAudioEncoder, Probe) {
        let probe = try Probe(full: full)
        let encoder: NativeAudioEncoder
        if useDefault {
            encoder = try NativeAudioEncoder(output: probe.output, meter: { _ in }, recordingTiming: probe.timing)
        } else {
            encoder = try NativeAudioEncoder(receiptMode: full ? .fullGenerationLedger : .legacy,
                output: probe.output, meter: { _ in }, recordingTiming: probe.timing)
        }
        probe.encoder = encoder
        return (encoder, probe)
    }
    private static func format(_ rate: Int, _ channels: UInt32) throws -> AVAudioFormat {
        guard let value = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(rate), channels: channels, interleaved: true) else {
            throw CaptureFailure(message: "Generated PCM format")
        }; return value
    }
    private static func pcm(_ format: AVAudioFormat, time: CMTime, frames: Int) throws -> CMSampleBuffer {
        try requireReceipts((1...480).contains(frames), "Generated PCM bound")
        let channels = Int(format.channelCount)
        // Deterministic, nonperiodic synthetic waveform. No input file or device.
        var words = [Int16](repeating: 0, count: frames * channels)
        for n in 0..<frames {
            let tick = UInt64(n) &+ UInt64(max(0, time.value))
            let value = Int16(Int((tick &* 1664525 &+ 1013904223) % 20001) - 10000)
            for channel in 0..<channels { words[n * channels + channel] = value }
        }
        let bytes = words.count * MemoryLayout<Int16>.size
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: bytes,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0, dataLength: bytes,
            flags: 0, blockBufferOut: &block) == noErr, let block else { throw CaptureFailure(message: "Generated PCM storage") }
        let copied = words.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: bytes) }
        try requireReceipts(copied == noErr, "Generated PCM copy")
        var description: CMAudioFormatDescription?
        guard CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: format.streamDescription,
            layoutSize: 0, layout: nil, magicCookieSize: 0, magicCookie: nil, extensions: nil,
            formatDescriptionOut: &description) == noErr, let description else { throw CaptureFailure(message: "Generated PCM description") }
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: Int32(format.sampleRate)), presentationTimeStamp: time, decodeTimeStamp: time)
        var sampleSize = channels * MemoryLayout<Int16>.size
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: description,
            sampleCount: frames, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize, sampleBufferOut: &sample) == noErr, let sample else { throw CaptureFailure(message: "Generated PCM sample") }
        return sample
    }
    private static func feed(_ encoder: NativeAudioEncoder, _ probe: Probe, token: NativeAudioEncoder.InputToken,
                             format: AVAudioFormat, start: CMTime, frames: Int) throws {
        try requireReceipts(frames > 0 && frames <= 48_000, "Raw fixture extent bound")
        for at in stride(from: 0, to: frames, by: 480) {
            let count = min(480, frames - at), time = CMTimeAdd(start, CMTime(value: Int64(at), timescale: Int32(format.sampleRate)))
            try encoder.encode(pcm(format, time: time, frames: count), time: time, token: token)
            probe.acceptedInput(token, frames: count); try probe.check()
        }
    }
    private static func zeros(_ encoder: NativeAudioEncoder, _ probe: Probe, token: NativeAudioEncoder.InputToken, frames: UInt64) throws {
        try requireReceipts(frames <= 48_000, "Zero fixture extent bound")
        var remaining = frames
        while remaining > 0 {
            let count = UInt32(min(480, remaining)); try encoder.encodeSilence(frames: count, token: token)
            probe.acceptedInput(token, frames: Int(count)); try probe.check(); remaining -= UInt64(count)
        }
    }
    private static func finish(_ encoder: NativeAudioEncoder, _ probe: Probe, generations: Int) throws {
        let retiredToken = encoder.inputToken
        try encoder.finish(); try probe.check()
        try requireReceipts(probe.receipts.count == generations && probe.file?.generationCount == UInt64(generations) &&
            probe.file == encoder.generationLedger.completed && probe.legacy == nil, "Full receipt final ledger identity")
        let count = probe.events.count, units = probe.units
        try encoder.finish(); try probe.check()
        try requireReceipts(probe.events.count == count && probe.units == units, "Repeated finish changed EOF/output")
        if let retiredToken {
            let source = try format(48_000, 1)
            try rejectStale(encoder, probe, token: retiredToken, format: source, time: .zero)
        }
        try probe.finishSDK()
    }
    private static func rejectStale(_ encoder: NativeAudioEncoder, _ probe: Probe, token: NativeAudioEncoder.InputToken,
                                    format: AVAudioFormat, time: CMTime) throws {
        let events = probe.events.count, units = probe.units, before = String(describing: encoder.generationLedger)
        var rejected = false
        do {
            if token.source == .zero { try encoder.encodeSilence(frames: 17, token: token) }
            else { try encoder.encode(pcm(format, time: time, frames: 17), time: time, token: token) }
        } catch AudioRecordingTiming.Failure.stale { rejected = true }
        try requireReceipts(rejected && probe.events.count == events && probe.units == units &&
            String(describing: encoder.generationLedger) == before, "Stale input mutated current native owner")
    }
    private static func raw(full: Bool, rate: Int, channels: UInt32, useDefault: Bool = false) throws -> [String: Any] {
        let (encoder, probe) = try make(full: full, useDefault: useDefault); defer { try? probe.muxer?.cancel() }
        let source = try format(rate, channels), token = encoder.inputToken!
        try feed(encoder, probe, token: token, format: source, start: .zero, frames: rate == 48_000 ? 16_137 : 14_837)
        if full {
            try finish(encoder, probe, generations: 1)
            try requireReceipts(probe.receipts[0].inputRate == UInt32(rate) &&
                probe.receipts[0].sourceMapping == (rate == 48_000 ? .nearest : .floor), "Actual source rate/mapping lost")
        } else {
            try encoder.finish(); try probe.check()
            try requireReceipts(probe.events.first == "began" && probe.events.last == "legacy-ended" &&
                probe.receipts.isEmpty && probe.file == nil && probe.legacy != nil, "Default legacy callbacks changed")
            let events = probe.events.count; try encoder.finish(); try requireReceipts(probe.events.count == events, "Legacy repeated EOF")
        }
        var summary = probe.summary(full ? "raw-\(rate)" : "default-legacy-raw", encoder)
        summary["sourceFormat"] = NativeAudioEncoder.formatFacts(source)
        return summary
    }
    private static func initialZero(resume: Bool, full: Bool = true) throws -> [String: Any] {
        let (encoder, probe) = try make(full: full, useDefault: !full); defer { try? probe.muxer?.cancel() }
        let source = try format(48_000, 1), original = encoder.inputToken!
        let zero = try encoder.beginStandby(format: source, at: .zero)
        try rejectStale(encoder, probe, token: original, format: source, time: .zero)
        try zeros(encoder, probe, token: zero, frames: 20_017)
        if resume {
            let time = CMTime(value: 20_017, timescale: 48_000), raw = try encoder.resumeInput(from: zero, at: time)
            try requireReceipts(raw.generation == zero.generation && raw.revision != zero.revision, "Resume recreated generation")
            try rejectStale(encoder, probe, token: zero, format: source, time: time)
            try feed(encoder, probe, token: raw, format: source, start: time, frames: 16_039)
        }
        if full {
            try finish(encoder, probe, generations: 1)
            guard let receipt = probe.receipts.first else { throw CaptureFailure(message: "Missing mixed-source receipt") }
            try requireReceipts(receipt.zeroFrames == 20_017 && receipt.rawFrames == (resume ? 16_039 : 0), "Initial raw/zero receipt lost provenance")
        } else {
            try encoder.finish(); try probe.check()
            try requireReceipts(probe.receipts.isEmpty && probe.file == nil && probe.legacy != nil, "Legacy initial standby callback shape changed")
        }
        return probe.summary(!full ? "default-legacy-initial-resume" : resume ? "initial-zero-resume-same-generation" : "initial-zero-only", encoder)
    }
    private static func transitions(full: Bool = true) throws -> [String: Any] {
        let (encoder, probe) = try make(full: full, useDefault: !full); defer { try? probe.muxer?.cancel() }
        let source = try format(48_000, 1), original = encoder.inputToken!
        try feed(encoder, probe, token: original, format: source, start: .zero, frames: 16_137)
        let zero = try encoder.beginStandby(from: original)
        let target: UInt64 = 32_154, plan = try encoder.standbyPlan(validFramesThrough: target, token: zero)
        try requireReceipts(plan.attainableValidFrames == target, "Unexpected 48k target quantization")
        try zeros(encoder, probe, token: zero, frames: plan.additionalSourceFrames)
        let time = CMTime(value: Int64(target), timescale: 48_000), raw = try encoder.resumeInput(from: zero, at: time)
        try rejectStale(encoder, probe, token: original, format: source, time: time)
        try rejectStale(encoder, probe, token: zero, format: source, time: time)
        try feed(encoder, probe, token: raw, format: source, start: time, frames: 16_083)
        let last = try encoder.beginStandby(from: raw)
        let lastTarget: UInt64 = 64_256, lastPlan = try encoder.standbyPlan(validFramesThrough: lastTarget, token: last)
        try requireReceipts(lastPlan.attainableValidFrames == lastTarget, "Unexpected final target quantization")
        try zeros(encoder, probe, token: last, frames: lastPlan.additionalSourceFrames)
        try finish(encoder, probe, generations: 3)
        try requireReceipts(probe.receipts[1].rawFrames == 16_083 && probe.receipts[1].zeroFrames == plan.additionalSourceFrames,
            "Multi-generation mixed raw/zero facts changed")
        return probe.summary(full ? "three-generations" : "default-legacy-three-generations", encoder)
    }
    private static func empty(initial: Bool, trailing: Bool) throws -> [String: Any] {
        let (encoder, probe) = try make(full: true); defer { try? probe.muxer?.cancel() }
        let source = try format(48_000, 1)
        if trailing {
            try feed(encoder, probe, token: encoder.inputToken!, format: source, start: .zero, frames: 16_137)
            _ = try encoder.beginStandby(from: encoder.inputToken!)
            try finish(encoder, probe, generations: 1)
        } else {
            if initial { _ = try encoder.beginStandby(format: source, at: .zero) }
            try encoder.finish(); try probe.check()
            try requireReceipts(probe.units == 0 && probe.receipts.isEmpty && probe.file == nil && probe.failedEvents == 0,
                "Empty input fabricated native generation/file EOF")
            let count = probe.events.count; try encoder.finish(); try requireReceipts(count == probe.events.count, "Empty repeated finish changed events")
        }
        return probe.summary(trailing ? "empty-trailing-converter" : initial ? "empty-initial-zero" : "empty-uncreated", encoder)
    }
    private static func failure(prefix: Bool = false) throws -> [String: Any] {
        let (encoder, probe) = try make(full: true); defer { try? probe.muxer?.cancel() }
        let source = try format(48_000, 1), token = encoder.inputToken!
        try feed(encoder, probe, token: token, format: source, start: .zero, frames: 16_137)
        var current = token
        if prefix {
            let zero = try encoder.beginStandby(from: token)
            try zeros(encoder, probe, token: zero, frames: 20_017)
            guard let time = encoder.nextInputTime else { throw CaptureFailure(message: "Missing actual next input time") }
            current = try encoder.resumeInput(from: zero, at: time)
        }
        try requireReceipts(probe.units > 0 && probe.began != nil, "Failure control never produced native AAC")
        let priorReceipts = probe.receipts.count
        let wrong = CMTimeAdd(encoder.nextInputTime!, CMTime(value: 1, timescale: 1))
        var refused = false
        do { try encoder.encode(pcm(source, time: wrong, frames: 17), time: wrong, token: current) } catch { refused = true }
        try requireReceipts(refused, "Discontinuous actual PCM accepted")
        let count = probe.events.count, units = probe.units
        for _ in 0..<2 {
            var failed = false; do { try encoder.finish() } catch { failed = true }
            try requireReceipts(failed, "Sticky native-owner failure lost at EOF")
        }
        try probe.check()
        try requireReceipts(probe.failedEvents == 1 && probe.receipts.count == priorReceipts && priorReceipts == (prefix ? 1 : 0) && probe.file == nil &&
            probe.events.count == count && probe.units == units, "Failed owner fabricated EOF or duplicate failure")
        var summary = probe.summary(prefix ? "sticky-time-failure-keeps-prefix" : "sticky-time-failure-no-EOF", encoder)
        summary["failureStage"] = "actual input timestamp guard before native EOF; not a native converter EOF fault"
        return summary
    }
    private static func targetFailure() throws -> [String: Any] {
        let (encoder, probe) = try make(full: true); defer { try? probe.muxer?.cancel() }
        let source = try format(48_000, 1), zero = try encoder.beginStandby(format: source, at: .zero)
        try zeros(encoder, probe, token: zero, frames: 20_017)
        try requireReceipts(probe.units > 0, "Target failure control has no native output")
        var refused = false
        do { try encoder.finishStandbyThrough(validFrames: 1, token: zero) } catch { refused = true }
        let count = probe.events.count, units = probe.units
        for _ in 0..<2 {
            var failed = false; do { try encoder.finish() } catch { failed = true }
            try requireReceipts(failed, "Invalid target failed owner became complete")
        }
        try probe.check()
        try requireReceipts(refused && probe.failedEvents == 1 && probe.file == nil && probe.receipts.isEmpty &&
            count == probe.events.count && units == probe.units, "Invalid target invented successful EOF")
        var result = probe.summary("invalid-target-no-EOF", encoder)
        result["failureStage"] = "actual standby target guard before native EOF; not a converter EOF fault"
        return result
    }
    static func main() {
        guard CommandLine.arguments.count == 1 else { exit(2) }
        var rows: [[String: Any]] = []
        let cases: [(String, () throws -> [String: Any])] = [
            ("raw-48000", { try raw(full: true, rate: 48_000, channels: 1) }),
            ("raw-44100", { try raw(full: true, rate: 44_100, channels: 2) }),
            ("initial-zero-resume-same-generation", { try initialZero(resume: true) }),
            ("initial-zero-only", { try initialZero(resume: false) }),
            ("three-generations", { try transitions() }),
            ("empty-uncreated", { try empty(initial: false, trailing: false) }),
            ("empty-initial-zero", { try empty(initial: true, trailing: false) }),
            ("empty-trailing-converter", { try empty(initial: false, trailing: true) }),
            ("sticky-time-failure-no-EOF", { try failure() }),
            ("sticky-time-failure-keeps-prefix", { try failure(prefix: true) }),
            ("invalid-target-no-EOF", { try targetFailure() }),
            ("default-legacy-raw", { try raw(full: false, rate: 48_000, channels: 1, useDefault: true) }),
            ("default-legacy-initial-resume", { try initialZero(resume: true, full: false) }),
            ("default-legacy-three-generations", { try transitions(full: false) })
        ]
        for (name, body) in cases {
            do { rows.append(try body()) }
            catch { rows.append(["name": name, "passed": false, "error": String(describing: error)]) }
        }
        let passed = rows.count == 14 && rows.allSatisfy { $0["passed"] as? Bool == true }
        let result: [String: Any] = ["passed": passed, "cases": rows, "generatedOnly": true,
            "scope": "Actual native AAC receipt delivery and real current SDK acceptance only; no native decode/presentation, capture, storage durability, app adoption or physical proof",
            "nativeConverterEOFFaultInjection": false]
        do { FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])); FileHandle.standardOutput.write(Data([10])) }
        catch { exit(2) }
        exit(passed ? 0 : 1)
    }
}
