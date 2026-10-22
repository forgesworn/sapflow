import AVFoundation

/// Bounded metadata only. Preserve full native ASBD and channel layout identity;
/// AVAudioFormat equality alone may ignore representation differences.
struct CapturePCMFormat {
    struct Failure: LocalizedError {
        var errorDescription: String? { "Native PCM format/layout metadata is unsupported" }
    }
    let format: AVAudioFormat
    let description: CMAudioFormatDescription
    init(_ description: CMAudioFormatDescription) throws {
        guard CMFormatDescriptionGetMediaType(description) == kCMMediaType_Audio,
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description),
              asbd.pointee.mFormatID == kAudioFormatLinearPCM,
              asbd.pointee.mSampleRate.isFinite, asbd.pointee.mSampleRate > 0,
              asbd.pointee.mSampleRate <= 192_000,
              (1...8).contains(asbd.pointee.mChannelsPerFrame),
              let format = AVAudioFormat(streamDescription: asbd),
              format.commonFormat == .pcmFormatFloat32 || format.commonFormat == .pcmFormatInt16 else {
            throw Failure()
        }
        var size = 0
        if let layout = CMAudioFormatDescriptionGetChannelLayout(description, sizeOut: &size) {
            guard layout.pointee.mNumberChannelDescriptions <= 8,
                  size >= MemoryLayout<AudioChannelLayout>.size - MemoryLayout<AudioChannelDescription>.size,
                  size <= MemoryLayout<AudioChannelLayout>.size + 7 * MemoryLayout<AudioChannelDescription>.size else {
                throw Failure()
            }
        }
        self.format = format; self.description = description
    }
    func matches(_ actual: CMAudioFormatDescription) -> Bool {
        guard (try? CapturePCMFormat(actual)) != nil,
              let a = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee,
              let b = CMAudioFormatDescriptionGetStreamBasicDescription(actual)?.pointee,
              a.mSampleRate == b.mSampleRate, a.mFormatID == b.mFormatID, a.mFormatFlags == b.mFormatFlags,
              a.mBytesPerPacket == b.mBytesPerPacket, a.mFramesPerPacket == b.mFramesPerPacket,
              a.mBytesPerFrame == b.mBytesPerFrame, a.mChannelsPerFrame == b.mChannelsPerFrame,
              a.mBitsPerChannel == b.mBitsPerChannel, a.mReserved == b.mReserved else { return false }
        var aSize = 0; var bSize = 0
        let aLayout = CMAudioFormatDescriptionGetChannelLayout(description, sizeOut: &aSize)
        let bLayout = CMAudioFormatDescriptionGetChannelLayout(actual, sizeOut: &bSize)
        guard (aLayout == nil) == (bLayout == nil), aSize == bSize else { return false }
        if let aLayout, let bLayout {
            // Native metadata only; the checked maximum is eight descriptions.
            return memcmp(aLayout, bLayout, aSize) == 0
        }
        return true
    }
}
