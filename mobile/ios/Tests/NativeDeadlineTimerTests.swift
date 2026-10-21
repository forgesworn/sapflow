import XCTest
@testable import SapflowBroadcaster

final class NativeDeadlineTimerTests: XCTestCase {
    func testRescheduleUsesLatestGenerationAndCancelsAfterFire() {
        let queue = DispatchQueue(label: "sapflow.deadline-test")
        let timer = NativeDeadlineTimer(queue: queue)
        let fired = expectation(description: "latest deadline")
        let old = expectation(description: "superseded generation")
        old.isInverted = true
        queue.async {
            let now = DispatchTime.now().uptimeNanoseconds
            timer.schedule(atNs: now + 100_000_000, generation: 1) { _ in old.fulfill() }
            timer.schedule(atNs: now + 10_000_000, generation: 2) { generation in
                XCTAssertEqual(generation, 2)
                fired.fulfill()
                timer.schedule(atNs: nil, generation: 2) { _ in old.fulfill() }
            }
        }
        wait(for: [fired, old], timeout: 0.3)
    }
    func testCancelledSourceCannotFireAfterRearm() {
        let queue = DispatchQueue(label: "sapflow.cancelled-deadline-test")
        let timer = NativeDeadlineTimer(queue: queue)
        let cancelled = expectation(description: "cancelled callback")
        cancelled.isInverted = true
        let rearmed = expectation(description: "rearmed source")
        queue.async {
            let now = DispatchTime.now().uptimeNanoseconds
            timer.schedule(atNs: now, generation: 1) { _ in cancelled.fulfill() }
            timer.cancel()
            timer.schedule(atNs: now + 10_000_000, generation: 2) { generation in
                XCTAssertEqual(generation, 2)
                rearmed.fulfill()
                timer.cancel()
            }
        }
        wait(for: [cancelled, rearmed], timeout: 0.3)
    }
}
