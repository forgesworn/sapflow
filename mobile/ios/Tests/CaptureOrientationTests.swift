import XCTest
@testable import SapflowBroadcaster

final class CaptureOrientationTests: XCTestCase {
    func testFixedPhysicalChoicesResolveToExistingNativeCaptureConvention() throws {
        XCTAssertEqual(CaptureOrientationChoice.allCases.count, 4)
        XCTAssertNil(CaptureOrientationChoice.automatic.fixedOrientation)
        XCTAssertEqual(CaptureOrientationChoice.portrait.fixedOrientation, .portrait)
        XCTAssertEqual(CaptureOrientationChoice.landscapeTopLeft.fixedOrientation, CaptureOrientation.fromDevicePose(.landscapeLeft))
        XCTAssertEqual(CaptureOrientationChoice.landscapeTopRight.fixedOrientation, CaptureOrientation.fromDevicePose(.landscapeRight))
        XCTAssertEqual(CaptureOrientationChoice.landscapeTopLeft.label, "Landscape (top left)")
        XCTAssertEqual(CaptureOrientationChoice.landscapeTopRight.label, "Landscape (top right)")
        for choice in CaptureOrientationChoice.allCases.filter({ $0 != .automatic }) {
            let orientation = try XCTUnwrap(choice.fixedOrientation)
            for profile in CaptureProfile.allCases {
                let dimensions = profile.dimensions(for: orientation)
                XCTAssertEqual(dimensions.width < dimensions.height, choice == .portrait)
            }
        }
    }
    func testNewPreparationCannotInheritOldPendingRotationOrCanvas() throws {
        var plan = OrientationRestartPlan()
        let old = try XCTUnwrap(plan.request(.landscapeLeft, running: true))
        plan.prepare(.landscapeRight)
        XCTAssertEqual(plan.desired, .landscapeRight); XCTAssertNil(plan.pending)
        XCTAssertNil(plan.complete(old))
        plan.prepare(.portrait)
        XCTAssertEqual(plan.desired, .portrait); XCTAssertNil(plan.pending)
    }
    func testPhysicalPoseMappingAndEncodedDimensions() {
        XCTAssertEqual(CaptureOrientation.fromDevicePose(.portrait), .portrait)
        XCTAssertEqual(CaptureOrientation.fromDevicePose(.portraitUpsideDown), .portraitUpsideDown)
        XCTAssertEqual(CaptureOrientation.fromDevicePose(.landscapeLeft), .landscapeRight)
        XCTAssertEqual(CaptureOrientation.fromDevicePose(.landscapeRight), .landscapeLeft)
        for orientation in CaptureOrientation.allCases {
            XCTAssertEqual(orientation.width * orientation.height, 1280 * 720)
            XCTAssertEqual(orientation.width < orientation.height, orientation.isPortrait)
        }
        for pose in [CaptureOrientation.DevicePose.faceUp, .faceDown, .unknown] {
            XCTAssertNil(CaptureOrientation.fromDevicePose(pose))
        }
    }
    func testPreparedOrientationChangesDoNotStartPublisher() {
        var plan = OrientationRestartPlan()
        XCTAssertNil(plan.request(.landscapeLeft, running: false))
        XCTAssertEqual(plan.desired, .landscapeLeft)
        XCTAssertNil(plan.pending)
    }
    func testRotationsCoalesceIntoOneNewEpochUsingLatestPose() throws {
        var plan = OrientationRestartPlan()
        let token = try XCTUnwrap(plan.request(.landscapeRight, running: true))
        XCTAssertNil(plan.request(.portraitUpsideDown, running: false))
        XCTAssertNil(plan.request(.landscapeLeft, running: false))
        XCTAssertEqual(plan.pending, token)
        XCTAssertEqual(plan.complete(token), .landscapeLeft)
        XCTAssertNil(plan.complete(token))
    }
    func testStopOrTimeoutFencesLateCompletionAndNextSession() throws {
        var plan = OrientationRestartPlan()
        let old = try XCTUnwrap(plan.request(.landscapeLeft, running: true))
        plan.cancel()
        XCTAssertNil(plan.complete(old))
        let next = try XCTUnwrap(plan.request(.portrait, running: true))
        XCTAssertNotEqual(old, next)
        XCTAssertNil(plan.complete(old))
        XCTAssertEqual(plan.complete(next), .portrait)
    }
}
