import AVFoundation
import AudioToolbox

/// Confined to one serial samples owner, including transitions and final EOF.
/// Cold snapshots are read on that owner too. Callbacks must not re-enter the
/// encoder; they share/queue immutable compressed output as before.
final class NativeAudioEncoder {
    private var converter: AVAudioConverter?
    private var inputFormat: AVAudioFormat?
    private let outputFormat: AVAudioFormat
    private var firstTime: CMTime?
    private var emittedFrames: Int64 = 0
    private var expectedInputTime: CMTime?
    private var compressedBuffer: AVAudioCompressedBuffer?
    private var compressedDescription: CMAudioFormatDescription?
    private var codecCookie: Data?
    private var codecASBD: AudioStreamBasicDescription?
    private var generationFormatVerified = false
    private(set) var verifiedConfigurationGenerations: UInt64 = 0
    private let output: (CompressedUnit) -> Void
    private let meter: (Float) -> Void
    private let recordingTiming: ((AudioRecordingTiming.Event) -> Void)?
    enum ReceiptMode { case legacy, fullGenerationLedger }
    private let receiptMode: ReceiptMode
    typealias InputToken = AudioRecordingTiming.InputToken
    struct StandbyPlan {
        let additionalSourceFrames: UInt64
        let requestedValidFrames: UInt64
        let attainableValidFrames: UInt64
        let quantizationOutputFrames: Int64
        let nextInputTime: CMTime
        let firstFreshOutputTime: CMTime
        let sourceMapping: AudioRecordingTiming.SourceFrameMapping
        let sourceExtent: AudioRecordingTiming.SourceFrameExtent
    }
    private let admission = AudioRecordingTiming.InputAdmission()
    private var ledger = AudioRecordingTiming.Ledger()
    private var nativeToken: InputToken?
    private var generationInput: UInt64 = 0
    private var generationRaw: UInt64 = 0
    private var generationZero: UInt64 = 0
    private var generationRate: UInt32 = 0
    private var generationCodedStart: UInt64 = 0
    private var generationLeading: UInt32?
    private var generationMapping: AudioRecordingTiming.SourceFrameMapping = .nearest
    private var fileLeading: UInt32?
    private var zeroStorage: AVAudioPCMBuffer?
    private var hasTransitions = false
    private var terminalError: Error?
    private var timingFailed = false
    private var timingFinished = false
    private var closed = false
    private(set) var lastOutputGeneration: UInt64?
    private var lastAttemptedGeneration: AudioRecordingTiming.GenerationExtent?
    var inputToken: InputToken? { admission.token }
    var nextInputTime: CMTime? { expectedInputTime }
    var generationLedger: AudioRecordingTiming.LedgerSnapshot {
        .init(completed: ledger.extent, lastGeneration: ledger.last, lastAttemptedGeneration: lastAttemptedGeneration,
            activeGeneration: nativeToken?.generation,
            activeInputFrames: generationInput, activeRawFrames: generationRaw, activeZeroFrames: generationZero,
            activeInputRate: generationRate, activeEncodedFrames: UInt64(max(0, emittedFrames)) - generationCodedStart,
            nativeLeadingFrames: generationLeading, failed: terminalError != nil, finished: closed)
    }

    init(receiptMode: ReceiptMode = .legacy, output: @escaping (CompressedUnit) -> Void, meter: @escaping (Float) -> Void, recordingTiming: ((AudioRecordingTiming.Event) -> Void)? = nil) throws {
        guard let format = AVAudioFormat(settings: [AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 96_000]) else {
            throw CaptureFailure(message: "Native AAC-LC format is unavailable")
        }
        outputFormat = format; self.output = output; self.meter = meter; self.recordingTiming = recordingTiming
        self.receiptMode = receiptMode
    }

