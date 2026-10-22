import XCTest
@testable import SapflowBroadcaster

final class CaptureAdmissionTests: XCTestCase {
    func testEndRetiresEveryInitialPreparationPhaseBeforeStartCommits() {
        for stage in 0...4 {
            let gate = CaptureAdmission(), ticket = gate.reservePreparation()!
            if stage >= 1 { XCTAssertTrue(gate.attached(ticket)) }
            if stage >= 2 { XCTAssertTrue(gate.beginPermissions(ticket)) }
            if stage >= 3 { XCTAssertTrue(gate.prepared(ticket)) }
            let start = stage == 4 ? gate.reserveStart(ticket) : nil
            let stop = gate.stopIfPending(ticket)!
            XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertNil(gate.reserveStart(ticket))
            if let start { XCTAssertFalse(gate.started(start)) }
            if stage == 0 {
                XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true))
                XCTAssertTrue(gate.candidateClosed(ticket, cleanupComplete: true))
            } else { XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true)) }
            XCTAssertNotNil(gate.reservePreparation())
        }
    }
    func testEndWinningBeforeStartedRequiresAssignedRunCleanup() {
        let gate = CaptureAdmission(), intent = preparing(gate)
        XCTAssertTrue(gate.prepared(intent)); let start = gate.reserveStart(intent)!
        let end = gate.stopIfPending(intent)!
        XCTAssertFalse(gate.started(start))
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(end, cleanupComplete: true))
    }
    func testStartedWinningBeforeEndPreservesIndependentRecording() {
        let gate = CaptureAdmission(), intent = running(gate)
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertEqual(gate.currentPreparation, intent); XCTAssertNil(gate.reservePreparation())
    }
    func testEndPreservesExistingRecordingThroughRotationAndItsStartingPhase() {
        let gate = CaptureAdmission(), intent = running(gate)
        let child = gate.beginRotation(intent)!
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertTrue(gate.permitsRotation(child))
        let restart = gate.reserveRotationStart(child)!
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertTrue(gate.permitsStart(restart))
        XCTAssertTrue(gate.started(restart)); XCTAssertEqual(gate.currentPreparation, intent)
    }
    func testOldEndCannotRetireOrFenceSuccessorPreparation() {
        let gate = CaptureAdmission(), old = preparing(gate)
        let stopped = gate.stopIfPending(old)!
        XCTAssertTrue(gate.completeStop(stopped, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertNil(gate.stopIfPending(old)); XCTAssertFalse(gate.permitsStop(stopped))
        XCTAssertTrue(gate.permitsPreparation(next))
    }
    func testRepeatedEndJoinsFactoryAndActualCandidateCleanupBarrier() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!
        let end = gate.stopIfPending(factory)!
        XCTAssertEqual(gate.stopIfPending(factory), end)
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertEqual(gate.stopIfPending(factory), end)
        XCTAssertTrue(gate.candidateClosed(factory, cleanupComplete: true)); XCTAssertNil(gate.stopIfPending(factory))
    }
    func testLateCandidateCannotReleaseEndWithUnconfirmedNativeCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, end = gate.stopIfPending(factory)!
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: false))
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(end, cleanupComplete: true)); XCTAssertNotNil(gate.reservePreparation())
    }
    func testPreparationFailureCannotRetireCommittedRecordingIntent() {
        let gate = CaptureAdmission(), intent = running(gate)
        XCTAssertFalse(gate.failed(intent, cleanupComplete: true)); XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = preparing(gate); XCTAssertNotNil(gate.stopIfPending(next))
    }
    private func preparing(_ gate: CaptureAdmission) -> CaptureAdmission.Preparation {
        let ticket = gate.reservePreparation()!
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        return ticket
    }
    private func running(_ gate: CaptureAdmission) -> CaptureAdmission.Preparation {
        let ticket = preparing(gate); XCTAssertTrue(gate.prepared(ticket))
        XCTAssertTrue(gate.started(gate.reserveStart(ticket)!)); return ticket
    }
    func testStopBeforePermissionResponseCannotReprepareEvenAfterCleanupFinishes() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        let stop = gate.stop()
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
        XCTAssertNil(gate.preparedTicket); XCTAssertNotNil(gate.reservePreparation())
    }
    func testStopBetweenPermissionResponseAndActorDispatchInvalidatesContinuation() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.permitsPreparation(ticket)) // first permission answer
        _ = gate.stop() // second answer/control task has not been delivered
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
    }
    func testDuplicatePrepareAndPermissionRequestsAreRejectedBeforeQueueing() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertNil(gate.reservePreparation()); XCTAssertFalse(gate.beginPermissions(ticket))
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertNil(gate.reservePreparation())
    }
    func testDestinationMustBeTheExactCurrentReservationBeforePermissions() {
        let gate = CaptureAdmission(), old = gate.reservePreparation()!
        XCTAssertFalse(gate.beginPermissions(old))
        XCTAssertFalse(gate.candidateClosed(old, cleanupComplete: true))
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = gate.reservePreparation()!
        XCTAssertFalse(gate.attached(old)); XCTAssertFalse(gate.beginPermissions(old))
        XCTAssertTrue(gate.attached(next)); XCTAssertTrue(gate.beginPermissions(next))
    }
    func testQueuedStartCannotWinSynchronousStop() {
        let gate = CaptureAdmission(), prepared = preparing(gate)
        XCTAssertTrue(gate.prepared(prepared)); let start = gate.reserveStart(prepared)!
        XCTAssertTrue(gate.permitsStart(start)); _ = gate.stop()
        XCTAssertFalse(gate.permitsStart(start)); XCTAssertFalse(gate.started(start))
    }
    func testOldStartApprovalCannotStartNewPreparation() {
        let gate = CaptureAdmission(), old = preparing(gate)
        XCTAssertTrue(gate.prepared(old)); let oldStart = gate.reserveStart(old)!
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = preparing(gate); XCTAssertTrue(gate.prepared(next))
        let newStart = gate.reserveStart(next)!
        XCTAssertFalse(gate.started(oldStart)); XCTAssertTrue(gate.started(newStart))
    }
    func testStartConsumesOnlyItsPreparedDestinationOnce() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.prepared(ticket))
        let start = gate.reserveStart(ticket)!
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.started(start))
        XCTAssertFalse(gate.started(start)); XCTAssertNil(gate.preparedTicket)
    }
    func testHealthyRotationIsChildOfSameIntentAndRequiresFreshStartApproval() {
        let gate = CaptureAdmission(), intent = running(gate)
        let rotation = gate.beginRotation(intent)!
        XCTAssertEqual(gate.beginRotation(intent), rotation)
        XCTAssertEqual(rotation.preparation, intent); XCTAssertTrue(gate.permitsRotation(rotation))
        let restart = gate.reserveRotationStart(rotation)!
        XCTAssertFalse(gate.permitsRotation(rotation)); XCTAssertNil(gate.reserveRotationStart(rotation))
        XCTAssertEqual(restart.preparation, intent); XCTAssertTrue(gate.started(restart))
        XCTAssertEqual(gate.currentPreparation, intent)
    }
    func testStopFencesRotationBeforeQueuedOrientationCancelOrOldCleanup() {
        let gate = CaptureAdmission(), intent = running(gate)
        let rotation = gate.beginRotation(intent)!, stop = gate.stop()
        XCTAssertFalse(gate.permitsRotation(rotation)); XCTAssertNil(gate.reserveRotationStart(rotation))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNil(gate.reserveRotationStart(rotation)); XCTAssertNil(gate.currentPreparation)
    }
    func testStopAfterRotationApprovalStillRejectsDelayedRestart() {
        let gate = CaptureAdmission(), intent = running(gate)
        let start = gate.reserveRotationStart(gate.beginRotation(intent)!)!
        _ = gate.stop(); XCTAssertFalse(gate.started(start))
    }
    func testFailedCleanupRetainsCapacityUntilActualMatchingOwnerCompletes() {
        let gate = CaptureAdmission(); _ = running(gate); let stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertEqual(gate.currentPhase, .retained); XCTAssertNil(gate.reservePreparation())
        XCTAssertEqual(gate.stop(), stop); XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertEqual(gate.currentPhase, .idle); XCTAssertNotNil(gate.reservePreparation())
    }
    func testOldCleanupCannotUnlockNewOwnerAndDuplicateStopIsCoalesced() {
        let gate = CaptureAdmission(); _ = preparing(gate); let old = gate.stop()
        XCTAssertEqual(gate.stop(), old); XCTAssertTrue(gate.completeStop(old, cleanupComplete: true))
        _ = preparing(gate); let current = gate.stop()
        XCTAssertNotEqual(current, old); XCTAssertFalse(gate.completeStop(old, cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation()); XCTAssertTrue(gate.completeStop(current, cleanupComplete: true))
    }
    func testFactoryAndPermissionFailuresRetireOnlyExactIntent() {
        let gate = CaptureAdmission(), old = gate.reservePreparation()!
        XCTAssertTrue(gate.failed(old, cleanupComplete: true)) // no sink was constructed
        let next = preparing(gate)
        XCTAssertFalse(gate.failed(old, cleanupComplete: true))
        XCTAssertTrue(gate.permitsPreparation(next))
        XCTAssertTrue(gate.failed(next, cleanupComplete: true)) // denied permission/native cleanup done
        XCTAssertNotNil(gate.reservePreparation())
    }
    func testLateFailureCannotBypassStopBarrierAndUnknownConfigurationCleanupBlocks() {
        let gate = CaptureAdmission(), ticket = preparing(gate), stop = gate.stop()
        XCTAssertFalse(gate.failed(ticket, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertFalse(gate.failed(next, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
    }
    func testIdleStopHoldsQueueBarrierAndNeverStartsCapture() {
        let gate = CaptureAdmission(), stop = gate.stop()
        XCTAssertNil(stop.preparation); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNil(gate.currentPreparation); XCTAssertNotNil(gate.reservePreparation())
    }
    func testDelayedDuplicateStopContinuationCannotResampleAndStopSuccessor() {
        let gate = CaptureAdmission(); _ = preparing(gate)
        let stopA = gate.stop(), duplicateA = gate.stop()
        var touched: [String] = []
        // This is the exact guard used before the Engine's queued Stop and
        // interruption continuations mutate native state or sample another Stop.
        let delayed = {
            guard gate.permitsStop(duplicateA) else { return }
            touched.append("stopped"); _ = gate.stop()
        }
        XCTAssertTrue(gate.completeStop(stopA, cleanupComplete: true))
        let next = preparing(gate)
        delayed()
        XCTAssertTrue(touched.isEmpty); XCTAssertTrue(gate.permitsPreparation(next))
        XCTAssertEqual(gate.currentPhase, .permissions)
    }
    func testStopWaitsForFactoryDeliveryAndActualRejectedCandidateCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true)) // old ingress is already closed
        XCTAssertNil(gate.reservePreparation()); XCTAssertTrue(gate.awaitsCandidate(factory))
        // A's factory returns only now. Stop has not released the reservation.
        XCTAssertFalse(gate.attached(factory)); XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: false))
        XCTAssertEqual(gate.currentPhase, .retained); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.candidateClosed(factory, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true))
        XCTAssertTrue(gate.permitsPreparation(next))
    }
    func testFactoryThrowDuringStopDoesNotBypassOutstandingNativeCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true)) // factory threw, no candidate
        XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNotNil(gate.reservePreparation())
    }
    func testActualCandidateCloseCannotReleaseFailedIngressCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation()); XCTAssertEqual(gate.currentPhase, .retained)
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
    }
    func testTicketExhaustionNeverWrapsIntoStaleApproval() {
        let gate = CaptureAdmission(initialSerial: UInt64.max - 1), ticket = gate.reservePreparation()!
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket)); XCTAssertTrue(gate.prepared(ticket))
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation())
    }
}
