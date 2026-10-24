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
    func testReservationFromAnotherOwnerCannotReleaseCapacity() throws {
        let first = EncodedWorkBudget(bytes: 1, count: 1)
        let second = EncodedWorkBudget(bytes: 1, count: 1)
        let a = try XCTUnwrap(first.reserve(bytes: 1))
        let b = try XCTUnwrap(second.reserve(bytes: 1))
        first.release(b)
        XCTAssertEqual(first.pendingCount, 1)
        XCTAssertNil(first.reserve(bytes: 1))
        first.release(a); second.release(b)
        XCTAssertEqual(first.pendingCount, 0)
        XCTAssertEqual(second.pendingCount, 0)
    }
    func testHeldOwnerDrainsAcceptedPrefixBeforeFinishAndRejectsLateWork() throws {
        let budget = EncodedWorkBudget()
        let queue = DispatchQueue(label: "test.encoded-held-owner")
        let finished = expectation(description: "Actual close barrier")
        var events: [Int] = [] // The exact serial owner reads and writes this.
        queue.suspend()
        XCTAssertNotNil(budget.enqueue(bytes: 1, on: queue) { ticket in
            events.append(1); budget.release(ticket)
        })
        XCTAssertTrue(budget.close(enqueueOn: queue) {
            events.append(2)
            XCTAssertEqual(budget.pendingCount, 0)
            XCTAssertEqual(events, [1, 2])
            finished.fulfill()
        })
        XCTAssertNil(budget.enqueue(bytes: 1, on: queue) { _ in XCTFail("Late work ran") })
        queue.resume()
        wait(for: [finished], timeout: 2)
    }
    func testRepeatedJoinDoesNotQueueANewFaultAfterOrdinaryStop() {
        let budget = EncodedWorkBudget()
        let queue = DispatchQueue(label: "test.encoded-close-joins")
        let finished = expectation(description: "Repeated close joins")
        XCTAssertTrue(budget.close(enqueueOn: queue) {})
        XCTAssertFalse(budget.close(enqueueOn: queue, firstOnly: true) { XCTFail("Stop became a fault") })
        XCTAssertFalse(budget.close(enqueueOn: queue) { finished.fulfill() })
        wait(for: [finished], timeout: 2)
    }
}
