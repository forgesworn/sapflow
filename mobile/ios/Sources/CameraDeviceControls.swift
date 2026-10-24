import Foundation
import CoreMedia
import CoreVideo

/// Inspect borrowed native metadata only; never map, retain or copy image bytes.
enum CameraFrameGeometry {
    static func read(_ sample: CMSampleBuffer) -> (width: Int, height: Int)? {
        guard CMSampleBufferDataIsReady(sample), let image = CMSampleBufferGetImageBuffer(sample),
              let format = CMSampleBufferGetFormatDescription(sample),
              CMFormatDescriptionGetMediaType(format) == kCMMediaType_Video,
              CVPixelBufferGetPixelFormatType(image) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
              CVPixelBufferGetPlaneCount(image) == 2 else { return nil }
        let dimensions = CMVideoFormatDescriptionGetDimensions(format)
        let width = CVPixelBufferGetWidth(image), height = CVPixelBufferGetHeight(image)
        guard width > 0, height > 0, width == Int(dimensions.width), height == Int(dimensions.height) else { return nil }
        return (width, height)
    }
}


/// Validate the existing run timeline; source replacement never resets it or
/// rewrites a camera timestamp to conceal a regression.
struct CameraVideoTimeline {
    private var last: CMTime?
    private var switchBoundary: (last: CMTime, end: CMTime)?
    mutating func beginSourceSwitch(sealedDuration: CMTime) -> Bool {
        guard let last, sealedDuration.isNumeric, CMTimeCompare(sealedDuration, .zero) > 0 else { return false }
        switchBoundary = (last, CMTimeAdd(last, sealedDuration)); return true
    }
    mutating func accept(_ value: CMTime) -> Bool {
        guard value.isNumeric, CMTimeCompare(value, .zero) >= 0,
              last.map({ CMTimeCompare(value, $0) > 0 }) ?? true else { return false }
        if let boundary = switchBoundary {
            let gap = CMTimeSubtract(value, boundary.last)
            guard CMTimeCompare(value, boundary.end) >= 0, CMTimeCompare(gap, CMTime(value: 2, timescale: 1)) < 0 else { return false }
        }
        last = value; switchBoundary = nil; return true
    }
}


struct CameraNativeValues: Equatable {
    var zoom: Double = 1
    var focus = CameraAutomaticMode.automatic
    var exposure = CameraAutomaticMode.automatic
    var bias: Double = 0
    var whiteBalance = CameraAutomaticMode.automatic
}

/// Production uses this cold transaction as well as the host spies. Preflight
/// may fail without retiring the old source. After retirement, every failure
/// belongs to exact-show Stop; it must never silently resume the old source.
enum CameraSourceTransaction {
    enum Failure: Error { case retiredSourceLost }
    static func onSampleOwner<Source>(queue: DispatchQueue, retire: () throws -> Source?,
                                      seal: (Source) throws -> Void = { _ in },
                                      sealFailed: (Source, Error) -> Void = { _, _ in },
                                      requestKeyframe: (Source) -> Void) throws -> Source? {
        dispatchPrecondition(condition: .notOnQueue(queue))
        return try queue.sync {
            guard let source = try retire() else { return nil }
            do { try seal(source) }
            catch { sealFailed(source, error); throw error }
            requestKeyframe(source); return source
        }
    }
    /// Claim terminal ownership before crossing another queue or marking the
    /// recorder. Ordinary Stop may already own cleanup; cancellation must not
    /// turn its otherwise clean accepted prefix into a capture failure.
    @discardableResult static func claimFailure<Stop>(claim: () -> Stop?, markIncomplete: () -> Void,
                                                      stop: (Stop) -> Void) -> Bool {
        guard let ticket = claim() else { return false }
        markIncomplete(); stop(ticket); return true
    }
    static func perform<Candidate, Source>(preflight: () throws -> Candidate,
                                           retire: () throws -> Source?,
                                           isCurrent: (Source) -> Bool = { _ in true },
                                           retirementStarted: () -> Bool = { false },
                                           replace: (Candidate, Source) throws -> Void) throws -> Bool {
        let candidate = try preflight()
        guard let source = try retire() else {
            if retirementStarted() { throw Failure.retiredSourceLost }
            return false
        }
        guard isCurrent(source) else { throw Failure.retiredSourceLost }
        try replace(candidate, source)
        return true
    }
}

