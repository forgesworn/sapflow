import Foundation

/// Describes the encoded, upright pixel canvas, not a display-only transform.
enum CaptureOrientation: String, CaseIterable {
    case portrait, portraitUpsideDown, landscapeLeft, landscapeRight
    enum DevicePose { case portrait, portraitUpsideDown, landscapeLeft, landscapeRight, faceUp, faceDown, unknown }
    static func fromDevicePose(_ pose: DevicePose) -> CaptureOrientation? {
        switch pose {
        case .portrait: return .portrait
        case .portraitUpsideDown: return .portraitUpsideDown
        // Device and interface/capture landscape conventions are opposite.
        case .landscapeLeft: return .landscapeRight
        case .landscapeRight: return .landscapeLeft
        case .faceUp, .faceDown, .unknown: return nil
        }
    }
    var isPortrait: Bool { self == .portrait || self == .portraitUpsideDown }
    var width: Int32 { isPortrait ? 720 : 1280 }
    var height: Int32 { isPortrait ? 1280 : 720 }
    var label: String { isPortrait ? "portrait" : "landscape" }
}

/// Coalesces rotations during finalisation. Cancellation/timeout fences a late
/// completion so it cannot unexpectedly turn the camera or publisher back on.
struct OrientationRestartPlan {
    private(set) var desired: CaptureOrientation = .portrait
    private(set) var pending: UInt64?
    private var generation: UInt64 = 0
    mutating func request(_ orientation: CaptureOrientation, running: Bool) -> UInt64? {
        guard desired != orientation else { return nil }
        desired = orientation
        guard running, pending == nil else { return nil }
        generation &+= 1
        pending = generation
        return generation
    }
    mutating func cancel() { pending = nil; generation &+= 1 }
    mutating func complete(_ token: UInt64) -> CaptureOrientation? {
        guard pending == token else { return nil }
        pending = nil
        return desired
    }
}

#if canImport(UIKit)
import UIKit
import AVFoundation

extension CaptureOrientation {
    static func device(_ orientation: UIDeviceOrientation) -> CaptureOrientation? {
        switch orientation {
        case .portrait: return fromDevicePose(.portrait)
        case .portraitUpsideDown: return fromDevicePose(.portraitUpsideDown)
        case .landscapeLeft: return fromDevicePose(.landscapeLeft)
        case .landscapeRight: return fromDevicePose(.landscapeRight)
        default: return nil
        }
    }
    static func interface(_ orientation: UIInterfaceOrientation) -> CaptureOrientation? {
        switch orientation {
        case .portrait: return .portrait
        case .portraitUpsideDown: return .portraitUpsideDown
        case .landscapeLeft: return .landscapeLeft
        case .landscapeRight: return .landscapeRight
        default: return nil
        }
    }
    var capture: AVCaptureVideoOrientation {
        switch self {
        case .portrait: return .portrait
        case .portraitUpsideDown: return .portraitUpsideDown
        case .landscapeLeft: return .landscapeLeft
        case .landscapeRight: return .landscapeRight
        }
    }
}
#endif
