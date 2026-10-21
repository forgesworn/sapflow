import XCTest
@testable import SapflowBroadcaster

final class CaptureOrientationTests: XCTestCase {
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