#if os(iOS)
import AVFoundation

/// Native device getters/setters are cold and control-queue owned. The immutable
/// delegate is allocated once per source, never once per captured frame.
final class CameraDeviceControls {
    struct Candidate {
        let device: AVCaptureDevice
        let input: AVCaptureDeviceInput
        let format: AVCaptureDevice.Format
    }
    struct Failure: LocalizedError { let errorDescription: String? }
    final class Delegate: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
        let source: CaptureAdmission.CameraSource
        let frame: (CaptureAdmission.CameraSource, CMSampleBuffer) -> Void
        let dropped: (CaptureAdmission.CameraSource) -> Void
        init(source: CaptureAdmission.CameraSource,
             frame: @escaping (CaptureAdmission.CameraSource, CMSampleBuffer) -> Void,
             dropped: @escaping (CaptureAdmission.CameraSource) -> Void) {
            self.source = source; self.frame = frame; self.dropped = dropped
        }
        func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) { frame(source, sampleBuffer) }
        func captureOutput(_ output: AVCaptureOutput, didDrop sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) { dropped(source) }
    }
    let source: CaptureAdmission.CameraSource
    let candidate: Candidate
    let output: AVCaptureVideoDataOutput
    private let delegate: Delegate
    init(candidate: Candidate, source: CaptureAdmission.CameraSource,
         frame: @escaping (CaptureAdmission.CameraSource, CMSampleBuffer) -> Void,
         dropped: @escaping (CaptureAdmission.CameraSource) -> Void) {
        self.candidate = candidate; self.source = source
        output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        delegate = Delegate(source: source, frame: frame, dropped: dropped)
    }
    static func preflight(position: CameraPosition, profile: CaptureProfile) throws -> Candidate {
        guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video,
                                                   position: position == .front ? .front : .back) else {
            throw Failure(errorDescription: "Selected camera is unavailable")
        }
        let rate = Double(profile.frameRate)
        guard let format = device.formats.prefix(512).first(where: {
            let dimensions = CMVideoFormatDescriptionGetDimensions($0.formatDescription)
            return dimensions.width == profile.landscapeWidth && dimensions.height == profile.landscapeHeight &&
                $0.videoSupportedFrameRateRanges.contains { $0.minFrameRate <= rate && $0.maxFrameRate >= rate }
        }) else { throw Failure(errorDescription: "Selected camera cannot provide the prepared dimensions and frame rate") }
        return Candidate(device: device, input: try AVCaptureDeviceInput(device: device), format: format)
    }
    /// Call inside the session configuration transaction, after adding input and
    /// output. Width/height and native rotation are explicit on iOS 16 and later.
    func configure(orientation: CaptureOrientation, profile: CaptureProfile) throws {
        let device = candidate.device
        try device.lockForConfiguration(); defer { device.unlockForConfiguration() }
        device.activeFormat = candidate.format
        device.activeVideoMinFrameDuration = CMTime(value: 1, timescale: profile.frameRate)
        device.activeVideoMaxFrameDuration = CMTime(value: 1, timescale: profile.frameRate)
        device.videoZoomFactor = device.minAvailableVideoZoomFactor
        if device.minExposureTargetBias <= 0 && device.maxExposureTargetBias >= 0 { device.setExposureTargetBias(0, completionHandler: nil) }
        if device.isFocusModeSupported(.continuousAutoFocus) { device.focusMode = .continuousAutoFocus }
        if device.isExposureModeSupported(.continuousAutoExposure) { device.exposureMode = .continuousAutoExposure }
        if device.isWhiteBalanceModeSupported(.continuousAutoWhiteBalance) { device.whiteBalanceMode = .continuousAutoWhiteBalance }
        guard let connection = output.connection(with: .video), connection.isVideoOrientationSupported else {
            throw Failure(errorDescription: "Camera cannot provide the prepared native orientation")
        }
        connection.videoOrientation = orientation.capture
        connection.automaticallyAdjustsVideoMirroring = false
        if connection.isVideoMirroringSupported { connection.isVideoMirrored = source.position.encodedMirrored }
        guard !connection.isVideoMirrored else { throw Failure(errorDescription: "Unmirrored camera output is unavailable") }
        guard output.availableVideoPixelFormatTypes.contains(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) else {
            throw Failure(errorDescription: "Selected camera cannot provide the prepared native pixel format")
        }
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                               kCVPixelBufferWidthKey as String: source.canvas.width,
                               kCVPixelBufferHeightKey as String: source.canvas.height]
    }
    func deliver(on queue: DispatchQueue) { output.setSampleBufferDelegate(delegate, queue: queue) }
    func pause() { output.setSampleBufferDelegate(nil, queue: nil) }
    var capabilities: CameraCapabilities {
        let device = candidate.device
        return CameraCapabilities(minimumZoom: Double(device.minAvailableVideoZoomFactor),
                                  maximumZoom: Double(device.maxAvailableVideoZoomFactor),
                                  focusAuto: device.isFocusModeSupported(.continuousAutoFocus),
                                  focusLock: device.isFocusModeSupported(.locked), focusPoint: device.isFocusPointOfInterestSupported,
                                  exposureAuto: device.isExposureModeSupported(.continuousAutoExposure),
                                  exposureLock: device.isExposureModeSupported(.locked), exposurePoint: device.isExposurePointOfInterestSupported,
                                  minimumBias: Double(device.minExposureTargetBias), maximumBias: Double(device.maxExposureTargetBias),
                                  whiteBalanceAuto: device.isWhiteBalanceModeSupported(.continuousAutoWhiteBalance),
                                  whiteBalanceLock: device.isWhiteBalanceModeSupported(.locked))
    }
    var values: CameraNativeValues {
        let device = candidate.device
        return CameraNativeValues(zoom: Double(device.videoZoomFactor), focus: device.focusMode == .locked ? .locked : .automatic,
                                  exposure: device.exposureMode == .locked ? .locked : .automatic,
                                  bias: Double(device.exposureTargetBias), whiteBalance: device.whiteBalanceMode == .locked ? .locked : .automatic)
    }
    func apply(_ command: CameraCommand, settled: @escaping () -> Void) throws {
        let device = candidate.device
        try device.lockForConfiguration()
        guard capabilities.accepts(command) else {
            device.unlockForConfiguration()
            throw Failure(errorDescription: "Camera control is unsupported or outside its current range")
        }
        var asynchronous = false
        switch command {
        case let .zoom(value): device.videoZoomFactor = CGFloat(value)
        case let .focus(mode): device.focusMode = mode == .automatic ? .continuousAutoFocus : .locked
        case let .focusPoint(point):
            device.focusPointOfInterest = CGPoint(x: point.x, y: point.y); device.focusMode = .continuousAutoFocus
        case let .exposure(mode): device.exposureMode = mode == .automatic ? .continuousAutoExposure : .locked
        case let .exposurePoint(point):
            device.exposurePointOfInterest = CGPoint(x: point.x, y: point.y); device.exposureMode = .continuousAutoExposure
        case let .bias(value): device.setExposureTargetBias(Float(value)) { _ in settled() }; asynchronous = true
        case let .whiteBalance(mode): device.whiteBalanceMode = mode == .automatic ? .continuousAutoWhiteBalance : .locked
        }
        device.unlockForConfiguration()
        if !asynchronous { settled() }
    }
}
#endif
