import XCTest
@testable import SapflowBroadcaster

final class CaptureAudioAnchorTests: XCTestCase {
    private func owner() throws -> CaptureAudioAnchor<Int> {
        try XCTUnwrap(CaptureAudioAnchor(owner: 1, hasVideo: false, optedIn: true))
    }
    private func running() throws -> CaptureAudioAnchor<Int> {
        var value = try owner()
        XCTAssertTrue(value.begin(1, exactStart: true, permissionGranted: true))
        XCTAssertTrue(value.started(1, nativeRunning: true, stillCurrent: true))
        return value
    }
    func testCameraDefaultOffAndPreparationCannotStartBackgroundIO() throws {
        XCTAssertNil(CaptureAudioAnchor(owner: 1, hasVideo: true, optedIn: true))
        XCTAssertNil(CaptureAudioAnchor(owner: 1, hasVideo: false, optedIn: false))
        var value = try owner()
        XCTAssertFalse(value.begin(1, exactStart: false, permissionGranted: true))
        XCTAssertFalse(value.begin(1, exactStart: true, permissionGranted: false))
        XCTAssertFalse(value.retainForStandby(1, committed: true, permissionGranted: true))
        XCTAssertEqual(value.phase, .ready)
    }
    func testExplicitStartStillNeedsActualInputAndCommittedOptInBeforeStandby() throws {
        var value = try owner()
        XCTAssertTrue(value.begin(1, exactStart: true, permissionGranted: true))
        XCTAssertFalse(value.retainForStandby(1, committed: true, permissionGranted: true))
        XCTAssertTrue(value.started(1, nativeRunning: true, stillCurrent: true))
        XCTAssertFalse(value.retainForStandby(1, committed: false, permissionGranted: true))
        XCTAssertTrue(value.retainForStandby(1, committed: true, permissionGranted: true))
        XCTAssertTrue(value.standbyInputActive)
    }
    func testRepeatStandbyAndPublishingEndKeepSameNativeOwnerWithoutRestart() throws {
        var value = try running()
        XCTAssertTrue(value.retainForStandby(1, committed: true, permissionGranted: true))
        XCTAssertTrue(value.retainForStandby(1, committed: true, permissionGranted: true))
        // Publishing is deliberately absent from this native recording owner.
        XCTAssertTrue(value.standbyInputActive)
        XCTAssertFalse(value.begin(1, exactStart: true, permissionGranted: true))
        XCTAssertTrue(value.resume(1)); XCTAssertFalse(value.standbyInputActive)
        XCTAssertEqual(value.phase, .active)
    }
    func testStopWinningWhileNativeStartIsHeldRetainsLateOwnerUntilActualClosure() throws {
        var value = try owner()
        XCTAssertTrue(value.begin(1, exactStart: true, permissionGranted: true))
        XCTAssertTrue(value.stop(1)); XCTAssertFalse(value.stop(1))
        XCTAssertFalse(value.started(1, nativeRunning: true, stillCurrent: false))
        XCTAssertFalse(value.retainForStandby(1, committed: true, permissionGranted: true))
        value.closedRoute(1); value.deactivated(1, outcome: .confirmed)
        XCTAssertFalse(value.cleanupComplete)
        value.stoppedInput(1, confirmed: true)
        XCTAssertFalse(value.cleanupComplete) // A held native callback still owns capacity.
        value.closedCallbacks(1); XCTAssertTrue(value.cleanupComplete)
        XCTAssertFalse(value.begin(1, exactStart: true, permissionGranted: true))
    }
    func testIndependentReceiptsCannotTurnUnknownNativeCleanupIntoClosed() throws {
        var value = try running(); XCTAssertTrue(value.stop(1))
        value.stoppedInput(1, confirmed: true); value.closedCallbacks(1); value.closedRoute(1)
        XCTAssertFalse(value.cleanupComplete)
        value.deactivated(1, outcome: .unknown)
        XCTAssertEqual(value.phase, .retained)
        value.deactivated(1, outcome: .confirmed)
        XCTAssertFalse(value.cleanupComplete) // No retrospective retry releases unknown ownership.
    }
    func testDocumentedInactiveBusyNeverProvesOtherOwnersClosed() throws {
        var value = try running(); XCTAssertTrue(value.stop(1))
        value.deactivated(1, outcome: .inactiveWithBusy)
        XCTAssertTrue(value.misuseFault); XCTAssertFalse(value.cleanupComplete)
        value.stoppedInput(1, confirmed: true); value.closedCallbacks(1)
        XCTAssertFalse(value.cleanupComplete)
        value.closedRoute(1); XCTAssertTrue(value.cleanupComplete)
    }
    func testForeignRunAndRevokedPermissionCannotRetainOrRetireOwner() throws {
        var value = try running()
        XCTAssertFalse(value.retainForStandby(2, committed: true, permissionGranted: true))
        XCTAssertFalse(value.retainForStandby(1, committed: true, permissionGranted: false))
        XCTAssertFalse(value.stop(2)); value.closedRoute(2); value.closedCallbacks(2)
        value.stoppedInput(2, confirmed: true); value.deactivated(2, outcome: .confirmed)
        XCTAssertEqual(value.phase, .active); XCTAssertFalse(value.cleanupComplete)
        XCTAssertFalse(value.resume(2))
    }
    func testFaultOrStopRetirementCannotBeResurrectedByLateResumeOrSuccessorReceipts() throws {
        var value = try running(); XCTAssertTrue(value.stop(1))
        XCTAssertFalse(value.resume(1)); XCTAssertFalse(value.retainForStandby(1, committed: true, permissionGranted: true))
        value.stoppedInput(1, confirmed: false)
        value.stoppedInput(1, confirmed: true); value.closedCallbacks(1); value.closedRoute(1)
        value.deactivated(1, outcome: .confirmed)
        XCTAssertEqual(value.phase, .retained); XCTAssertFalse(value.cleanupComplete)
    }
    func testFailedNativeStartStillJoinsRealNonrunningInputAndSessionCleanup() throws {
        var value = try owner()
        XCTAssertTrue(value.begin(1, exactStart: true, permissionGranted: true))
        XCTAssertFalse(value.started(1, nativeRunning: false, stillCurrent: true))
        XCTAssertEqual(value.phase, .stopping); XCTAssertFalse(value.cleanupComplete)
        value.stoppedInput(1, confirmed: true); value.closedCallbacks(1); value.closedRoute(1)
        value.deactivated(1, outcome: .confirmed)
        XCTAssertTrue(value.cleanupComplete)
    }
    func testActualAdmissionPublishingEndPreservesCommittedAudioAnchorButStopDoesNot() throws {
        let admission = CaptureAdmission()
        XCTAssertTrue(admission.selectBackgroundAudio(true))
        let prep = try XCTUnwrap(admission.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(admission.attached(prep)); XCTAssertTrue(admission.beginPermissions(prep)); XCTAssertTrue(admission.prepared(prep))
        let start = try XCTUnwrap(admission.reserveStart(prep))
        let run = try XCTUnwrap(admission.reserveMediaRun(start))
        var value = try XCTUnwrap(CaptureAudioAnchor(owner: run, hasVideo: prep.profile.hasVideo, optedIn: prep.continuesAudioInBackground))
        XCTAssertTrue(value.begin(run, exactStart: admission.permitsStart(start), permissionGranted: true))
        XCTAssertTrue(value.started(run, nativeRunning: true, stillCurrent: admission.permitsStart(start)))
        XCTAssertFalse(value.retainForStandby(run, committed: admission.backgroundContinuation == prep, permissionGranted: true))
        XCTAssertTrue(admission.commitMediaRun(run, start: start, nowNs: 0, broadcasting: true))
        admission.broadcastEnded(prep, nowNs: 1)
        XCTAssertTrue(value.retainForStandby(run, committed: admission.backgroundContinuation == prep, permissionGranted: true))
        _ = admission.stop(nowNs: 2)
        XCTAssertFalse(value.retainForStandby(run, committed: admission.backgroundContinuation == prep, permissionGranted: true))
        XCTAssertTrue(value.stop(run)); XCTAssertFalse(value.resume(run))
    }

    func testExactStopDuringHeldDelegateHandoffPreventsNativeStartEvenBeforeColdStopRuns() throws {
        let admission = CaptureAdmission(); XCTAssertTrue(admission.selectBackgroundAudio(true))
        let prep = try XCTUnwrap(admission.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(admission.attached(prep)); XCTAssertTrue(admission.beginPermissions(prep)); XCTAssertTrue(admission.prepared(prep))
        let start = try XCTUnwrap(admission.reserveStart(prep))
        let run = try XCTUnwrap(admission.reserveMediaRun(start))
        var value = try XCTUnwrap(CaptureAudioAnchor(owner: run, hasVideo: false, optedIn: true))
        XCTAssertTrue(value.begin(run, exactStart: admission.permitsStart(start), permissionGranted: true))
        let held = { value.permitsNativeStart(run, exactStart: admission.permitsStart(start), permissionGranted: true) }
        _ = admission.stop(nowNs: 1) // The native control owner's stop has not run.
        XCTAssertEqual(value.phase, .starting)
        XCTAssertFalse(held())
        XCTAssertFalse(value.permitsNativeStart(run, exactStart: true, permissionGranted: false))
    }

}
