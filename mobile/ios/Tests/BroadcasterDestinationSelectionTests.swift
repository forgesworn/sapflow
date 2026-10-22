import XCTest
@testable import SapflowBroadcaster

final class BroadcasterDestinationSelectionTests: XCTestCase {
    private func prepared(_ admission: CaptureAdmission) throws -> CaptureAdmission.Preparation {
        let ticket = try XCTUnwrap(admission.reservePreparation())
        XCTAssertTrue(admission.attached(ticket)); XCTAssertTrue(admission.beginPermissions(ticket))
        XCTAssertTrue(admission.prepared(ticket)); return ticket
    }
    func testModeExitEndsCapturedManualPublisherDespiteLateRunningDisplay() throws {
        let admission = CaptureAdmission()
        var cancelled: [CaptureAdmission.Preparation] = [], ends = 0
        let selection = BroadcasterDestinationSelection { ticket in
            cancelled.append(ticket); XCTAssertNil(admission.stopIfPending(ticket))
        }
        selection.select(.manual)
        let ticket = try prepared(admission); XCTAssertTrue(selection.reserve(ticket))
        XCTAssertTrue(selection.installManual(ticket) { ends += 1 })
        let start = try XCTUnwrap(admission.reserveStart(ticket)); XCTAssertTrue(admission.started(start))
        // No running snapshot is delivered to UI before this selection.
        selection.select(.recording)
        XCTAssertEqual(ends, 1); XCTAssertEqual(cancelled, [ticket])
        XCTAssertEqual(admission.currentPhase, .running); XCTAssertNil(selection.preparation)
        XCTAssertEqual(selection.mode, .recording)
    }
    func testModeExitWinsPendingStartAndRetainsActualCleanupBarrier() throws {
        let admission = CaptureAdmission(); var stop: CaptureAdmission.Stop?
        let selection = BroadcasterDestinationSelection { stop = admission.stopIfPending($0) }
        selection.select(.manual)
        let ticket = try prepared(admission); XCTAssertTrue(selection.reserve(ticket))
        var ends = 0; XCTAssertTrue(selection.installManual(ticket) { ends += 1 })
        let start = try XCTUnwrap(admission.reserveStart(ticket)); selection.select(.v4v)
        XCTAssertFalse(admission.started(start)); XCTAssertEqual(ends, 1)
        let stopped = try XCTUnwrap(stop)
        XCTAssertFalse(admission.completeStop(stopped, cleanupComplete: false))
        XCTAssertNil(admission.reservePreparation())
        XCTAssertTrue(admission.completeStop(stopped, cleanupComplete: true))
        XCTAssertNotNil(admission.reservePreparation())
    }
    func testLateFactoryCandidateCannotReplaceModeAndMustKeepCameraBarrier() throws {
        let admission = CaptureAdmission(); var stop: CaptureAdmission.Stop?
        let selection = BroadcasterDestinationSelection { stop = admission.stopIfPending($0) }
        selection.select(.manual)
        let ticket = try XCTUnwrap(admission.reservePreparation()); XCTAssertTrue(selection.reserve(ticket))
        selection.select(.recording)
        var lateEnds = 0
        XCTAssertFalse(selection.installManual(ticket) { lateEnds += 1 })
        XCTAssertEqual(lateEnds, 0) // Constructed candidate goes to the camera close owner.
        let stopped = try XCTUnwrap(stop)
        XCTAssertFalse(admission.completeStop(stopped, cleanupComplete: true))
        XCTAssertFalse(admission.candidateClosed(ticket, cleanupComplete: false))
        XCTAssertNil(admission.reservePreparation())
        XCTAssertTrue(admission.candidateClosed(ticket, cleanupComplete: true))
    }
    func testSameModePreservesExactManualAuthorityAndStopFencesOnce() throws {
        let admission = CaptureAdmission(); var cancellations = 0, ends = 0
        let selection = BroadcasterDestinationSelection { _ in cancellations += 1 }
        selection.select(.manual)
        let ticket = try prepared(admission); XCTAssertTrue(selection.reserve(ticket))
        XCTAssertTrue(selection.installManual(ticket) { ends += 1 })
        selection.select(.manual)
        XCTAssertEqual(selection.preparation, ticket); XCTAssertEqual(ends, 0)
        selection.stop(); selection.stop(); selection.select(.recording)
        XCTAssertEqual(ends, 1); XCTAssertEqual(cancellations, 0)
    }
    func testOldFactoryFailureCannotDiscardSuccessorReservationOrEnd() throws {
        let admission = CaptureAdmission(); let selection = BroadcasterDestinationSelection { _ in }
        selection.select(.manual)
        let first = try XCTUnwrap(admission.reservePreparation()); XCTAssertTrue(selection.reserve(first))
        XCTAssertTrue(admission.failed(first, cleanupComplete: true)); selection.forgetFailedReservation(first)
        let second = try prepared(admission); XCTAssertTrue(selection.reserve(second))
        var ends = 0; XCTAssertTrue(selection.installManual(second) { ends += 1 })
        selection.forgetFailedReservation(first); selection.forgetFailedReservation(second)
        XCTAssertEqual(selection.preparation, second)
        selection.select(.recording); XCTAssertEqual(ends, 1)
    }
    func testV4vCannotReserveManualAuthorityOrInheritAnOldEndClosure() throws {
        let admission = CaptureAdmission(); var ends = 0
        let selection = BroadcasterDestinationSelection { _ in }
        selection.select(.manual)
        let ticket = try prepared(admission); XCTAssertTrue(selection.reserve(ticket))
        XCTAssertTrue(selection.installManual(ticket) { ends += 1 })
        selection.select(.v4v); XCTAssertEqual(ends, 1)
        XCTAssertFalse(selection.reserve(ticket)); XCTAssertFalse(selection.installManual(ticket) { ends += 1 })
        selection.select(.recording); selection.stop(); XCTAssertEqual(ends, 1)
    }
    func testExplicitRetryAfterActualPermissionCleanupNeedsNoStaleDisplayReset() throws {
        let admission = CaptureAdmission(); var oldEnds = 0, newEnds = 0
        let selection = BroadcasterDestinationSelection { _ in }
        selection.select(.manual)
        let first = try XCTUnwrap(admission.reservePreparation()); XCTAssertTrue(selection.reserve(first))
        XCTAssertTrue(selection.installManual(first) { oldEnds += 1 })
        XCTAssertTrue(admission.attached(first)); XCTAssertTrue(admission.beginPermissions(first))
        XCTAssertFalse(admission.failed(first, cleanupComplete: false)); XCTAssertNil(admission.reservePreparation())
        XCTAssertTrue(admission.failed(first, cleanupComplete: true))
        let second = try XCTUnwrap(admission.reservePreparation()); XCTAssertTrue(selection.reserve(second))
        XCTAssertEqual(oldEnds, 1); XCTAssertEqual(selection.preparation, second)
        XCTAssertTrue(selection.installManual(second) { newEnds += 1 })
        selection.select(.recording); XCTAssertEqual(oldEnds, 1); XCTAssertEqual(newEnds, 1)
    }
}
