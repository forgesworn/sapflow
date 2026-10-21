import XCTest
@testable import SapflowBroadcaster

final class EncodedWorkBudgetTests: XCTestCase {
    func testBytesCountAndTimeHaveIndependentBounds() throws {
        let bytes = EncodedWorkBudget(bytes: 8, count: 3, span: 2)
        let first = try XCTUnwrap(bytes.reserve(bytes: 6, time: 0))
        XCTAssertNil(bytes.reserve(bytes: 3, time: 0.1))
        bytes.release(first)
        XCTAssertNotNil(bytes.reserve(bytes: 8, time: 1))
        let count = EncodedWorkBudget(bytes: 100, count: 1, span: 2)
        XCTAssertNotNil(count.reserve(bytes: 1, time: 0))
        XCTAssertNil(count.reserve(bytes: 1, time: 0))
        let time = EncodedWorkBudget(bytes: 100, count: 3, span: 2)
        XCTAssertNotNil(time.reserve(bytes: 1, time: 0))
        XCTAssertNil(time.reserve(bytes: 1, time: 2.01))
    }
    func testOutOfOrderDrainAndCloseAreSafe() throws {
        let budget = EncodedWorkBudget(bytes: 100, count: 3, span: 2)
        let first = try XCTUnwrap(budget.reserve(bytes: 1, time: 0))
        let second = try XCTUnwrap(budget.reserve(bytes: 2, time: 1))
        budget.release(second)
        XCTAssertNil(budget.reserve(bytes: 1, time: 2.5))
        budget.release(first)
        let third = try XCTUnwrap(budget.reserve(bytes: 1, time: 2.5))
        XCTAssertEqual(budget.pendingCount, 1)
        XCTAssertTrue(budget.close())
        XCTAssertFalse(budget.close())
        XCTAssertNil(budget.reserve(bytes: 1, time: 2.5))
        budget.release(third); budget.release(third)
        XCTAssertEqual(budget.pendingCount, 0)
    }
}
