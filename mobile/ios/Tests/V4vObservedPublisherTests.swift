import XCTest
import Sapflow
@testable import SapflowBroadcaster

// Real NativeSession/ForAttempt orchestration, with generated codec descriptors
// and explicitly simulated transport/status/decode facts. No camera, files,
// sockets or physical decode evidence is supplied by these fixtures.
private final class PublisherBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: T
    init(_ value: T) { stored = value }
    var value: T { get { lock.withLock { stored } } set { lock.withLock { stored = newValue } } }
    func update(_ body: (inout T) -> Void) { lock.withLock { body(&stored) } }
}
private final class PublisherClock {
    let base = DispatchTime.now().uptimeNanoseconds
    func now() -> V4vClock {
        let now = DispatchTime.now().uptimeNanoseconds
        return V4vClock(wallMs: V4vFixture.time + (now - base) / 1_000_000, monotonicNs: now)
    }
}
private final class PublisherJournal: V4vJournalStore {
    var value: V4vJSON?
    func load() -> V4vJSON? { value }
    func save(_ next: V4vJSON) { value = next }
}
private final class PublisherTransport: EncodedPublishingSink {
    let observe: (PublishingAttemptEvent, UInt64) -> Void
    let closed = PublisherBox(false), delayed = PublisherBox(false)
    let completion = PublisherBox<(() -> Void)?>(nil)
    var status: String { "Generated transport fixture" }
    init(_ observe: @escaping (PublishingAttemptEvent, UInt64) -> Void) { self.observe = observe }
    func start() {}
    func configure(video: MediaCodec, audio: MediaCodec) throws { emit(.accepted); emit(.sending) }
    func offer(_ sample: MediaSample) {}
    func emit(_ event: PublishingAttemptEvent) { observe(event, DispatchTime.now().uptimeNanoseconds) }
    func endDestination() { closed.value = true }
    func stop() { endDestination() }
    func stop(completion: @escaping () -> Void) {
        endDestination()
        if delayed.value { self.completion.value = completion } else { completion() }
    }
    func release() { let callback = completion.value; completion.value = nil; callback?() }
}
private final class PublisherStatus: OriginSessionStatusFetching {
    let pending = PublisherBox<[(Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void]>([])
    private let closed = PublisherBox(false)
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { closed.value }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) { pending.update { $0.append(completion) } }
    func close() { closed.value = true; cleanup.complete() }
    func reply(_ index: Int, scope: V4vObservedAttempt, expiry: UInt64, part: UInt64 = 2) throws {
        let origin = V4vFixture.edit(V4vFixture.origin(state: "live"), ["connectionAttemptId": .string(scope.nonce), "freshMedia": V4vFixture.fresh(part)])
        let receipt = try V4vSessionReceipt(V4vFixture.session(origin: origin))
        pending.value[index](.success(OriginStatusSnapshot(receipt: receipt, expiresAtNs: expiry)))
    }
}
private final class PublisherMedia: OriginMediaFetching {
    private let closed = PublisherBox(false)
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { closed.value }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void) { completion(.success(Data([1]))) }
    func close() { closed.value = true; cleanup.complete() }
}
private final class PublisherDecoder: OriginMediaDecoding {
    let pending = PublisherBox<[(Result<OriginDecodedMedia, OriginObservationFailure>) -> Void]>([])
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { true }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) { pending.update { $0.append(completion) } }
    func close() { cleanup.complete() }
    func reply() { pending.value[0](.success(OriginDecodedMedia(videoOutputs: 2, pcmFrames: 2048, pcmOutputs: 1, hardwareVideo: true))) }
}
private final class PublisherObservation {
    let scope: V4vObservedAttempt
    let status = PublisherStatus(), media = PublisherMedia(), decoder = PublisherDecoder()
    let observer: V4vOriginObserver
    init(scope: V4vObservedAttempt, gate: OriginConfirmationGate, owner: DispatchQueue, clock: @escaping () -> V4vClock,
         confirm: @escaping (OriginAttemptIdentity, UInt64) throws -> Void) throws {
        self.scope = scope
        observer = try V4vOriginObserver(attempt: scope, profile: V4vFixture.profile(), gate: gate, status: status, media: media, decoder: decoder,
                                         owner: owner, clock: clock, confirm: confirm)
    }
}
private final class PublisherHarness {
    let owner = DispatchQueue(label: "publisher.actual-native-test")
    let clock = PublisherClock()
    let sinks = PublisherBox<[PublisherTransport]>([])
    let wireIDs = PublisherBox<[String]>([])
    let observations = PublisherBox<[PublisherObservation]>([])
    let engine: V4vSessionEngine
    let slot: V4vObservationSlot
    let publisher: V4vObservedPublisher
    init() throws {
        let owner = owner, clock = clock, sinks = sinks, wires = wireIDs, observations = observations
        engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: PublisherJournal(), clock: clock.now(), fencePublisher: {})
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: clock.now())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: clock.now())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: clock.now())
        slot = V4vObservationSlot(owner: owner, clock: clock.now)
        publisher = try V4vObservedPublisher(destination: 1, permit: engine.publishingGrant(clock: clock.now()), profile: V4vFixture.profile(), engine: engine,
            owner: owner, slot: slot, clock: clock.now, transport: { destination, limits, observe in
                XCTAssertEqual(limits.bandwidthBytesPerSecond, 625_000)
                XCTAssertTrue(destination.encrypted)
                wires.update { $0.append(destination.streamID) }
                let sink = PublisherTransport(observe); sinks.update { $0.append(sink) }; return sink
            }, observation: { scope, gate, confirm in
                let fixture = try! PublisherObservation(scope: scope, gate: gate, owner: owner, clock: clock.now, confirm: confirm)
                observations.update { $0.append(fixture) }; return fixture.observer
            })
    }
    func startEpoch() throws {
        // Existing generated SPS/PPS used by sapflow-client's descriptor tests.
        let video = try MediaCodec.h264(sps: Data([0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0]), pps: Data([0x68, 0xef, 0xbc, 0x80]))
        let audio = try MediaCodec.aac(asc: Data([0x11, 0x90]))
        try publisher.start(); try publisher.configure(video: video, audio: audio)
    }
    func stop() { publisher.endDestination(); for value in observations.value { value.observer.cancel() } }
}

