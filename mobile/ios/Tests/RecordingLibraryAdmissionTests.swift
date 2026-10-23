import XCTest
@testable import SapflowBroadcaster

final class RecordingLibraryAdmissionTests: XCTestCase {
    func testCameraScanIsDistinctFromCatalogueScanAndExcludesPrepare() throws {
        let gate = RecordingLibraryAdmission()
        XCTAssertNil(gate.reserve(.cameraScan))
        XCTAssertNil(gate.reserveCameraScan { false })
        let scan = try XCTUnwrap(gate.reserveCameraScan { true })
        XCTAssertNil(gate.withCaptureReservation { "no camera sharing" })
        XCTAssertNil(gate.reserve(.scan)); XCTAssertNil(gate.beginExport { true })
        gate.cancel(scan)
        XCTAssertNil(gate.withCaptureReservation { "cancel is not close" })
        XCTAssertFalse(gate.finish(scan, cleanupComplete: false)); XCTAssertTrue(gate.cleanupRetained)
        XCTAssertFalse(gate.finish(scan, cleanupComplete: true)); XCTAssertTrue(gate.idle)
    }
    func testStaleCameraCloseCannotReleaseNextCameraLease() throws {
        let gate = RecordingLibraryAdmission()
        let a = try XCTUnwrap(gate.reserveCameraScan { true })
        XCTAssertTrue(gate.finish(a, cleanupComplete: true))
        let b = try XCTUnwrap(gate.reserveCameraScan { true })
        gate.cancel(a); XCTAssertFalse(gate.finish(a, cleanupComplete: true))
        XCTAssertTrue(gate.owns(b)); XCTAssertNil(gate.withCaptureReservation { "still blocked" })
    }
    func testConcurrentCameraScanAndActualPreparationHaveOneWinner() {
        final class State: @unchecked Sendable {
            let gate = RecordingLibraryAdmission()
            var idle = true // Shared only beneath the library lock.
            var camera = false, capture = false // Independent writers, joined before read.
        }
        for _ in 0..<64 {
            let state = State(), start = DispatchSemaphore(value: 0), group = DispatchGroup()
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }; start.wait()
                state.camera = state.gate.reserveCameraScan { state.idle } != nil
            }
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }; start.wait()
                state.capture = state.gate.withCaptureReservation { state.idle = false; return true } == true
            }
            start.signal(); start.signal()
            XCTAssertEqual(group.wait(timeout: .now() + 3), .success)
            XCTAssertNotEqual(state.camera, state.capture)
        }
    }
    func testPickerBlocksNewPrepareUntilActualDismissal() {
        let gate = RecordingLibraryAdmission()
        let picker = gate.beginExport { true }!
        XCTAssertNil(gate.withCaptureReservation { "new capture" })
        gate.cancel(picker)
        XCTAssertFalse(gate.idle)
        XCTAssertNil(gate.withCaptureReservation { "still presented" })
        XCTAssertFalse(gate.dismissPicker(picker, selected: true))
        XCTAssertTrue(gate.idle)
        XCTAssertEqual(gate.withCaptureReservation { "new capture" }, "new capture")
    }
    func testActualCaptureReservationPreventsLaterExport() {
        let gate = RecordingLibraryAdmission()
        var captureIdle = true
        XCTAssertEqual(gate.withCaptureReservation { captureIdle = false; return "prepared" }, "prepared")
        XCTAssertNil(gate.beginExport { captureIdle })
        captureIdle = true
        XCTAssertNotNil(gate.beginExport { captureIdle })
    }
    func testCopyBeginsOnlyAfterPickerDismissalAndCancelledQueueAcknowledges() {
        let gate = RecordingLibraryAdmission()
        let ticket = gate.beginExport { true }!
        XCTAssertFalse(gate.begin(ticket))
        XCTAssertFalse(gate.finish(ticket, cleanupComplete: true))
        XCTAssertTrue(gate.dismissPicker(ticket, selected: true))
        gate.cancel(ticket)
        XCTAssertFalse(gate.begin(ticket)); XCTAssertFalse(gate.idle)
        XCTAssertFalse(gate.finish(ticket, cleanupComplete: true))
        XCTAssertTrue(gate.idle); XCTAssertFalse(gate.canDeliver(ticket))
    }
    func testRunningCancelWaitsForActualCloseAndUnknownCleanupRetains() {
        let gate = RecordingLibraryAdmission()
        let ticket = gate.beginExport { true }!
        XCTAssertTrue(gate.dismissPicker(ticket, selected: true)); XCTAssertTrue(gate.begin(ticket))
        gate.cancel(ticket)
        XCTAssertTrue(gate.shouldCancel(ticket))
        XCTAssertFalse(gate.finish(ticket, cleanupComplete: false))
        XCTAssertTrue(gate.cleanupRetained)
        XCTAssertNil(gate.reserve(.scan)); XCTAssertNil(gate.withCaptureReservation { "capture" })
        XCTAssertFalse(gate.finish(ticket, cleanupComplete: true))
        XCTAssertTrue(gate.idle); XCTAssertFalse(gate.cleanupRetained)
    }
    func testStalePickerAndOldCloseCannotReleaseSuccessor() {
        let gate = RecordingLibraryAdmission()
        let old = gate.beginExport { true }!
        XCTAssertFalse(gate.dismissPicker(old, selected: false))
        let current = gate.beginExport { true }!
        XCTAssertFalse(gate.dismissPicker(old, selected: true))
        XCTAssertFalse(gate.finish(old, cleanupComplete: true))
        XCTAssertFalse(gate.idle)
        XCTAssertTrue(gate.dismissPicker(current, selected: true)); XCTAssertTrue(gate.begin(current))
    }
    func testScanCanCoexistWithCaptureButNeverAdmitsAnotherFileJob() {
        let gate = RecordingLibraryAdmission()
        let scan = gate.reserve(.scan)!
        XCTAssertEqual(gate.withCaptureReservation { "capture" }, "capture")
        XCTAssertNil(gate.reserve(.scan)); XCTAssertNil(gate.beginExport { true })
        XCTAssertTrue(gate.begin(scan)); XCTAssertTrue(gate.finish(scan, cleanupComplete: true))
        XCTAssertTrue(gate.canDeliver(scan))
        XCTAssertNotNil(gate.reserve(.scan)); XCTAssertFalse(gate.canDeliver(scan))
    }
    func testSetupAndSerialExhaustionRefuseNewOwners() {
        let gate = RecordingLibraryAdmission(initialSerial: UInt64.max - 1)
        let setup = gate.reserve(.setup)!
        XCTAssertNil(gate.withCaptureReservation { "capture" })
        XCTAssertTrue(gate.begin(setup)); XCTAssertTrue(gate.finish(setup, cleanupComplete: true))
        XCTAssertNil(gate.reserve(.scan)); XCTAssertNil(gate.beginExport { true })
        XCTAssertNil(gate.reserve(.export))
    }
    func testUncertainScanCleanupBlocksCaptureUntilItsActualReceipt() {
        let gate = RecordingLibraryAdmission()
        let scan = gate.reserve(.scan)!
        XCTAssertTrue(gate.begin(scan))
        XCTAssertFalse(gate.finish(scan, cleanupComplete: false))
        XCTAssertNil(gate.withCaptureReservation { "capture" })
        XCTAssertTrue(gate.finish(scan, cleanupComplete: true))
        XCTAssertEqual(gate.withCaptureReservation { "capture" }, "capture")
    }
    func testConcurrentPrepareAndExportHaveOnlyOneWinner() {
        final class State: @unchecked Sendable {
            let gate = RecordingLibraryAdmission()
            var captureIdle = true // Accessed only inside the shared gate.
            var prepared = false // Written by the capture task, read after join.
            var exported = false // Written by the export task, read after join.
        }
        for _ in 0..<64 {
            let state = State(), start = DispatchSemaphore(value: 0), group = DispatchGroup()
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }; start.wait()
                state.prepared = state.gate.withCaptureReservation { state.captureIdle = false; return true } == true
            }
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }; start.wait()
                state.exported = state.gate.beginExport { state.captureIdle } != nil
            }
            start.signal(); start.signal()
            XCTAssertEqual(group.wait(timeout: .now() + 3), .success)
            XCTAssertNotEqual(state.prepared, state.exported)
        }
    }
    func testClosedUiCannotReleasePickerOrDeliverOldResult() {
        let gate = RecordingLibraryAdmission()
        let picker = gate.beginExport { true }!
        gate.close()
        XCTAssertFalse(gate.finish(picker, cleanupComplete: true))
        XCTAssertFalse(gate.dismissPicker(picker, selected: true))
        XCTAssertFalse(gate.canDeliver(picker)); XCTAssertNil(gate.reserve(.scan))
    }
}
