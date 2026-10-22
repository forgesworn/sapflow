/// Immutable capture intent selected before Prepare. A grant is a ceiling;
/// insufficient capacity never selects a different profile automatically.
enum CaptureProfile: String, CaseIterable {
    case sd480p30 = "480p30"
    case hd720p30 = "720p30"
    case fullHD1080p30 = "1080p30"

    struct Dimensions: Equatable {
        let width: Int32
        let height: Int32
    }

    static let defaultProfile: CaptureProfile = .hd720p30

    var landscapeWidth: Int32 {
        switch self {
        case .sd480p30: return 640
        case .hd720p30: return 1280
        case .fullHD1080p30: return 1920
        }
    }
    var landscapeHeight: Int32 {
        switch self {
        case .sd480p30: return 480
        case .hd720p30: return 720
        case .fullHD1080p30: return 1080
        }
    }
    var videoBps: UInt64 {
        switch self {
        case .sd480p30: return 600_000
        case .hd720p30: return 2_500_000
        case .fullHD1080p30: return 5_000_000
        }
    }
    var audioBps: UInt64 { 96_000 }
    var frameRate: Int32 { 30 }
    var gopFrames: Int { 30 }
    var gopSeconds: Int { 1 }
    var elementaryBps: UInt64 { videoBps + audioBps }
    var minimumPublishBps: UInt64 {
        switch self {
        case .sd480p30: return 1_000_000
        case .hd720p30: return 4_000_000
        case .fullHD1080p30: return 8_000_000
        }
    }
    /// Engineering admission allowance above target H.264 + AAC payload.
    /// This is not a guaranteed peak, TS overhead or retransmission budget.
    var transportHeadroomBps: UInt64 { minimumPublishBps - elementaryBps }
    /// Capacity testing alone does not qualify a supported product preset.
    var isEngineeringOnly: Bool { self == .fullHD1080p30 }
    var label: String { "\(rawValue) · \(landscapeWidth)×\(landscapeHeight)" }

    func dimensions(for orientation: CaptureOrientation) -> Dimensions {
        orientation.isPortrait
            ? Dimensions(width: landscapeHeight, height: landscapeWidth)
            : Dimensions(width: landscapeWidth, height: landscapeHeight)
    }
    func requireGrant(maxPublishBps: UInt64) throws {
        guard maxPublishBps >= minimumPublishBps else { throw CaptureProfileFailure.insufficientPublishCap }
    }
}

enum CaptureProfileFailure: Error, Equatable { case insufficientPublishCap }

/// Frozen before permission, grant admission or native owner construction.
/// Audio-only has no video profile, dimensions or orientation epoch.
enum CaptureIntent: Equatable, Hashable, CaseIterable, RawRepresentable {
    case audioVideo(CaptureProfile)
    case audioOnly48kMono

    static let defaultProfile: Self = .audioVideo(.defaultProfile)
    static let allCases: [Self] = CaptureProfile.allCases.map(Self.audioVideo) + [.audioOnly48kMono]
    // Camera quality names remain convenient at existing typed call sites.
    static let sd480p30: Self = .audioVideo(.sd480p30)
    static let hd720p30: Self = .audioVideo(.hd720p30)
    static let fullHD1080p30: Self = .audioVideo(.fullHD1080p30)

    init?(rawValue: String) {
        if rawValue == "audio-48k-mono" { self = .audioOnly48kMono }
        else if let video = CaptureProfile(rawValue: rawValue) { self = .audioVideo(video) }
        else { return nil }
    }
    var rawValue: String { videoProfile?.rawValue ?? "audio-48k-mono" }
    var videoProfile: CaptureProfile? {
        if case let .audioVideo(profile) = self { return profile }
        return nil
    }
    var hasVideo: Bool { videoProfile != nil }
    var hasAudio: Bool { true }
    var audioBps: UInt64 { 96_000 }
    var elementaryBps: UInt64 { (videoProfile?.videoBps ?? 0) + audioBps }
    /// Existing engineering admission allowance, not an audio wire-rate claim.
    /// Qualify lower caps with the actual native TS/pacing path separately.
    var minimumPublishBps: UInt64 { videoProfile?.minimumPublishBps ?? 1_000_000 }
    var isEngineeringOnly: Bool { videoProfile?.isEngineeringOnly ?? false }
    var label: String { videoProfile?.label ?? "Audio only · 48 kHz mono" }
    var requiresCameraPermission: Bool { hasVideo }
    func requireGrant(maxPublishBps: UInt64) throws {
        guard maxPublishBps >= minimumPublishBps else { throw CaptureProfileFailure.insufficientPublishCap }
    }
}

#if canImport(AVFoundation)
import AVFoundation

extension CaptureProfile {
    /// Native camera output, not a software upscale or rescale. The caller
    /// must still admit the actual camera format, frame rate and hardware
    /// encoder for the prepared dimensions; no fallback is implied.
    var sessionPreset: AVCaptureSession.Preset {
        switch self {
        case .sd480p30: return .vga640x480
        case .hd720p30: return .hd1280x720
        case .fullHD1080p30: return .hd1920x1080
        }
    }
}
#endif
