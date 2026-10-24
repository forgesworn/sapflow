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
                         close: @escaping (Int32) -> Int32 = Darwin.close) throws -> Fixture {
        let root = scratch.appendingPathComponent(UUID().uuidString)
        let append = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { _ in 1024 * 1024 * 1024 })
        let catalogue = try RecordingCatalogue(root: root, appendIO: append, checkpointIO: io, closeFileDescriptor: close)
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

}
