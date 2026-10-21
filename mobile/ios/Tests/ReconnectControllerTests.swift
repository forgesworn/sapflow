import XCTest
@testable import SapflowBroadcaster

final class ReconnectControllerTests: XCTestCase {
    private enum Failure: Error { case clock }
    private final class Policy: ReconnectPolicyPort {
        var calls: [String] = []
        var observationTimes: [UInt64] = []
        var generation: UInt64 = 7
        var attemptEpoch: UInt64 = 0
        var nextDeadlineNs: UInt64?
        var exhausted = false
        var nextTick = ReconnectAction.none
        var failureAction = ReconnectAction.retryAt(50)
        var rejectSending = false
        var rejectConfirmation = false
        func start(_ now: UInt64) throws -> ReconnectAction { calls.append("start"); attemptEpoch += 1; return .connect }
        func replace(_ now: UInt64) throws -> ReconnectAction { calls.append("replace"); if exhausted { return .expired }; attemptEpoch += 1; return .connect }
        func connected(_ now: UInt64) throws { observationTimes.append(now); calls.append("accepted") }
        func sending(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction {
            observationTimes.append(now); calls.append("sending:\(epoch)"); if rejectSending { throw Failure.clock }; return .none
        }
        func recovered(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { observationTimes.append(now); calls.append("peer:\(epoch)"); nextDeadlineNs = nil; return .none }
        func confirmed(_ context: ReconnectAttemptContext, _ now: UInt64) throws {
            if rejectConfirmation { throw Failure.clock }
            calls.append("confirmed:\(context.sdkGeneration):\(context.sdkAttempt)")
            nextDeadlineNs = nil
        }
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
        var contexts: [ReconnectAttemptContext] = []
        var ready: [(ReconnectAttemptContext, UInt64)] = []
        lazy var controller = ReconnectController(policy: policy, now: { [unowned self] in time },
            activeChanged: { [unowned self] in active = $0 }, changed: {},
            attemptReady: { [unowned self] context, now in ready.append((context, now)) })
        func attach(_ epoch: UInt64 = 1) {
            controller.attach(epoch: epoch, contextualFactory: { [unowned self] context, observe in
                contexts.append(context)
                let attempt = Attempt(observe); attempts.append(attempt); return attempt
            })
        }
        func acceptAndSend() {
            attempts.last!.events(.accepted, time)
            attempts.last!.events(.sending, time)
        }
    }
    func testThrowingFactoryNeverStartsAnOwnerAndKeepsRecordingOnlyEpoch() {
        let h = Harness(); var captured: ReconnectAttemptContext?
        h.controller.attach(epoch: 1, contextualFactory: { context, _ in
            captured = context
            throw Failure.clock // expired grant or canonical credential refusal
        })
        XCTAssertEqual(captured?.sdkGeneration, 7); XCTAssertEqual(captured?.sdkAttempt, 1)
        XCTAssertTrue(h.controller.cleanupComplete); XCTAssertTrue(h.attempts.isEmpty)
        XCTAssertTrue(h.controller.status.contains("local recording continues"))
        XCTAssertTrue(h.policy.calls.contains("stop")); XCTAssertTrue(h.ready.isEmpty)
        h.attach(2); XCTAssertTrue(h.attempts.isEmpty)
    }
    func testConfirmedEpochDetachesWithoutAdvertisingOldLiveThroughDelayedCleanup() throws {
        let h = Harness(); h.attach(); h.acceptAndSend(); let old = h.contexts[0]
        try h.controller.confirmObservedMedia(old, nowNs: 1)
        XCTAssertTrue(h.controller.status.hasPrefix("Live"))
        var done = false
        h.controller.detach(epoch: 1) { _ in done = true }
        XCTAssertFalse(h.controller.status.hasPrefix("Live")); XCTAssertFalse(done)
        h.attempts[0].events(.peerMediaProgress, 2)
        XCTAssertFalse(h.controller.status.hasPrefix("Live"))
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(old, nowNs: 3))
        h.attempts[0].release(); h.attach(2); h.acceptAndSend()
        XCTAssertTrue(done); XCTAssertFalse(h.controller.status.hasPrefix("Live"))
        try h.controller.confirmObservedMedia(h.contexts[1], nowNs: 4)
        XCTAssertTrue(h.controller.status.hasPrefix("Live"))
    }
    func testFactoryCapturesPolicyAndCodecIdentityBeforeTransportStarts() {
        let h = Harness(); h.attach(12)
        XCTAssertEqual(h.contexts, [ReconnectAttemptContext(codecEpoch: 12, token: 1, sdkGeneration: 7, sdkAttempt: 1)])
        XCTAssertEqual(h.attempts[0].starts, 1)
        h.time = 10; h.attempts[0].events(.failed, 10)
        h.policy.nextTick = .connect; h.time = 50; h.controller.tick(); h.attempts[0].release()
        XCTAssertEqual(h.contexts[1], ReconnectAttemptContext(codecEpoch: 12, token: 2, sdkGeneration: 7, sdkAttempt: 2))
    }
    func testObservationReadyRequiresAcceptedSendingAndFiresOnlyOnce() {
        let h = Harness(); h.attach()
        h.attempts[0].events(.peerMediaProgress, 1)
        XCTAssertTrue(h.ready.isEmpty)
        h.time = 3; h.attempts[0].events(.accepted, 2)
        XCTAssertEqual(h.ready.count, 1); XCTAssertEqual(h.ready[0].0, h.contexts[0]); XCTAssertEqual(h.ready[0].1, 3)
        h.attempts[0].events(.sending, 4); h.attempts[0].events(.accepted, 5)
        XCTAssertEqual(h.ready.count, 1)
    }
    func testDecodedConfirmationRequiresExactAcceptedSendingContextAndDoesNotDowngrade() throws {
        let h = Harness(); h.attach(); let current = h.contexts[0]
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(current, nowNs: 1))
        h.attempts[0].events(.accepted, 2)
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(current, nowNs: 2))
        h.attempts[0].events(.sending, 3)
        let wrong = ReconnectAttemptContext(codecEpoch: 1, token: 1, sdkGeneration: 8, sdkAttempt: 1)
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(wrong, nowNs: 4))
        try h.controller.confirmObservedMedia(current, nowNs: 5)
        XCTAssertEqual(h.policy.calls.last, "confirmed:7:1")
        let verified = h.controller.status
        h.attempts[0].events(.peerMediaProgress, 6); h.attempts[0].events(.sending, 7)
        XCTAssertEqual(h.controller.status, verified)
        XCTAssertTrue(verified.hasPrefix("Live"))
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(current, nowNs: 8))
    }
    func testOldDecodedReplyCannotConfirmNewAttemptInSameSdkGeneration() throws {
        let h = Harness(); h.attach(); h.acceptAndSend(); let old = h.contexts[0]
        h.time = 10; h.attempts[0].events(.failed, 10)
        h.policy.nextTick = .connect; h.time = 50; h.controller.tick(); h.attempts[0].release(); h.acceptAndSend()
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(old, nowNs: 51))
        XCTAssertEqual(h.policy.nextDeadlineNs, 100)
        XCTAssertFalse(h.policy.calls.contains { $0.hasPrefix("confirmed:") })
        try h.controller.confirmObservedMedia(h.contexts[1], nowNs: 52)
        XCTAssertEqual(h.policy.calls.last, "confirmed:7:2")
    }
    func testStopFailureAndCodecCloseRejectPendingDecodedResults() {
        for action in 0..<3 {
            let h = Harness(); h.attach(); h.acceptAndSend(); let captured = h.contexts[0]
            if action == 0 { h.controller.stop() }
            else if action == 1 { h.time = 10; h.attempts[0].events(.failed, 10) }
            else { h.controller.detach(epoch: 1) { _ in } }
            XCTAssertThrowsError(try h.controller.confirmObservedMedia(captured, nowNs: 11))
            XCTAssertFalse(h.policy.calls.contains { $0.hasPrefix("confirmed:") })
            if action == 1 { XCTAssertEqual(h.policy.nextDeadlineNs, 100) }
        }
    }
    func testRejectedNativeConfirmationRemainsUnconfirmedWithoutTransportFailure() {
        let h = Harness(); h.attach(); h.acceptAndSend(); h.policy.rejectConfirmation = true
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(h.contexts[0], nowNs: 3))
        XCTAssertTrue(h.controller.status.contains("unconfirmed")); XCTAssertEqual(h.attempts[0].closes, 0)
        XCTAssertFalse(h.policy.calls.contains("stop"))
    }
    func testRotationCapturesNewCodecEpochWithoutStartingOutageOrChangingGeneration() {
        let h = Harness(); h.attach(); h.acceptAndSend(); let old = h.contexts[0]
        h.controller.detach(epoch: 1) { _ in }; h.attempts[0].release(); h.attach(2); h.acceptAndSend()
        XCTAssertEqual(h.contexts[1].sdkGeneration, old.sdkGeneration)
        XCTAssertEqual(h.contexts[1].sdkAttempt, 2); XCTAssertEqual(h.contexts[1].codecEpoch, 2)
        XCTAssertNil(h.policy.nextDeadlineNs)
        XCTAssertThrowsError(try h.controller.confirmObservedMedia(old, nowNs: 1))
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
