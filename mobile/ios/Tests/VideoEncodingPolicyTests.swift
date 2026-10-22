import CoreMedia
import CoreVideo
import XCTest
@testable import SapflowBroadcaster

final class VideoEncodingPolicyTests: XCTestCase {
    private struct Token: Equatable { let run: Int; let revision: Int; let synthetic: Bool }
    private let raw = Token(run: 1, revision: 0, synthetic: false)
    private let slate = Token(run: 1, revision: 1, synthetic: true)
    private func time(_ value: Int64) -> CMTime { CMTime(value: value, timescale: 30) }

    func testFixedCreditBoundsRejectUnsupportedCapacity() {
        XCTAssertThrowsError(try VideoEncodingPolicy<Int>(capacity: 0))
        XCTAssertThrowsError(try VideoEncodingPolicy<Int>(capacity: 129))
        XCTAssertNoThrow(try VideoEncodingPolicy<Int>(capacity: 128))
    }
    func testOriginalTokenSurvivesNativeDelayAndPrivacyRevision() throws {
        var queue = try VideoCallbackQueue<Token, String>()
        let old = try queue.reserve(token: raw, time: time(0))
        let fresh = try queue.reserve(token: slate, time: time(1))
        try queue.complete(fresh.serial, time: time(1), frame: "slate")
        XCTAssertTrue(queue.electDrainer()); XCTAssertNil(queue.takeNext())
        try queue.complete(old.serial, time: time(0), frame: "raw-prefix")
        XCTAssertTrue(queue.electDrainer())
        let prefix = try XCTUnwrap(queue.takeNext())
        XCTAssertEqual(prefix.0.token, raw); XCTAssertEqual(prefix.1, "raw-prefix")
        let replacement = try XCTUnwrap(queue.takeNext())
        XCTAssertEqual(replacement.0.token, slate); XCTAssertEqual(replacement.1, "slate")
        XCTAssertNil(queue.takeNext()); XCTAssertTrue(queue.quiescent)
    }
    func testInlineCallbackMayCompleteBeforeNativeSubmitReturns() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        let original = try queue.reserve(token: raw, time: time(0))
        try queue.complete(original.serial, time: original.time, frame: 7)
        XCTAssertTrue(queue.electDrainer())
        let output = try XCTUnwrap(queue.takeNext())
        XCTAssertEqual(output.0, original); XCTAssertEqual(output.1, 7)
        XCTAssertNil(queue.takeNext()); XCTAssertTrue(queue.quiescent)
    }
    func testCompletedCreditIsNotReusableUntilActualDelivery() throws {
        var policy = try VideoEncodingPolicy<Token>(capacity: 1)
        let submission = try policy.reserve(token: raw, time: time(0))
        try policy.complete(submission.serial, time: time(0))
        XCTAssertEqual(policy.count, 1)
        XCTAssertThrowsError(try policy.reserve(token: slate, time: time(1)))
        XCTAssertTrue(policy.failed)
        XCTAssertEqual(policy.takeNext()?.token, raw)
        XCTAssertTrue(policy.quiescent)
    }
    func testRepeatedNativeCallbackFailsStickyWithoutReleasingSuccessor() throws {
        var policy = try VideoEncodingPolicy<Token>(capacity: 1)
        let old = try policy.reserve(token: raw, time: time(0))
        try policy.complete(old.serial, time: old.time)
        XCTAssertNotNil(policy.takeNext())
        let successor = try policy.reserve(token: slate, time: time(1))
        XCTAssertThrowsError(try policy.complete(old.serial, time: old.time))
        XCTAssertEqual(policy.submission(successor.serial), successor)
        XCTAssertEqual(policy.count, 1); XCTAssertTrue(policy.failed)
    }
    func testDuplicateBeforeDeliveryCannotConsumeTwoCredits() throws {
        var policy = try VideoEncodingPolicy<Token>()
        let original = try policy.reserve(token: raw, time: time(0))
        try policy.complete(original.serial, time: original.time)
        XCTAssertThrowsError(try policy.complete(original.serial, time: original.time))
        XCTAssertEqual(policy.count, 1)
        XCTAssertEqual(policy.takeNext(), original); XCTAssertNil(policy.takeNext())
    }
    func testUnknownAndNilRefconCannotUseCurrentOwner() throws {
        var policy = try VideoEncodingPolicy<Token>()
        let original = try policy.reserve(token: raw, time: time(0))
        XCTAssertNil(policy.submission(0))
        XCTAssertThrowsError(try policy.complete(0, time: time(0)))
        XCTAssertEqual(policy.submission(original.serial)?.token, raw)
        XCTAssertTrue(policy.failed)
    }
    func testWrongPTSSettlesOriginalCreditButFailsClosed() throws {
        var policy = try VideoEncodingPolicy<Token>()
        let original = try policy.reserve(token: raw, time: time(0))
        XCTAssertThrowsError(try policy.complete(original.serial, time: time(1)))
        XCTAssertTrue(policy.failed); XCTAssertEqual(policy.takeNext(), original)
        XCTAssertTrue(policy.quiescent)
        XCTAssertThrowsError(try policy.reserve(token: slate, time: time(2)))
    }
    func testEquivalentRationalCallbackPTSIsAccepted() throws {
        var policy = try VideoEncodingPolicy<Token>()
        let original = try policy.reserve(token: raw, time: time(1))
        try policy.complete(original.serial, time: CMTime(value: 3_000, timescale: 90_000))
        XCTAssertEqual(policy.takeNext(), original)
    }
    func testOrdinaryStopClosesNewInputButAllowsActualAcceptedPrefix() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        let prefix = try queue.reserve(token: raw, time: time(0))
        queue.closeInput()
        XCTAssertFalse(queue.failed)
        XCTAssertThrowsError(try queue.reserve(token: slate, time: time(1)))
        try queue.complete(prefix.serial, time: prefix.time, frame: 1)
        XCTAssertTrue(queue.electDrainer())
        XCTAssertEqual(queue.takeNext()?.0.token, raw)
        XCTAssertNil(queue.takeNext()); XCTAssertTrue(queue.quiescent); XCTAssertFalse(queue.failed)
    }
    func testNativeFaultBeforeStopRemainsStickyAndCreditsStillClose() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        let accepted = try queue.reserve(token: raw, time: time(0))
        queue.fail(); queue.closeInput()
        try queue.complete(accepted.serial, time: nil, frame: nil)
        XCTAssertTrue(queue.electDrainer())
        XCTAssertEqual(queue.takeNext()?.0.token, raw); XCTAssertNil(queue.takeNext())
        XCTAssertTrue(queue.quiescent); XCTAssertTrue(queue.failed)
    }
    func testOneInlineDrainerKeepsCreditsUntilQuiescence() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        let a = try queue.reserve(token: raw, time: time(0))
        let b = try queue.reserve(token: slate, time: time(1))
        try queue.complete(a.serial, time: a.time, frame: 1)
        XCTAssertTrue(queue.electDrainer())
        try queue.complete(b.serial, time: b.time, frame: 2)
        XCTAssertFalse(queue.electDrainer())
        XCTAssertEqual(queue.takeNext()?.1, 1); XCTAssertEqual(queue.takeNext()?.1, 2)
        XCTAssertFalse(queue.quiescent) // callback drainer has not relinquished
        XCTAssertNil(queue.takeNext()); XCTAssertTrue(queue.quiescent)
    }
    func testOutstandingNativeFrameCannotBecomeSuccessfulDrainReceipt() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        _ = try queue.reserve(token: raw, time: time(0))
        XCTAssertFalse(queue.quiescent)
        XCTAssertTrue(queue.electDrainer()); XCTAssertNil(queue.takeNext())
        XCTAssertFalse(queue.quiescent)
    }
    func testSerialExhaustionNeverReusesRefcon() throws {
        var policy = try VideoEncodingPolicy<Token>(startingSerial: UInt64(Int.max) - 1)
        let last = try policy.reserve(token: raw, time: time(0))
        XCTAssertEqual(last.serial, UInt64(Int.max))
        try policy.complete(last.serial, time: last.time); XCTAssertNotNil(policy.takeNext())
        XCTAssertThrowsError(try policy.reserve(token: slate, time: time(1)))
        XCTAssertTrue(policy.failed)
    }
    func testInvalidAndRepeatedInputTimeFailClosed() throws {
        for invalid in [CMTime.invalid, .positiveInfinity, CMTime(value: -1, timescale: 30)] {
            var policy = try VideoEncodingPolicy<Token>()
            XCTAssertThrowsError(try policy.reserve(token: raw, time: invalid)); XCTAssertTrue(policy.failed)
        }
        var policy = try VideoEncodingPolicy<Token>()
        _ = try policy.reserve(token: raw, time: time(0))
        XCTAssertThrowsError(try policy.reserve(token: raw, time: time(0)))
    }
    func testHalfSecondSourceGapSucceedsAndTwoSecondsFailsEvenAfterSealing() throws {
        var policy = try VideoEncodingPolicy<Token>()
        let old = try policy.reserve(token: raw, time: time(0))
        try policy.complete(old.serial, time: old.time); XCTAssertNotNil(policy.takeNext())
        XCTAssertNoThrow(try policy.reserve(token: slate, time: time(15)))
        var excessive = try VideoEncodingPolicy<Token>()
        let prefix = try excessive.reserve(token: raw, time: time(0))
        try excessive.complete(prefix.serial, time: prefix.time); XCTAssertNotNil(excessive.takeNext())
        XCTAssertThrowsError(try excessive.reserve(token: slate, time: time(60)))
        XCTAssertTrue(excessive.failed)
    }
    func testNominalSealCannotOverlapNewSourceAndDoesNotRetime() throws {
        let last = time(30)
        let end = try VideoEncodingTiming.sealedEnd(last: last, frameRate: 30)
        XCTAssertEqual(end, time(31))
        XCTAssertThrowsError(try VideoEncodingTiming.requireNoOverlap(CMTime(value: 101, timescale: 100), sealedEnd: end))
        XCTAssertNoThrow(try VideoEncodingTiming.requireNoOverlap(end, sealedEnd: end))
        XCTAssertNoThrow(try VideoEncodingTiming.requireNoOverlap(time(45), sealedEnd: end))
        XCTAssertEqual(last, time(30))
    }
    func testInitialEmptyLedgerIsQuiescentWithoutInventedPicture() throws {
        var queue = try VideoCallbackQueue<Token, Int>()
        XCTAssertTrue(queue.quiescent); XCTAssertNil(queue.takeNext())
        XCTAssertNoThrow(try VideoEncodingTiming.requireNoOverlap(.zero, sealedEnd: nil))
    }
    func testSlateUsesImmutablePreparedNV12AndOriginalNeutralMarking() throws {
        let slate = try PreparedVideoSlate(width: 640, height: 480)
        var identity: CVPixelBuffer?
        try slate.withImage { image in
            identity = image
            XCTAssertEqual(CVPixelBufferGetWidth(image), 640); XCTAssertEqual(CVPixelBufferGetHeight(image), 480)
            XCTAssertEqual(CVPixelBufferGetPixelFormatType(image), kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
            XCTAssertEqual(CVPixelBufferLockBaseAddress(image, .readOnly), kCVReturnSuccess)
            defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
            let y = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 0)).assumingMemoryBound(to: UInt8.self)
            let uv = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 1)).assumingMemoryBound(to: UInt8.self)
            XCTAssertEqual(y[0], 16); XCTAssertEqual(uv[0], 128)
            let offset = 240 * CVPixelBufferGetBytesPerRowOfPlane(image, 0) + 320 - 2 * (640 / 24)
            XCTAssertEqual(y[offset], 160)
        }
        slate.withImage { XCTAssertTrue($0 === identity) }
    }
    func testSlateRefusesUnsupportedDimensionsBeforeNativeAllocation() {
        for size in [(0, 480), (641, 480), (640, 479), (4098, 480)] {
            XCTAssertThrowsError(try PreparedVideoSlate(width: size.0, height: size.1))
        }
    }
}