    /// Cold construction diagnostics only. Scalar format facts contain no PCM,
    /// device identity or channel-description payloads and have fixed size.
    static func formatFacts(_ format: AVAudioFormat) -> String {
        let value = format.streamDescription.pointee
        let layout: String
        if let channels = format.channelLayout {
            let raw = channels.layout.pointee
            layout = "tag=\(raw.mChannelLayoutTag),bitmap=\(raw.mChannelBitmap),descriptions=\(raw.mNumberChannelDescriptions),channels=\(channels.channelCount)"
        } else { layout = "absent" }
        return "\(asbdFacts(value)) layout{\(layout)}"
    }
    private static func asbdFacts(_ value: AudioStreamBasicDescription) -> String {
        "ASBD{rate=\(value.mSampleRate),id=\(value.mFormatID),flags=\(value.mFormatFlags),bytesPerPacket=\(value.mBytesPerPacket),framesPerPacket=\(value.mFramesPerPacket),bytesPerFrame=\(value.mBytesPerFrame),channels=\(value.mChannelsPerFrame),bits=\(value.mBitsPerChannel),reserved=\(value.mReserved)}"
    }
    private static func sameASBD(_ a: AudioStreamBasicDescription, _ b: AudioStreamBasicDescription) -> Bool {
        a.mSampleRate == b.mSampleRate && a.mFormatID == b.mFormatID && a.mFormatFlags == b.mFormatFlags &&
        a.mBytesPerPacket == b.mBytesPerPacket && a.mFramesPerPacket == b.mFramesPerPacket &&
        a.mBytesPerFrame == b.mBytesPerFrame && a.mChannelsPerFrame == b.mChannelsPerFrame &&
        a.mBitsPerChannel == b.mBitsPerChannel && a.mReserved == b.mReserved
    }

