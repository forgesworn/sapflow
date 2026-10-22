import AVFoundation
import AudioToolbox

final class NativeAudioEncoder {
    private var converter: AVAudioConverter?
    private var inputFormat: AVAudioFormat?
    private let outputFormat: AVAudioFormat
    private var firstTime: CMTime?
    private var emittedFrames: Int64 = 0
    private var expectedInputTime: CMTime?
    private var compressedBuffer: AVAudioCompressedBuffer?
    private var compressedDescription: CMAudioFormatDescription?
    private let output: (CompressedUnit) -> Void
    private let meter: (Float) -> Void
    private let recordingTiming: ((AudioRecordingTiming.Event) -> Void)?
    private var recordingInputFrames: UInt64 = 0
    private var recordingInputRate: UInt32 = 0
    private var recordingTimingFinished = false

    init(output: @escaping (CompressedUnit) -> Void, meter: @escaping (Float) -> Void, recordingTiming: ((AudioRecordingTiming.Event) -> Void)? = nil) throws {
        guard let format = AVAudioFormat(settings: [AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 96_000]) else {
            throw CaptureFailure(message: "Native AAC-LC format is unavailable")
        }
        outputFormat = format; self.output = output; self.meter = meter; self.recordingTiming = recordingTiming
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
        return "ASBD{rate=\(value.mSampleRate),id=\(value.mFormatID),flags=\(value.mFormatFlags),bytesPerPacket=\(value.mBytesPerPacket),framesPerPacket=\(value.mFramesPerPacket),bytesPerFrame=\(value.mBytesPerFrame),channels=\(value.mChannelsPerFrame),bits=\(value.mBitsPerChannel),reserved=\(value.mReserved)} layout{\(layout)}"
    }