final class V4vObservedPublisherTests: XCTestCase {
    func testPublishingStartsBeforeEarliestActivatedCodecDiscovery() throws {
        let h = try PublisherHarness(); defer { h.stop() }
        let ingress = RustEncodedIngress(); ingress.destination = h.publisher
        let worker = IngressWorker(ingress: ingress, failure: { _ in XCTFail("Unexpected publishing failure") }, statistics: { _, _ in })
        // Activation makes native callbacks eligible immediately. Discovering
        // both codecs here must configure the already started logical epoch.
        XCTAssertTrue(worker.startPublishing {
            do {
                try ingress.prepare(
                    video: .h264(sps: Data([0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0]), pps: Data([0x68, 0xef, 0xbc, 0x80])),
                    audio: .aac(asc: Data([0x11, 0x90])))
            } catch { XCTFail("Codec discovery failed") }
        })
        until { h.sinks.value.count == 1 && h.observations.value.count == 1 }
        XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        let closed = expectation(description: "actual epoch cleanup")
        worker.finish { result in
            if case .failure = result { XCTFail("Unexpected retained publisher") }
            closed.fulfill()
        }
        wait(for: [closed], timeout: 1)
    }
    func testEndedPublisherStartupClosesOnlyIngressAcrossRecordingEpochs() throws {
        let h = try PublisherHarness(); defer { h.stop() }
        h.publisher.endDestination()
        for _ in 0..<2 {
            let ingress = RustEncodedIngress(); ingress.destination = h.publisher
            let failed = expectation(description: "publishing failure reported once")
            let failures = PublisherBox(0)
            let worker = IngressWorker(ingress: ingress, failure: { _ in failures.update { $0 += 1 }; failed.fulfill() }, statistics: { _, _ in })
            XCTAssertFalse(worker.startPublishing())
            XCTAssertFalse(worker.startPublishing())
            wait(for: [failed], timeout: 1)
            XCTAssertEqual(failures.value, 1)
            let closed = expectation(description: "actual publisher epoch cleanup")
            worker.finish { result in
                if case .failure = result { XCTFail("Unexpected retained publisher") }
                closed.fulfill()
            }
            wait(for: [closed], timeout: 1)
        }
        h.owner.sync {}
        XCTAssertTrue(h.sinks.value.isEmpty)
        XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
    }
    func testPublisherRejectsAnotherProfileBeforeAllocatingTransportOrObserver() throws {
        let owner = DispatchQueue(label: "publisher.profile.binding"), clock = PublisherClock()
        let profile = try V4vFixture.profile()
        let engine = try V4vSessionEngine(profile: profile, store: PublisherJournal(), clock: clock.now(), fencePublisher: {})
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: clock.now())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: clock.now())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: clock.now())
        let other = try V4vConnectorProfile(apiOrigin: "https://another.example.invalid", playbackOrigin: profile.playbackOrigin, playbackPrefix: profile.playbackPrefix)
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        var transports = 0, observations = 0
        XCTAssertThrowsError(try V4vObservedPublisher(destination: 1, permit: engine.publishingGrant(clock: clock.now()), profile: other, engine: engine, owner: owner, slot: slot, clock: clock.now, transport: { _, _, observe in
            transports += 1; return PublisherTransport(observe)
        }, observation: { _, _, _ in observations += 1; throw OriginObservationFailure.scope }))
        XCTAssertEqual(transports, 0); XCTAssertEqual(observations, 0)
    }
    private func until(_ predicate: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let end = Date().addingTimeInterval(2)
        while Date() < end && !predicate() { Thread.sleep(forTimeInterval: 0.001) }
        XCTAssertTrue(predicate(), file: file, line: line)
    }
    func testRealNativePolicyConfirmsOnlyAfterDecodeAndFreshPostStatus() throws {
        let h = try PublisherHarness(); defer { h.stop() }
        try h.startEpoch(); until { h.observations.value.count == 1 }
        let observed = h.observations.value[0]
        until { observed.status.pending.value.count == 1 }
        XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        try observed.status.reply(0, scope: observed.scope, expiry: h.clock.base + 60_000_000_000)
        until { observed.decoder.pending.value.count == 1 }; XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        observed.decoder.reply(); until { observed.status.pending.value.count == 2 }; XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        try observed.status.reply(1, scope: observed.scope, expiry: h.clock.base + 60_000_000_000, part: 3)
        until { h.publisher.status.hasPrefix("Live") }
        h.sinks.value[0].emit(.peerMediaProgress); h.owner.sync {}
        XCTAssertTrue(h.publisher.status.hasPrefix("Live"))
        h.sinks.value[0].delayed.value = true
        h.publisher.closeCodecEpoch(); XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        let closed = expectation(description: "actual transport close")
        h.publisher.finishEpoch { result in if case .failure = result { XCTFail("Unexpected retained owner") }; closed.fulfill() }
        until { h.sinks.value[0].completion.value != nil }
        XCTAssertFalse(h.publisher.cleanupComplete); XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        h.sinks.value[0].emit(.peerMediaProgress); h.owner.sync {}
        XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        h.sinks.value[0].release(); wait(for: [closed], timeout: 1); h.owner.sync {}
    }
    func testHealthyRotationCapturesNewSDKAttemptAndNonceWithoutChangingGrant() throws {
        let h = try PublisherHarness(); defer { h.stop() }
        try h.startEpoch(); until { h.observations.value.count == 1 }
        let first = h.observations.value[0].scope
        h.publisher.closeCodecEpoch()
        let closed = expectation(description: "first epoch closed")
        h.publisher.finishEpoch { _ in closed.fulfill() }
        wait(for: [closed], timeout: 1); h.owner.sync {}
        try h.startEpoch(); until { h.observations.value.count == 2 }
        let second = h.observations.value[1].scope
        XCTAssertEqual(first.identity, second.identity)
        XCTAssertEqual(first.local.sdkGeneration, second.local.sdkGeneration)
        XCTAssertGreaterThan(second.local.sdkAttempt, first.local.sdkAttempt)
        XCTAssertGreaterThan(second.local.codecEpoch, first.local.codecEpoch)
        XCTAssertNotEqual(first.nonce, second.nonce)
        XCTAssertNotEqual(h.wireIDs.value[0], h.wireIDs.value[1])
        XCTAssertTrue(h.wireIDs.value[1].hasSuffix("forgesworn_attempt=" + second.nonce))
        XCTAssertTrue(try h.engine.captureGrant(clock: h.clock.now()).streamId.hasPrefix("publish:live/"))
    }
    func testSynchronousStopRefusesLateNativeProofOnActualSession() throws {
        let h = try PublisherHarness(); defer { h.stop() }
        try h.startEpoch(); until { h.observations.value.count == 1 }
        let observed = h.observations.value[0]
        until { observed.status.pending.value.count == 1 }
        try observed.status.reply(0, scope: observed.scope, expiry: h.clock.base + 60_000_000_000)
        until { observed.decoder.pending.value.count == 1 }
        h.publisher.endDestination(); XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        observed.decoder.reply(); h.owner.sync {}; h.owner.sync {}
        XCTAssertFalse(h.publisher.status.hasPrefix("Live"))
        XCTAssertEqual(observed.status.pending.value.count, 1)
    }
    func testAcknowledgedRefreshAndExpiryRaceCannotReviveOrKillRenewedLease() throws {
        let first = try V4vPublishingGrantPermit(grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: 60_000)
        let renewed = try V4vPublishingGrantPermit(grant: V4vGrant(V4vFixture.grant(2, expiry: V4vFixture.time + 120_000), nowMs: V4vFixture.time), expiresAtNs: 120_000)
        let live = try V4vPublishingLifetime(first, nowNs: 1)
        try live.committed(renewed, nowNs: 50_000)
        XCTAssertFalse(live.expire(nowNs: 60_000)); XCTAssertEqual(try live.current(nowNs: 80_000).grant.identity.revision, 2)
        let expired = try V4vPublishingLifetime(first, nowNs: 1)
        XCTAssertTrue(expired.expire(nowNs: 60_000)); XCTAssertThrowsError(try expired.committed(renewed, nowNs: 60_001))
        live.close(); XCTAssertThrowsError(try live.current(nowNs: 80_000))
    }
}
