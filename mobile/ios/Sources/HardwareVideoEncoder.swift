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
    private typealias Policy = VideoEncodingPolicy<CaptureAdmission.MediaToken?>
    private var callbacks: VideoCallbackQueue<CaptureAdmission.MediaToken?, CMSampleBuffer>
    private var faultDelivered = false
    private var pending: (sample: CMSampleBuffer, submission: Policy.Submission)?
    private var sealedEnd: CMTime?
    private var preparedSlate: PreparedVideoSlate?
    private var cachedFormat: CMFormatDescription?
    private var configuration: CodecDescription?
    private let state = NSCondition()
    private let output: (CompressedUnit, CaptureAdmission.MediaToken?) -> Void
    private let fault: (CaptureAdmission.MediaToken?, Error) -> Void
    private let permitsInput: (CaptureAdmission.MediaToken) -> Bool
    private let strict: Bool
    private(set) var implementation = "Hardware H.264"

    /// Legacy capture/fixture mode has no privacy-attribution authority. Strict
    /// runtime composition uses the token-bearing initializer and encode APIs.
    convenience init(orientation: CaptureOrientation, profile: CaptureProfile = .defaultProfile,
                     output: @escaping (CompressedUnit) -> Void, failure: @escaping (Error) -> Void) throws {
        try self.init(orientation: orientation, profile: profile, strict: false,
            output: { unit, _ in output(unit) }, fault: { _, error in failure(error) }, permitsInput: { _ in false })
    }
    convenience init(orientation: CaptureOrientation, profile: CaptureProfile = .defaultProfile,
                     output: @escaping (CompressedUnit, CaptureAdmission.MediaToken) -> Void,
                     fault: @escaping (CaptureAdmission.MediaToken?, Error) -> Void,
                     permitsInput: @escaping (CaptureAdmission.MediaToken) -> Bool) throws {
        try self.init(orientation: orientation, profile: profile, strict: true,
            output: { unit, token in if let token { output(unit, token) } }, fault: fault, permitsInput: permitsInput)
    }
    private init(orientation: CaptureOrientation, profile: CaptureProfile, strict: Bool,
                 output: @escaping (CompressedUnit, CaptureAdmission.MediaToken?) -> Void,
                 fault: @escaping (CaptureAdmission.MediaToken?, Error) -> Void,
                 permitsInput: @escaping (CaptureAdmission.MediaToken) -> Bool) throws {
        self.output = output; self.fault = fault; self.permitsInput = permitsInput; self.strict = strict
        self.callbacks = try .init()
        self.profile = profile
        let dimensions = profile.dimensions(for: orientation)
        width = Int(dimensions.width); height = Int(dimensions.height)
        let callback: VTCompressionOutputCallback = { context, reference, status, flags, sample in
            guard let context else { return }
            let encoder = Unmanaged<HardwareVideoEncoder>.fromOpaque(context).takeUnretainedValue()
            encoder.receive(reference: reference, status: status, flags: flags, sample: sample)
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
    /// Allocate once, cold, independently of any private camera frame.
    func prepareSlate() throws -> PreparedVideoSlate {
        if let preparedSlate { return preparedSlate }
        let created = try PreparedVideoSlate(width: width, height: height)
        preparedSlate = created; return created
    }

    /// Accepted old native output keeps its original token. Empty is valid for
    /// initial Standby; camera switching keeps its existing nonempty contract.
    func sealForPrivacy() throws -> CMTime? {
        try completeNativePrefix()
        guard let previous = pending else { return nil }
        let duration = CMTime(value: 1, timescale: profile.frameRate)
        do { try emit(previous.sample, duration: duration, submission: previous.submission) }
        catch { latchFault(previous.submission.token, error); throw error }
        pending = nil
        sealedEnd = try VideoEncodingTiming.sealedEnd(last: previous.submission.time, frameRate: profile.frameRate)
        return duration
    }
    func sealForCameraSwitch() throws -> CMTime {
        guard let duration = try sealForPrivacy() else {
            throw CaptureFailure(message: "No accepted video boundary is available for camera switch")
        }
        return duration
    }
    private func completeNativePrefix() throws {
        guard let session else { throw CaptureFailure(message: "Video encoder is closed") }
        // Never hold state while VT waits for callbacks. Callbacks never wait
        // for the samples/control owner or downstream IO.
        let status = VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        if status != noErr {
            let error = CaptureFailure(message: "Could not drain accepted hardware video (\(status))")
            latchFault(nil, error); throw error
        }
        state.lock()
        while callbacks.draining { state.wait() }
        let failure = encoderFailure
        let quiet = callbacks.quiescent
        state.unlock()
        if let failure { throw failure }
        guard quiet else {
            let error = CaptureFailure(message: "Hardware video callback ownership did not close")
            latchFault(nil, error); throw error
        }
    }
    func requestKeyframe() { keyframeRequested = true }
    @discardableResult func encode(_ sample: CMSampleBuffer, time: CMTime) throws -> Bool {
        guard !strict else { throw CaptureFailure(message: "Video input requires its captured media token") }
        guard let image = CMSampleBufferGetImageBuffer(sample) else { throw CaptureFailure(message: "Video capture lost its pixel buffer") }
        return try submit(image, time: time, token: nil, forceIdr: false)
    }
    @discardableResult func encode(_ sample: CMSampleBuffer, time: CMTime, token: CaptureAdmission.MediaToken) throws -> Bool {
        guard token.kind == .raw else {
            let error = CaptureFailure(message: "Camera sample cannot claim synthetic video provenance")
            latchFault(token, error); throw error
        }
        guard let image = CMSampleBufferGetImageBuffer(sample) else { throw CaptureFailure(message: "Video capture lost its pixel buffer") }
        return try submit(image, time: time, token: token, forceIdr: false)
    }
    @discardableResult func encode(_ image: CVPixelBuffer, time: CMTime, token: CaptureAdmission.MediaToken, forceIdr: Bool = false) throws -> Bool {
        return try submit(image, time: time, token: token, forceIdr: forceIdr)
    }
    @discardableResult func encode(_ slate: PreparedVideoSlate, time: CMTime, token: CaptureAdmission.MediaToken, forceIdr: Bool = false) throws -> Bool {
        return try slate.withImage { try submit($0, time: time, token: token, forceIdr: forceIdr) }
    }
    private func submit(_ image: CVPixelBuffer, time: CMTime, token: CaptureAdmission.MediaToken?, forceIdr: Bool) throws -> Bool {
        guard let session else { throw CaptureFailure(message: "Video encoder is closed") }
        if strict {
            guard let token, permitsInput(token) else { return false } // supersession/Stop is ordinary cancellation
        }
        do {
            if let token {
                guard token.kind == .raw || (token.kind == .slate && preparedSlate?.contains(image) == true) else {
                    throw CaptureFailure(message: "Video token does not identify this prepared synthetic source")
                }
            }
            try VideoEncodingTiming.requireNoOverlap(time, sealedEnd: sealedEnd)
            guard CVPixelBufferGetWidth(image) == width, CVPixelBufferGetHeight(image) == height else {
                throw CaptureFailure(message: "Video no longer matches its prepared canvas or sealed timeline")
            }
            state.lock()
            let submission: Policy.Submission
            do { submission = try callbacks.reserve(token: token, time: time) }
            catch { state.unlock(); throw error }
            state.unlock()
            var flags = VTEncodeInfoFlags(rawValue: 0)
            let status = VTCompressionSessionEncodeFrame(session, imageBuffer: image, presentationTimeStamp: time,
                duration: CMTime(value: 1, timescale: profile.frameRate),
                frameProperties: keyframeRequested || forceIdr ? keyframeProperties : nil,
                sourceFrameRefcon: UnsafeMutableRawPointer(bitPattern: UInt(submission.serial)), infoFlagsOut: &flags)
            guard status == noErr, !flags.contains(.frameDropped) else {
                // The rejected native call may have returned without a callback.
                // CompleteFrames/invalidation still own actual native teardown.
                let error = CaptureFailure(message: "Hardware video submission failed or dropped (\(status))")
                latchFault(token, error); throw error
            }
            keyframeRequested = false
            return true
        } catch { latchFault(token, error); throw error }
    }
    private func receive(reference: UnsafeMutableRawPointer?, status: OSStatus, flags: VTEncodeInfoFlags, sample: CMSampleBuffer?) {
        let serial = reference.map { UInt64(UInt(bitPattern: $0)) } ?? 0
        let time = sample.map { CMSampleBufferGetPresentationTimeStamp($0) }
        let usable = status == noErr && !flags.contains(.frameDropped) && sample.map({ CMSampleBufferDataIsReady($0) }) == true
        state.lock()
        let submission = callbacks.submission(serial)
        var error: Error?
        do { try callbacks.complete(serial, time: time, frame: usable ? sample : nil) } catch let problem { error = problem }
        if !usable { error = error ?? CaptureFailure(message: "Hardware video callback failed or dropped (\(status))") }
        if error != nil { callbacks.fail() }
        let elected = callbacks.electDrainer()
        state.unlock()
        if let error { latchFault(submission?.token, error) }
        if elected { drainCallbacks() }
    }
    /// One inline callback drainer, no per-frame dispatch or native query. A
    /// concurrent callback only fills its already reserved bounded slot.
    private func drainCallbacks() {
        while true {
            state.lock()
            guard let (submission, sample) = callbacks.takeNext() else {
                state.broadcast(); state.unlock(); return
            }
            let failed = encoderFailure != nil || callbacks.failed
            state.unlock()
            guard !failed, let sample else { continue }
            do {
                if let previous = pending {
                    let duration = try VideoEncodingTiming.duration(from: previous.submission.time, to: submission.time)
                    try emit(previous.sample, duration: duration, submission: previous.submission)
                }
                pending = (sample, submission)
            } catch { pending = nil; latchFault(submission.token, error) }
        }
    }
    /// The caller's callback must synchronously latch ONLY the immutable run's
    /// scalar fault. Never samples.sync, control.sync or perform file/socket IO.
    private func latchFault(_ token: CaptureAdmission.MediaToken?, _ error: Error) {
        state.lock(); callbacks.fail(); encoderFailure = encoderFailure ?? error
        let notify = !faultDelivered; faultDelivered = true
        state.broadcast(); state.unlock()
        if notify { fault(token, error) }
    }
    private func emit(_ sample: CMSampleBuffer, duration: CMTime, submission: Policy.Submission) throws {
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
        output(CompressedUnit(buffer: retimed, isVideo: true, keyframe: keyframe, time: pts, duration: exactDuration, configuration: configuration), submission.token)
    }
    /// Checked terminal barrier for strict runtime. Even a failed prefix must
    /// orderly invalidate the native session; the error remains sticky.
    func finishChecked() throws {
        guard let current = session else {
            state.lock(); let failure = encoderFailure; state.unlock()
            if let failure { throw failure }; return
        }
        state.lock(); callbacks.closeInput(); state.unlock()
        defer {
            // Even a failed CompleteFrames must not race a still-delivering
            // callback's retained pending sample during actual native teardown.
            VTCompressionSessionInvalidate(current)
            session = nil
            state.lock()
            while callbacks.draining { state.wait() }
            state.unlock()
            pending = nil
        }
        _ = try sealForPrivacy()
    }
    func finish() {
        do { try finishChecked() } catch { latchFault(nil, error) }
    }
    deinit { if let session { VTCompressionSessionInvalidate(session) } }
}
