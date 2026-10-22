import XCTest
import Darwin
@testable import SapflowBroadcaster

final class RecordingExportDestinationTests: XCTestCase {
    private func folder() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-export-generated-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        addTeardownBlock { try FileManager.default.removeItem(at: root) }
        return root
    }
    func testExclusiveCopyPersistsGeneratedBytesAndNeedsActualClose() throws {
        let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Recording.mov")
        let bytes = Data((0..<64000).map { UInt8($0 % 127) })
        try file.write(bytes); try file.finish()
        XCTAssertFalse(file.cleanupComplete)
        try file.close(); XCTAssertTrue(file.cleanupComplete); try file.close()
        XCTAssertEqual(try Data(contentsOf: root.appendingPathComponent("Recording.mov")), bytes)
    }
    func testExistingFilesLinksAndUnsafeNamesAreNeverOverwritten() throws {
        let root = try folder(), destination = root.appendingPathComponent("Recording.mov")
        let original = Data([9, 8, 7]); try original.write(to: destination)
        XCTAssertThrowsError(try RecordingExportDestination(folder: root, filename: "Recording.mov"))
        for name in ["../outside.mov", "/outside.mov", "back\\slash.mov", "recording.mp4", String(repeating: "a", count: 161) + ".mov"] {
            XCTAssertThrowsError(try RecordingExportDestination(folder: root, filename: name))
        }
        let link = root.appendingPathComponent("Link.mov")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: destination)
        XCTAssertThrowsError(try RecordingExportDestination(folder: root, filename: "Link.mov"))
        XCTAssertEqual(try Data(contentsOf: destination), original)
    }
    func testPartialOrCancelledCopyRemainsWithoutRemovingOriginalOrCopy() throws {
        let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Partial.mov")
        try file.write(Data([1, 2, 3])); try file.close()
        XCTAssertEqual(try Data(contentsOf: root.appendingPathComponent("Partial.mov")), Data([1, 2, 3]))
        XCTAssertThrowsError(try file.write(Data([4])))
    }
    func testDestinationPathReplacementIsDetectedWithoutDeletingEitherFile() throws {
        let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Recording.mov")
        try file.write(Data([1, 2, 3]))
        let named = root.appendingPathComponent("Recording.mov"), retained = root.appendingPathComponent("Retained.mov")
        try FileManager.default.moveItem(at: named, to: retained)
        try Data([9]).write(to: named)
        XCTAssertThrowsError(try file.finish())
        try file.close(); XCTAssertTrue(file.cleanupComplete)
        XCTAssertEqual(try Data(contentsOf: retained), Data([1, 2, 3])); XCTAssertEqual(try Data(contentsOf: named), Data([9]))
    }
    func testUnexpectedHardLinkIsRefusedAtCompletion() throws {
        let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Recording.mov")
        try file.write(Data([1]))
        try FileManager.default.linkItem(at: root.appendingPathComponent("Recording.mov"), to: root.appendingPathComponent("Alias.mov"))
        XCTAssertThrowsError(try file.finish()); try file.close()
    }
    func testSameInodeTruncationOrGrowthCannotBeReportedAsACompleteCopy() throws {
        for truncate in [true, false] {
            let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Recording.mov")
            try file.write(Data(repeating: 1, count: 64))
            let changed = try FileHandle(forWritingTo: root.appendingPathComponent("Recording.mov"))
            if truncate { try changed.truncate(atOffset: 1) }
            else { try changed.seekToEnd(); try changed.write(contentsOf: Data([2])) }
            try changed.close()
            XCTAssertThrowsError(try file.finish()); try file.close()
            XCTAssertTrue(file.cleanupComplete)
        }
    }
    func testAnIncorrectPartialWriteCannotProduceASuccessfulFinish() throws {
        let root = try folder()
        let file = try RecordingExportDestination(folder: root, filename: "Recording.mov", writeData: { handle, data in
            try handle.write(contentsOf: data.prefix(1))
        })
        try file.write(Data(repeating: 1, count: 64))
        XCTAssertThrowsError(try file.finish()); try file.close()
    }
    func testCloseFailureCannotBecomeSuccessThroughRepeatedNoOp() throws {
        let root = try folder()
        var closes = 0
        let file = try RecordingExportDestination(folder: root, filename: "Recording.mov", closeHandle: { handle in
            closes += 1; try handle.close()
            if closes == 1 { throw NSError(domain: "GeneratedCloseFailure", code: 1) }
        })
        try file.write(Data([1]))
        XCTAssertThrowsError(try file.close())
        XCTAssertFalse(file.cleanupComplete); XCTAssertEqual(closes, 2)
        XCTAssertThrowsError(try file.close())
        XCTAssertFalse(file.cleanupComplete); XCTAssertEqual(closes, 2)
    }
    func testOversizedChunkIsRefusedWithoutWriting() throws {
        let root = try folder(), file = try RecordingExportDestination(folder: root, filename: "Recording.mov")
        XCTAssertThrowsError(try file.write(Data(repeating: 1, count: 65_537)))
        try file.close()
        XCTAssertEqual(try Data(contentsOf: root.appendingPathComponent("Recording.mov")).count, 0)
    }
}
