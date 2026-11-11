import XCTest
@testable import SapflowBroadcaster

final class SrtRecoveryPolicyTests: XCTestCase {
    func testMobileDefaultLeavesRecoveryRoomBeforeSenderExpiry() {
        XCTAssertEqual(SrtRecoveryPolicy.mobile.requestedLatencyMs, 1000)
        XCTAssertEqual(SrtRecoveryPolicy.mobile.maxPacketAgeMs, 1250)
    }
    func testRequestBoundsRejectRatherThanClamp() throws {
        for invalid: UInt32 in [0, 1001, .max] {
            XCTAssertThrowsError(try SrtRecoveryPolicy(requestedLatencyMs: invalid))
        }
        for valid: UInt32 in [1, 120, 1000] {
            XCTAssertEqual(try SrtRecoveryPolicy(requestedLatencyMs: valid).requestedLatencyMs, valid)
        }
    }
    func testRetentionFloorAndFractionalMillisecondsRoundUp() throws {
        for (request, retention): (UInt32, UInt32) in [(1, 1000), (120, 1000), (800, 1000), (801, 1002), (999, 1249), (1000, 1250)] {
            XCTAssertEqual(try SrtRecoveryPolicy(requestedLatencyMs: request).maxPacketAgeMs, retention)
        }
        for request in UInt32(1)...1000 {
            let policy = try SrtRecoveryPolicy(requestedLatencyMs: request)
            XCTAssertTrue((1000...1250).contains(policy.maxPacketAgeMs))
            XCTAssertGreaterThanOrEqual(policy.maxPacketAgeMs * 4, request * 5)
        }
    }
}
