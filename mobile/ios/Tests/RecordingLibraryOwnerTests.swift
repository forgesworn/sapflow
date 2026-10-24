import XCTest
@testable import SapflowBroadcaster

/// Real generated-byte local IO with deterministic worker/delivery queues.
/// Neither UIKit pickers nor an external document provider run in these tests.
final class RecordingLibraryOwnerTests: XCTestCase {
    final class Queue {
        var jobs: [() -> Void] = []
        var replies: [() -> Void] = []
        func run() { jobs.removeFirst()() }
        func reply() { replies.removeFirst()() }
        func drain() {
            var count = 0
            while !jobs.isEmpty || !replies.isEmpty {
                count += 1; precondition(count < 100)
                if !jobs.isEmpty { run() }
                if !replies.isEmpty { reply() }
            }
        }
    }
    private func folder() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-library-generated-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        addTeardownBlock { try FileManager.default.removeItem(at: root) }
        return root
    }
    private func fixture(access: RecordingExportAccess? = nil,
                         makeDestination: ((URL, String) throws -> RecordingExportDestination)? = nil,
                         recovery: RecordingLibraryOwner.RecoveryAction? = nil) throws -> (RecordingLibraryOwner, RecordingCatalogue, Queue, URL) {
        let root = try folder(), queue = Queue()
        let access = access ?? RecordingExportAccess(begin: { _ in {} }, coordinate: { url, _, block in block(url) })
        let owner = RecordingLibraryOwner(root: root.appendingPathComponent("private"), schedule: { queue.jobs.append($0) },
            deliver: { queue.replies.append($0) }, access: access, recoverCheckpoint: recovery,
            makeDestination: makeDestination ?? { try RecordingExportDestination(folder: $0, filename: $1) })
        var store: RecordingCatalogue?
        owner.initialise { value, _, complete in store = value; complete(true) }
        queue.drain(); XCTAssertTrue(owner.ready)
        addTeardownBlock { owner.close(); queue.drain() }
        return (owner, store!, queue, root)
    }
    private func finished(_ store: RecordingCatalogue, bytes: Int = 129_001) throws -> RecordingCatalogue.Entry {
        let writer = try store.begin(identity: store.newShow(profile: .defaultProfile).nextIdentity())
        try Data((0..<bytes).map { UInt8($0 % 127) }).write(to: writer.fileURL)
        try writer.complete(durationMs: 1500, outcome: .finalised)
        return try store.scan().first { $0.segmentID == writer.fileURL.deletingPathExtension().lastPathComponent.replacingOccurrences(of: "Sapflow-", with: "") }!
    }
    private func select(_ owner: RecordingLibraryOwner, _ entry: RecordingCatalogue.Entry, folder: URL) -> RecordingLibraryOwner.Selection {
        owner.choose(entry: entry, allowUnverified: false, canExport: { true })
        let selection = owner.picker!
        owner.receivedFolder(selection, folder: folder)
        owner.pickerDismissed(selection)
        return selection
    }
    func testAudioOnlyLibraryExportUsesFrozenM4ASelection() throws {
        let (owner, store, queue, root) = try fixture()
        let writer = try store.begin(identity: store.newShow(profile: .audioOnly48kMono).nextIdentity())
        try Data([1, 2, 3]).write(to: writer.fileURL); try writer.complete(durationMs: 1, outcome: .finalised)
        let entry = try XCTUnwrap(store.scan().first)
        let output = root.appendingPathComponent("copies"); try FileManager.default.createDirectory(at: output, withIntermediateDirectories: false)
        _ = select(owner, entry, folder: output); queue.drain()
        let copy = try XCTUnwrap(FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: nil).first)
        XCTAssertEqual(copy.pathExtension, "m4a"); XCTAssertEqual(try Data(contentsOf: copy), Data([1, 2, 3]))
        XCTAssertTrue(owner.captureAllowed)
    }
    func testSetupIsColdAndCaptureWaitsForActualInstallerReceipt() throws {
        let root = try folder(), queue = Queue()
        let privateRoot = root.appendingPathComponent("private")
        let owner = RecordingLibraryOwner(root: privateRoot, schedule: { queue.jobs.append($0) }, deliver: { queue.replies.append($0) })
        var complete: ((Bool) -> Void)?
        owner.initialise { _, _, reply in complete = reply }
        XCTAssertFalse(FileManager.default.fileExists(atPath: privateRoot.path)); XCTAssertFalse(owner.captureAllowed)
        queue.run(); queue.reply()
        XCTAssertFalse(owner.ready); XCTAssertFalse(owner.captureAllowed)
        complete!(true); queue.drain()
        XCTAssertTrue(owner.ready); XCTAssertTrue(owner.captureAllowed)
        owner.close(); queue.drain()
    }
    func testPickerReplyDoesNotReleaseCaptureUntilActualDismissalAndClose() throws {
        let (owner, store, queue, root) = try fixture()
        let entry = try finished(store), output = root.appendingPathComponent("copies")
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: false)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true })
        let selection = owner.picker!
        owner.receivedFolder(selection, folder: output)
        XCTAssertNil(owner.picker); XCTAssertTrue(owner.presentation === selection)
        XCTAssertFalse(owner.captureAllowed); XCTAssertTrue(queue.jobs.isEmpty)
        owner.pickerDismissed(selection)
        XCTAssertFalse(owner.captureAllowed); queue.drain()
        XCTAssertTrue(owner.captureAllowed); XCTAssertFalse(owner.busy)
        let copy = try FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: nil).single!
        XCTAssertEqual(try Data(contentsOf: copy).count, Int(entry.bytes))
        XCTAssertEqual(try store.scan().single?.bytes, entry.bytes)
    }
    func testCancelledPickerRetainsItsExactReservationUntilDismissal() throws {
        let (owner, store, queue, _) = try fixture(); let entry = try finished(store)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true })
        let selection = owner.picker!
        owner.cancelOperation()
        XCTAssertNil(owner.picker); XCTAssertFalse(owner.captureAllowed); XCTAssertTrue(owner.busy)
        owner.pickerDismissed(selection)
        XCTAssertTrue(owner.captureAllowed); XCTAssertFalse(owner.busy); XCTAssertTrue(queue.jobs.isEmpty)
    }
    func testDuplicateOldRepliesAndDismissalCannotAffectSuccessorPicker() throws {
        let (owner, store, _, root) = try fixture(); let entry = try finished(store)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true }); let old = owner.picker!
        owner.receivedFolder(old, folder: nil); owner.pickerDismissed(old)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true }); let current = owner.picker!
        owner.receivedFolder(old, folder: root); owner.requestPickerDismissal(old); owner.pickerDismissed(old)
        XCTAssertTrue(owner.picker === current); XCTAssertTrue(owner.presentation === current)
        XCTAssertFalse(owner.captureAllowed)
        owner.receivedFolder(current, folder: nil); owner.pickerDismissed(current)
    }
    func testActualCapturePredicateIsRecheckedRatherThanTrustingDisplayedState() throws {
        let (owner, store, _, _) = try fixture(); let entry = try finished(store)
        owner.choose(entry: entry, allowUnverified: false, canExport: { false })
        XCTAssertNil(owner.picker); XCTAssertFalse(owner.busy); XCTAssertTrue(owner.captureAllowed)
    }
    func testQueuedCancellationNeverOpensScopeOrCreatesACopy() throws {
        var opens = 0
        let access = RecordingExportAccess(begin: { _ in opens += 1; return {} }, coordinate: { url, _, block in block(url) })
        let (owner, store, queue, root) = try fixture(access: access); let entry = try finished(store)
        _ = select(owner, entry, folder: root)
        owner.cancelOperation(); XCTAssertFalse(owner.captureAllowed)
        queue.drain(); XCTAssertEqual(opens, 0); XCTAssertTrue(owner.captureAllowed)
        XCTAssertEqual(try store.scan().single?.bytes, entry.bytes)
    }
    func testRunningCancellationKeepsOriginalAndClosesBeforeScopeRelease() throws {
        var ownerReference: RecordingLibraryOwner?, storeReference: RecordingCatalogue?
        var ended = 0, closes = 0
        let access = RecordingExportAccess(begin: { _ in
            { ended += 1; XCTAssertFalse(ownerReference!.captureAllowed); XCTAssertFalse(storeReference!.cleanupRetained); XCTAssertEqual(closes, 2) }
        }, coordinate: { url, _, block in block(url) })
        let (owner, store, queue, root) = try fixture(access: access, makeDestination: { folder, name in
            try RecordingExportDestination(folder: folder, filename: name, closeHandle: { closes += 1; try $0.close() },
                writeData: { handle, data in try handle.write(contentsOf: data); ownerReference!.cancelOperation() })
        })
        ownerReference = owner; storeReference = store
        let entry = try finished(store), copies = root.appendingPathComponent("copies")
        try FileManager.default.createDirectory(at: copies, withIntermediateDirectories: false)
        _ = select(owner, entry, folder: copies); queue.drain()
        XCTAssertEqual(ended, 1); XCTAssertTrue(owner.captureAllowed)
        let copy = try FileManager.default.contentsOfDirectory(at: copies, includingPropertiesForKeys: nil).single!
        XCTAssertLessThan(try Data(contentsOf: copy).count, Int(entry.bytes))
        XCTAssertEqual(try store.scan().single?.bytes, entry.bytes)
    }
    func testActualDestinationCloseFailureRetainsCapacityAfterSourceClose() throws {
        var closes = 0, scopeEnded = 0
        let access = RecordingExportAccess(begin: { _ in { scopeEnded += 1 } }, coordinate: { url, _, block in block(url) })
        let (owner, store, queue, root) = try fixture(access: access, makeDestination: { folder, name in
            try RecordingExportDestination(folder: folder, filename: name, closeHandle: { handle in
                closes += 1; try handle.close()
                if closes == 1 { throw NSError(domain: "GeneratedFailure", code: 1) }
            })
        })
        let entry = try finished(store); _ = select(owner, entry, folder: root); queue.drain()
        XCTAssertTrue(owner.cleanupRetained); XCTAssertTrue(owner.busy); XCTAssertFalse(owner.captureAllowed)
        XCTAssertFalse(store.cleanupRetained)
        XCTAssertEqual(scopeEnded, 0)
        XCTAssertEqual(try store.scan().single?.bytes, entry.bytes)
    }
    func testChangedSourceIsRefusedBeforeCreatingDestination() throws {
        var creates = 0
        let (owner, store, queue, root) = try fixture(makeDestination: { folder, name in
            creates += 1; return try RecordingExportDestination(folder: folder, filename: name)
        })
        let entry = try finished(store)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true }); let selection = owner.picker!
        let writer = try store.begin(identity: store.newShow(profile: .defaultProfile).nextIdentity())
        try Data([1]).write(to: writer.fileURL); try writer.complete(durationMs: 1, outcome: .finalised)
        // Replace the exact source's receipt: choosing an old row must not
        // silently choose any newer segment or a changed original.
        let source = store.recordingRootURL.appendingPathComponent("Sapflow-\(entry.segmentID).mov")
        let file = try FileHandle(forWritingTo: source); try file.seekToEnd(); try file.write(contentsOf: Data([2])); try file.close()
        owner.receivedFolder(selection, folder: root); owner.pickerDismissed(selection); queue.drain()
        XCTAssertEqual(creates, 0); XCTAssertTrue(owner.captureAllowed); XCTAssertFalse(store.cleanupRetained)
    }
    func testUncertainDestinationKeepsCataloguePinnedAfterOwnerClose() throws {
        var closes = 0
        let (owner, store, queue, root) = try fixture(makeDestination: { folder, name in
            try RecordingExportDestination(folder: folder, filename: name, closeHandle: { handle in
                closes += 1; try handle.close()
                if closes == 1 { throw NSError(domain: "GeneratedCloseFailure", code: 1) }
            })
        })
        let entry = try finished(store); _ = select(owner, entry, folder: root); queue.drain()
        XCTAssertTrue(owner.cleanupRetained)
        owner.close(); queue.drain()
        XCTAssertFalse(store.cleanupComplete)
        XCTAssertThrowsError(try RecordingCatalogue(root: store.recordingRootURL)) { error in
            XCTAssertEqual(error as? RecordingCatalogue.Failure, .busy)
        }
    }
    func testClosingOwnerKeepsPickerFenceUntilItsActualDismissal() throws {
        let (owner, store, queue, _) = try fixture(); let entry = try finished(store)
        owner.choose(entry: entry, allowUnverified: false, canExport: { true })
        let selection = owner.picker!
        owner.close(); queue.drain()
        XCTAssertFalse(store.cleanupComplete)
        XCTAssertThrowsError(try RecordingCatalogue(root: store.recordingRootURL)) { error in
            XCTAssertEqual(error as? RecordingCatalogue.Failure, .busy)
        }
        owner.receivedFolder(selection, folder: store.recordingRootURL) // Late reply is retired.
        owner.pickerDismissed(selection); queue.drain()
        XCTAssertTrue(store.cleanupComplete)
        let fresh = try RecordingCatalogue(root: store.recordingRootURL)
        try fresh.close()
    }
    func testScopeDenialAndCoordinatorFailureLeaveNoSourceLease() throws {
        for failScope in [true, false] {
            var ended = 0
            let access = RecordingExportAccess(begin: { _ in
                if failScope { throw NSError(domain: "GeneratedScopeFailure", code: 1) }
                return { ended += 1 }
            }, coordinate: { _, _, _ in throw NSError(domain: "GeneratedCoordinatorFailure", code: 1) })
            let (owner, store, queue, root) = try fixture(access: access); let entry = try finished(store)
            _ = select(owner, entry, folder: root); queue.drain()
            XCTAssertTrue(owner.captureAllowed); XCTAssertFalse(store.cleanupRetained)
            XCTAssertEqual(ended, failScope ? 0 : 1)
        }
    }
    func testHealthyLiveScanDoesNotPoisonCaptureAndOldScanCannotReplaceNewResult() throws {
        let (owner, store, queue, _) = try fixture()
        let writer = try store.begin(identity: store.newShow(profile: .defaultProfile).nextIdentity())
        try Data([1, 2, 3]).write(to: writer.fileURL)
        owner.reload(); queue.run(); queue.reply()
        XCTAssertEqual(owner.entries.single?.state, .writing); XCTAssertTrue(owner.captureAllowed)
        writer.cancel(); try writer.ownersClosed()
        owner.reload(); queue.run()
        owner.reload() // Invalidates the already-posted earlier result.
        queue.reply(); queue.drain()
        XCTAssertEqual(owner.entries.single?.state, .incomplete); XCTAssertFalse(owner.busy)
    }
    func testSetupFailureCanBeExplicitlyRetried() throws {
        let root = try folder(), queue = Queue()
        let owner = RecordingLibraryOwner(root: root.appendingPathComponent("private"), schedule: { queue.jobs.append($0) }, deliver: { queue.replies.append($0) })
        var attempts = 0
        owner.initialise { _, _, complete in attempts += 1; complete(attempts > 1) }
        queue.drain(); XCTAssertFalse(owner.ready); XCTAssertFalse(owner.busy)
        owner.retryInitialisation(); queue.drain()
        XCTAssertTrue(owner.ready); XCTAssertEqual(attempts, 2)
        owner.close(); queue.drain()
    }
    private func incomplete(_ store: RecordingCatalogue) throws -> RecordingCatalogue.Entry {
        let writer = try store.begin(identity: store.newShow(profile: .audioOnly48kMono).nextIdentity())
        try Data([1, 2, 3, 4]).write(to: writer.fileURL); try writer.complete(durationMs: 0, outcome: .incomplete)
        return try XCTUnwrap(store.scan().first { $0.segmentID == writer.identity.segmentID })
    }
    func testRecoveryRechecksCaptureAndAcknowledgesQueuedCancellationWithoutCallingSdkAction() throws {
        var calls = 0
        let (owner, store, queue, _) = try fixture(recovery: { _, _, _, _, _ in calls += 1; throw RecordingCatalogue.Failure.ineligible })
        let entry = try incomplete(store)
        owner.recover(entry: entry, canRecover: { false }); XCTAssertFalse(owner.busy); XCTAssertEqual(calls, 0)
        owner.recover(entry: entry, canRecover: { true }); XCTAssertTrue(owner.busy); XCTAssertFalse(owner.captureAllowed)
        owner.cancelOperation(); XCTAssertFalse(owner.captureAllowed)
        queue.drain(); XCTAssertEqual(calls, 0); XCTAssertTrue(owner.captureAllowed); XCTAssertFalse(owner.busy)
        XCTAssertEqual(try store.scan().first?.state, .incomplete)
    }
    func testRecoveryRefusalKeepsOriginalAndHoldsGateUntilActualWorkerAcknowledgement() throws {
        var calls = 0
        var ownerReference: RecordingLibraryOwner?
        let (owner, store, queue, _) = try fixture(recovery: { _, _, _, cancelled, publish in
            calls += 1; XCTAssertFalse(cancelled()); XCTAssertFalse(ownerReference!.captureAllowed)
            XCTAssertTrue(publish()); XCTAssertFalse(publish())
            throw RecordingCatalogue.Failure.changed
        })
        ownerReference = owner
        let entry = try incomplete(store)
        owner.recover(entry: entry, canRecover: { true }); queue.run()
        // Worker cleanup acknowledged, delivery still queued: no fabricated result.
        XCTAssertEqual(calls, 1); queue.reply(); XCTAssertFalse(owner.busy); XCTAssertTrue(owner.captureAllowed)
        XCTAssertEqual(try store.scan().count, 1); XCTAssertEqual(try store.scan().first?.state, .incomplete)
        XCTAssertTrue(try XCTUnwrap(owner.message).contains("preserved"))
    }
    func testLibraryWithoutRealRecoveryCompositionCannotScheduleRecovery() throws {
        let (owner, store, queue, _) = try fixture(); let entry = try incomplete(store)
        XCTAssertFalse(owner.recoveryAvailable)
        owner.recover(entry: entry, canRecover: { true })
        XCTAssertTrue(queue.jobs.isEmpty); XCTAssertFalse(owner.busy); XCTAssertTrue(owner.captureAllowed)
        XCTAssertEqual(try store.scan().count, 1)
    }

}

private extension Array {
    var single: Element? { count == 1 ? first : nil }
}
