import XCTest
import Foundation
import CryptoKit
import Darwin
import Sapflow
@testable import SapflowBroadcaster

/// Actual SDK + generated metadata/files, not decodable AAC or native EOF.
/// Package-backed compilation/execution is gated separately and remains UNRUN.
final class RecordingCheckpointProbeTests: XCTestCase {
    private var scratch: URL!
    override func setUpWithError() throws {
        scratch = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-checkpoint-probe-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: false)
    }
    override func tearDownWithError() throws { try FileManager.default.removeItem(at: scratch) }
    private struct Fixture {
        let root: URL; let catalogue: RecordingCatalogue; let identity: RecordingCatalogue.Identity
        let firstSlot: Data
    }
    private func fixture(sequence: UInt32, checkpointIO: RecordingCatalogue.CheckpointIO = .native,
                         closeFileDescriptor: @escaping (Int32) -> Int32 = Darwin.close) throws -> Fixture {
        let root = scratch.appendingPathComponent(UUID().uuidString)
        let io = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { _ in 1024 * 1024 * 1024 })
        let catalogue = try RecordingCatalogue(root: root, appendIO: io, checkpointIO: checkpointIO,
            closeFileDescriptor: closeFileDescriptor)
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let owner = try RecordingCheckpointOwner.open(catalogue: catalogue, identity: identity,
            mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        var closed = false
        defer { if !closed { _ = try? owner.closeIncomplete() } }
        try owner.beginAudio(firstLeadingFrames: 0) // Explicit synthetic SDK unit fixture, not measured leading.
        let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        var first = Data()
        for index in 0..<64 {
            let sample = try codec.sample(bytes: Data([UInt8(index % 251), 4, 9]), pts: Int64(index * 1024),
                dts: Int64(index * 1024), duration: 1024, timescale: 48_000, keyframe: false)
            try owner.push(sample: sample)
            if let frontier = owner.frontier {
                if frontier.sequence == 1 && first.isEmpty {
                    first = try Data(contentsOf: root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.1"))
                }
                if frontier.sequence == sequence { break }
            }
        }
        XCTAssertEqual(owner.frontier?.sequence, sequence); XCTAssertFalse(first.isEmpty)
        _ = try owner.closeIncomplete(); closed = true
        XCTAssertFalse(owner.normalStopAvailable); XCTAssertNil(owner.fileURL)
        return Fixture(root: root, catalogue: catalogue, identity: identity, firstSlot: first)
    }
    private func selected(_ f: Fixture) throws -> RecordingCatalogue.Entry { try XCTUnwrap(f.catalogue.scan().first) }
    private func slot(_ f: Fixture, _ index: Int) -> URL {
        f.root.appendingPathComponent("Sapflow-\(f.identity.segmentID).checkpoint.\(index)")
    }
    private func bindings(_ f: Fixture) throws -> RecordingCatalogue.AppendBindings {
        var root = stat(), media = stat()
        XCTAssertEqual(lstat(f.root.path, &root), 0); XCTAssertEqual(lstat(f.root.appendingPathComponent(f.identity.filename).path, &media), 0)
        let source = RecordingCatalogue.AppendSource(rootDevice: root.st_dev, rootInode: root.st_ino,
            device: media.st_dev, inode: media.st_ino, birthSeconds: Int64(media.st_birthtimespec.tv_sec),
            birthNanoseconds: Int64(media.st_birthtimespec.tv_nsec))
        return try source.bindings(identity: f.identity)
    }
    private func decoded(_ f: Fixture, _ index: Int) throws -> DecodedRecordingCheckpoint {
        let expected = try bindings(f)
        return try decodeRecordingCheckpoint(bytes: Data(contentsOf: slot(f, index)),
            expectedRecordingBinding: expected.recording, expectedSourceFileBinding: expected.sourceFile)
    }
    private func inspect(_ f: Fixture) throws -> RecordingCheckpointProbe.Observation {
        try RecordingCheckpointProbe.inspect(catalogue: f.catalogue, selected: selected(f), maximumFragmentBytes: 1024 * 1024)
    }
    private func replace(_ bytes: Data, at path: URL) throws {
        let fd = Darwin.open(path.path, O_WRONLY | O_TRUNC | O_NOFOLLOW)
        XCTAssertGreaterThanOrEqual(fd, 0)
        var position = 0
        while position < bytes.count {
            let count = bytes.withUnsafeBytes { Darwin.write(fd, $0.baseAddress!.advanced(by: position), bytes.count - position) }
            guard count > 0 else { _ = Darwin.close(fd); throw RecordingCatalogue.Failure.io }; position += count
        }
        XCTAssertEqual(Darwin.close(fd), 0)
    }

    func testRealSdkPairSelectsNewestPhysicalPrefixAndExactSavedRecoveryHeader() throws {
        let f = try fixture(sequence: 3); defer { try? f.catalogue.close() }
        let old = try decoded(f, 0), latest = try decoded(f, 1)
        try validateRecordingCheckpointSuccessor(prior: old, next: latest)
        let result = try inspect(f), info = latest.info()
        XCTAssertEqual(result.identity, f.identity); XCTAssertEqual(result.sequence, 3)
        XCTAssertEqual(result.committedOffset, info.committedOffset); XCTAssertEqual(result.fragmentRegionBytes, info.fragmentRegionBytes)
        XCTAssertEqual(result.savedRecoveryHeader, latest.header())
        XCTAssertEqual(result.selectedSlotSHA256, Data(SHA256.hash(data: try Data(contentsOf: slot(f, 1)))))
        let bytes = try Data(contentsOf: f.root.appendingPathComponent(f.identity.filename))
        XCTAssertEqual(Data(SHA256.hash(data: bytes.subdata(in: Int(info.initialHeaderLen)..<Int(info.committedOffset)))), info.fragmentRegionSha256)
        XCTAssertFalse(f.catalogue.cleanupRetained); XCTAssertEqual(try selected(f).state, .incomplete)
    }

    func testOnlyLoneSequenceOneIsAllowedAndBothAbsentRefuse() throws {
        let first = try fixture(sequence: 1); defer { try? first.catalogue.close() }
        XCTAssertFalse(FileManager.default.fileExists(atPath: slot(first, 0).path))
        XCTAssertEqual(try inspect(first).sequence, 1)
        try FileManager.default.removeItem(at: slot(first, 1))
        XCTAssertThrowsError(try inspect(first)) {
            guard case RecordingCheckpointProbe.Failure.noCheckpoint = $0 else { return XCTFail("Wrong missing-slot refusal: \($0)") }
        }
        XCTAssertFalse(first.catalogue.cleanupRetained)
        let later = try fixture(sequence: 2); defer { try? later.catalogue.close() }
        try FileManager.default.removeItem(at: slot(later, 1))
        XCTAssertThrowsError(try inspect(later)) {
            guard case RecordingCheckpointProbe.Failure.parityOrSequence = $0 else { return XCTFail("Accepted lone noninitial slot") }
        }
        XCTAssertFalse(later.catalogue.cleanupRetained)
    }

    func testAnyPresentChecksumOrEnvelopeRefusalBlocksFallbackAndPreservesActualError() throws {
        for envelope in [false, true] {
            let f = try fixture(sequence: 3); defer { try? f.catalogue.close() }
            let original = try Data(contentsOf: slot(f, 1))
            var changed = original
            if envelope { changed = Data(original.prefix(2)) }
            else { changed[changed.count - 1] ^= 1 }
            try replace(changed, at: slot(f, 1))
            XCTAssertThrowsError(try inspect(f)) {
                guard case let RecordingCheckpointProbe.Failure.decoderRefusal(index, actual) = $0 else { return XCTFail("Lost actual decoder refusal: \($0)") }
                XCTAssertEqual(index, 1); XCTAssertEqual(actual, envelope ? .Envelope : .Checksum)
            }
            XCTAssertEqual(try Data(contentsOf: slot(f, 1)), changed)
            XCTAssertFalse(f.catalogue.cleanupRetained)
        }
    }

    func testWrongParityAndNonadjacentActualSdkSnapshotsCannotBecomeAnOlderFallback() throws {
        let parity = try fixture(sequence: 1); defer { try? parity.catalogue.close() }
        try parity.firstSlot.write(to: slot(parity, 0), options: .withoutOverwriting); XCTAssertEqual(chmod(slot(parity, 0).path, 0o600), 0)
        XCTAssertThrowsError(try inspect(parity)) {
            guard case RecordingCheckpointProbe.Failure.parityOrSequence = $0 else { return XCTFail("Accepted wrong slot parity") }
        }
        let gap = try fixture(sequence: 4); defer { try? gap.catalogue.close() }
        try replace(gap.firstSlot, at: slot(gap, 1))
        XCTAssertThrowsError(try inspect(gap)) { XCTAssertEqual($0 as? RecordingCheckpointError, .Successor) }
        XCTAssertEqual(try Data(contentsOf: slot(gap, 1)), gap.firstSlot)
        XCTAssertFalse(parity.catalogue.cleanupRetained); XCTAssertFalse(gap.catalogue.cleanupRetained)
    }

    func testChecksumValidForeignActualSdkSlotRefusesAgainstActualFileBindings() throws {
        let local = try fixture(sequence: 3), foreign = try fixture(sequence: 3)
        defer { try? local.catalogue.close(); try? foreign.catalogue.close() }
        let original = try Data(contentsOf: slot(foreign, 1)); try replace(original, at: slot(local, 1))
        XCTAssertThrowsError(try inspect(local)) {
            guard case let RecordingCheckpointProbe.Failure.decoderRefusal(index, actual) = $0 else { return XCTFail("Foreign slot lost exact SDK error") }
            XCTAssertEqual(index, 1); XCTAssertEqual(actual, .Binding)
        }
        XCTAssertEqual(try Data(contentsOf: slot(local, 1)), original); XCTAssertFalse(local.catalogue.cleanupRetained)
    }

    func testActualFragmentHashIsMandatoryWhileCurrentHeaderAndUncommittedTailStayAdvisory() throws {
        for change in ["fragment", "header", "tail"] {
            let f = try fixture(sequence: 3); defer { try? f.catalogue.close() }
            let latest = try decoded(f, 1), info = latest.info(), path = f.root.appendingPathComponent(f.identity.filename)
            let original = try Data(contentsOf: path)
            let fd = Darwin.open(path.path, O_WRONLY | O_NOFOLLOW); XCTAssertGreaterThanOrEqual(fd, 0)
            var byte: UInt8 = 121
            let offset = change == "fragment" ? off_t(info.initialHeaderLen) : change == "tail" ? off_t(original.count) : 0
            XCTAssertEqual(Darwin.pwrite(fd, &byte, 1, offset), 1); XCTAssertEqual(Darwin.close(fd), 0)
            if change == "fragment" {
                XCTAssertThrowsError(try inspect(f)) {
                    guard case RecordingCheckpointProbe.Failure.physicalPrefix = $0 else { return XCTFail("Actual fragment hash was not required") }
                }
            } else {
                let observation = try inspect(f)
                XCTAssertEqual(observation.committedOffset, info.committedOffset)
                XCTAssertEqual(observation.savedRecoveryHeader, latest.header())
                XCTAssertEqual(observation.savedRecoveryHeader.count, Int(info.initialHeaderLen))
                // No equality of recovery header hash and original INITIAL hash.
            }
            let after = try Data(contentsOf: path)
            XCTAssertEqual(after[Int(offset)], byte); XCTAssertEqual(after.count, original.count + (change == "tail" ? 1 : 0))
            XCTAssertFalse(f.catalogue.cleanupRetained)
        }
    }

    func testUnknownFinalClosePreservesActualIoErrorRetainsLeaseAndNeverRetriesFd() throws {
        var armed = false, failedFD: Int32?, attempts: [Int32: Int] = [:]
        let f = try fixture(sequence: 3, closeFileDescriptor: { fd in
            if armed { attempts[fd, default: 0] += 1 }
            let result = Darwin.close(fd)
            if armed, failedFD == nil { failedFD = fd; errno = EIO; return -1 }
            return result
        })
        // scan closes receipt descriptors; capture the selection BEFORE arming.
        let selection = try selected(f); armed = true
        XCTAssertThrowsError(try RecordingCheckpointProbe.inspect(catalogue: f.catalogue, selected: selection,
            maximumFragmentBytes: 1024 * 1024)) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .io) }
        XCTAssertNotNil(failedFD); XCTAssertEqual(attempts.count, 4)
        XCTAssertTrue(attempts.values.allSatisfy { $0 == 1 })
        XCTAssertTrue(f.catalogue.cleanupUncertain); XCTAssertTrue(f.catalogue.cleanupRetained)
        XCTAssertThrowsError(try RecordingCheckpointProbe.inspect(catalogue: f.catalogue, selected: selection,
            maximumFragmentBytes: 1024 * 1024)) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .closed) }
        XCTAssertThrowsError(try f.catalogue.close()) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .busy) }
        XCTAssertEqual(attempts.count, 4); XCTAssertTrue(attempts.values.allSatisfy { $0 == 1 })
        // Exact uncertain owner/root intentionally stay retained until process exit.
    }

    func testActualSdkDecodeThenMetadataReadbackMutationRefusesAndPreservesChangedSlot() throws {
        var armed = false, sawMediaHash = false, mutated = false
        var mediaInode: UInt64 = 0, slotInode: UInt64 = 0, targetPath: String?
        let io = RecordingCatalogue.CheckpointIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            rename: { Darwin.renameat($0, $1, $0, $2) }, synchronizeDirectory: Darwin.fsync,
            read: { fd, pointer, count in
                var actual = stat()
                if armed, fstat(fd, &actual) == 0 {
                    if actual.st_ino == mediaInode { sawMediaHash = true }
                    // First snapshot was read before media hashing. This later
                    // slot read is the checked finish's metadata readback,
                    // after real SDK decode/successor and both physical hashes.
                    if sawMediaHash, !mutated, actual.st_ino == slotInode, let targetPath {
                        let writer = Darwin.open(targetPath, O_RDWR | O_NOFOLLOW)
                        XCTAssertGreaterThanOrEqual(writer, 0)
                        var byte: UInt8 = 0
                        XCTAssertEqual(Darwin.pread(writer, &byte, 1, 0), 1); byte ^= 1
                        XCTAssertEqual(Darwin.pwrite(writer, &byte, 1, 0), 1); XCTAssertEqual(Darwin.close(writer), 0)
                        mutated = true
                    }
                }
                return Darwin.read(fd, pointer, count)
            })
        let f = try fixture(sequence: 3, checkpointIO: io); defer { try? f.catalogue.close() }
        let selection = try selected(f), original = try Data(contentsOf: slot(f, 1))
        var media = stat(), metadata = stat()
        XCTAssertEqual(lstat(f.root.appendingPathComponent(f.identity.filename).path, &media), 0)
        XCTAssertEqual(lstat(slot(f, 1).path, &metadata), 0)
        mediaInode = media.st_ino; slotInode = metadata.st_ino; targetPath = slot(f, 1).path; armed = true
        XCTAssertThrowsError(try RecordingCheckpointProbe.inspect(catalogue: f.catalogue, selected: selection,
            maximumFragmentBytes: 1024 * 1024)) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .changed) }
        XCTAssertTrue(sawMediaHash); XCTAssertTrue(mutated)
        XCTAssertNotEqual(try Data(contentsOf: slot(f, 1)), original)
        XCTAssertFalse(f.catalogue.cleanupRetained); XCTAssertFalse(f.catalogue.cleanupUncertain)
    }

}
