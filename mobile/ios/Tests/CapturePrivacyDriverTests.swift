import XCTest
import CoreMedia
@testable import SapflowBroadcaster

final class CapturePrivacyDriverTests: XCTestCase {
    func testRationalVideoAndAudioCadenceDoesNotAccumulateRoundedDrift() throws {
        var cadence = try CapturePrivacyCadence(nextVideoFrame: 0)
        var pictures = 0
        for tick in 0...1_000 {
            let plan = try cadence.plan(elapsedNs: UInt64(tick) * 10_000_000, video: true)
            pictures += plan.videoFrames.count
            XCTAssertEqual(plan.audioFramesThrough, UInt64(tick) * 480)
        }
        XCTAssertEqual(pictures, 301)
    }
    func testAudioOnlyNeverProducesAPictureOrNeedsACameraPose() throws {
        var cadence = try CapturePrivacyCadence(nextVideoFrame: 0)
        let plan = try cadence.plan(elapsedNs: 123_456_789, video: false)
        XCTAssertTrue(plan.videoFrames.isEmpty)
        XCTAssertEqual(plan.audioFramesThrough, 5_925)
    }
    func testWithheldTimerCannotHideAStallOrResetItsClock() throws {
        var cadence = try CapturePrivacyCadence(nextVideoFrame: 0)
        _ = try cadence.plan(elapsedNs: 0, video: true)
        XCTAssertThrowsError(try cadence.plan(elapsedNs: 500_000_000, video: true))
        var audio = try CapturePrivacyCadence(nextVideoFrame: 0)
        _ = try audio.plan(elapsedNs: 0, video: false)
        XCTAssertThrowsError(try audio.plan(elapsedNs: 200_000_001, video: false))
    }
    func testRollbackAndCounterOverflowAreRefused() throws {
        XCTAssertThrowsError(try CapturePrivacyCadence(nextVideoFrame: -1))
        var cadence = try CapturePrivacyCadence(nextVideoFrame: 0)
        _ = try cadence.plan(elapsedNs: 100, video: false)
        XCTAssertThrowsError(try cadence.plan(elapsedNs: 99, video: false))
        XCTAssertThrowsError(try cadence.plan(elapsedNs: UInt64.max, video: false))
    }
    func testResumeBridgeUsesOriginalActualPTSAndExposesQuantization() throws {
        let actual = CMTime(value: 1_000_001, timescale: 1_000_000)
        let next = CMTime(value: 1, timescale: 1)
        let plan = try XCTUnwrap(CaptureResumeBridge.plan(actualPTS: actual, nextInputTime: next, sourceRate: 48_000))
        XCTAssertEqual(plan.zeroFrames, 0)
        XCTAssertEqual(CMTimeCompare(plan.residual, CMTime(value: 1, timescale: 1_000_000)), 0)
        let exact = try XCTUnwrap(CaptureResumeBridge.plan(actualPTS: CMTime(value: 1_024, timescale: 44_100), nextInputTime: .zero, sourceRate: 44_100))
        XCTAssertEqual(exact.zeroFrames, 1_024)
        XCTAssertEqual(CMTimeCompare(exact.residual, .zero), 0)
    }
    func testOldProbeWaitsWhileUnboundedGapAndInvalidRateFail() throws {
        XCTAssertNil(try CaptureResumeBridge.plan(actualPTS: .zero, nextInputTime: CMTime(value: 1, timescale: 48_000), sourceRate: 48_000))
        XCTAssertThrowsError(try CaptureResumeBridge.plan(actualPTS: CMTime(value: 1, timescale: 1), nextInputTime: .zero, sourceRate: 48_000))
        XCTAssertThrowsError(try CaptureResumeBridge.plan(actualPTS: .invalid, nextInputTime: .zero, sourceRate: 48_000))
        XCTAssertThrowsError(try CaptureResumeBridge.plan(actualPTS: .zero, nextInputTime: .zero, sourceRate: 0))
    }
    func testClockBridgeUsesTheDeclaredSourceClockAndEpoch() throws {
        let host = CMClockGetHostTimeClock()
        let bridge = try CaptureMediaClock(clock: host, epoch: CMTime(value: 100, timescale: 1))
        XCTAssertEqual(CMTimeCompare(try bridge.time(CMTime(value: 401, timescale: 4), from: host), CMTime(value: 1, timescale: 4)), 0)
        XCTAssertThrowsError(try bridge.time(CMTime(value: 99, timescale: 1), from: host))
        XCTAssertThrowsError(try CaptureMediaClock(clock: host, epoch: .invalid))
    }
    func testOffGridSealedEndWaitsForActualFuturePictureSlot() throws {
        let last = CMTime(value: 345, timescale: 1000)
        let end = CMTimeAdd(last, CMTime(value: 1, timescale: 30))
        let first = try CapturePrivacyCadence.firstVideoFrame(now: CMTime(value: 355, timescale: 1000), sealedEnd: end)
        XCTAssertEqual(first, 12)
        XCTAssertGreaterThanOrEqual(CMTimeCompare(CMTime(value: first, timescale: 30), end), 0)
        var cadence = try CapturePrivacyCadence(nextVideoFrame: first)
        XCTAssertTrue(try cadence.plan(elapsedNs: 355_000_000, video: true).videoFrames.isEmpty)
        XCTAssertTrue(try cadence.plan(elapsedNs: 399_000_000, video: true).videoFrames.isEmpty)
        XCTAssertEqual(try cadence.plan(elapsedNs: 400_000_000, video: true).videoFrames, 12..<13)
        XCTAssertThrowsError(try cadence.plan(elapsedNs: 399_000_000, video: true))
    }
    func testMeasuredFreshAudioFutureCursorWaitsBeforeNativePlanWithoutRetiming() throws {
        // Native receipt values in the regression example, not production constants.
        let next = CMTime(value: 146_432, timescale: 48_000)
        XCTAssertFalse(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 144_960, nextInputTime: next))
        XCTAssertFalse(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 146_431, nextInputTime: next))
        XCTAssertTrue(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 146_432, nextInputTime: next))
        XCTAssertTrue(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 146_433, nextInputTime: next))
        let fractional = CMTime(value: 1, timescale: 44_100)
        XCTAssertFalse(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 1, nextInputTime: fractional))
        XCTAssertTrue(try CapturePrivacyCadence.audioTargetIsDue(framesThrough: 2, nextInputTime: fractional))
    }
    func testHeldCameraMultipleMicrophoneProbesCommitOnlyCurrentPCM() throws {
        var cutover = CaptureResumeCutover<Int>(hasVideo: true)
        XCTAssertTrue(cutover.generatesSlate); XCTAssertTrue(cutover.generatesZero)
        cutover.sourceStarted()
        XCTAssertFalse(cutover.generatesSlate); XCTAssertTrue(cutover.generatesZero)
        let next = CMTime(value: 90, timescale: 100), cutoff = next
        for pts in 100...109 {
            guard case .wait = cutover.currentAudio(pts: CMTime(value: Int64(pts), timescale: 100), cutoff: cutoff, nextInput: next, sealedVideoEnd: next) else { return XCTFail("Audio was retained/committed before camera") }
        }
        XCTAssertTrue(cutover.offerPicture(110, pts: CMTime(value: 110, timescale: 100), cutoff: cutoff, sealedEnd: next))
        XCTAssertFalse(cutover.generatesZero)
        let current = CMTime(value: 111, timescale: 100)
        guard case .cutover(110?) = cutover.currentAudio(pts: current, cutoff: cutoff, nextInput: next, sealedVideoEnd: next) else { return XCTFail("Current PCM cannot cut over") }
        let bridge = try XCTUnwrap(CaptureResumeBridge.plan(actualPTS: current, nextInputTime: next, sourceRate: 48_000))
        XCTAssertEqual(bridge.zeroFrames, 10_080) // Bridge to actual 1.11, never held 1.00.
    }
    func testHeldMicrophoneUsesLatestPictureAndDiscardsStaleReciprocalProbe() throws {
        var cutover = CaptureResumeCutover<Int>(hasVideo: true); cutover.sourceStarted()
        let cutoff = CMTime(value: 1, timescale: 1)
        XCTAssertTrue(cutover.offerPicture(1, pts: cutoff, cutoff: cutoff, sealedEnd: cutoff))
        XCTAssertTrue(cutover.offerPicture(2, pts: CMTime(value: 11, timescale: 10), cutoff: cutoff, sealedEnd: cutoff))
        guard case .cutover(2?) = cutover.currentAudio(pts: CMTime(value: 111, timescale: 100), cutoff: cutoff, nextInput: cutoff, sealedVideoEnd: cutoff) else { return XCTFail("Latest picture was not retained") }
        guard case .wait = cutover.currentAudio(pts: CMTime(value: 14, timescale: 10), cutoff: cutoff, nextInput: cutoff, sealedVideoEnd: cutoff) else { return XCTFail("Stale cached video was admitted") }
        XCTAssertTrue(cutover.generatesZero)
        XCTAssertFalse(cutover.offerPicture(0, pts: .zero, cutoff: cutoff, sealedEnd: cutoff))
        XCTAssertTrue(cutover.offerPicture(3, pts: CMTime(value: 14, timescale: 10), cutoff: cutoff, sealedEnd: cutoff))
        guard case .cutover(3?) = cutover.currentAudio(pts: CMTime(value: 141, timescale: 100), cutoff: cutoff, nextInput: cutoff, sealedVideoEnd: cutoff) else { return XCTFail("Fresh replacement cannot cut over") }
    }
    func testAudioOnlyCutoverHasNoVideoEvidenceAndStillNeedsCurrentSourceCursor() throws {
        var cutover = CaptureResumeCutover<Int>(hasVideo: false)
        XCTAssertTrue(cutover.generatesZero); cutover.sourceStarted(); XCTAssertFalse(cutover.generatesZero)
        let next = CMTime(value: 1, timescale: 1)
        guard case .wait = cutover.currentAudio(pts: .zero, cutoff: .zero, nextInput: next, sealedVideoEnd: .zero) else { return XCTFail("Old PCM admitted") }
        guard case .cutover(nil) = cutover.currentAudio(pts: next, cutoff: .zero, nextInput: next, sealedVideoEnd: .zero) else { return XCTFail("Audio-only incorrectly required camera") }
    }

}