    func encode(_ sample: CMSampleBuffer, time: CMTime) throws {
        guard let description = CMSampleBufferGetFormatDescription(sample),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description),
              let source = AVAudioFormat(streamDescription: asbd) else { throw CaptureFailure(message: "Microphone PCM format unavailable") }
        if let expectedInputTime, abs(CMTimeSubtract(time, expectedInputTime).seconds) > 0.005 {
            throw CaptureFailure(message: "Microphone capture timestamps skipped; prepare a new session")
        }
        expectedInputTime = CMTimeAdd(time, CMTime(seconds: Double(CMSampleBufferGetNumSamples(sample)) / source.sampleRate, preferredTimescale: 1_000_000_000))
        if let inputFormat, inputFormat != source { throw CaptureFailure(message: "Audio route changed its format; stop and prepare again") }
        if converter == nil {
            guard let created = AVAudioConverter(from: source, to: outputFormat) else {
                throw CaptureFailure(message: "Native AAC conversion is unsupported; input \(Self.formatFacts(source)); output \(Self.formatFacts(outputFormat))")
            }
            created.bitRate = 96_000
            created.primeMethod = .none
            converter = created; inputFormat = source
            compressedBuffer = AVAudioCompressedBuffer(format: outputFormat, packetCapacity: 1, maximumPacketSize: max(2048, created.maximumOutputPacketSize))
            firstTime = CMTimeConvertScale(time, timescale: 48_000, method: .roundHalfAwayFromZero)
            if let recordingTiming {
                if source.sampleRate > 0, source.sampleRate <= 192_000, source.sampleRate.rounded() == source.sampleRate {
                    recordingInputRate = UInt32(source.sampleRate)
                }
                recordingTiming(.began(leadingFrames: created.primeInfo.leadingFrames))
            }
        }
        guard let converter, let compressed = compressedBuffer else { throw CaptureFailure(message: "AAC encoder is not ready") }
        // Wrap the platform PCM storage without copying it into Rust or Data.
        guard source.channelCount <= 8 else { throw CaptureFailure(message: "Microphone channel count exceeds the supported bound") }
        let listBytes = MemoryLayout<AudioBufferList>.size + max(0, Int(source.channelCount) - 1) * MemoryLayout<AudioBuffer>.stride
        let listStorage = UnsafeMutableRawPointer.allocate(byteCount: listBytes, alignment: 16)
        defer { listStorage.deallocate() }
        let list = listStorage.bindMemory(to: AudioBufferList.self, capacity: 1)
        var retained: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: nil,
            bufferListOut: list, bufferListSize: listBytes,
            blockBufferAllocator: kCFAllocatorDefault, blockBufferMemoryAllocator: kCFAllocatorDefault,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, blockBufferOut: &retained)
        guard status == noErr, let pcm = AVAudioPCMBuffer(pcmFormat: source, bufferListNoCopy: UnsafePointer(list), deallocator: nil) else {
            throw CaptureFailure(message: "Microphone PCM storage could not be read (\(status))")
        }
        pcm.frameLength = AVAudioFrameCount(CMSampleBufferGetNumSamples(sample))
        updateMeter(pcm)
        var supplied = false
        // Output capacity is bounded, and the converter may consume a partial
        // input callback before enough PCM exists for a 1024-sample AAC frame.
        for _ in 0..<16 {
            compressed.packetCount = 0; compressed.byteLength = 0
            var error: NSError?
            let result = withExtendedLifetime(retained) {
                converter.convert(to: compressed, error: &error) { _, state in
                    if supplied { state.pointee = .noDataNow; return nil }
                    supplied = true
                    if self.recordingTiming != nil {
                        let next = self.recordingInputFrames.addingReportingOverflow(UInt64(pcm.frameLength))
                        self.recordingInputFrames = next.overflow ? UInt64.max : next.partialValue
                    }
                    state.pointee = .haveData; return pcm
                }
            }
            if let error { throw error }
            if compressed.packetCount > 0 { try emit(compressed, converter: converter) }
            if result == .inputRanDry || result == .endOfStream { break }
            if result == .error { throw CaptureFailure(message: "Native AAC encoder failed") }
            if compressed.packetCount == 0 { break }
        }
    }

    /// Finish the native codec before closing either compressed sink. A final
    /// partial PCM frame is padded by AAC; trim/priming metadata qualification
    /// remains a physical recording acceptance check.
    func finish() throws {
        guard let converter, let compressed = compressedBuffer else { return }
        for _ in 0..<16 {
            compressed.packetCount = 0; compressed.byteLength = 0
            var error: NSError?
            let result = converter.convert(to: compressed, error: &error) { _, state in
                state.pointee = .endOfStream
                return nil
            }
            if let error { throw error }
            if compressed.packetCount > 0 { try emit(compressed, converter: converter) }
            if result == .endOfStream || result == .inputRanDry { finishRecordingTiming(converter); return }
            if result == .error { throw CaptureFailure(message: "AAC finalisation failed") }
            if compressed.packetCount == 0 { finishRecordingTiming(converter); return }
        }
        throw CaptureFailure(message: "AAC finalisation exceeded its bounded packet allowance")
    }

    private func finishRecordingTiming(_ converter: AVAudioConverter) {
        guard let recordingTiming, !recordingTimingFinished else { return }
        recordingTimingFinished = true
        let prime = converter.primeInfo
        recordingTiming(.ended(AudioRecordingTiming.Extent(inputFrames: recordingInputFrames, inputRate: recordingInputRate,
            encodedFrames: UInt64(max(0, emittedFrames)), leadingFrames: prime.leadingFrames, trailingFrames: prime.trailingFrames)))
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
        guard let firstTime else { throw CaptureFailure(message: "AAC clock is unavailable") }
        if compressedDescription == nil {
            let cookie = converter.magicCookie ?? Data()
            let made = cookie.withUnsafeBytes { bytes in
                CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: outputFormat.streamDescription,
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
        emittedFrames += 1024
        output(CompressedUnit(buffer: sample, isVideo: false, keyframe: false, time: pts, duration: duration, configuration: .aac(asc: Data([0x11, 0x88]))))
    }
}
