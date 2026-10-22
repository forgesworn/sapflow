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

/// User choice names a physical pose, viewed from the screen. AVCapture's
/// landscape enum names are opposite to UIDevice's physical landscape names.
enum CaptureOrientationChoice: String, CaseIterable, Identifiable {
    case automatic = "auto", portrait, landscapeTopLeft, landscapeTopRight
    var id: String { rawValue }
    var label: String {
        switch self {
        case .automatic: return "Automatic"
        case .portrait: return "Portrait"
        case .landscapeTopLeft: return "Landscape (top left)"
        case .landscapeTopRight: return "Landscape (top right)"
        }
    }
    var fixedOrientation: CaptureOrientation? {
        switch self {
        case .automatic: return nil
        case .portrait: return .portrait
        case .landscapeTopLeft: return .landscapeRight
        case .landscapeTopRight: return .landscapeLeft
        }
    }
}

/// Coalesces rotations during finalisation. Cancellation/timeout fences a late
/// completion so it cannot unexpectedly turn the camera or publisher back on.
struct OrientationRestartPlan {
    private(set) var desired: CaptureOrientation = .portrait
    private(set) var pending: UInt64?
    private var generation: UInt64 = 0
    /// A new exact preparation establishes its own initial canvas. This never
    /// starts a publisher or carries a retired show's pending rotation token.
    mutating func prepare(_ orientation: CaptureOrientation) {
        cancel(); desired = orientation
    }
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
