import AVFoundation
import VideoToolbox

final class HardwareVideoEncoder {
    private var session: VTCompressionSession?
    private let width: Int
    private let height: Int
    private let profile: CaptureProfile
    private var encoderFailure: Error?
    private var keyframeRequested = false // samples queue only
    private let keyframeProperties = [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary
    private var pending: CMSampleBuffer?
    private var cachedFormat: CMFormatDescription?
    private var configuration: CodecDescription?
    private let lock = NSLock()
    private let output: (CompressedUnit) -> Void
    private let failure: (Error) -> Void
    private(set) var implementation = "Hardware H.264"

    init(orientation: CaptureOrientation, profile: CaptureProfile = .defaultProfile, output: @escaping (CompressedUnit) -> Void, failure: @escaping (Error) -> Void) throws {
        self.output = output; self.failure = failure
        self.profile = profile
        let dimensions = profile.dimensions(for: orientation)
        width = Int(dimensions.width); height = Int(dimensions.height)
        let callback: VTCompressionOutputCallback = { context, _, status, _, sample in
            guard let context else { return }
            let encoder = Unmanaged<HardwareVideoEncoder>.fromOpaque(context).takeUnretainedValue()
            guard status == noErr, let sample, CMSampleBufferDataIsReady(sample) else {
                encoder.failure(CaptureFailure(message: "Hardware video encoder failed (\(status))")); return
            }
            encoder.receive(sample)
        }
        var created: VTCompressionSession?
        var encoderList: CFArray?
        guard VTCopyVideoEncoderList(nil, &encoderList) == noErr,
              let entries = encoderList as? [[CFString: Any]],
              let selected = entries.first(where: {
                  ($0[kVTVideoEncoderList_CodecType] as? NSNumber)?.uint32Value == kCMVideoCodecType_H264 &&
                  ($0[kVTVideoEncoderList_IsHardwareAccelerated] as? Bool) == true
              }), let encoderID = selected[kVTVideoEncoderList_EncoderID] as? String else {
            throw CaptureFailure(message: "No explicitly identified hardware H.264 encoder is available")
        }
        implementation = encoderID
        var specification: [CFString: Any] = [kVTVideoEncoderSpecification_EncoderID: encoderID]
        if #available(iOS 17.4, *) { specification[kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder] = true }
        let status = VTCompressionSessionCreate(allocator: nil, width: dimensions.width, height: dimensions.height, codecType: kCMVideoCodecType_H264,
            encoderSpecification: specification as CFDictionary,
            imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: callback,
            refcon: Unmanaged.passUnretained(self).toOpaque(), compressionSessionOut: &created)
        guard status == noErr, let created else { throw CaptureFailure(message: "Hardware H.264 at \(width)×\(height) is unavailable (\(status))") }
        session = created
        do {
            try set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
            try set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Main_AutoLevel)
            try set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
            try set(kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: profile.videoBps))
            try set(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: profile.frameRate))
            try set(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: profile.gopFrames))
            try set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: profile.gopSeconds))
            let prepared = VTCompressionSessionPrepareToEncodeFrames(created)
            guard prepared == noErr else { throw CaptureFailure(message: "Video encoder preparation failed (\(prepared))") }
            guard try property(kVTCompressionPropertyKey_EncoderID, session: created) as? String == encoderID else {
                throw CaptureFailure(message: "Video encoder identity changed from the selected hardware implementation")
            }
            if #available(iOS 17.4, *) {
                guard try property(kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, session: created) as? Bool == true else {
                    throw CaptureFailure(message: "Video encoder did not confirm hardware acceleration")
                }
            }
        } catch { VTCompressionSessionInvalidate(created); session = nil; throw error }
    }

    private func property(_ key: CFString, session: VTCompressionSession) throws -> CFTypeRef {
        var reference: Unmanaged<CFTypeRef>?
        let status = withUnsafeMutablePointer(to: &reference) { pointer in
            VTSessionCopyProperty(session, key: key, allocator: nil, valueOut: UnsafeMutableRawPointer(pointer))
        }
        guard status == noErr, let reference else { throw CaptureFailure(message: "Video encoder property read failed (\(status))") }
        return reference.takeRetainedValue()
    }
    private func set(_ key: CFString, _ value: CFTypeRef) throws {
        guard let session else { throw CaptureFailure(message: "Encoder is closed") }
        let status = VTSessionSetProperty(session, key: key, value: value)
        guard status == noErr else { throw CaptureFailure(message: "Video encoder setting failed (\(status))") }
    }
    /// Complete accepted old frames without invalidating the encoder. The held
    /// final old picture gets one nominal frame, not the camera-switch gap.
    func sealForCameraSwitch() throws -> CMTime {
        guard let session else { throw CaptureFailure(message: "Video encoder is closed") }
        let status = VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        guard status == noErr else { throw CaptureFailure(message: "Could not drain video before camera switch") }
        lock.lock(); defer { lock.unlock() }
        if let encoderFailure { throw encoderFailure }
        guard let previous = pending else { throw CaptureFailure(message: "No accepted video boundary is available for camera switch") }
        let duration = CMTime(value: 1, timescale: profile.frameRate)
        try emit(previous, duration: duration); pending = nil
        return duration
    }
    func requestKeyframe() { keyframeRequested = true }
    func encode(_ sample: CMSampleBuffer, time: CMTime) throws {
        guard let session, let image = CMSampleBufferGetImageBuffer(sample) else { throw CaptureFailure(message: "Video capture lost its pixel buffer") }
        guard CVPixelBufferGetWidth(image) == width, CVPixelBufferGetHeight(image) == height else { throw CaptureFailure(message: "Camera no longer produces the prepared \(width)×\(height) format") }
        let status = VTCompressionSessionEncodeFrame(session, imageBuffer: image, presentationTimeStamp: time,
            duration: CMTime(value: 1, timescale: profile.frameRate), frameProperties: keyframeRequested ? keyframeProperties : nil, sourceFrameRefcon: nil, infoFlagsOut: nil)
        guard status == noErr else { throw CaptureFailure(message: "Hardware encoding failed (\(status))") }
        keyframeRequested = false
    }
    private func receive(_ sample: CMSampleBuffer) {
        lock.lock(); defer { lock.unlock() }
        do {
            // Hold one compressed frame so its duration follows actual capture
            // PTS, rather than asserting fixed cadence across camera jitter.
            if let previous = pending {
                let delta = CMTimeSubtract(CMSampleBufferGetPresentationTimeStamp(sample), CMSampleBufferGetPresentationTimeStamp(previous))
                guard delta.isValid, delta.seconds > 0, delta.seconds < 2 else { throw CaptureFailure(message: "Video timestamps jumped; prepare a new session") }
                try emit(previous, duration: delta)
            }
            pending = sample
        } catch { pending = nil; encoderFailure = encoderFailure ?? error; failure(error) }
    }
    private func emit(_ sample: CMSampleBuffer, duration: CMTime) throws {
        guard let format = CMSampleBufferGetFormatDescription(sample) else { throw CaptureFailure(message: "H.264 format is unavailable") }
        let dimensions = CMVideoFormatDescriptionGetDimensions(format)
        guard Int(dimensions.width) == width, Int(dimensions.height) == height else {
            throw CaptureFailure(message: "Hardware H.264 no longer matches the prepared \(width)×\(height) format")
        }
        func parameter(_ index: Int) throws -> Data {
            var pointer: UnsafePointer<UInt8>?
            var size = 0; var count = 0; var header: Int32 = 0
            let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index,
                parameterSetPointerOut: &pointer, parameterSetSizeOut: &size, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &header)
            guard status == noErr, header == 4, let pointer, size > 0 else { throw CaptureFailure(message: "Encoder did not produce supported AVCC configuration") }
            return Data(bytes: pointer, count: size)
        }
        if let cachedFormat, !CFEqual(cachedFormat, format) { throw CaptureFailure(message: "Hardware video configuration changed during capture") }
        if configuration == nil {
            configuration = .h264(sps: try parameter(0), pps: try parameter(1))
            cachedFormat = format
        }
        guard let configuration else { throw CaptureFailure(message: "Video parameter sets are unavailable") }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        let keyframe = (attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true
        let pts = CMTimeConvertScale(CMSampleBufferGetPresentationTimeStamp(sample), timescale: 90_000, method: .roundHalfAwayFromZero)
        let end = CMTimeConvertScale(CMTimeAdd(CMSampleBufferGetPresentationTimeStamp(sample), duration), timescale: 90_000, method: .roundHalfAwayFromZero)
        let exactDuration = CMTimeSubtract(end, pts)
        guard exactDuration.value > 0, exactDuration.value <= Int64(UInt32.max) else { throw CaptureFailure(message: "Video duration is invalid") }
        var timing = CMSampleTimingInfo(duration: exactDuration, presentationTimeStamp: pts, decodeTimeStamp: pts)
        var retimed: CMSampleBuffer?
        let status = CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: sample,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &retimed)
        guard status == noErr, let retimed else { throw CaptureFailure(message: "Video timing conversion failed") }
        output(CompressedUnit(buffer: retimed, isVideo: true, keyframe: keyframe, time: pts, duration: exactDuration, configuration: configuration))
    }
    func finish() {
        guard let session else { return }
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        lock.lock()
        if let pending { do { try emit(pending, duration: CMTime(value: 1, timescale: profile.frameRate)) } catch { failure(error) } }
        pending = nil
        lock.unlock()
        VTCompressionSessionInvalidate(session)
        self.session = nil
    }
    deinit { if let session { VTCompressionSessionInvalidate(session) } }
}
