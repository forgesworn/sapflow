import XCTest
@testable import SapflowBroadcaster
#if canImport(Sapflow)
import Sapflow
#endif

final class PublishingTelemetryTests: XCTestCase {
    private func sample(_ bytes: UInt64, _ time: UInt64, available: Bool = true, complete: Bool = true, continuity: UInt64 = 0) -> SocketOutboundSample {
        SocketOutboundSample(completedBytes: bytes, measuredAtNs: time, available: available, complete: complete, continuity: continuity)
    }
    private func context(_ token: UInt64, epoch: UInt64 = 1) -> ReconnectAttemptContext {
        ReconnectAttemptContext(codecEpoch: epoch, token: token, sdkGeneration: 3, sdkAttempt: token)
    }
    func testOnlySuccessfulExistingCompletionsCountBytes() {
        var counter = SocketCompletionCounter()
        let first = counter.begin(bytes: 1500)
        XCTAssertEqual(counter.sample(nowNs: 1, available: true).completedBytes, 0)
        counter.completed(first, success: false)
        XCTAssertEqual(counter.sample(nowNs: 2, available: true).completedBytes, 0)
        XCTAssertFalse(counter.sample(nowNs: 2, available: true).complete)
        let next = counter.begin(bytes: 1200); counter.completed(next, success: true)
        XCTAssertEqual(counter.sample(nowNs: 3, available: true).completedBytes, 1200)
        XCTAssertFalse(counter.seal(nowNs: 4).complete)
    }
    func testDuplicateAndLateWriteCannotCreditANewerPendingWrite() {
        var counter = SocketCompletionCounter()
        let old = counter.begin(bytes: 100); counter.completed(old, success: true)
        let next = counter.begin(bytes: 200)
        XCTAssertEqual(counter.completed(old, success: true), .stale)
        XCTAssertEqual(counter.sample(nowNs: 1, available: true).completedBytes, 100)
        counter.completed(next, success: true); counter.completed(next, success: true)
        XCTAssertEqual(counter.sample(nowNs: 2, available: true).completedBytes, 300)
    }
    func testUnavailableMetricsNeverBlockRealWriteCompletionButStaleTicketDoes() {
        var counter = SocketCompletionCounter(completedBytes: UInt64.max)
        let write = counter.begin(bytes: 1)
        XCTAssertEqual(counter.completed(write, success: true), .matched) // Overflow is only a metric failure.
        XCTAssertFalse(counter.sample(nowNs: 1, available: true).complete)
        XCTAssertEqual(counter.completed(write, success: true), .stale)
        XCTAssertNil(counter.begin(bytes: 0))
        XCTAssertEqual(counter.completed(nil, success: true), .untracked) // The adapter must still drive its real socket.
    }
    func testUnresolvedFinalWriteIsUnknownAndSealedTotalNeverGrows() {
        var counter = SocketCompletionCounter()
        let first = counter.begin(bytes: 100); counter.completed(first, success: true)
        let pending = counter.begin(bytes: 500)
        let final = counter.seal(nowNs: 10)
        XCTAssertEqual(final.completedBytes, 100); XCTAssertFalse(final.complete); XCTAssertFalse(final.available)
        counter.completed(pending, success: true) // A late cancellation callback cannot rewrite a final report.
        XCTAssertEqual(counter.seal(nowNs: 20), final)
        XCTAssertEqual(counter.sample(nowNs: 30, available: true), final)
    }
    func testOverflowDoesNotWrapOrSaturateIntoAPlausibleRate() {
        var counter = SocketCompletionCounter(completedBytes: UInt64.max - 1)
        let write = counter.begin(bytes: 2); counter.completed(write, success: true)
        let value = counter.sample(nowNs: 1, available: true)
        XCTAssertFalse(value.complete); XCTAssertEqual(value.completedBytes, UInt64.max - 1)
        var rate = SocketOutboundRate(); rate.update(value, nowNs: 1)
        XCTAssertNil(rate.kilobitsPerSecond)
    }
    func testFinalAccountingPrecedesSuccessorAndDuplicateFinalCannotDoubleCount() {
        var ledger = PublishingTelemetryAggregate<ReconnectAttemptContext>()
        let old = context(1), next = context(2, epoch: 2)
        ledger.opened(old); XCTAssertEqual(ledger.read(old, sample: sample(100, 1))?.completedBytes, 100)
        ledger.closing(old)
        var order: [String] = []
        let actualClose = {
            ledger.finished(old, sample: self.sample(150, 2, available: false)); order.append("final")
            ledger.opened(next); order.append("successor")
        }
        actualClose()
        ledger.finished(old, sample: sample(150, 2, available: false))
        XCTAssertEqual(order, ["final", "successor"])
        XCTAssertEqual(ledger.read(next, sample: sample(25, 3))?.completedBytes, 175)
    }
    func testSameEpochStaleTokenCannotUpdateOrRetireCurrentContext() {
        var ledger = PublishingTelemetryAggregate<ReconnectAttemptContext>()
        let old = context(1), next = context(2)
        ledger.opened(old); ledger.closing(old); ledger.finished(old, sample: sample(10, 1, available: false))
        ledger.opened(next)
        XCTAssertNil(ledger.read(old, sample: sample(9999, 2)))
        ledger.finished(old, sample: sample(9999, 2, available: false))
        XCTAssertEqual(ledger.read(next, sample: sample(20, 3))?.completedBytes, 30)
    }
    func testIncompleteClosingCounterBreaksRateContinuityWithoutBlockingSuccessor() throws {
        var ledger = PublishingTelemetryAggregate<ReconnectAttemptContext>(), rate = SocketOutboundRate()
        let old = context(1), next = context(2)
        ledger.opened(old)
        rate.update(ledger.read(old, sample: sample(100, 1_000_000_000)), nowNs: 1_000_000_000)
        ledger.closing(old); ledger.finished(old, sample: sample(150, 2_000_000_000, available: false, complete: false))
        ledger.opened(next)
        let first = try XCTUnwrap(ledger.read(next, sample: sample(20, 3_000_000_000)))
        XCTAssertEqual(first.continuity, 1)
        rate.update(first, nowNs: 3_000_000_000); XCTAssertNil(rate.kilobitsPerSecond)
        rate.update(ledger.read(next, sample: sample(120, 4_000_000_000)), nowNs: 4_000_000_000)
        XCTAssertEqual(rate.kilobitsPerSecond, 0.8)
    }
    func testAggregateCounterOrClockRollbackIsUnavailable() {
        var ledger = PublishingTelemetryAggregate<ReconnectAttemptContext>()
        let attempt = context(1); ledger.opened(attempt)
        _ = ledger.read(attempt, sample: sample(50, 10))
        XCTAssertEqual(ledger.read(attempt, sample: sample(49, 11))?.complete, false)
        var clock = PublishingTelemetryAggregate<Int>(); clock.opened(1)
        _ = clock.read(1, sample: sample(10, 10))
        XCTAssertEqual(clock.read(1, sample: sample(11, 9))?.complete, false)
    }
    func testFreshZeroRequiresTwoCompleteSamplesAndBlockedReadExpiresToUnknown() {
        var rate = SocketOutboundRate()
        rate.update(sample(10, 1_000_000_000), nowNs: 1_000_000_000); XCTAssertNil(rate.kilobitsPerSecond)
        rate.update(sample(10, 2_000_000_000), nowNs: 2_000_000_000); XCTAssertEqual(rate.kilobitsPerSecond, 0)
        rate.expire(nowNs: 5_000_000_001); XCTAssertNil(rate.kilobitsPerSecond)
        rate.update(sample(10, 6_000_000_000), nowNs: 6_000_000_000); XCTAssertNil(rate.kilobitsPerSecond)
        rate.update(sample(10, 7_000_000_000, available: false), nowNs: 7_000_000_000)
        XCTAssertNil(rate.kilobitsPerSecond) // End never leaves the historical number current.
    }
    func testActualElapsedNanosecondsAndBitsConvertToDecimalKilobits() {
        var rate = SocketOutboundRate()
        rate.update(sample(123, 1_000_000_000), nowNs: 1_000_000_000)
        rate.update(sample(250_123, 3_000_000_000), nowNs: 3_000_000_000)
        XCTAssertEqual(rate.kilobitsPerSecond, 1000)
        rate.update(sample(UInt64.max, 3_000_000_001), nowNs: 3_000_000_001)
        XCTAssertTrue(rate.kilobitsPerSecond?.isFinite == true) // No UInt64 bytes*8 overflow.
    }
    func testMissingIncompleteFutureAndOldSamplesDoNotBecomeZero() {
        for bad in [nil, sample(0, 1, complete: false), sample(0, 20), sample(0, 1)] as [SocketOutboundSample?] {
            var rate = SocketOutboundRate()
            let now: UInt64 = bad?.measuredAtNs == 20 ? 10 : 4_000_000_000
            rate.update(bad, nowNs: now); XCTAssertNil(rate.kilobitsPerSecond)
        }
    }
    func testOneOutstandingReadAndRetiredCallbackCannotReleaseSuccessorRead() throws {
        var reads = TelemetryReadAdmission<Int>()
        let old = try XCTUnwrap(reads.begin(1))
        for _ in 0..<20 { XCTAssertNil(reads.begin(1)) }
        reads.invalidate()
        let next = try XCTUnwrap(reads.begin(2))
        XCTAssertFalse(reads.complete(old)); XCTAssertNil(reads.begin(2))
        XCTAssertTrue(reads.complete(next)); XCTAssertFalse(reads.complete(next))
        XCTAssertNotNil(reads.begin(2))
    }
    func testRetryExhaustionWhileSendingDoesNotDeclareTerminalTime() {
        final class Policy: ReconnectPolicyPort {
            var generation: UInt64 = 1, attemptEpoch: UInt64 = 1
            var nextDeadlineNs: UInt64? { nil }; var exhausted = false
            var action = ReconnectAction.none
            func start(_ now: UInt64) throws -> ReconnectAction { .none }
            func replace(_ now: UInt64) throws -> ReconnectAction { .none }
            func connected(_ now: UInt64) throws {}
            func sending(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { .none }
            func recovered(_ epoch: UInt64, _ now: UInt64) throws -> ReconnectAction { .none }
            func confirmed(_ context: ReconnectAttemptContext, _ now: UInt64) throws {}
            func failed(_ now: UInt64) throws -> ReconnectAction { .none }
            func tick(_ now: UInt64) throws -> ReconnectAction { action }
            func stop() throws {}
        }
        let policy = Policy(); var now: UInt64 = 10
        let controller = ReconnectController(policy: policy, now: { now }, activeChanged: { _ in }, changed: {})
        policy.action = .exhausted; controller.tick(); XCTAssertNil(controller.terminalAtNs)
        policy.action = .expired; now = 20; controller.tick(); XCTAssertEqual(controller.terminalAtNs, 20)
        now = 30; controller.stop(); XCTAssertEqual(controller.terminalAtNs, 20)
    }
    #if canImport(Sapflow)
    private func until(_ predicate: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let limit = Date().addingTimeInterval(2)
        while !predicate() && Date() < limit { Thread.sleep(forTimeInterval: 0.001) }
        XCTAssertTrue(predicate(), file: file, line: line)
    }
    private func start(_ publisher: ReconnectingPublisher) throws {
        let video = try MediaCodec.h264(sps: Data([0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0]), pps: Data([0x68, 0xef, 0xbc, 0x80]))
        let audio = try MediaCodec.aac(asc: Data([0x11, 0x90]))
        try publisher.start(); try publisher.configure(video: video, audio: audio)
    }
    func testActualLogicalCloseFoldsFinalBeforeFreshRotationFactory() throws {
        let sinks = TelemetryFixtureBox<[TelemetryFixtureSink]>([])
        let publisher = try ReconnectingPublisher(source: .srtPeerAcknowledgedWatermark, contextualFactory: { _, observe in
            let sink = TelemetryFixtureSink(observe); sinks.change { $0.append(sink) }; return sink
        })
        defer { publisher.endDestination() }
        try start(publisher); until { sinks.value.count == 1 && publisher.status.hasPrefix("Sending") }
        let first = sinks.value[0]; first.holdClose.value = true
        let read = expectation(description: "first live counter")
        publisher.readSocketOutbound { value in XCTAssertEqual(value?.completedBytes, 100); read.fulfill() }
        until { first.reads.value == 1 }; first.answer(100)
        wait(for: [read], timeout: 2)
        let closed = expectation(description: "actual old close")
        publisher.closeCodecEpoch(); publisher.finishEpoch { result in
            if case .failure = result { XCTFail("Unexpected retained fixture") }; closed.fulfill()
        }
        until { first.closeCallback.value != nil }
        XCTAssertEqual(sinks.value.count, 1)
        first.finishClose(bytes: 150)
        wait(for: [closed], timeout: 2)
        try start(publisher); until { sinks.value.count == 2 && publisher.status.hasPrefix("Sending") }
        let second = sinks.value[1], next = expectation(description: "continuous counter")
        publisher.readSocketOutbound { value in XCTAssertEqual(value?.completedBytes, 175); next.fulfill() }
        until { second.reads.value == 1 }; second.answer(25)
        wait(for: [next], timeout: 2)
    }
    func testActualBlockedReadAndLateAnswerCannotSurviveDestinationEnd() throws {
        let sinks = TelemetryFixtureBox<[TelemetryFixtureSink]>([]), ends = TelemetryFixtureBox<[UInt64]>([])
        let publisher = try ReconnectingPublisher(source: .srtPeerAcknowledgedWatermark, contextualFactory: { _, observe in
            let sink = TelemetryFixtureSink(observe); sinks.change { $0.append(sink) }; return sink
        })
        publisher.observeBroadcastEnd { value in ends.change { $0.append(value) } }
        try start(publisher); until { sinks.value.count == 1 && publisher.status.hasPrefix("Sending") }
        let sink = sinks.value[0], first = expectation(description: "late read refused"), busy = expectation(description: "bounded busy read")
        let deliveries = TelemetryFixtureBox(0)
        publisher.readSocketOutbound { value in XCTAssertNil(value); deliveries.change { $0 += 1 }; first.fulfill() }
        until { sink.reads.value == 1 }
        publisher.readSocketOutbound { value in XCTAssertNil(value); busy.fulfill() }
        wait(for: [busy], timeout: 2); XCTAssertEqual(sink.reads.value, 1)
        publisher.endDestination(); publisher.endDestination()
        XCTAssertEqual(ends.value.count, 1)
        sink.answer(100); sink.answer(100) // Duplicate delivery cannot satisfy another query.
        wait(for: [first], timeout: 2)
        until { publisher.cleanupComplete }
        XCTAssertEqual(deliveries.value, 1)
    }
    func testActualRetryKeepsOriginalLogicalCounterUntilOldCleanupFinishes() throws {
        let sinks = TelemetryFixtureBox<[TelemetryFixtureSink]>([]), contexts = TelemetryFixtureBox<[ReconnectAttemptContext]>([])
        let publisher = try ReconnectingPublisher(source: .srtPeerAcknowledgedWatermark, contextualFactory: { context, observe in
            contexts.change { $0.append(context) }
            let sink = TelemetryFixtureSink(observe); sinks.change { $0.append(sink) }; return sink
        })
        defer { publisher.endDestination() }
        try start(publisher); until { sinks.value.count == 1 && publisher.status.hasPrefix("Sending") }
        let old = sinks.value[0]; old.holdClose.value = true
        old.observe(.failed, DispatchTime.now().uptimeNanoseconds)
        until { old.closeCallback.value != nil }
        XCTAssertEqual(sinks.value.count, 1)
        old.finishClose(bytes: 200)
        until { sinks.value.count == 2 && publisher.status.hasPrefix("Sending") }
        XCTAssertEqual(contexts.value[0].codecEpoch, contexts.value[1].codecEpoch)
        XCTAssertNotEqual(contexts.value[0].token, contexts.value[1].token)
        let read = expectation(description: "retry cumulative completed bytes"), next = sinks.value[1]
        publisher.readSocketOutbound { value in XCTAssertEqual(value?.completedBytes, 230); read.fulfill() }
        until { next.reads.value == 1 }; next.answer(30)
        wait(for: [read], timeout: 2)
    }
    #endif
}

#if canImport(Sapflow)
private final class TelemetryFixtureBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: T
    init(_ value: T) { stored = value }
    var value: T { get { lock.lock(); defer { lock.unlock() }; return stored } set { lock.lock(); stored = newValue; lock.unlock() } }
    func change(_ update: (inout T) -> Void) { lock.lock(); defer { lock.unlock() }; update(&stored) }
}
private final class TelemetryFixtureSink: EncodedPublishingSink {
    let observe: (PublishingAttemptEvent, UInt64) -> Void
    let reads = TelemetryFixtureBox(0), holdClose = TelemetryFixtureBox(false)
    let readCallback = TelemetryFixtureBox<((SocketOutboundSample?) -> Void)?>(nil)
    let closeCallback = TelemetryFixtureBox<(() -> Void)?>(nil)
    let final = TelemetryFixtureBox<SocketOutboundSample?>(nil)
    init(_ observe: @escaping (PublishingAttemptEvent, UInt64) -> Void) { self.observe = observe }
    var status: String { "Synthetic socket completion fixture" }
    var finalSocketOutbound: SocketOutboundSample? { final.value }
    func start() {}
    func configure(video: MediaCodec, audio: MediaCodec) { observe(.accepted, DispatchTime.now().uptimeNanoseconds); observe(.sending, DispatchTime.now().uptimeNanoseconds) }
    func offer(_ sample: MediaSample) {}
    func readSocketOutbound(completion: @escaping (SocketOutboundSample?) -> Void) { readCallback.value = completion; reads.change { $0 += 1 } }
    func answer(_ bytes: UInt64) { readCallback.value?(SocketOutboundSample(completedBytes: bytes, measuredAtNs: DispatchTime.now().uptimeNanoseconds, available: true, complete: true)) }
    func endDestination() {}
    func stop() {}
    func stop(completion: @escaping () -> Void) {
        closeCallback.value = completion
        if !holdClose.value { finishClose(bytes: 0) }
    }
    func finishClose(bytes: UInt64) {
        final.value = SocketOutboundSample(completedBytes: bytes, measuredAtNs: DispatchTime.now().uptimeNanoseconds, available: false, complete: true)
        let callback = closeCallback.value; closeCallback.value = nil; callback?()
    }
}
#endif
