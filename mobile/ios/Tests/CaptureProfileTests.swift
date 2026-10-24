import XCTest
@testable import SapflowBroadcaster
#if canImport(AVFoundation)
import AVFoundation
#endif

final class CaptureProfileTests: XCTestCase {
    func testAudioOnlyIntentHasNoCameraAndRequiresItsExplicitGrantCeiling() {
        let intent = CaptureIntent.audioOnly48kMono
        XCTAssertNil(intent.videoProfile); XCTAssertFalse(intent.hasVideo)
        XCTAssertTrue(intent.hasAudio); XCTAssertFalse(intent.requiresCameraPermission)
        XCTAssertEqual(intent.audioBps, 96_000); XCTAssertEqual(intent.elementaryBps, 96_000)
        XCTAssertEqual(intent.minimumPublishBps, 1_000_000)
        XCTAssertNoThrow(try intent.requireGrant(maxPublishBps: 1_000_000))
        XCTAssertThrowsError(try intent.requireGrant(maxPublishBps: 999_999))
        XCTAssertEqual(intent, .audioOnly48kMono)
        XCTAssertEqual(CaptureIntent.defaultProfile, .audioVideo(.hd720p30))
    }
    func testCaptureIntentPersistencePreservesCameraIDsAndRejectsAmbiguousAudioIDs() {
        for intent in CaptureIntent.allCases {
            XCTAssertEqual(CaptureIntent(rawValue: intent.rawValue), intent)
        }
        XCTAssertEqual(CaptureIntent.audioOnly48kMono.rawValue, "audio-48k-mono")
        for invalid in ["audio", "audio-only", "audio-48k-stereo", "audio-48k-mono ", "AUDIO-48K-MONO"] {
            XCTAssertNil(CaptureIntent(rawValue: invalid))
        }
        XCTAssertEqual(CaptureIntent(rawValue: "720p30"), .audioVideo(.hd720p30))
    }
    func testCommonDefaultPreservesExisting720EncodingTargets() {
        let profile = CaptureProfile.defaultProfile
        XCTAssertEqual(profile, .hd720p30)
        XCTAssertEqual(profile.videoBps, 2_500_000)
        XCTAssertEqual(profile.audioBps, 96_000)
        XCTAssertEqual(profile.minimumPublishBps, 4_000_000)
        XCTAssertEqual(profile.dimensions(for: .landscapeLeft), .init(width: 1280, height: 720))
        XCTAssertEqual(profile.dimensions(for: .portrait), .init(width: 720, height: 1280))
    }
    func testExplicitRatesCadenceAndEngineeringAdmissionMargins() {
        let expected: [(CaptureProfile, UInt64, UInt64, UInt64)] = [
            (.sd480p30, 600_000, 696_000, 304_000),
            (.hd720p30, 2_500_000, 2_596_000, 1_404_000),
            (.fullHD1080p30, 5_000_000, 5_096_000, 2_904_000)
        ]
        for (profile, video, payload, margin) in expected {
            XCTAssertEqual(profile.videoBps, video)
            XCTAssertEqual(profile.audioBps, 96_000)
            XCTAssertEqual(profile.elementaryBps, payload)
            XCTAssertEqual(profile.transportHeadroomBps, margin)
            XCTAssertEqual(profile.elementaryBps + profile.transportHeadroomBps, profile.minimumPublishBps)
            XCTAssertEqual(profile.frameRate, 30)
            XCTAssertEqual(profile.gopFrames, Int(profile.frameRate) * profile.gopSeconds)
            XCTAssertEqual(profile.gopSeconds, 1)
        }
    }
    func testGrantAcceptsExactCapAndRejectsOneBitBelowIt() {
        for profile in CaptureProfile.allCases {
            XCTAssertNoThrow(try profile.requireGrant(maxPublishBps: profile.minimumPublishBps))
            XCTAssertNoThrow(try profile.requireGrant(maxPublishBps: UInt64.max))
            for cap in [UInt64(0), profile.minimumPublishBps - 1] {
                XCTAssertThrowsError(try profile.requireGrant(maxPublishBps: cap)) { error in
                    XCTAssertEqual(error as? CaptureProfileFailure, .insufficientPublishCap)
                }
            }
        }
    }
    func testInsufficientGrantNeverDowngradesSelectedProfile() {
        let selected = CaptureProfile.hd720p30
        XCTAssertThrowsError(try selected.requireGrant(maxPublishBps: 1_000_000))
        XCTAssertEqual(selected, .hd720p30)
        XCTAssertEqual(selected.dimensions(for: .portrait), .init(width: 720, height: 1280))
        XCTAssertNoThrow(try CaptureProfile.sd480p30.requireGrant(maxPublishBps: 1_000_000))
        XCTAssertThrowsError(try CaptureProfile.fullHD1080p30.requireGrant(maxPublishBps: 4_000_000))
    }
    func testEveryCardinalPoseHasExactNativeProfileDimensions() {
        let expected: [(CaptureProfile, Int32, Int32)] = [
            (.sd480p30, 640, 480), (.hd720p30, 1280, 720), (.fullHD1080p30, 1920, 1080)
        ]
        for (profile, width, height) in expected {
            XCTAssertEqual(profile.landscapeWidth, width)
            XCTAssertEqual(profile.landscapeHeight, height)
            for orientation in [CaptureOrientation.portrait, .portraitUpsideDown] {
                XCTAssertEqual(profile.dimensions(for: orientation), .init(width: height, height: width))
            }
            for orientation in [CaptureOrientation.landscapeLeft, .landscapeRight] {
                XCTAssertEqual(profile.dimensions(for: orientation), .init(width: width, height: height))
            }
        }
    }
    func testNative480AspectDiffersFromAndroidCanvasWithoutRescaling() {
        let dimensions = CaptureProfile.sd480p30.dimensions(for: .landscapeRight)
        XCTAssertEqual(dimensions.width * 3, dimensions.height * 4)
        XCTAssertEqual(dimensions.width, 640)
        XCTAssertNotEqual(dimensions.width, 848)
        XCTAssertNotEqual(dimensions.width * 9, dimensions.height * 16)
        let hd = CaptureProfile.hd720p30.dimensions(for: .landscapeRight)
        XCTAssertEqual(hd.width * 9, hd.height * 16)
    }
    func testOnly1080IsMarkedEngineeringAndNotCommonDefault() {
        XCTAssertFalse(CaptureProfile.sd480p30.isEngineeringOnly)
        XCTAssertFalse(CaptureProfile.hd720p30.isEngineeringOnly)
        XCTAssertTrue(CaptureProfile.fullHD1080p30.isEngineeringOnly)
        XCTAssertNotEqual(CaptureProfile.defaultProfile, .fullHD1080p30)
    }
    func testExactIdentifiersAndDimensionLabelsDoNotInferFallback() {
        XCTAssertEqual(CaptureProfile.allCases.map(\.rawValue), ["480p30", "720p30", "1080p30"])
        for invalid in ["", "720p", "HD_720", "720P30", "720p30 ", "1080p60", "auto"] {
            XCTAssertNil(CaptureProfile(rawValue: invalid))
        }
        XCTAssertEqual(CaptureProfile.sd480p30.label, "480p30 · 640×480")
        XCTAssertEqual(CaptureProfile.hd720p30.label, "720p30 · 1280×720")
        XCTAssertEqual(CaptureProfile.fullHD1080p30.label, "1080p30 · 1920×1080")
    }
    func testNativeCameraPresetsMatchTheExactCanvasWithoutFallback() {
        #if canImport(AVFoundation)
        XCTAssertEqual(CaptureProfile.sd480p30.sessionPreset, AVCaptureSession.Preset.vga640x480)
        XCTAssertEqual(CaptureProfile.hd720p30.sessionPreset, AVCaptureSession.Preset.hd1280x720)
        XCTAssertEqual(CaptureProfile.fullHD1080p30.sessionPreset, AVCaptureSession.Preset.hd1920x1080)
        #else
        XCTFail("This gate requires the actual Apple AVFoundation SDK")
        #endif
    }
}
