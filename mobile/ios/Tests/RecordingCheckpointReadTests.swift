import XCTest
import Darwin
import CryptoKit
@testable import SapflowBroadcaster

/// Actual host IO/ownership only. Slot bytes below are deliberately untrusted,
/// NOT encoded SFCP or proof of recovery/playability/native source completion.
final class RecordingCheckpointReadTests: XCTestCase {
    private var scratch: URL!
    override func setUpWithError() throws {
        scratch = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-checkpoint-read-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: false)
    }
    override func tearDownWithError() throws { try FileManager.default.removeItem(at: scratch) }
    private let header = Data(repeating: 3, count: 8)
    private let first = Data([1, 2, 3, 4, 5, 6, 7])
    private let second = Data([11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21])
    private struct Fixture {
        let root: URL; let catalogue: RecordingCatalogue; let identity: RecordingCatalogue.Identity
        let selected: RecordingCatalogue.Entry
    }
    private func fixture(slots: Bool = true, io: RecordingCatalogue.CheckpointIO = .native,
                         close: @escaping (Int32) -> Int32 = Darwin.close,
                         appendIO: RecordingCatalogue.AppendIO? = nil) throws -> Fixture {
        let root = scratch.appendingPathComponent(UUID().uuidString)
        let append = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { _ in 1024 * 1024 * 1024 })
        let catalogue = try RecordingCatalogue(root: root, appendIO: appendIO ?? append, checkpointIO: io, closeFileDescriptor: close)
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let writer = try catalogue.beginAppend(identity: identity, payloadLimit: 4096)
        _ = try writer.writeInitial(header)
        _ = try writer.appendFragment(first, expectedEnd: 15)
        _ = try writer.appendFragment(second, expectedEnd: 26)
        _ = try writer.synchronizeMedia(); _ = try writer.closeIncomplete()
        if slots {
            try privateWrite(Data([41, 42]), root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.0"))
            try privateWrite(Data([51, 52, 53]), root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.1"))
        }
        return Fixture(root: root, catalogue: catalogue, identity: identity, selected: try XCTUnwrap(catalogue.scan().first))
    }
    private func privateWrite(_ bytes: Data, _ path: URL) throws {
        try bytes.write(to: path, options: .withoutOverwriting); XCTAssertEqual(chmod(path.path, 0o600), 0)
    }
    private func assertFailure(_ expected: RecordingCatalogue.Failure, _ action: () throws -> Void) {
        XCTAssertThrowsError(try action()) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, expected) }
    }
    private func slot(_ f: Fixture, _ index: Int) -> URL { f.root.appendingPathComponent("Sapflow-\(f.identity.segmentID).checkpoint.\(index)") }
    private func metadataIO(read: @escaping (Int32, UnsafeMutableRawPointer, Int) -> Int) -> RecordingCatalogue.CheckpointIO {
        RecordingCatalogue.CheckpointIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            rename: { Darwin.renameat($0, $1, $0, $2) }, synchronizeDirectory: Darwin.fsync, read: read)
    }

    func testUntrustedSnapshotUsesPersistedActualBindingsAndRetainsTheCatalogueLease() throws {
        let f = try fixture(slots: false); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
        let snapshot = try reader.snapshot(use)
        XCTAssertEqual(snapshot.identity, f.identity); XCTAssertEqual(snapshot.mediaBytes, 26)
        XCTAssertEqual(snapshot.slots.count, 2); XCTAssertNil(snapshot.slots[0]); XCTAssertNil(snapshot.slots[1])
        var root = stat(), media = stat()
        XCTAssertEqual(lstat(f.root.path, &root), 0); XCTAssertEqual(lstat(f.root.appendingPathComponent(f.identity.filename).path, &media), 0)
        let source = RecordingCatalogue.AppendSource(rootDevice: root.st_dev, rootInode: root.st_ino,
            device: media.st_dev, inode: media.st_ino, birthSeconds: Int64(media.st_birthtimespec.tv_sec), birthNanoseconds: Int64(media.st_birthtimespec.tv_nsec))
        XCTAssertEqual(snapshot.bindings, try source.bindings(identity: f.identity))
        assertFailure(.busy) { try reader.close() }; assertFailure(.busy) { _ = try reader.start() }
        assertFailure(.busy) { try f.catalogue.close() }
        assertFailure(.busy) { _ = try RecordingCatalogue(root: f.root) }
        try reader.finish(use, snapshot: snapshot); XCTAssertTrue(reader.cleanupComplete)
        XCTAssertFalse(f.catalogue.cleanupRetained)
    }

    func testOnePartialReadPassHashesBothPrefixesAndPermitsChangedCurrentInitial() throws {
        var mediaInode: UInt64 = 0, mediaReadBytes = 0
        let io = metadataIO { fd, pointer, count in
            let n = Darwin.read(fd, pointer, min(count, 3)); var value = stat()
            if fstat(fd, &value) == 0, value.st_ino == mediaInode, n > 0 { mediaReadBytes += n }
            return n
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let path = f.root.appendingPathComponent(f.identity.filename), changedHeader = Data(repeating: 99, count: 8)
        let fd = Darwin.open(path.path, O_WRONLY); XCTAssertGreaterThanOrEqual(fd, 0)
        XCTAssertEqual(changedHeader.withUnsafeBytes { Darwin.pwrite(fd, $0.baseAddress!, $0.count, 0) }, 8); XCTAssertEqual(Darwin.close(fd), 0)
        var actual = stat(); XCTAssertEqual(lstat(path.path, &actual), 0); mediaInode = actual.st_ino
        let selected = try XCTUnwrap(f.catalogue.scan().first)
        let reader = try f.catalogue.beginCheckpointRead(selected: selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        let facts = try reader.hashPrefixes(use, snapshot: snapshot, initialLength: 8, committedOffsets: [15, 26], maximumFragmentBytes: 18)
        XCTAssertEqual(facts.fragmentHashes, [Data(SHA256.hash(data: first)), Data(SHA256.hash(data: first + second))])
        XCTAssertEqual(facts.currentHeaderSHA256, Data(SHA256.hash(data: changedHeader)))
        try reader.finish(use, snapshot: snapshot); XCTAssertEqual(mediaReadBytes, 26)
        XCTAssertEqual(try Data(contentsOf: path), changedHeader + first + second)
    }

    func testReplacedReceiptOrMediaCannotReuseAnOldSelection() throws {
        for receipt in [true, false] {
            let f = try fixture(); defer { try? f.catalogue.close() }
            let name = receipt ? "Sapflow-\(f.identity.segmentID).receipt.json" : f.identity.filename
            let path = f.root.appendingPathComponent(name), bytes = try Data(contentsOf: path)
            try FileManager.default.moveItem(at: path, to: f.root.appendingPathComponent("preserved-original")); try privateWrite(bytes, path)
            let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
            assertFailure(.changed) { _ = try reader.snapshot(use) }
            try reader.abort(use); XCTAssertTrue(reader.cleanupComplete); XCTAssertEqual(try Data(contentsOf: path), bytes)
        }
    }

    func testLegacyReceiptWithoutAppendSourceRemainsProbeIneligible() throws {
        let root = scratch.appendingPathComponent("legacy"), catalogue = try RecordingCatalogue(root: root)
        defer { try? catalogue.close() }
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let writer = try catalogue.begin(identity: identity)
        try privateWrite(header + first, writer.fileURL); try writer.complete(durationMs: 1, outcome: .incomplete)
        let reader = try catalogue.beginCheckpointRead(selected: XCTUnwrap(catalogue.scan().first)), use = try reader.start()
        assertFailure(.ineligible) { _ = try reader.snapshot(use) }; try reader.abort(use)
        XCTAssertEqual(try Data(contentsOf: writer.fileURL), header + first)
    }

    func testSlotBoundsPrivateModeLinksAndPresentEmptyBytesAreNotTrusted() throws {
        for kind in ["oversize", "symlink", "hardlink", "mode", "empty"] {
            let f = try fixture(slots: false); defer { try? f.catalogue.close() }
            let path = slot(f, 1), target = f.root.appendingPathComponent("preserved-target")
            if kind == "symlink" || kind == "hardlink" {
                try privateWrite(Data([8]), target)
                if kind == "symlink" { try FileManager.default.createSymbolicLink(at: path, withDestinationURL: target) }
                else { XCTAssertEqual(Darwin.link(target.path, path.path), 0) }
            } else {
                try privateWrite(Data(repeating: 7, count: kind == "oversize" ? 66_081 : kind == "empty" ? 0 : 1), path)
                if kind == "mode" { XCTAssertEqual(chmod(path.path, 0o644), 0) }
            }
            let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
            if kind == "empty" {
                let snapshot = try reader.snapshot(use); XCTAssertNotNil(snapshot.slots[1]); XCTAssertTrue(snapshot.slots[1]!.isEmpty)
                try reader.finish(use, snapshot: snapshot) // IO only, no SDK eligibility.
            } else {
                assertFailure(kind == "oversize" ? .capacity : .unsafe) { _ = try reader.snapshot(use) }; try reader.abort(use)
            }
            XCTAssertTrue(reader.cleanupComplete)
        }
    }

    func testCrashLeftTemporaryNamesRefuseAndRemainUntouched() throws {
        for suffix in ["checkpoint.tmp", "receipt.tmp"] {
            let f = try fixture(); defer { try? f.catalogue.close() }
            let path = f.root.appendingPathComponent("Sapflow-\(f.identity.segmentID).\(suffix)")
            try privateWrite(Data([1, 9]), path)
            let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
            assertFailure(.changed) { _ = try reader.snapshot(use) }; try reader.abort(use)
            XCTAssertEqual(try Data(contentsOf: path), Data([1, 9]))
        }
    }

    func testSlotMutationDuringReadCannotPublishASnapshot() throws {
        var path: URL?, victim: UInt64 = 0, changed = false
        let io = metadataIO { fd, pointer, count in
            let n = Darwin.read(fd, pointer, count); var value = stat()
            if !changed, victim != 0, fstat(fd, &value) == 0, value.st_ino == victim, let path {
                changed = true
                let writeFD = Darwin.open(path.path, O_WRONLY)
                var byte: UInt8 = 88; _ = Darwin.pwrite(writeFD, &byte, 1, 0); _ = Darwin.close(writeFD)
            }
            return n
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }; path = slot(f, 1)
        var value = stat(); XCTAssertEqual(lstat(path!.path, &value), 0); victim = value.st_ino
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
        assertFailure(.changed) { _ = try reader.snapshot(use) }; try reader.abort(use)
        XCTAssertTrue(changed); XCTAssertEqual(try Data(contentsOf: path!).first, 88)
    }

    func testMissingSlotAppearingAfterSnapshotFencesFinishWithoutDeletingIt() throws {
        let f = try fixture(slots: false); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        try privateWrite(Data([9]), slot(f, 0))
        assertFailure(.changed) { try reader.finish(use, snapshot: snapshot) }
        XCTAssertTrue(reader.cleanupComplete); try reader.abort(use)
        XCTAssertEqual(try Data(contentsOf: slot(f, 0)), Data([9]))
    }

    func testReadCallbackCancellationAndReentrantCloseCannotEscapeTheExactUse() throws {
        var reader: RecordingCatalogue.CheckpointRead?, interrupted = false
        let io = metadataIO { fd, pointer, count in
            let n = Darwin.read(fd, pointer, count)
            if let reader, !interrupted {
                interrupted = true
                self.assertFailure(.busy) { try reader.close() }
                self.assertFailure(.busy) { _ = try reader.start() }
                reader.cancel()
            }
            return n
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let child = try f.catalogue.beginCheckpointRead(selected: f.selected); reader = child
        let use = try child.start()
        assertFailure(.cancelled) { _ = try child.snapshot(use) }; try child.abort(use)
        XCTAssertTrue(interrupted); XCTAssertTrue(child.cleanupComplete); XCTAssertFalse(f.catalogue.cleanupRetained)
    }

    func testForeignUseSnapshotAndOutOfRangeHashBudgetsRefuse() throws {
        let a = try fixture(), b = try fixture(); defer { try? a.catalogue.close(); try? b.catalogue.close() }
        let one = try a.catalogue.beginCheckpointRead(selected: a.selected), two = try b.catalogue.beginCheckpointRead(selected: b.selected)
        let u = try one.start(), v = try two.start(), x = try one.snapshot(u), y = try two.snapshot(v)
        assertFailure(.closed) { _ = try one.hashPrefixes(v, snapshot: x, initialLength: 8, committedOffsets: [26], maximumFragmentBytes: 18) }
        assertFailure(.closed) { _ = try one.hashPrefixes(u, snapshot: y, initialLength: 8, committedOffsets: [26], maximumFragmentBytes: 18) }
        let invalidEnds: [[UInt64]] = [[15, 15], [26, 15], [27], [UInt64.max]]
        for ends in invalidEnds {
            assertFailure(.capacity) { _ = try one.hashPrefixes(u, snapshot: x, initialLength: 8, committedOffsets: ends, maximumFragmentBytes: 18) }
        }
        assertFailure(.capacity) { _ = try one.hashPrefixes(u, snapshot: x, initialLength: 8, committedOffsets: [26], maximumFragmentBytes: 17) }
        try one.abort(u); try two.abort(v)
    }

    func testUnknownCloseRetainsAggregateAndNeverRetriesTheNumericDescriptor() throws {
        var armed = false, failedFD: Int32?, attempts: [Int32: Int] = [:]
        let f = try fixture(close: { fd in
            if armed { attempts[fd, default: 0] += 1 }
            let result = Darwin.close(fd)
            if armed, failedFD == nil { failedFD = fd; errno = EIO; return -1 }
            return result
        })
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        armed = true
        assertFailure(.io) { try reader.finish(use, snapshot: snapshot) }
        XCTAssertEqual(attempts[try XCTUnwrap(failedFD)], 1)
        assertFailure(.closed) { try reader.abort(use) }; assertFailure(.busy) { try reader.close() }
        assertFailure(.busy) { try f.catalogue.close() }
        XCTAssertEqual(attempts[try XCTUnwrap(failedFD)], 1)
        XCTAssertFalse(reader.cleanupComplete); XCTAssertTrue(f.catalogue.cleanupUncertain); XCTAssertTrue(f.catalogue.cleanupRetained)
    }

    func testReadFailureAndChangedRootPreserveFilesAndCloseKnownOwners() throws {
        var failRead = false
        let io = metadataIO { fd, pointer, count in if failRead { errno = EIO; return -1 }; return Darwin.read(fd, pointer, count) }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(); failRead = true
        assertFailure(.io) { _ = try reader.snapshot(use) }; try reader.abort(use); XCTAssertTrue(reader.cleanupComplete)
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), header + first + second)
        let g = try fixture(); defer { try? g.catalogue.close() }
        let child = try g.catalogue.beginCheckpointRead(selected: g.selected), active = try child.start(), snapshot = try child.snapshot(active)
        let moved = scratch.appendingPathComponent("moved-root")
        try FileManager.default.moveItem(at: g.root, to: moved)
        try FileManager.default.createDirectory(at: g.root, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        assertFailure(.changed) { _ = try child.hashPrefixes(active, snapshot: snapshot, initialLength: 8, committedOffsets: [26], maximumFragmentBytes: 18) }
        try child.abort(active); XCTAssertTrue(child.cleanupComplete)
        XCTAssertEqual(try Data(contentsOf: moved.appendingPathComponent(g.identity.filename)), header + first + second)
    }

    func testPartialMetadataReadRejectsReentrantAbortAndNestedSnapshotBeforeAnyClose() throws {
        var child: RecordingCatalogue.CheckpointRead?, active: RecordingCatalogue.CheckpointRead.Use?
        var armed = false, attempted = false, closes = 0
        let io = metadataIO { fd, pointer, count in
            var before = stat(); XCTAssertEqual(fstat(fd, &before), 0)
            let n = Darwin.read(fd, pointer, min(count, 2))
            if armed, !attempted, let child, let active {
                attempted = true
                self.assertFailure(.busy) { try child.abort(active) }
                self.assertFailure(.busy) { _ = try child.snapshot(active) }
                self.assertFailure(.busy) { try child.close() }
                XCTAssertEqual(closes, 0)
                var after = stat(); XCTAssertEqual(fstat(fd, &after), 0)
                XCTAssertEqual(before.st_dev, after.st_dev); XCTAssertEqual(before.st_ino, after.st_ino)
            }
            return n
        }
        let f = try fixture(io: io, close: { fd in if armed { closes += 1 }; return Darwin.close(fd) })
        defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
        child = reader; active = use; armed = true
        let snapshot = try reader.snapshot(use)
        XCTAssertTrue(attempted); XCTAssertEqual(closes, 0); XCTAssertTrue(f.catalogue.cleanupRetained)
        XCTAssertEqual(snapshot.slots[0], Data([41, 42])); XCTAssertEqual(snapshot.slots[1], Data([51, 52, 53]))
        try reader.finish(use, snapshot: snapshot)
        XCTAssertEqual(closes, 4); XCTAssertTrue(reader.cleanupComplete); XCTAssertFalse(f.catalogue.cleanupRetained)
    }

    func testPartialMediaHashRejectsReentrantFinishAbortAndNestedHashWithoutFdReuse() throws {
        var child: RecordingCatalogue.CheckpointRead?, active: RecordingCatalogue.CheckpointRead.Use?
        var captured: RecordingCatalogue.CheckpointRead.Snapshot?
        var armed = false, attempted = false, closes = 0
        let io = metadataIO { fd, pointer, count in
            var before = stat(); XCTAssertEqual(fstat(fd, &before), 0)
            let n = Darwin.read(fd, pointer, min(count, 3))
            if armed, !attempted, let child, let active, let captured {
                attempted = true
                self.assertFailure(.busy) { try child.finish(active, snapshot: captured) }
                self.assertFailure(.busy) { try child.abort(active) }
                self.assertFailure(.busy) { _ = try child.hashPrefixes(active, snapshot: captured, initialLength: 8,
                    committedOffsets: [26], maximumFragmentBytes: 18) }
                XCTAssertEqual(closes, 0)
                var after = stat(); XCTAssertEqual(fstat(fd, &after), 0)
                XCTAssertEqual(before.st_dev, after.st_dev); XCTAssertEqual(before.st_ino, after.st_ino)
            }
            return n
        }
        let f = try fixture(io: io, close: { fd in if armed { closes += 1 }; return Darwin.close(fd) })
        defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        child = reader; active = use; captured = snapshot; armed = true
        let hashes = try reader.hashPrefixes(use, snapshot: snapshot, initialLength: 8,
            committedOffsets: [15, 26], maximumFragmentBytes: 18)
        XCTAssertTrue(attempted); XCTAssertEqual(closes, 0); XCTAssertTrue(f.catalogue.cleanupRetained)
        XCTAssertEqual(hashes.fragmentHashes, [Data(SHA256.hash(data: first)), Data(SHA256.hash(data: first + second))])
        try reader.finish(use, snapshot: snapshot)
        XCTAssertEqual(closes, 4); XCTAssertTrue(reader.cleanupComplete); XCTAssertFalse(f.catalogue.cleanupRetained)
    }


    func testFinishAfterAbortOrFailedFinishRefusesWithoutRetryingClosedDescriptors() throws {
        for abortFirst in [true, false] {
            var armed = false, closes = 0
            let f = try fixture(slots: false, close: { fd in if armed { closes += 1 }; return Darwin.close(fd) })
            defer { try? f.catalogue.close() }
            let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
            armed = true
            if abortFirst { try reader.abort(use) }
            try privateWrite(Data([81]), slot(f, 0))
            if !abortFirst { assertFailure(.changed) { try reader.finish(use, snapshot: snapshot) } }
            XCTAssertTrue(reader.cleanupComplete); XCTAssertEqual(closes, 2)
            assertFailure(.closed) { try reader.finish(use, snapshot: snapshot) }
            try reader.abort(use); try reader.close()
            XCTAssertEqual(closes, 2)
            XCTAssertEqual(try Data(contentsOf: slot(f, 0)), Data([81]))
        }
    }

    // These are untrusted storage recipes over real host IO, NOT fake SDK
    // validators. The separate Probe tests must exercise real SFCP decoding.
    private func storageRecipe(_ snapshot: RecordingCatalogue.CheckpointRead.Snapshot) throws -> RecordingCatalogue.RecoveryRecipe {
        .init(sequence: 3, checkpointSHA256: Data(SHA256.hash(data: try XCTUnwrap(snapshot.slots[1]))),
              header: Data(repeating: 9, count: 8), fragmentBytes: 18,
              fragmentSHA256: Data(SHA256.hash(data: first + second)))
    }
    private func storageRecovery(_ f: Fixture) throws -> RecordingCatalogue.RecoveredPrefix {
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        return try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18)
    }
    private struct DescriptorIncarnation: Hashable {
        let descriptor: Int32
        let device: dev_t
        let inode: ino_t
    }
    private func descriptorIncarnation(_ descriptor: Int32) -> DescriptorIncarnation? {
        var node = stat()
        guard fstat(descriptor, &node) == 0 else { return nil }
        return .init(descriptor: descriptor, device: node.st_dev, inode: node.st_ino)
    }
    func testRecoveryStorageUsesSavedHeaderExactPrefixAndNewUnverifiedProvenance() throws {
        let f = try fixture(); defer { try? f.catalogue.close() }
        let original = try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename))
        let result = try storageRecovery(f)
        XCTAssertFalse(result.reused); XCTAssertEqual(result.entry.state, .recovered); XCTAssertNil(result.entry.durationMs)
        let destination = f.root.appendingPathComponent(try XCTUnwrap(result.entry.identity).filename)
        XCTAssertEqual(try Data(contentsOf: destination), Data(repeating: 9, count: 8) + first + second)
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), original)
        let source = try XCTUnwrap(f.catalogue.scan().first { $0.segmentID == f.identity.segmentID })
        XCTAssertEqual(source.state, .incomplete)
        let receipt = try Data(contentsOf: f.root.appendingPathComponent("Sapflow-\(result.entry.segmentID).receipt.json"))
        XCTAssertTrue(RecordingCatalogue.checkRecoveryReceiptBytes(receipt))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: receipt) as? [String: Any])
        XCTAssertEqual(object["version"] as? Int, 2); XCTAssertEqual(object["state"] as? String, "recovered")
        XCTAssertNil(object["durationMs"]); XCTAssertNil(object["appendSource"])
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(receipt + Data([32])))
        for (key, value) in [("version", 3), ("durationMs", 0)] {
            var invalid = object; invalid[key] = value
            let bytes = try JSONSerialization.data(withJSONObject: invalid, options: [.sortedKeys, .withoutEscapingSlashes])
            XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(bytes))
        }
        var invalid = object
        var origin = try XCTUnwrap(invalid["recovery"] as? [String: Any]); origin["sourceFileBinding"] = Data([1]).base64EncodedString()
        invalid["recovery"] = origin
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(try JSONSerialization.data(withJSONObject: invalid, options: [.sortedKeys, .withoutEscapingSlashes])))
        XCTAssertFalse(f.catalogue.cleanupRetained)
    }
    func testRecoverySuccessRetryVerifiesSameFileAndRejectsTamperedResult() throws {
        let f = try fixture(); defer { try? f.catalogue.close() }
        let first = try storageRecovery(f), path = f.root.appendingPathComponent(try XCTUnwrap(first.entry.identity).filename)
        let originalCopy = try Data(contentsOf: path)
        let repeated = try storageRecovery(f)
        XCTAssertTrue(repeated.reused); XCTAssertEqual(repeated.entry.segmentID, first.entry.segmentID)
        XCTAssertEqual(try f.catalogue.scan().count, 2); XCTAssertEqual(try Data(contentsOf: path), originalCopy)
        try f.catalogue.close()
        let reopened = try RecordingCatalogue(root: f.root); defer { try? reopened.close() }
        let freshSelection = try XCTUnwrap(reopened.scan().first { $0.segmentID == f.identity.segmentID })
        let cold = Fixture(root: f.root, catalogue: reopened, identity: f.identity, selected: freshSelection)
        let coldRetry = try storageRecovery(cold)
        XCTAssertTrue(coldRetry.reused); XCTAssertEqual(coldRetry.entry.segmentID, first.entry.segmentID)
        let fd = Darwin.open(path.path, O_WRONLY | O_NOFOLLOW); XCTAssertGreaterThanOrEqual(fd, 0)
        var changed: UInt8 = 77; XCTAssertEqual(Darwin.pwrite(fd, &changed, 1, 8), 1); XCTAssertEqual(Darwin.close(fd), 0)
        let tampered = try Data(contentsOf: path)
        XCTAssertThrowsError(try storageRecovery(cold)); XCTAssertEqual(try Data(contentsOf: path), tampered)
        XCTAssertFalse(reopened.cleanupRetained); XCTAssertEqual(try reopened.scan().count, 2)
    }
    func testRecoveryPartialWritePreservesOriginalAndOccupiedAttemptWithoutRetry() throws {
        var armed = false, writes = 0
        let append = RecordingCatalogue.AppendIO(write: { fd, pointer, count in
            if !armed { return Darwin.write(fd, pointer, count) }
            writes += 1
            if writes == 1 { return Darwin.write(fd, pointer, min(3, count)) }
            errno = EIO; return -1
        }, synchronize: Darwin.fsync, availableBytes: { _ in 1024 * 1024 * 1024 })
        let f = try fixture(appendIO: append); defer { try? f.catalogue.close() }
        let original = try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)); armed = true
        XCTAssertThrowsError(try storageRecovery(f)); XCTAssertEqual(writes, 2)
        let partial = try XCTUnwrap(f.catalogue.scan().first { $0.segmentID != f.identity.segmentID })
        XCTAssertEqual(partial.state, .orphanUnverified); XCTAssertEqual(partial.bytes, 3)
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), original)
        XCTAssertThrowsError(try storageRecovery(f)); XCTAssertEqual(writes, 2)
        XCTAssertEqual(try f.catalogue.scan().count, 2); XCTAssertFalse(f.catalogue.cleanupRetained)
    }
    func testRecoveryCancellationDuringActualReadStopsBeforeAnotherReadAndKeepsOriginal() throws {
        var child: RecordingCatalogue.CheckpointRead?, armed = false, reads = 0
        let io = metadataIO { fd, pointer, count in
            let n = Darwin.read(fd, pointer, min(count, 3))
            if armed { reads += 1; child?.cancel() }
            return n
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        child = reader; armed = true
        assertFailure(.cancelled) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        XCTAssertEqual(reads, 1); XCTAssertTrue(reader.cleanupComplete); XCTAssertFalse(f.catalogue.cleanupRetained)
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), header + first + second)
    }
    func testRecoveryInFlightIORejectsReentrantRetirementAndSecondRecovery() throws {
        var child: RecordingCatalogue.CheckpointRead?, active: RecordingCatalogue.CheckpointRead.Use?
        var captured: RecordingCatalogue.CheckpointRead.Snapshot?
        var armed = false, attempted = false, observingSnapshot = false
        var allCloseAttempts = 0
        var sourceInodes = Set<ino_t>(), sources = Set<DescriptorIncarnation>()
        var sourceCloses: [DescriptorIncarnation: Int] = [:]
        let io = metadataIO { fd, pointer, count in
            if let key = self.descriptorIncarnation(fd), observingSnapshot || armed && sourceInodes.contains(key.inode) {
                sources.insert(key)
            }
            if armed, !attempted, let child, let active, let captured {
                attempted = true
                let before = allCloseAttempts
                self.assertFailure(.busy) { try child.abort(active) }
                self.assertFailure(.busy) { try child.finish(active, snapshot: captured) }
                self.assertFailure(.busy) { _ = try child.recover(active, snapshot: captured, recipe: self.storageRecipe(captured), maximumFragmentBytes: 18) }
                // Capacity enumeration may close separate receipt readers.
                // None of this live Use's actual source descriptors may close.
                XCTAssertTrue(sourceCloses.isEmpty)
                XCTAssertEqual(allCloseAttempts, before)
            }
            return Darwin.read(fd, pointer, min(count, 3))
        }
        let f = try fixture(io: io, close: { fd in
            if armed { allCloseAttempts += 1 }
            if armed, let key = self.descriptorIncarnation(fd), sources.contains(key) { sourceCloses[key, default: 0] += 1 }
            return Darwin.close(fd)
        })
        defer { try? f.catalogue.close() }
        for path in try FileManager.default.contentsOfDirectory(at: f.root, includingPropertiesForKeys: nil) {
            var node = stat(); XCTAssertEqual(lstat(path.path, &node), 0); sourceInodes.insert(node.st_ino)
        }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
        observingSnapshot = true
        let snapshot = try reader.snapshot(use)
        observingSnapshot = false
        XCTAssertEqual(sources.count, 3) // The retained receipt and both slots were actually read.
        child = reader; active = use; captured = snapshot; armed = true
        let result = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18)
        XCTAssertTrue(attempted); XCTAssertEqual(result.entry.state, .recovered); XCTAssertTrue(reader.cleanupComplete)
        XCTAssertFalse(f.catalogue.cleanupRetained); XCTAssertEqual(sources.count, 4)
        XCTAssertEqual(Set(sourceCloses.keys), sources); XCTAssertTrue(sourceCloses.values.allSatisfy { $0 == 1 })
    }
    func testRecoverySlotReplacementDuringCopyRefusesAndPreservesReplacement() throws {
        var armed = false, changed = false, target: URL?
        let io = metadataIO { fd, pointer, count in
            let n = Darwin.read(fd, pointer, min(count, 3))
            if armed, !changed, let target {
                changed = true
                do {
                    try FileManager.default.moveItem(at: target, to: target.appendingPathExtension("preserved"))
                    try self.privateWrite(Data([99]), target)
                } catch { XCTFail("Replacement fixture failed: \(error)") }
            }
            return n
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        target = slot(f, 1); armed = true
        assertFailure(.changed) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        XCTAssertTrue(changed); XCTAssertEqual(try Data(contentsOf: slot(f, 1)), Data([99]))
        XCTAssertTrue(reader.cleanupComplete); XCTAssertFalse(f.catalogue.cleanupRetained)
    }
    func testRecoveryUnknownTargetCloseRetainsAggregateWithoutNumericRetryOrPublication() throws {
        var armed = false, failed = false, closes = 0
        var originalInodes = Set<UInt64>()
        let f = try fixture(close: { fd in
            var node = stat()
            let target = armed && fstat(fd, &node) == 0 && !originalInodes.contains(node.st_ino)
            if armed && (failed || target) { closes += 1 }
            let result = Darwin.close(fd)
            if target && !failed { failed = true; errno = EIO; return -1 }
            return result
        })
        defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        for path in try FileManager.default.contentsOfDirectory(at: f.root, includingPropertiesForKeys: nil) {
            var node = stat(); XCTAssertEqual(lstat(path.path, &node), 0); originalInodes.insert(node.st_ino)
        }
        armed = true
        assertFailure(.io) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        XCTAssertTrue(failed); XCTAssertTrue(f.catalogue.cleanupRetained); XCTAssertFalse(reader.cleanupComplete); XCTAssertEqual(closes, 5)
        assertFailure(.closed) { try reader.abort(use) }; XCTAssertEqual(closes, 5)
        let names = try FileManager.default.contentsOfDirectory(atPath: f.root.path)
        XCTAssertEqual(names.filter { $0.hasSuffix(".receipt.json") }.count, 1)
    }
    func testRecoveryRefusesForeignUseBadBudgetAndUnboundSlotWithoutNewFiles() throws {
        let f = try fixture(), other = try fixture(); defer { try? f.catalogue.close(); try? other.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        let foreign = try other.catalogue.beginCheckpointRead(selected: other.selected), foreignUse = try foreign.start()
        assertFailure(.closed) { _ = try reader.recover(foreignUse, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        assertFailure(.capacity) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 17) }
        try foreign.abort(foreignUse); XCTAssertEqual(try f.catalogue.scan().count, 1)
        let again = try f.catalogue.beginCheckpointRead(selected: f.selected), active = try again.start(), snap = try again.snapshot(active)
        let bad = RecordingCatalogue.RecoveryRecipe(sequence: 3, checkpointSHA256: Data(repeating: 0, count: 32),
            header: header, fragmentBytes: 18, fragmentSHA256: Data(SHA256.hash(data: first + second)))
        assertFailure(.ineligible) { _ = try again.recover(active, snapshot: snap, recipe: bad, maximumFragmentBytes: 18) }
        XCTAssertEqual(try f.catalogue.scan().count, 1); XCTAssertFalse(f.catalogue.cleanupRetained)
    }
    func testRecoveryReceiptRenameFailureKeepsClosedCopyAndTempAndRetainsRootLease() throws {
        var armed = false, observingSnapshot = false, renameAttempted = false
        var allCloseAttempts = 0
        var sourceInodes = Set<ino_t>(), owners = Set<DescriptorIncarnation>()
        var ownerCloses: [DescriptorIncarnation: Int] = [:]
        var io = RecordingCatalogue.CheckpointIO(write: { fd, pointer, count in
            if armed, let key = self.descriptorIncarnation(fd) { owners.insert(key) } // Actual new receipt temporary.
            return Darwin.write(fd, pointer, count)
        }, synchronize: Darwin.fsync,
            rename: { Darwin.renameat($0, $1, $0, $2) }, synchronizeDirectory: Darwin.fsync,
            read: { fd, pointer, count in
                if let key = self.descriptorIncarnation(fd), observingSnapshot || armed && sourceInodes.contains(key.inode) {
                    owners.insert(key)
                }
                return Darwin.read(fd, pointer, count)
            })
        let append = RecordingCatalogue.AppendIO(write: { fd, pointer, count in
            if armed, let key = self.descriptorIncarnation(fd) { owners.insert(key) } // Actual new media target.
            return Darwin.write(fd, pointer, count)
        }, synchronize: Darwin.fsync, availableBytes: { _ in 1024 * 1024 * 1024 })
        io.renameExclusive = { _, _, _ in
            renameAttempted = true; XCTAssertTrue(armed)
            XCTAssertEqual(owners.count, 6) // Four source owners, target and receipt temporary.
            XCTAssertEqual(Set(ownerCloses.keys), owners); XCTAssertTrue(ownerCloses.values.allSatisfy { $0 == 1 })
            errno = EIO; return -1
        }
        let f = try fixture(io: io, close: { fd in
            if armed { allCloseAttempts += 1 }
            if armed, let key = self.descriptorIncarnation(fd), owners.contains(key) { ownerCloses[key, default: 0] += 1 }
            return Darwin.close(fd)
        }, appendIO: append)
        defer { try? f.catalogue.close() }
        for path in try FileManager.default.contentsOfDirectory(at: f.root, includingPropertiesForKeys: nil) {
            var node = stat(); XCTAssertEqual(lstat(path.path, &node), 0); sourceInodes.insert(node.st_ino)
        }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start()
        observingSnapshot = true
        let snapshot = try reader.snapshot(use)
        observingSnapshot = false
        XCTAssertEqual(owners.count, 3)
        armed = true
        assertFailure(.io) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        XCTAssertTrue(renameAttempted); XCTAssertTrue(f.catalogue.cleanupRetained)
        XCTAssertEqual(owners.count, 6); XCTAssertEqual(Set(ownerCloses.keys), owners)
        XCTAssertTrue(ownerCloses.values.allSatisfy { $0 == 1 })
        let names = try FileManager.default.contentsOfDirectory(atPath: f.root.path)
        XCTAssertTrue(names.contains { $0.hasSuffix(".receipt.tmp") })
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), header + first + second)
        let closedOwners = ownerCloses
        let attempts = allCloseAttempts
        assertFailure(.closed) { try reader.abort(use) }
        XCTAssertEqual(ownerCloses, closedOwners); XCTAssertEqual(allCloseAttempts, attempts)
    }

    func testRecoveryAtomicPublicationNeverOverwritesRacingReceipt() throws {
        let occupied = Data([91, 92, 93])
        var raced = false, occupiedPath: URL?
        var io = RecordingCatalogue.CheckpointIO.native
        io.renameExclusive = { fd, from, to in
            let newFD = openat(fd, to, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
            XCTAssertGreaterThanOrEqual(newFD, 0)
            XCTAssertEqual(occupied.withUnsafeBytes { Darwin.write(newFD, $0.baseAddress!, $0.count) }, occupied.count)
            XCTAssertEqual(Darwin.close(newFD), 0); raced = true
            return Darwin.renameatx_np(fd, from, fd, to, UInt32(RENAME_EXCL))
        }
        let f = try fixture(io: io); defer { try? f.catalogue.close() }
        let reader = try f.catalogue.beginCheckpointRead(selected: f.selected), use = try reader.start(), snapshot = try reader.snapshot(use)
        assertFailure(.io) { _ = try reader.recover(use, snapshot: snapshot, recipe: storageRecipe(snapshot), maximumFragmentBytes: 18) }
        for path in try FileManager.default.contentsOfDirectory(at: f.root, includingPropertiesForKeys: nil)
            where path.lastPathComponent.hasSuffix(".receipt.json") && !path.lastPathComponent.contains(f.identity.segmentID) { occupiedPath = path }
        XCTAssertTrue(raced); XCTAssertEqual(try Data(contentsOf: XCTUnwrap(occupiedPath)), occupied)
        XCTAssertTrue(f.catalogue.cleanupRetained); XCTAssertFalse(reader.cleanupComplete)
        XCTAssertEqual(try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename)), header + first + second)
    }

    func testRecoveryReceiptRejectsExactFuzzInvalidUtf8AndUnpairedEscapedKeys() throws {
        // Exact synthetic 984-byte libFuzzer crash, no media or real source IDs.
        // Embedded bytes keep the portable concatenated XCTest source independent
        // of resource paths. The same exact bytes are a committed fuzz seed.
        let encoded =
            "eyJjcmVhdGVkQXRNcyI6MSwiZmlsZSI6eyJjaGFuZ2VkTmFub3OuY29uZHMiOjAsImNoYW5nZWRTZWNvbmRzIjoxLCJkZXZpY2Ui" +
            "OjEsImlub2RlIjoyLCJtb2RpZmllZE5hbm9zZWNvbmRzIjowLCJtb2RpZmllZFNlY29uZHMiOjEsInNpemUiOjE3fSwiaWRlbnRp" +
            "dHkiOnsiY29udGFpbmVyIjoibTRhIiwicHJvZmlsZUlEIjoiYXVkaW8tNDhrLW1vbm8iLCJzZWdtZW50SUQiOiJjZDZhMTYxOS03" +
            "OTgxLTZlNmYtNGM0OS1mZTFmNDM1ZDIxZGQiLCJzZWdtZW50SW5kZXgiOjAsInNob3dJRCI6IjAwMDAwMDAwLTAwMDAtMDAwMC0w" +
            "MDAwLTAwMDAwMDAwMDAwMiJ9LCJyZWNvdmVyeSI6eyJjaGVja3BvaW50U0hBMjU2IjoiQXdNREF3TURBd01EQXdNREF3TURBd01E" +
            "QXdNREF3TURBd01EQXdNREF3TT0iLCJmcmFnbWVudEJ5dGVzIjo5LCJmcmFnbWVudFNIQTI1NiI6IkJRVUZCUVVGQlFVRkJRVUZC" +
            "UVVGQlFVRkJRVUZCUVVGQlFVRkJRVUZCUVU9IiwiaGVhZGVyQnl0ZXMiOjgsImhlYWRlclNIQTI1NiI6IkJBUUVCQVFFQkFRRUJB" +
            "UUVCQVFFQkFRRUJBUUVCQVFFQkFRRUJBUUVCQVE9Iiwib3V0cHV0U0hBMjU2IjoiQmdZR0JnWUdCZ1lHQmdZR0JnWUdCZ1lHQmdZ" +
            "R0JnWUdCZ1lHQmdZR0JnWT0iLCJyZWNvcmRpbmdCaW5kaW5nIjoiQVFFQkFRRUJBUUVCQVFFQkFRRUJBUUVCQVFFQkFRRUJBUUVC" +
            "QVFFQkFRRT0iLCJzZXF1ZW5jZSI6MSwic291cmNlIjp7ImNvbnRhaW5lciI6Im00YSIsInByb2ZpbGVJRCI6ImF1ZGlvLTQ4ay1t" +
            "b25vIiwic2VnbWVudElEIjoiMDAwMDAwMDAtMDAwMC0wMDAwLTAwMDAtMDAwMDAwMDAwMDAxIiwic2VnbWVudEluZGV4IjowLCJz" +
            "aG93SUQiOiIwMDAwMDAwMC0wMDAwLTAwMDAtMDAwMC0wMDAwMDAwMDAwMDIifSwic291cmNlRmlsZUJpbmRpbmciOiJBZ0lDQWdJ" +
            "Q0FnSUNBZ0lDQWdJQ0FnSUNBZ0lDQWdJQ0FnSUNBZ0lDQWdJPSIsInZlcnNpb24iOjF9LCJzdGF0ZSI6InJlY292ZXJlZCIsInZl" +
            "cnNpb24iOjJ9"
        let malformed = try XCTUnwrap(Data(base64Encoded: encoded))
        XCTAssertEqual(malformed.count, 984); XCTAssertEqual(malformed[38], 0xae)
        XCTAssertEqual(SHA256.hash(data: malformed).map { String(format: "%02x", $0) }.joined(),
            "f181ea0caf04ce17f0cd3f224a13d88c6b1970623c53c0cded53a5021b1aa779")
        XCTAssertNil(String(data: malformed, encoding: .utf8))
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(malformed))
        var canonical = malformed; canonical[38] = 0x65 // Original invented seed's only mutated byte.
        XCTAssertEqual(SHA256.hash(data: canonical).map { String(format: "%02x", $0) }.joined(),
            "7e6c2b8eab1dd2d5bf022325d8484208853217c8abaa4b1a20ae2bf5fa5ff805")
        XCTAssertTrue(RecordingCatalogue.checkRecoveryReceiptBytes(canonical))
        let text = try XCTUnwrap(String(data: canonical, encoding: .utf8))
        // Valid ASCII/UTF-8 is necessary, not sufficient JSON Unicode. These
        // must refuse through the actual parser, never by a fake validator.
        // Actual Foundation behaviour is measured only when root runs this test.
        for escaped in [#"\ud800"#, #"\udfff"#, #"\ud800\u0041"#] {
            let changed = text.replacingOccurrences(of: "\"changedNanoseconds\"", with: "\"" + escaped + "\"")
            XCTAssertNotEqual(changed, text)
            let bytes = Data(changed.utf8)
            XCTAssertTrue(bytes.allSatisfy { $0 < 128 }); XCTAssertNotNil(String(data: bytes, encoding: .utf8))
            XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(bytes))
        }
    }

    func testRecoveryReceiptRejectsExactFuzzInvalidUtf8NestedContainerKey() throws {
        // Exact synthetic 781-byte libFuzzer crash b98f41bc: a lone raw 0xa9 at
        // offset 550 in the nested recovery.source container key, whose start
        // Foundation reports at index 546. Embedded bytes keep the portable
        // concatenated XCTest source independent of resource paths. The same
        // exact bytes are a committed fuzz seed.
        let encoded =
            "eyJjcmVhdGVkQXRNcyI6MSwiZmlsZSI6eyJjaGFuZ2VkTmFub3NlY29uZHMiOjAsImNoYW5nZWRTZWNvbmRzIjoxLCJkZXZpY2Ui" +
            "OjEsImlub2RlIjoyLCJtb2RpZmllZE5hbm9zZWNvbmRzIjowLCJtb2RpZmllZFNlY29uZHMiOjEsInNpemUiOjE3fSwiaWRlbnRp" +
            "dHkiOnsiY29udGFpbmVyIjoibTRhIiwicHJvZmlsZUlEIjoiYXVkaW8tNDhrLW1vbm8iLCJzZWdtZW50SUQiOiJjZDZhMTYxOS03" +
            "OTgxLTZlNmYtNGM0OS1mZTFmNDM1ZDIxZGQiLCJzZWdtZW50SW5kZXgiOjAsInNob3dJRCI6IjAwMDAwMDAwLTAwMDAtMDAwMC0w" +
            "MDAwLTAwMDAwMDAwMDAwMiJ9LCJyZWNvdmVyeSI6eyJjaGVja3BvaW50U0hBMjU2IjoiQXdNREF3TURBd01EQXdNREF3LE1EQXdN" +
            "REF3TURBd01EQXdNREF3TURBd009IiwiZnJhZ21lbnRCeXRlcyI6OSwiZnJhZ21lbnRTSEEyNTYiOiJCUVVGQlFVRkJRWUdCZ1lH" +
            "QmdZPSIsInJlY29yZGluZ0JpbmRpbmciOiJBUUVCQVFFQkFRRUJBUUVCQVFFQkFRRUJBUUVCQVFFQkFRRUJBUUVCQVFFPSIsInNl" +
            "cXVlbmNlIjoxLCJzb3VyY2UiOnsiY29udKlpbmVyIjoibTRhIiwicHJvZmlsZUlEIjoiYXVkaW8tNDhrLW1vbm8iLCJzZWdtZW50" +
            "SUQiOiIwMDAwMDAwMC0wMDAwLTAwMDAtMDAwMC0wMDAwMDAwMDAwMDEiLCJzZWdtZW50SW5kZXgiOjAsInNob3dJRCI6IjAwMDAw" +
            "MDAwLTAwMDAtMDAwMC0wMDAwLTAwMDAwMDAwMDAwMiJ9LCJzb3VyY2VGaWxlQmluZGluZyI6IkFRPT0iLCJ2ZXJzaW9uIjoxfSwi" +
            "c3RhdGUiOiJyZWNvdmVyZWQiLCJ2ZXJzaW9uIjozfQ=="
        let malformed = try XCTUnwrap(Data(base64Encoded: encoded))
        XCTAssertEqual(malformed.count, 781); XCTAssertEqual(malformed[550], 0xa9)
        XCTAssertEqual(SHA256.hash(data: malformed).map { String(format: "%02x", $0) }.joined(),
            "b98f41bccffb1076e5bb7dd49fee76b8e6ed17ae100fa30a16e5fbbe1da05926")
        XCTAssertTrue(malformed.enumerated().allSatisfy { $0.element < 128 || $0.offset == 550 })
        // This exact input passed Foundation's JSON syntax validation, then its
        // keyed decoder trapped materialising the malformed nested key. The
        // strict oracle is therefore the standard library transcoder, never
        // String(data:encoding:), which accepted this failure's encoding.
        XCTAssertTrue(transcode(malformed.makeIterator(), from: UTF8.self, to: UTF8.self,
                                stoppingOnError: true, into: { _ in }))
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(malformed))
        // The remaining corruptions stay rejected on the post-transcode layers:
        // strict UTF-8 acceptance alone must never admit this invented receipt.
        var reverted = malformed; reverted[550] = 0x61
        XCTAssertFalse(transcode(reverted.makeIterator(), from: UTF8.self, to: UTF8.self,
                                 stoppingOnError: true, into: { _ in }))
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(reverted))
    }

    func testRecoveryReceiptRejectsOversizedDirectInputBeforeTraversal() throws {
        // checkRecoveryReceiptBytes accepts Data directly, so the 4096-byte
        // bound must refuse oversized attacker input BEFORE any UTF-8
        // traversal; only the disk reader's own cap may bound file reads. All
        // spaces are valid UTF-8, so the bound alone drives this rejection.
        let oversized = Data(repeating: 0x20, count: 4097)
        XCTAssertFalse(transcode(oversized.makeIterator(), from: UTF8.self, to: UTF8.self,
                                 stoppingOnError: true, into: { _ in }))
        XCTAssertFalse(RecordingCatalogue.checkRecoveryReceiptBytes(oversized))
    }

}