    /// All native calls share the capture samples owner. The compatibility entry
    /// remains raw-only; entering standby never silently accepts old raw work.
    func encode(_ sample: CMSampleBuffer, time: CMTime) throws {
        guard let token = inputToken, token.source == .raw else { throw AudioRecordingTiming.Failure.stale }
        try encode(sample, time: time, token: token)
    }
    func encode(_ sample: CMSampleBuffer, time: CMTime, token: InputToken) throws {
        guard admission.permits(token, source: .raw) else { throw AudioRecordingTiming.Failure.stale }
        try guarded {
            guard let description = CMSampleBufferGetFormatDescription(sample),
                  let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description),
                  let source = AVAudioFormat(streamDescription: asbd) else { throw CaptureFailure(message: "Microphone PCM format unavailable") }
            try self.validateTime(time)
            if let inputFormat = self.inputFormat, inputFormat != source { throw CaptureFailure(message: "Audio route changed its format; stop and prepare again") }
            if self.converter == nil { try self.create(source, time: time, token: token) }
            guard source.channelCount <= 8 else { throw CaptureFailure(message: "Microphone channel count exceeds the supported bound") }
            let count = CMSampleBufferGetNumSamples(sample)
            guard count > 0, count <= Int(UInt32.max) else { throw CaptureFailure(message: "Microphone PCM frame count is unsupported") }
            let layout = try AudioRecordingTiming.PCMBufferLayout(channels: source.channelCount, interleaved: source.isInterleaved)
            let listBytes = try layout.byteCount(headerBytes: MemoryLayout<AudioBufferList>.size, bufferStride: MemoryLayout<AudioBuffer>.stride)
            let storage = UnsafeMutableRawPointer.allocate(byteCount: listBytes, alignment: 16)
            defer { storage.deallocate() }
            memset(storage, 0, listBytes)
            let list = storage.bindMemory(to: AudioBufferList.self, capacity: 1)
            list.pointee.mNumberBuffers = UInt32(layout.bufferCount)
            var retained: CMBlockBuffer?
            var requiredSize = 0
            let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: &requiredSize,
                bufferListOut: list, bufferListSize: listBytes, blockBufferAllocator: kCFAllocatorDefault,
                blockBufferMemoryAllocator: kCFAllocatorDefault,
                flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, blockBufferOut: &retained)
            guard status == noErr, let pcm = AVAudioPCMBuffer(pcmFormat: source, bufferListNoCopy: UnsafePointer(list), deallocator: nil) else {
                throw CaptureFailure(message: "Microphone PCM storage could not be read (\(status)); providedListBytes=\(listBytes),requiredListBytes=\(requiredSize),frames=\(count),interleaved=\(source.isInterleaved); source \(Self.formatFacts(source))")
            }
            pcm.frameLength = AVAudioFrameCount(count)
            self.updateMeter(pcm)
            try withExtendedLifetime(retained) { try self.consume(pcm, token: token) }
            self.expectedInputTime = CMTimeAdd(time, CMTime(value: Int64(count), timescale: Int32(self.generationRate)))
        }
    }
    /// Entry fences raw input before even checking format support. Failure keeps
    /// the old native owner for Stop and is sticky; there is no raw fallback.
    func beginStandby(from token: InputToken) throws -> InputToken {
        try admission.fence(token)
        return try guarded {
            guard let source = self.inputFormat, self.converter != nil, self.firstTime != nil else { throw AudioRecordingTiming.Failure.unsupported }
            let previousZero = self.zeroStorage
            try self.prepareZero(source)
            self.hasTransitions = true
            try withExtendedLifetime(previousZero) { try self.endGeneration() }
            self.retireConverter()
            let next = try self.admission.installZero(initial: false)
            try self.create(source, time: self.firstTime!, token: next)
            guard let leading = self.fileLeading, let freshLeading = self.generationLeading,
                  self.generationCodedStart + UInt64(freshLeading) >= UInt64(leading) else { throw AudioRecordingTiming.Failure.inconsistent }
            let position = self.generationCodedStart + UInt64(freshLeading) - UInt64(leading)
            guard position <= UInt64(Int64.max) else { throw AudioRecordingTiming.Failure.overflow }
            self.expectedInputTime = CMTimeAdd(self.firstTime!, CMTime(value: Int64(position), timescale: 48_000))
            return next
        }
    }
    /// Initial standby uses observed native FORMAT only, never a microphone
    /// sample. The first converter therefore has no raw history at all.
    func beginStandby(format: AVAudioFormat, at time: CMTime) throws -> InputToken {
        guard let token = inputToken else { throw AudioRecordingTiming.Failure.closed }
        try admission.fence(token)
        return try guarded {
            guard self.converter == nil, self.firstTime == nil, self.emittedFrames == 0 else { throw AudioRecordingTiming.Failure.inconsistent }
            try self.prepareZero(format)
            let next = try self.admission.installZero(initial: true)
            try self.create(format, time: time, token: next)
            self.expectedInputTime = time
            return next
        }
    }
    func standbyPlan(validFramesThrough target: UInt64, token: InputToken) throws -> StandbyPlan {
        guard admission.permits(token, source: .zero), let nextInputTime = expectedInputTime,
              let firstTime, let leading = fileLeading, let currentLeading = generationLeading else { throw AudioRecordingTiming.Failure.stale }
        let plan = try AudioRecordingTiming.zeroPlan(target: target, firstLeading: leading, priorCoded: generationCodedStart,
            currentLeading: currentLeading, currentInput: generationInput, inputRate: generationRate,
            sourceMapping: generationMapping)
        return StandbyPlan(additionalSourceFrames: plan.additionalSourceFrames, requestedValidFrames: target,
            attainableValidFrames: plan.attainableValidFrames, quantizationOutputFrames: plan.quantizationOutputFrames,
            nextInputTime: nextInputTime,
            firstFreshOutputTime: CMTimeAdd(firstTime, CMTime(value: Int64(generationCodedStart), timescale: 48_000)),
            sourceMapping: plan.sourceMapping, sourceExtent: plan.sourceExtent)
    }
    func encodeSilence(frames: UInt32, token: InputToken) throws {
        guard admission.permits(token, source: .zero) else { throw AudioRecordingTiming.Failure.stale }
        try guarded {
            guard frames > 0, frames <= 4096, let pcm = self.zeroStorage, let time = self.expectedInputTime else { throw AudioRecordingTiming.Failure.unsupported }
            pcm.frameLength = frames
            try self.consume(pcm, token: token)
            self.expectedInputTime = CMTimeAdd(time, CMTime(value: Int64(frames), timescale: Int32(self.generationRate)))
            self.meter(-60)
        }
    }
    /// Explicit Resume reuses the warmed converter. It never shifts raw source
    /// timestamps to hide a gap; the caller must supply the next actual time.
    func resumeInput(from token: InputToken, at time: CMTime) throws -> InputToken {
        guard admission.permits(token, source: .zero) else { throw AudioRecordingTiming.Failure.stale }
        return try guarded {
            try self.validateTime(time)
            let next = try self.admission.resume(token)
            self.nativeToken = next
            return next
        }
    }
    /// Fixture/cold final-EOF seam, not a live Resume. The plan must already be
    /// fully fed; no hidden PCM loop or timestamp adjustment occurs here.
    func finishStandbyThrough(validFrames target: UInt64, token: InputToken) throws {
        guard admission.permits(token, source: .zero) else { throw AudioRecordingTiming.Failure.stale }
        try guarded {
            let plan = try self.standbyPlan(validFramesThrough: target, token: token)
            guard plan.additionalSourceFrames == 0, plan.attainableValidFrames == target else { throw AudioRecordingTiming.Failure.inconsistent }
            try self.finish()
            guard let extent = self.ledger.extent,
                  try AudioRecordingTiming.validFrames(extent, observedUnits: UInt64(self.emittedFrames / 1024), leadingFrames: extent.leadingFrames) == Int64(target) else { throw AudioRecordingTiming.Failure.inconsistent }
        }
    }
    private func validateTime(_ time: CMTime) throws {
        guard time.isNumeric, time.seconds.isFinite else { throw CaptureFailure(message: "Microphone PCM clock is unavailable") }
        if let expectedInputTime, abs(CMTimeSubtract(time, expectedInputTime).seconds) > 0.005 {
            throw CaptureFailure(message: "Microphone capture timestamps skipped; prepare a new session")
        }
    }
    private func prepareZero(_ source: AVAudioFormat) throws {
        guard source.sampleRate > 0, source.sampleRate <= 192_000, source.sampleRate.rounded() == source.sampleRate,
              (1...8).contains(source.channelCount), source.commonFormat == .pcmFormatInt16 || source.commonFormat == .pcmFormatFloat32,
              let pcm = AVAudioPCMBuffer(pcmFormat: source, frameCapacity: 4096) else { throw AudioRecordingTiming.Failure.unsupported }
        pcm.frameLength = 4096
        let planes = UnsafeMutableAudioBufferListPointer(pcm.mutableAudioBufferList)
        guard !planes.isEmpty, planes.count <= 8 else { throw AudioRecordingTiming.Failure.unsupported }
        for plane in planes {
            guard let bytes = plane.mData, plane.mDataByteSize > 0, plane.mDataByteSize <= 4096 * 4 * 8 else { throw AudioRecordingTiming.Failure.unsupported }
            memset(bytes, 0, Int(plane.mDataByteSize))
        }
        zeroStorage = pcm // Payload stays immutable for the whole native owner.
    }
    private func create(_ source: AVAudioFormat, time: CMTime, token: InputToken) throws {
        guard converter == nil, time.isNumeric, source.sampleRate > 0, source.sampleRate <= 192_000,
              source.sampleRate.rounded() == source.sampleRate else { throw AudioRecordingTiming.Failure.unsupported }
        guard let created = AVAudioConverter(from: source, to: outputFormat) else {
            throw CaptureFailure(message: "Native AAC conversion is unsupported; input \(Self.formatFacts(source)); output \(Self.formatFacts(outputFormat))")
        }
        created.bitRate = 96_000
        // Keep the established 48 kHz AAC path byte-compatible. Native SRC
        // needs normal priming: none exposes SRC latency not described by the
        // composed AAC leading trim. No source/coded boundary is rebased here.
        generationMapping = source.sampleRate == 48_000 ? .nearest : .floor
        created.primeMethod = generationMapping == .floor ? .normal : .none
        _ = try AudioRecordingTiming.headUnits(created.primeInfo.leadingFrames)
        guard created.maximumOutputPacketSize >= 0, created.maximumOutputPacketSize <= 64 * 1024 else {
            throw AudioRecordingTiming.Failure.unsupported
        }
        converter = created; inputFormat = source; nativeToken = token
        compressedBuffer = AVAudioCompressedBuffer(format: outputFormat, packetCapacity: 1, maximumPacketSize: max(2048, created.maximumOutputPacketSize))
        generationInput = 0; generationRaw = 0; generationZero = 0
        generationRate = UInt32(source.sampleRate); generationCodedStart = UInt64(max(0, emittedFrames))
        generationLeading = created.primeInfo.leadingFrames
        generationFormatVerified = false
        if firstTime == nil {
            firstTime = CMTimeConvertScale(time, timescale: 48_000, method: .roundHalfAwayFromZero)
            fileLeading = created.primeInfo.leadingFrames
            recordingTiming?(.began(leadingFrames: created.primeInfo.leadingFrames))
        }
    }
    private func consume(_ pcm: AVAudioPCMBuffer, token: InputToken) throws {
        guard admission.permits(token, source: token.source), let converter, let compressed = compressedBuffer else { throw AudioRecordingTiming.Failure.stale }
        let next = generationInput.addingReportingOverflow(UInt64(pcm.frameLength))
        guard !next.overflow else { throw AudioRecordingTiming.Failure.overflow }
        var supplied = false
        for iteration in 0..<16 {
            compressed.packetCount = 0; compressed.byteLength = 0
            var error: NSError?
            let result = converter.convert(to: compressed, error: &error) { _, state in
                if supplied { state.pointee = .noDataNow; return nil }
                supplied = true; self.generationInput = next.partialValue
                if token.source == .raw { self.generationRaw += UInt64(pcm.frameLength) }
                else { self.generationZero += UInt64(pcm.frameLength) }
                state.pointee = .haveData; return pcm
            }
            if let error { throw error }
            if compressed.packetCount > 0 { try emit(compressed, converter: converter) }
            if result == .error { throw CaptureFailure(message: "Native AAC encoder failed") }
            if result == .inputRanDry || result == .endOfStream || compressed.packetCount == 0 { break }
            if iteration == 15 { throw CaptureFailure(message: "Native AAC conversion exceeded its bounded packet allowance") }
        }
        guard supplied else { throw CaptureFailure(message: "Native AAC input was not consumed") }
    }
    private func endGeneration() throws {
        guard let converter, let compressed = compressedBuffer, let token = nativeToken else { return }
        guard generationInput > 0 else { return }
        var completed = false
        for _ in 0..<16 {
            compressed.packetCount = 0; compressed.byteLength = 0
            var error: NSError?
            let result = converter.convert(to: compressed, error: &error) { _, state in state.pointee = .endOfStream; return nil }
            if let error { throw error }
            if compressed.packetCount > 0 { try emit(compressed, converter: converter) }
            if result == .error { throw CaptureFailure(message: "AAC finalisation failed") }
            if result == .endOfStream || result == .inputRanDry || compressed.packetCount == 0 { completed = true; break }
        }
        let prime = converter.primeInfo
        let receipt = AudioRecordingTiming.GenerationExtent(generation: token.generation, inputFrames: generationInput,
            rawFrames: generationRaw, zeroFrames: generationZero, inputRate: generationRate,
            encodedFrames: UInt64(emittedFrames) - generationCodedStart,
            leadingFrames: prime.leadingFrames, trailingFrames: prime.trailingFrames, sourceMapping: generationMapping)
        lastAttemptedGeneration = receipt
        guard completed, generationLeading == prime.leadingFrames else {
            throw CaptureFailure(message: "Native AAC EOF did not close; completed=\(completed),initialLeading=\(String(describing: generationLeading)),attempted=\(receipt)")
        }
        do { try ledger.append(receipt) }
        catch {
            let mapped = receipt.sourceExtent.map { String(describing: $0) } ?? "unavailable"
            throw CaptureFailure(message: "Native AAC EOF accounting was rejected (\(error)); sourceMapping=\(receipt.sourceMapping),sourceExtent=\(mapped),attempted=\(receipt)")
        }
        if receiptMode == .fullGenerationLedger || hasTransitions { recordingTiming?(.generationEnded(receipt)) }
    }
    private func retireConverter() {
        converter = nil; compressedBuffer = nil; nativeToken = nil; generationLeading = nil
    }
    /// Final EOF is one-way and idempotent. Every native generation validates
    /// internally even if no recorder timing callback is attached (A/V too).
    func finish() throws {
        if closed { if let terminalError { throw terminalError }; return }
        defer { retireConverter(); admission.finish(); closed = true }
        if let terminalError { throw terminalError }
        try guarded {
            try self.endGeneration()
            guard !self.timingFinished, let extent = self.ledger.extent else { return }
            self.timingFinished = true
            if self.receiptMode == .fullGenerationLedger || self.hasTransitions { self.recordingTiming?(.endedGenerations(extent)) }
            else if let last = self.ledger.last {
                self.recordingTiming?(.ended(.init(inputFrames: last.inputFrames, inputRate: last.inputRate,
                    encodedFrames: extent.encodedFrames, leadingFrames: extent.leadingFrames, trailingFrames: extent.trailingFrames,
                    sourceMapping: last.sourceMapping)))
            }
        }
    }
    private func guarded<T>(_ body: () throws -> T) throws -> T {
        do { return try body() }
        catch {
            terminalError = terminalError ?? error; admission.fail()
            if !timingFailed { timingFailed = true; recordingTiming?(.failed) }
            throw error
        }
    }

    private func updateMeter(_ pcm: AVAudioPCMBuffer) {
        let count = Int(pcm.frameLength)
        guard count > 0 else { return }
        var energy: Double = 0
        if let values = pcm.floatChannelData?[0] {
            for n in 0..<count { let value = Double(values[n * Int(pcm.stride)]); energy += value * value }
        } else if let values = pcm.int16ChannelData?[0] {
            for n in 0..<count { let value = Double(values[n * Int(pcm.stride)]) / 32768; energy += value * value }
        } else { return }
        meter(Float(max(-60, 10 * log10(max(1e-12, energy / Double(count))))))
    }

    private func emit(_ compressed: AVAudioCompressedBuffer, converter: AVAudioConverter) throws {
        guard let firstTime, let token = nativeToken, admission.permitsOutput(generation: token.generation) else {
            throw CaptureFailure(message: "AAC output belongs to a closed generation")
        }
        if !generationFormatVerified {
            let cookie = converter.magicCookie ?? Data()
            let actual = converter.outputFormat
            var resolved = actual.streamDescription.pointee
            var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
            var attempted = false
            var resolutionStatus: OSStatus = noErr
            if cookie.count <= 4096 {
                attempted = true
                resolutionStatus = cookie.withUnsafeBytes { bytes in
                    AudioFormatGetProperty(kAudioFormatProperty_FormatInfo, UInt32(cookie.count), bytes.baseAddress, &size, &resolved)
                }
            }
            guard attempted, resolutionStatus == noErr, size == UInt32(MemoryLayout<AudioStreamBasicDescription>.size),
                  resolved.mSampleRate == 48_000, resolved.mChannelsPerFrame == 1,
                  resolved.mFormatID == kAudioFormatMPEG4AAC, resolved.mFramesPerPacket == 1024 else {
                // Cold failure facts only: no cookie bytes, PCM, AAC payload,
                // route identity or user data are included in this diagnostic.
                let packet = compressed.packetDescriptions?.pointee
                let frames = packet.map { String($0.mVariableFramesInPacket) } ?? "absent"
                let packetBytes = packet.map { String($0.mDataByteSize) } ?? "absent"
                let offset = packet.map { String($0.mStartOffset) } ?? "absent"
                throw CaptureFailure(message: "Native AAC output configuration is unsupported; actual \(Self.formatFacts(actual)); resolutionAttempted=\(attempted),resolutionStatus=\(resolutionStatus),resolvedSize=\(size),resolved \(Self.asbdFacts(resolved)); cookieBytes=\(cookie.count); packetCount=\(compressed.packetCount),packetDescriptionFrames=\(frames),packetDescriptionBytes=\(packetBytes),packetDescriptionOffset=\(offset),outputBytes=\(compressed.byteLength)")
            }
            if let codecCookie, let codecASBD {
                guard codecCookie == cookie, Self.sameASBD(codecASBD, resolved) else { throw CaptureFailure(message: "Native AAC configuration changed across standby generations") }
            } else { codecCookie = cookie; codecASBD = resolved }
            guard verifiedConfigurationGenerations < UInt64.max else { throw AudioRecordingTiming.Failure.overflow }
            verifiedConfigurationGenerations += 1; generationFormatVerified = true
        }
        if compressedDescription == nil {
            guard let cookie = codecCookie, var asbd = codecASBD else { throw AudioRecordingTiming.Failure.inconsistent }
            let made = cookie.withUnsafeBytes { bytes in
                CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: &asbd,
                    layoutSize: 0, layout: nil, magicCookieSize: cookie.count, magicCookie: bytes.baseAddress,
                    extensions: nil, formatDescriptionOut: &compressedDescription)
            }
            guard made == noErr else { throw CaptureFailure(message: "AAC format description failed") }
        }
        guard let format = compressedDescription else { throw CaptureFailure(message: "AAC format description unavailable") }
        let length = Int(compressed.byteLength)
        guard length > 0, length <= 64 * 1024 else { throw CaptureFailure(message: "AAC packet size exceeded its bound") }
        var block: CMBlockBuffer?
        let allocated = CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: length,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0, dataLength: length,
            flags: 0, blockBufferOut: &block)
        guard allocated == kCMBlockBufferNoErr, let block else { throw CaptureFailure(message: "AAC sample allocation failed") }
        // One platform PCM->AAC output ownership copy, separate from the one
        // measured compressed ingress copy into Rust. AVAssetWriter shares it.
        guard CMBlockBufferReplaceDataBytes(with: compressed.data, blockBuffer: block, offsetIntoDestination: 0, dataLength: length) == kCMBlockBufferNoErr else {
            throw CaptureFailure(message: "AAC sample storage failed")
        }
        let pts = CMTimeAdd(firstTime, CMTime(value: emittedFrames, timescale: 48_000))
        let duration = CMTime(value: 1024, timescale: 48_000)
        var timing = CMSampleTimingInfo(duration: duration, presentationTimeStamp: pts, decodeTimeStamp: pts)
        var sampleSize = length
        var sample: CMSampleBuffer?
        let created = CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &sampleSize, sampleBufferOut: &sample)
        guard created == noErr, let sample else { throw CaptureFailure(message: "AAC compressed sample creation failed") }
        guard emittedFrames <= Int64.max - 1024 else { throw AudioRecordingTiming.Failure.overflow }
        emittedFrames += 1024
        lastOutputGeneration = token.generation
        output(CompressedUnit(buffer: sample, isVideo: false, keyframe: false, time: pts, duration: duration, configuration: .aac(asc: Data([0x11, 0x88]))))
    }
}
