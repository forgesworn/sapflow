import XCTest
@testable import SapflowBroadcaster

final class RecordingCaptureAdmissionTests: XCTestCase {
    private func prepared(_ capture: CaptureAdmission) -> CaptureAdmission.Preparation {
        let p = capture.reservePreparation()!
        XCTAssertTrue(capture.attached(p)); XCTAssertTrue(capture.beginPermissions(p)); XCTAssertTrue(capture.prepared(p))
        return p
    }
    func testStopDuringBlockedCreationCannotAdmitSuccessorOrActivateLateRecorder() {
        let capture = CaptureAdmission(); let recording = RecordingCaptureAdmission()
        let p = prepared(capture); let start = capture.reserveStart(p)!
        let work = recording.begin(.segment(start))!
        let stopped = capture.stop(nowNs: 10); recording.cancel(p)
        XCTAssertFalse(recording.permits(work)); XCTAssertFalse(recording.activate(work))
        XCTAssertTrue(recording.hasOwner); XCTAssertNil(recording.begin(.preflight(p)))
        // A caller cannot acknowledge the capture barrier while creation or its
        // late rejected recorder still has a real owner.
        XCTAssertFalse(capture.completeStop(stopped, cleanupComplete: !recording.hasOwner))
        XCTAssertNil(capture.reservePreparation())
        XCTAssertTrue(recording.closed(work, cleanupComplete: true))
        XCTAssertTrue(capture.completeStop(stopped, cleanupComplete: !recording.hasOwner))
        XCTAssertNotNil(capture.reservePreparation())
    }
    func testUnknownCleanupRetainsCapacityUntilSameActualCompletion() {
        let capture = CaptureAdmission(); let recording = RecordingCaptureAdmission(); let p = prepared(capture)
        let work = recording.begin(.segment(capture.reserveStart(p)!))!
        XCTAssertTrue(recording.activate(work)); XCTAssertFalse(recording.isWorking)
        recording.closing(work); XCTAssertTrue(recording.isWorking)
        XCTAssertFalse(recording.closed(work, cleanupComplete: false)); XCTAssertTrue(recording.hasOwner)
        XCTAssertNil(recording.begin(.preflight(p))); XCTAssertFalse(recording.activate(work))
        XCTAssertTrue(recording.closed(work, cleanupComplete: true)); XCTAssertFalse(recording.hasOwner)
    }
    func testOldCompletionAndCancellationCannotTouchNextOrientationSegment() {
        let capture = CaptureAdmission(); let recording = RecordingCaptureAdmission(); let p = prepared(capture)
        let start = capture.reserveStart(p)!; let first = recording.begin(.segment(start))!
        XCTAssertTrue(recording.activate(first)); XCTAssertTrue(capture.started(start, nowNs: 1))
        let rotate = capture.beginRotation(p)!
        recording.closing(first); XCTAssertTrue(recording.closed(first, cleanupComplete: true))
        let next = recording.begin(.segment(capture.reserveRotationStart(rotate)!))!
        XCTAssertFalse(recording.closed(first, cleanupComplete: true)); recording.closing(first)
        XCTAssertTrue(recording.permits(next)); XCTAssertTrue(recording.activate(next))
    }
    func testPreflightStopKeepsSlotUntilActualWorkerReturnsAndNoNewStartExists() {
        let capture = CaptureAdmission(); let recording = RecordingCaptureAdmission()
        let p = capture.reservePreparation()!; let work = recording.begin(.preflight(p))!
        XCTAssertTrue(recording.permits(work)); XCTAssertFalse(recording.activate(work))
        recording.cancel(p); XCTAssertFalse(recording.permits(work)); XCTAssertTrue(recording.hasOwner)
        XCTAssertTrue(recording.closed(work, cleanupComplete: true)); XCTAssertFalse(recording.hasOwner)
        let next = recording.begin(.preflight(p))!; recording.closing(work)
        XCTAssertTrue(recording.permits(next)); XCTAssertFalse(recording.closed(work, cleanupComplete: false))
    }
    func testStorageKeepsStrictExistingFloorAndSelectedProfileBudget() throws {
        for profile in CaptureProfile.allCases {
            let budget = try RecordingStorage.budget(profile: profile)
            XCTAssertFalse(RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: nil))
            XCTAssertFalse(RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: 200 * 1024 * 1024))
            XCTAssertFalse(RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: budget.requiredBytes - 1))
            XCTAssertTrue(RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: max(200 * 1024 * 1024 + 1, budget.requiredBytes)))
        }
    }
    func testHeldTimeoutCallbackCannotExtendOriginalRotationBudgetThroughCreationAndAdoption() throws {
        let capture = CaptureAdmission(); let recording = RecordingCaptureAdmission(); let p = prepared(capture)
        let first = capture.reserveStart(p)!; XCTAssertTrue(capture.started(first, nowNs: 1))
        let child = capture.beginRotation(p)!
        let deadline = try XCTUnwrap(RecordingCaptureAdmission.RotationDeadline(preparation: p, nowNs: 100))
        let next = capture.reserveRotationStart(child)!; let pending = recording.begin(.segment(next))!
        XCTAssertTrue(deadline.permits(p, nowNs: 6_000_000_099))
        XCTAssertFalse(deadline.permits(p, nowNs: 99))
        // The system's queued timeout never fires. A blocking cleanup or IO
        // completion returns at/after the original deadline: no reset to now+6.
        XCTAssertFalse(deadline.permits(p, nowNs: 6_000_000_100))
        XCTAssertFalse(deadline.permits(p, nowNs: 9_000_000_000))
        let stop = capture.stop(nowNs: 9_000_000_000); recording.cancel(p)
        XCTAssertFalse(recording.activate(pending)); XCTAssertFalse(capture.started(next, nowNs: 9_000_000_000))
        XCTAssertFalse(capture.completeStop(stop, cleanupComplete: !recording.hasOwner))
        XCTAssertTrue(recording.closed(pending, cleanupComplete: true))
        XCTAssertTrue(capture.completeStop(stop, cleanupComplete: true))
    }
    func testRotationDeadlineRejectsOverflowAndCannotApproveSuccessorPreparation() throws {
        let capture = CaptureAdmission(); let p = capture.reservePreparation()!
        XCTAssertNil(RecordingCaptureAdmission.RotationDeadline(preparation: p, nowNs: UInt64.max - 5_999_999_999))
        let deadline = try XCTUnwrap(RecordingCaptureAdmission.RotationDeadline(preparation: p, nowNs: 1))
        let stop = capture.stop(); _ = capture.candidateClosed(p, cleanupComplete: true)
        XCTAssertTrue(capture.completeStop(stop, cleanupComplete: true))
        let next = capture.reservePreparation()!
        XCTAssertFalse(deadline.permits(next, nowNs: 2))
    }
    func testDifferentPreparationCannotCancelCurrentPendingOwner() {
        let a = CaptureAdmission(); let b = CaptureAdmission()
        let p = a.reservePreparation()!; _ = b.reservePreparation()
        // Distinct serial/profile identity is the existing capture arbiter's
        // contract; use a later serial for an independently retired intent.
        _ = b.stop(); _ = b.candidateClosed(b.stop().preparation!, cleanupComplete: true)
        _ = b.completeStop(b.stop(), cleanupComplete: true)
        let other = b.reservePreparation()!
        let recording = RecordingCaptureAdmission(); let work = recording.begin(.preflight(p))!
        recording.cancel(other); XCTAssertTrue(recording.permits(work))
    }
}
