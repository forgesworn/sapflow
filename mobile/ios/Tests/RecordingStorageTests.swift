import XCTest
@testable import SapflowBroadcaster

final class RecordingStorageTests: XCTestCase {
    func testSelectedProfileBudgetUsesSixtySecondsAndHeadroomWithoutLimitingDuration() throws {
        for profile in CaptureProfile.allCases {
            let budget = try RecordingStorage.budget(profile: profile)
            XCTAssertEqual(budget.horizonSeconds, 60)
            XCTAssertEqual(budget.payloadBytes, profile.elementaryBps * 60 / 8)
            XCTAssertEqual(budget.requiredBytes, budget.payloadBytes + max(16 * 1024 * 1024, budget.payloadBytes / 2) + 32 * 1024 * 1024)
        }
    }
    func testExactCapacityLowAndUnavailableAreDistinct() throws {
        let budget = try RecordingStorage.budget(profile: .sd480p30)
        XCTAssertEqual(RecordingStorage.check(budget: budget) { budget.requiredBytes }.state, .available)
        XCTAssertEqual(RecordingStorage.check(budget: budget) { budget.requiredBytes - 1 }.state, .low)
        XCTAssertEqual(RecordingStorage.check(budget: budget) { nil }.state, .unavailable)
        XCTAssertEqual(RecordingStorage.check(budget: budget) { throw RecordingCatalogue.Failure.io }.state, .unavailable)
    }
    func testOverflowZeroAndReserveOverflowAreRefused() {
        XCTAssertThrowsError(try RecordingStorage.budget(profile: .sd480p30, horizonSeconds: 0))
        XCTAssertThrowsError(try RecordingStorage.budget(profile: .fullHD1080p30, horizonSeconds: UInt64.max))
        XCTAssertThrowsError(try RecordingStorage.budget(profile: .sd480p30, reserveBytes: UInt64.max))
    }
    func testBudgetPreservesExplicitLongerHorizon() throws {
        let short = try RecordingStorage.budget(profile: .hd720p30)
        let long = try RecordingStorage.budget(profile: .hd720p30, horizonSeconds: 3600)
        XCTAssertEqual(long.horizonSeconds, 3600)
        XCTAssertEqual(long.payloadBytes, short.payloadBytes * 60)
        XCTAssertGreaterThan(long.requiredBytes, short.requiredBytes)
    }
}
