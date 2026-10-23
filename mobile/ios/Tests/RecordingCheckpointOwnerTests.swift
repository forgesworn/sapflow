import XCTest
import Foundation
import Darwin
import CryptoKit
import Sapflow
@testable import SapflowBroadcaster

/// Real SDK + actual generated-byte files. Sample bytes/receipts below are
/// synthetic API/container controls, not actual AAC/native/decoder evidence.
final class RecordingCheckpointOwnerTests: XCTestCase {
    private var scratch: URL!
    override func setUpWithError() throws {
        scratch = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-checkpoint-owner-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: false)
    }
    override func tearDownWithError() throws { try FileManager.default.removeItem(at: scratch) }
    private func appendIO() -> RecordingCatalogue.AppendIO {
        RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync, availableBytes: { _ in 1024 * 1024 * 1024 })
    }
    private func open(_ catalogue: RecordingCatalogue) throws -> (RecordingCatalogue.Identity, RecordingCheckpointOwner) {
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let owner = try RecordingCheckpointOwner.open(catalogue: catalogue, identity: identity, mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        try owner.beginAudio(firstLeadingFrames: 0) // Explicit synthetic unit fixture.
        return (identity, owner)
    }
    private func sample(_ codec: MediaCodec, _ index: Int) throws -> MediaSample {
        let bytes = Data((0..<(index % 7 + 1)).map { UInt8((index + $0) % 251) })
        return try codec.sample(bytes: bytes, pts: Int64(index * 1024), dts: Int64(index * 1024), duration: 1024, timescale: 48_000, keyframe: false)
    }
    private func feed(_ owner: RecordingCheckpointOwner, _ codec: MediaCodec, _ range: Range<Int>) throws {
        for index in range { try owner.push(sample: sample(codec, index)) }
    }
    private func bindings(_ root: URL, _ identity: RecordingCatalogue.Identity) throws -> RecordingCatalogue.AppendBindings {
        var directory = stat(), media = stat()
        XCTAssertEqual(lstat(root.path, &directory), 0)
        XCTAssertEqual(lstat(root.appendingPathComponent(identity.filename).path, &media), 0)
        let source = RecordingCatalogue.AppendSource(rootDevice: directory.st_dev, rootInode: directory.st_ino,
            device: media.st_dev, inode: media.st_ino, birthSeconds: Int64(media.st_birthtimespec.tv_sec), birthNanoseconds: Int64(media.st_birthtimespec.tv_nsec))
        return try source.bindings(identity: identity)
    }
    private func slots(_ root: URL, _ identity: RecordingCatalogue.Identity) throws -> [DecodedRecordingCheckpoint] {
        let expected = try bindings(root, identity)
        var values: [DecodedRecordingCheckpoint] = []
        for slot in 0..<2 {
            let path = root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.\(slot)")
            if FileManager.default.fileExists(atPath: path.path) {
                let bytes = try Data(contentsOf: path)
                XCTAssertTrue((545...66_080).contains(bytes.count))
                let value = try decodeRecordingCheckpoint(bytes: bytes, expectedRecordingBinding: expected.recording, expectedSourceFileBinding: expected.sourceFile)
                XCTAssertEqual(Int(value.info().facts.sequence & 1), slot); values.append(value)
            }
        }
        return values.sorted { $0.info().facts.sequence < $1.info().facts.sequence }
    }
    private func receiptState(_ root: URL, _ identity: RecordingCatalogue.Identity) throws -> String? {
        let data = try Data(contentsOf: root.appendingPathComponent("Sapflow-\(identity.segmentID).receipt.json"))
        return (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["state"] as? String
    }
    func testRealSDKSlotsMatchPhysicalFragmentPrefixHashAndSharedSuccessor() throws {
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO())
        defer { try? catalogue.close() }
        let (identity, owner) = try open(catalogue), codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        try feed(owner, codec, 0..<20)
        let progress = try XCTUnwrap(owner.frontier), stored = try slots(root, identity)
        XCTAssertEqual(stored.count, 2)
        try validateRecordingCheckpointSuccessor(prior: stored[0], next: stored[1])
        let latest = try XCTUnwrap(stored.last), info = latest.info()
        let actual = try Data(contentsOf: root.appendingPathComponent(identity.filename))
        XCTAssertEqual(progress.fileEnd, UInt64(actual.count)); XCTAssertEqual(info.committedOffset, progress.fileEnd)
        XCTAssertEqual(info.fragmentRegionBytes, progress.fragmentBytes)
        XCTAssertEqual(info.facts.audioUnits, progress.audioUnits); XCTAssertEqual(info.facts.videoUnits, 0)
        XCTAssertEqual(Data(SHA256.hash(data: actual.prefix(Int(info.initialHeaderLen)))), info.initialHeaderSha256)
        let region = actual.subdata(in: Int(info.initialHeaderLen)..<Int(info.committedOffset))
        XCTAssertEqual(Data(SHA256.hash(data: region)), info.fragmentRegionSha256)
        XCTAssertEqual(latest.fragmentRegionSha256(), info.fragmentRegionSha256)
        XCTAssertFalse(progress.completeAudioEOF); XCTAssertFalse(owner.normalStopAvailable)
        _ = try owner.closeIncomplete(); XCTAssertTrue(owner.cleanupComplete)
        XCTAssertEqual(try receiptState(root, identity), "incomplete")
    }
    func testIncompleteFinishDefersFinalHeaderAndNeverRewritesOriginalInitial() throws {
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO())
        defer { try? catalogue.close() }
        let (identity, owner) = try open(catalogue), codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        try feed(owner, codec, 0..<12); try owner.finishPrefix(outcome: .incompletePrefix)
        let progress = try XCTUnwrap(owner.frontier), latest = try XCTUnwrap(slots(root, identity).last), info = latest.info()
        let actual = try Data(contentsOf: root.appendingPathComponent(identity.filename))
        XCTAssertEqual(actual.count, Int(progress.fileEnd)); XCTAssertFalse(progress.completeAudioEOF)
        XCTAssertEqual(Data(SHA256.hash(data: actual.prefix(Int(info.initialHeaderLen)))), info.initialHeaderSha256)
        let closed = try owner.closeIncomplete()
        XCTAssertTrue(closed.deferredFinalHeader); XCTAssertFalse(closed.completeAudioEOF)
        XCTAssertFalse(owner.normalStopAvailable); XCTAssertEqual(try receiptState(root, identity), "incomplete")
    }
    func testCompleteAudioCheckpointStillClosesOnlyAsIncompletePrefix() throws {
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO())
        defer { try? catalogue.close() }
        let (identity, owner) = try open(catalogue), codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        let units = 20, frames = UInt64(units * 1024)
        try feed(owner, codec, 0..<units)
        // Deliberately synthetic, arithmetically consistent SDK fixtures. These
        // values never stand in for actual platform raw/zero/native EOF facts.
        try owner.generationEnded(NativeAudioGenerationReceipt(generation: 1, inputFrames: frames, rawFrames: frames,
            zeroFrames: 0, inputRate: 48_000, encodedFrames: frames, leadingFrames: 0, trailingFrames: 0, sourceMapping: .nearest))
        try owner.completeAudio(NativeAudioFileReceipt(generationCount: 1, sourceFramesAt48k: frames,
            intentionalStandbyFrames: 0, encodedFrames: frames, firstLeadingFrames: 0, lastTrailingFrames: 0))
        try owner.finishPrefix(outcome: .complete)
        XCTAssertTrue(try XCTUnwrap(owner.frontier).completeAudioEOF)
        XCTAssertTrue(try XCTUnwrap(slots(root, identity).last).info().facts.completeAudioEof)
        let closed = try owner.closeIncomplete()
        XCTAssertTrue(closed.completeAudioEOF); XCTAssertTrue(closed.deferredFinalHeader)
        XCTAssertFalse(owner.normalStopAvailable); XCTAssertEqual(try receiptState(root, identity), "incomplete")
        XCTAssertEqual(try catalogue.scan().first?.state, .incomplete)
    }
    private final class HeldReadback {
        let lock = NSLock()
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0), done = DispatchSemaphore(value: 0)
        var armed = false, closes = 0
        var failure: Error?
        func arm() { lock.lock(); armed = true; closes = 0; lock.unlock() }
        func disarm() { lock.lock(); armed = false; lock.unlock() }
        func close(_ fd: Int32) -> Int32 {
            lock.lock(); if armed { closes += 1 }; let held = armed && closes == 2; lock.unlock()
            if held { entered.signal(); guard release.wait(timeout: .now() + 5) == .success else { errno = EIO; return -1 } }
            return Darwin.close(fd)
        }
        func failed(_ error: Error) { lock.lock(); failure = error; lock.unlock() }
        var observedFailure: Error? { lock.lock(); defer { lock.unlock() }; return failure }
    }
    private func heldReadback(cancel: Bool) throws {
        let gate = HeldReadback(), root = scratch.appendingPathComponent("recordings")
        let catalogue = try RecordingCatalogue(root: root, appendIO: appendIO(), closeFileDescriptor: gate.close)
        defer { try? catalogue.close() }
        let (_, owner) = try open(catalogue), codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        try feed(owner, codec, 0..<9)
        let prior = try XCTUnwrap(owner.frontier), probe = try sample(codec, 99)
        gate.arm()
        DispatchQueue.global().async {
            do {
                for index in 9..<24 {
                    try owner.push(sample: self.sample(codec, index))
                    if owner.frontier?.sequence != prior.sequence { break }
                }
            } catch { gate.failed(error) }
            gate.done.signal()
        }
        var joined = false
        defer { gate.disarm(); gate.release.signal(); if !joined { _ = gate.done.wait(timeout: .now() + 5) } }
        XCTAssertEqual(gate.entered.wait(timeout: .now() + 5), .success)
        XCTAssertEqual(owner.frontier, prior) // Real shared decode already ran; close has not returned.
        for action in [{ try owner.push(sample: probe) }, { try owner.finishPrefix(outcome: .incompletePrefix) }, { _ = try owner.closeIncomplete() }] {
            XCTAssertThrowsError(try action()) { XCTAssertEqual($0 as? RecordingCheckpointOwner.Failure, .busy) }
        }
        XCTAssertThrowsError(try catalogue.close()) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .busy) }
        if cancel { owner.cancel() }
        gate.disarm(); gate.release.signal()
        XCTAssertEqual(gate.done.wait(timeout: .now() + 5), .success); joined = true
        if cancel { XCTAssertNotNil(gate.observedFailure); XCTAssertEqual(owner.frontier, prior) }
        else { XCTAssertNil(gate.observedFailure); XCTAssertGreaterThan(try XCTUnwrap(owner.frontier).sequence, prior.sequence) }
        _ = try owner.closeIncomplete(); XCTAssertTrue(owner.cleanupComplete)
    }
    func testActualReadbackClosePrecedesScalarPublicationAndConcurrentOperationsFailBusy() throws { try heldReadback(cancel: false) }
    func testCancelDuringActualReadbackCloseCannotPublishRetiredProgress() throws { try heldReadback(cancel: true) }
    func testStorageFailurePreservesPreviousScalarAndRequiresExplicitIncompleteClose() throws {
        var fail = false
        let checkpointIO = RecordingCatalogue.CheckpointIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            rename: { fd, source, target in if fail { errno = EIO; return -1 }; return Darwin.renameat(fd, source, fd, target) },
            synchronizeDirectory: Darwin.fsync, read: { Darwin.read($0, $1, $2) })
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO(), checkpointIO: checkpointIO)
        defer { try? catalogue.close() }
        let (identity, owner) = try open(catalogue), codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        try feed(owner, codec, 0..<9); let prior = try XCTUnwrap(owner.frontier); fail = true
        XCTAssertThrowsError(try feed(owner, codec, 9..<24))
        XCTAssertEqual(owner.frontier, prior); XCTAssertFalse(owner.cleanupComplete)
        let stable = root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.\(prior.sequence & 1)")
        let expected = try bindings(root, identity)
        let decoded = try decodeRecordingCheckpoint(bytes: Data(contentsOf: stable), expectedRecordingBinding: expected.recording, expectedSourceFileBinding: expected.sourceFile)
        XCTAssertEqual(decoded.info().committedOffset, prior.fileEnd)
        XCTAssertThrowsError(try owner.push(sample: sample(codec, 99)))
        _ = try owner.closeIncomplete(); XCTAssertEqual(try receiptState(root, identity), "incomplete")
    }
    func testRealSDKRejectsForeignLiveTokenWithoutAnyProductionInjectionAccessor() throws {
        let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        let a = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true, mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        let b = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: false, hasAudio: true, mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        try a.beginNativeAudio(firstLeadingFrames: 0)
        var token: NativeRecordingCheckpoint?
        for index in 0..<9 {
            try a.push(sample: sample(codec, index))
            while let chunk = try a.takeNativeOutput() { if token == nil { token = chunk.checkpoint } }
        }
        let actual = try XCTUnwrap(token)
        XCTAssertThrowsError(try b.encodeCheckpoint(token: actual, recordingBinding: Data(repeating: 1, count: 32),
            sourceFileBinding: Data(repeating: 2, count: 32), fragmentRegionSha256: Data(repeating: 3, count: 32))) {
                XCTAssertEqual($0 as? RecordingCheckpointError, .ForeignCheckpoint)
            }
        try a.cancel(); try b.cancel()
    }
    func testDroppedCallerRetainsCompoundNativeAndFileOwnersUntilCheckedClose() throws {
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO())
        defer { try? catalogue.close() }
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        var caller: RecordingCheckpointOwner? = try RecordingCheckpointOwner.open(catalogue: catalogue, identity: identity,
            mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        weak var retained = caller; caller = nil
        XCTAssertNotNil(retained); XCTAssertThrowsError(try catalogue.close())
        _ = try retained?.closeIncomplete()
        XCTAssertNil(retained); XCTAssertFalse(catalogue.cleanupRetained)
    }
    func testClosedCompoundCannotPublishIntoOrReleaseSuccessor() throws {
        let root = scratch.appendingPathComponent("recordings"), catalogue = try RecordingCatalogue(root: root, appendIO: appendIO())
        defer { try? catalogue.close() }
        let codec = try MediaCodec.aac(asc: Data([0x11, 0x88])), (_, first) = try open(catalogue)
        try feed(first, codec, 0..<9); let old = try XCTUnwrap(first.frontier); _ = try first.closeIncomplete()
        let (_, next) = try open(catalogue); try feed(next, codec, 0..<9)
        XCTAssertNotEqual(old.owner, try XCTUnwrap(next.frontier).owner)
        _ = try first.closeIncomplete(); XCTAssertEqual(first.frontier, old)
        XCTAssertThrowsError(try first.push(sample: sample(codec, 9)))
        XCTAssertThrowsError(try catalogue.close()) { XCTAssertEqual($0 as? RecordingCatalogue.Failure, .busy) }
        XCTAssertFalse(next.cleanupComplete); _ = try next.closeIncomplete()
    }
}
