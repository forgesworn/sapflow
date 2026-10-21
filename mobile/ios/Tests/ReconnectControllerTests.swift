import XCTest
@testable import SapflowBroadcaster

final class ReconnectControllerTests: XCTestCase {
    private enum Failure: Error { case clock }
    private final class Policy: ReconnectPolicyPort {
        var calls: [String] = []
        var observationTimes: [UInt64] = []
        var attemptEpoch: UInt64 = 0
        var nextDeadlineNs: UInt64?
        var exhausted = false
        var nextTick = ReconnectAction.none
        var failureAction = ReconnectAction.retryAt(50)
        var rejectSending = false
        func start(_ now: UInt64) throws -> ReconnectAction { calls.append("start"); attemptEpoch += 1; return .connect }
        func replace(_ now: UInt64) throws -> ReconnectAction { calls.append("replace"); if exhausted { return .expired }; attemptEpoch += 1; return .connect }
        func connected(_ now: UInt64) throws { observationTimes.append(now); calls.append("accepted") }
        func sending(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction {
            observationTimes.append(now); calls.append("sending:\(epoch)"); if rejectSending { throw Failure.clock }; return .none
        }
        func recovered(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { observationTimes.append(now); calls.append("peer:\(epoch)"); nextDeadlineNs = nil; return .none }
        func failed(_ now: UInt64) throws -> ReconnectAction { calls.append("failed:\(now)"); nextDeadlineNs = 100; return failureAction }
        func tick(_ now: UInt64) throws -> ReconnectAction {
            calls.append("tick")
            let action = nextTick; nextTick = .none
            if action == .connect { attemptEpoch += 1 }
            if action == .exhausted { exhausted = true; nextDeadlineNs = nil }
            return action
        }
        func stop() throws { calls.append("stop"); nextDeadlineNs = nil }
    }
    private final class Attempt: ReconnectAttemptPort {
        let events: (PublishingAttemptEvent, UInt64) -> Void
        var starts = 0
        var closes = 0
        var completed: (() -> Void)?
        init(_ events: @escaping (PublishingAttemptEvent, UInt64) -> Void) { self.events = events }
        func start() throws { starts += 1 }
        func close(completion: @escaping () -> Void) { closes += 1; completed = completion }
        func release() { let done = completed; completed = nil; done?() }
    }
    private final class Harness {
        let policy = Policy()
        var time: UInt64 = 0
        var attempts: [Attempt] = []
        var active: ReconnectAttemptPort?
        lazy var controller = ReconnectController(policy: policy, now: { [unowned self] in time },
            activeChanged: { [unowned self] in active = $0 }, changed: {})
        func attach(_ epoch: UInt64 = 1) {
            controller.attach(epoch: epoch) { [unowned self] observe in
                let attempt = Attempt(observe); attempts.append(attempt); return attempt
            }
        }
        func acceptAndSend() {
            attempts.last!.events(.accepted, time)
            attempts.last!.events(.sending, time)
        }
    }
    func testColdProgressDrainExposesLegacySendingWithoutInboundTrafficAndDeduplicatesFusedFacts() {
        let latch = PublishingProgressLatch()
        var events: [PublishingAttemptEvent] = []
        // Legacy send while network output is outstanding has queued Sending.
        // The production cold output/completion path drains the same latch.
        var queued: PublishingAttemptEvent? = .sending
        latch.drain(next: { defer { queued = nil }; return queued }, notify: { events.append($0) })
        XCTAssertEqual(events.count, 1)
        if case .sending = events[0] {} else { XCTFail("Startup submission was hidden") }
        latch.observe(sending: true, peer: false) { events.append($0) } // later fused result
        latch.observe(sending: true, peer: true) { events.append($0) }
        XCTAssertEqual(events.count, 2)
        if case .peerMediaProgress = events[1] {} else { XCTFail("Peer fact missing") }
        var calls = 0
        latch.drain(next: { calls += 1; return .peerMediaProgress }, notify: { events.append($0) })
        XCTAssertEqual(calls, 4); XCTAssertEqual(events.count, 2)
    }
    func testSynchronousStopFenceRefusesQueuedNativeStartAdmission() {
        let fence = AttemptStartFence()
        var starts = 0; var stops = 0
        fence.close { stops += 1 }
        XCTAssertThrowsError(try fence.start { starts += 1 })
        XCTAssertEqual(starts, 0); XCTAssertEqual(stops, 1)
    }
    func testStopWaitsOnlyForAlreadyEnteredBoundedStartAdmission() {
        let fence = AttemptStartFence()
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let stopRequested = DispatchSemaphore(value: 0), stopped = DispatchSemaphore(value: 0)
        DispatchQueue(label: "test.start-admission").async {
            try? fence.start { entered.signal(); _ = release.wait(timeout: .now() + 1) }
        }
        XCTAssertEqual(entered.wait(timeout: .now() + 1), .success)
        DispatchQueue(label: "test.stop-admission").async {
            stopRequested.signal(); fence.close { stopped.signal() }
        }
        XCTAssertEqual(stopRequested.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(stopped.wait(timeout: .now() + 0.02), .timedOut)
        release.signal()
        XCTAssertEqual(stopped.wait(timeout: .now() + 1), .success)
        XCTAssertThrowsError(try fence.start {})
    }
    func testQueuedObservationUsesOwnerClockAfterAnInterleavedTimer() {
        let h = Harness(); h.attach()
        h.time = 20; h.controller.tick()
        h.time = 21
        h.attempts[0].events(.sending, 5)
        h.attempts[0].events(.accepted, 4)
        h.attempts[0].events(.peerMediaProgress, 6)
        XCTAssertEqual(h.policy.observationTimes, [21, 21, 21])
        XCTAssertEqual(h.attempts[0].closes, 0)
    }
    func testPreparedIsDormantAndAttemptWaitsForActualAcceptance() {
        let h = Harness()
        XCTAssertTrue(h.attempts.isEmpty)
        XCTAssertNil(h.controller.nextDeadlineNs)
        h.attach()
        XCTAssertEqual(h.attempts.count, 1)
        XCTAssertEqual(h.attempts[0].starts, 1)
        h.attempts[0].events(.sending, 1)
        XCTAssertEqual(h.policy.calls, ["start"])
        XCTAssertFalse(h.controller.status.contains("Destination accepted"))
    }
    func testEarlyPeerFactLatchesAndDrainsOnlyAcceptedSendingPeerOrder() {
        let h = Harness(); h.attach()
        h.attempts[0].events(.peerMediaProgress, 1)
        h.attempts[0].events(.sending, 2)
        XCTAssertEqual(h.policy.calls, ["start"])
        h.attempts[0].events(.accepted, 3)
        XCTAssertEqual(h.policy.calls, ["start", "accepted", "sending:1", "peer:1"])
        h.attempts[0].events(.peerMediaProgress, 4)
        XCTAssertEqual(h.policy.calls.count, 4)
        XCTAssertTrue(h.controller.status.contains("decoded playback unconfirmed"))
    }
    func testFailureStartsOriginalWindowBeforeCleanupAndRetryCannotOverlapOldOwner() {
        let h = Harness(); h.attach(); h.time = 10
        h.attempts[0].events(.failed, 7)
        XCTAssertTrue(h.policy.calls.contains("failed:10"))
        XCTAssertEqual(h.attempts[0].closes, 1)
        XCTAssertNil(h.active)
        XCTAssertFalse(h.controller.cleanupComplete)
        h.policy.nextTick = .connect; h.time = 50; h.controller.tick()
        XCTAssertEqual(h.attempts.count, 1)
        h.attempts[0].release()
        XCTAssertEqual(h.attempts.count, 2)
        XCTAssertEqual(h.policy.calls.filter { $0.hasPrefix("failed:") }, ["failed:10"])
    }
    func testStopFencesPendingRetryAndLateCallbacksBeforeOwnerRelease() {
        let h = Harness(); h.attach(); h.attempts[0].events(.failed, 1)
        h.policy.nextTick = .connect; h.controller.tick()
        h.controller.stop()
        let calls = h.policy.calls
        h.attempts[0].events(.accepted, 2)
        h.attempts[0].events(.peerMediaProgress, 3)
        h.attempts[0].release()
        XCTAssertEqual(h.policy.calls, calls)
        XCTAssertEqual(h.attempts.count, 1)
        XCTAssertTrue(h.controller.cleanupComplete)
        XCTAssertNil(h.controller.nextDeadlineNs)
    }
    func testOrientationWaitsForActualCancelAndReusesPolicyWithoutFailure() {
        let h = Harness(); h.attach(); h.acceptAndSend()
        var done = false
        h.controller.detach(epoch: 1) { result in if case .success = result { done = true } }
        XCTAssertFalse(done)
        h.attach(2) // ignored while retained owner is closing
        XCTAssertEqual(h.attempts.count, 1)
        h.attempts[0].release()
        XCTAssertTrue(done)
        XCTAssertEqual(h.policy.calls.filter { $0 == "replace" }.count, 1)
        h.attach(2)
        XCTAssertEqual(h.attempts.count, 2)
        XCTAssertEqual(h.policy.calls.filter { $0 == "start" }.count, 1)
        XCTAssertFalse(h.policy.calls.contains { $0.hasPrefix("failed:") })
    }
    func testOriginalDeadlineRemainsArmedAcrossEpochGapAndNewHandshake() {
        let h = Harness(); h.attach(); h.time = 10
        h.attempts[0].events(.failed, 10)
        XCTAssertEqual(h.controller.nextDeadlineNs, 100)
        h.controller.detach(epoch: 1) { _ in }
        h.time = 50; h.attempts[0].release()
        XCTAssertEqual(h.controller.nextDeadlineNs, 100)
        h.time = 60; h.attach(2)
        XCTAssertEqual(h.attempts.count, 2)
        XCTAssertEqual(h.controller.nextDeadlineNs, 100)
        h.time = 100; h.policy.nextTick = .expired; h.controller.tick()
        XCTAssertNil(h.active)
        XCTAssertEqual(h.attempts[1].closes, 1)
        h.attempts[1].events(.accepted, 101)
        XCTAssertFalse(h.policy.calls.contains("accepted"))
    }
    func testLateCleanupBeforeOverdueTimerCannotStartSuccessor() {
        let h = Harness(); h.attach(); h.time = 10
        h.attempts[0].events(.failed, 10)
        h.time = 50; h.policy.nextTick = .connect; h.controller.tick()
        XCTAssertEqual(h.attempts.count, 1)
        h.time = 101; h.policy.nextTick = .expired
        h.attempts[0].release() // cancellation beats the overdue timer delivery
        XCTAssertEqual(h.attempts.count, 1)
        XCTAssertNil(h.active)
        XCTAssertNil(h.controller.nextDeadlineNs)
        h.controller.tick(); h.attempts[0].events(.accepted, 102)
        XCTAssertEqual(h.attempts.count, 1)
        XCTAssertFalse(h.policy.calls.contains("accepted"))
    }
    func testOldAttemptCannotConfirmReplacement() {
        let h = Harness(); h.attach()
        h.controller.detach(epoch: 1) { _ in }; h.attempts[0].release(); h.attach(2)
        h.acceptAndSend()
        let calls = h.policy.calls
        h.attempts[0].events(.peerMediaProgress, 10)
        XCTAssertEqual(h.policy.calls, calls)
        h.attempts[1].events(.peerMediaProgress, 11)
        XCTAssertEqual(h.policy.calls.last, "peer:2")
    }
    func testExhaustedSenderContinuesWithoutAlarmButLateProofAndNextFailureCannotRearm() {
        let h = Harness(); h.attach(); h.acceptAndSend()
        h.policy.nextTick = .exhausted; h.controller.tick()
        XCTAssertNotNil(h.active)
        XCTAssertNil(h.controller.nextDeadlineNs)
        h.attempts[0].events(.peerMediaProgress, 100)
        XCTAssertFalse(h.policy.calls.contains { $0.hasPrefix("peer:") })
        h.policy.failureAction = .expired; h.attempts[0].events(.failed, 101)
        h.attempts[0].release()
        h.controller.detach(epoch: 1) { _ in }; h.attach(2)
        XCTAssertEqual(h.attempts.count, 1) // later recording epoch remains inert
    }
    func testCleanupDeadlineFailsClosedRetainsOwnerAndLateReleaseCannotResurrect() {
        let h = Harness(); h.attach(); h.time = 10
        var results = 0; var failed = false
        h.controller.detach(epoch: 1) { result in results += 1; if case .failure = result { failed = true } }
        h.time = 4_000_000_010; h.controller.tick()
        XCTAssertTrue(failed); XCTAssertEqual(results, 1)
        XCTAssertFalse(h.controller.cleanupComplete)
        XCTAssertNil(h.controller.nextDeadlineNs)
        h.attach(2); XCTAssertEqual(h.attempts.count, 1)
        h.attempts[0].release()
        XCTAssertTrue(h.controller.cleanupComplete)
        XCTAssertEqual(results, 1)
        h.attach(2); XCTAssertEqual(h.attempts.count, 1)
    }
    func testPolicyClockRejectionClosesInsteadOfInventingAWindow() {
        let h = Harness(); h.attach(); h.policy.rejectSending = true
        h.attempts[0].events(.accepted, 10)
        h.attempts[0].events(.sending, 9)
        XCTAssertTrue(h.policy.calls.contains("stop"))
        XCTAssertEqual(h.attempts[0].closes, 1)
        XCTAssertFalse(h.policy.calls.contains { $0.hasPrefix("peer:") })
    }
    func testStopBeforeConfigureAndStaleDetachHaveNoAutomaticStart() {
        let h = Harness(); h.controller.stop(); h.attach()
        var succeeded = false
        h.controller.detach(epoch: 1) { result in if case .success = result { succeeded = true } }
        XCTAssertTrue(succeeded); XCTAssertTrue(h.attempts.isEmpty)
        XCTAssertEqual(h.policy.calls, ["stop"])
    }
}
