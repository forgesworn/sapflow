import Foundation
import XCTest
@testable import SapflowBroadcaster

final class QuickTimeMovieLayoutTests: XCTestCase {
    typealias Scanner = QuickTimeMovieLayout
    static func be(_ n: UInt64, _ width: Int = 4) -> Data {
        Data((0..<width).reversed().map { UInt8(truncatingIfNeeded: n >> ($0 * 8)) })
    }
    static func header(_ kind: String, bytes: UInt64, wide: Bool = false) -> Data {
        (wide ? be(1) + Data(kind.utf8) + be(bytes, 8) : be(bytes) + Data(kind.utf8))
    }
    private func sparse(total: UInt64, headers: [UInt64: Data], expectedReads: [UInt64]) throws -> Scanner.Layout {
        var reads: [UInt64] = []
        let value = try Scanner.scan(fileBytes: total) { offset, count in
            XCTAssertEqual(count, 8); XCTAssertLessThanOrEqual(offset, total - 8)
            reads.append(offset)
            guard let bytes = headers[offset] else {
                XCTFail("Scanner attempted to read payload instead of a declared header")
                return Data()
            }
            return bytes
        }
        XCTAssertEqual(reads, expectedReads)
        return value
    }
    func testSkipsPayloadAndReturnsOnlyFinalMovie() throws {
        let result = try sparse(total: 112, headers: [0: Self.header("ftyp", bytes: 16),
            16: Self.header("mdat", bytes: 64), 80: Self.header("moov", bytes: 32)], expectedReads: [0, 16, 80])
        XCTAssertEqual(result.movie, .init(offset: 80, bytes: 32, headerBytes: 8))
        XCTAssertEqual(result.boxCount, 3); XCTAssertEqual(result.mediaBoxCount, 1)
    }
    func testSparseExtendedMediaAboveFourGiBUsesOnlyThreeReads() throws {
        let size: UInt64 = 1 << 42
        let result = try sparse(total: size + 32,
            headers: [0: Self.header("mdat", bytes: 1), 8: Self.be(size, 8), size: Self.header("moov", bytes: 32)],
            expectedReads: [0, 8, size])
        XCTAssertEqual(result.movie.offset, size)
    }
    func testExtendedMovieHeaderHasCheckedSizeAndTwoReads() throws {
        let result = try sparse(total: 48, headers: [0: Self.header("mdat", bytes: 16),
            16: Self.header("moov", bytes: 1), 24: Self.be(32, 8)], expectedReads: [0, 16, 24])
        XCTAssertEqual(result.movie, .init(offset: 16, bytes: 32, headerBytes: 16))
    }
    func testRejectsOpenEndedAndTooSmallBoxes() {
        for size: UInt64 in [0, 2, 7] {
            XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { _, _ in Self.header("mdat", bytes: size) })
        }
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { offset, _ in offset == 0 ? Self.header("mdat", bytes: 1) : Self.be(15, 8) })
    }
    func testTruncatedHeaderNeverRequestsUnavailableBytes() {
        for size: UInt64 in 0..<8 {
            XCTAssertThrowsError(try Scanner.scan(fileBytes: size) { _, _ in XCTFail("Unexpected read"); return Data() })
        }
        var calls = 0
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 12) { _, _ in calls += 1; return Self.header("mdat", bytes: 1) })
        XCTAssertEqual(calls, 1)
    }
    func testRejectsOverflowingOrOutOfFileExtents() {
        var calls = 0
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 128) { offset, _ in
            calls += 1
            if offset == 0 { return Self.header("free", bytes: 16) }
            return offset == 16 ? Self.header("mdat", bytes: 1) : Self.be(UInt64.max, 8)
        })
        XCTAssertEqual(calls, 3)
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { _, _ in Self.header("mdat", bytes: 33) })
    }
    func testRejectsShortAndOverlongReadPortResults() {
        for count in [0, 1, 7, 9, 16] {
            XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { _, _ in Data(repeating: 0, count: count) }) { error in
                XCTAssertEqual(error as? Scanner.Failure, .shortRead)
            }
        }
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { offset, _ in offset == 0 ? Self.header("mdat", bytes: 1) : Data() })
    }
    func testReadPortFailureIsPropagatedUnchanged() {
        enum Stop: Error { case exactOwner }
        XCTAssertThrowsError(try Scanner.scan(fileBytes: 32) { _, _ in throw Stop.exactOwner }) { error in
            guard case Stop.exactOwner = error else { return XCTFail("Lost read-owner failure") }
        }
    }
    func testRejectsMissingEarlyDuplicateMovieAndTrailingJunk() {
        for data in [Self.header("mdat", bytes: 8), Self.header("moov", bytes: 8),
                     Self.header("mdat", bytes: 8) + Self.header("moov", bytes: 8) + Self.header("moov", bytes: 8),
                     Self.header("mdat", bytes: 8) + Self.header("moov", bytes: 8) + Data([1])] {
            XCTAssertThrowsError(try Scanner.scan(fileBytes: UInt64(data.count)) { offset, count in
                data.subdata(in: Int(offset)..<(Int(offset) + count))
            })
        }
    }
    func testRejectsFragmentAndIndexBoxesWithoutPayloadReads() {
        for kind in ["moof", "mfra", "sidx"] {
            var calls = 0
            XCTAssertThrowsError(try Scanner.scan(fileBytes: 64) { _, _ in calls += 1; return Self.header(kind, bytes: 64) })
            XCTAssertEqual(calls, 1)
        }
    }
    func testBoxLimitHasConstantSpaceAndNoExtraRead() throws {
        for extra in [0, 1] {
            let count = Scanner.maximumBoxes + extra, total = UInt64(count) * 8
            var calls = 0
            let scan = { try Scanner.scan(fileBytes: total) { offset, bytes in
                calls += 1; XCTAssertEqual(bytes, 8)
                let index = offset / 8
                let kind = index == UInt64(count - 1) ? "moov" : (index == UInt64(count - 2) ? "mdat" : "free")
                return Self.header(kind, bytes: 8)
            } }
            if extra == 0 { XCTAssertEqual(try scan().boxCount, count) }
            else { XCTAssertThrowsError(try scan()) }
            XCTAssertEqual(calls, Scanner.maximumBoxes)
        }
    }
    func testMovieCapUsesScalarSizeWithoutReadingMovieBody() throws {
        for extra in [0, 1] {
            let size = UInt64(Scanner.maximumMovieBytes + extra)
            let scan = { try Scanner.scan(fileBytes: size + 8) { offset, _ in Self.header(offset == 0 ? "mdat" : "moov", bytes: offset == 0 ? 8 : size) } }
            if extra == 0 { XCTAssertEqual(try scan().movie.bytes, Scanner.maximumMovieBytes) }
            else { XCTAssertThrowsError(try scan()) { XCTAssertEqual($0 as? Scanner.Failure, .excessive) } }
        }
    }
    func testUInt64SparseEndDoesNotOverflowOrAllocateFileSize() throws {
        let offset = UInt64.max - 16
        let result = try sparse(total: UInt64.max, headers: [0: Self.header("mdat", bytes: 1),
            8: Self.be(offset, 8), offset: Self.header("moov", bytes: 16)], expectedReads: [0, 8, offset])
        XCTAssertEqual(result.movie.offset, offset); XCTAssertEqual(result.movie.bytes, 16)
    }
    func testMultipleMediaAndNonzeroDataIndices() throws {
        var calls = 0
        let result = try Scanner.scan(fileBytes: 24) { offset, _ in
            calls += 1
            return (Data([99]) + Self.header(offset == 16 ? "moov" : "mdat", bytes: 8)).dropFirst()
        }
        XCTAssertEqual(result.mediaBoxCount, 2); XCTAssertEqual(calls, 3)
    }
}
