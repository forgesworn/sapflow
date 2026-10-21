import XCTest
import CoreMedia
@testable import SapflowBroadcaster

private func originAssertFailure(_ result: Result<OriginDecodedMedia, OriginObservationFailure>, file: StaticString = #filePath, line: UInt = #line) {
    if case .success = result { XCTFail("Unexpected decoded confirmation", file: file, line: line) }
}
private func originAssertSuccess(_ result: Result<OriginDecodedMedia, OriginObservationFailure>, file: StaticString = #filePath, line: UInt = #line) {
    if case let .failure(error) = result { XCTFail("Unexpected observer failure: \(error)", file: file, line: line) }
}

private final class OriginTestBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: T
    init(_ value: T) { stored = value }
    var value: T { get { lock.withLock { stored } } set { lock.withLock { stored = newValue } } }
    func update(_ body: (inout T) -> Void) { lock.withLock { body(&stored) } }
}
private final class OriginTestClock {
    let base = DispatchTime.now().uptimeNanoseconds
    let offset = OriginTestBox<UInt64>(0)
    func now() -> V4vClock {
        let elapsed = DispatchTime.now().uptimeNanoseconds - base + offset.value
        return V4vClock(wallMs: V4vFixture.time + elapsed / 1_000_000, monotonicNs: base + elapsed)
    }
}
private enum OriginFixture {
    static let nonce = String(repeating: "c", count: 32)
    static func scope() throws -> V4vObservedAttempt {
        try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 2, token: 3, sdkGeneration: 4, sdkAttempt: 5), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), nonce: nonce)
    }
    static func receipt(part: UInt64 = 2, initialization: UInt64 = 0, epoch: UInt64 = 1, nonce: String = nonce) throws -> V4vSessionReceipt {
        let fresh = V4vFixture.edit(V4vFixture.fresh(part, initId: initialization), ["connectionEpoch": .integer(epoch)])
        let origin = V4vFixture.edit(V4vFixture.origin(state: "live"), ["connectionEpoch": .integer(epoch), "connectionAttemptId": .string(nonce), "freshMedia": fresh])
        return try V4vSessionReceipt(V4vFixture.session(origin: origin))
    }
    static func gate(_ clock: OriginTestClock) throws -> OriginConfirmationGate {
        let gate = OriginConfirmationGate()
        try gate.replace(OriginCurrentAuthority(attempt: scope(), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: clock.base + 60_000_000_000))
        return gate
    }
}

final class OriginObservationPolicyTests: XCTestCase {
    func testCapturedSDKIdentityRejectsZeroFields() {
        for index in 0..<5 {
            var fields: [UInt64] = [1, 2, 3, 4, 5]; fields[index] = 0
            XCTAssertThrowsError(try OriginAttemptIdentity(destination: fields[0], codecEpoch: fields[1], token: fields[2], sdkGeneration: fields[3], sdkAttempt: fields[4]))
        }
    }
    func testFreshNonceAndCanonicalWireDoNotChangeDurableGrant() throws {
        XCTAssertNotNil(try V4vObservedAttempt.randomNonce().range(of: #"^[0-9a-f]{32}\z"#, options: .regularExpression))
        let scope = try OriginFixture.scope(), grant = try V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time)
        let original = grant.streamId, wire = try scope.wireStreamID(grant: grant)
        XCTAssertTrue(wire.hasPrefix("#!::m=publish,r=live/" + V4vFixture.event))
        XCTAssertTrue(wire.hasSuffix("forgesworn_attempt=" + OriginFixture.nonce)); XCTAssertEqual(grant.streamId, original)
        XCTAssertFalse(String(reflecting: scope).contains(OriginFixture.nonce))
        let changed = try V4vGrant(V4vFixture.edit(grant.raw, ["limits": .object(["maxPublishBps": .integer(6_000_000), "maxQueueBytes": .integer(4_194_304)])]), nowMs: V4vFixture.time)
        XCTAssertThrowsError(try scope.wireStreamID(grant: changed))
    }
    func testFreshSelectionRequiresCurrentNonceAndBothTracks() throws {
        let attempt = try OriginFixture.scope()
        XCTAssertNoThrow(try OriginMediaSelection.match(OriginFixture.receipt(), attempt: attempt, nowMs: V4vFixture.time))
        XCTAssertThrowsError(try OriginMediaSelection.match(OriginFixture.receipt(nonce: String(repeating: "d", count: 32)), attempt: attempt, nowMs: V4vFixture.time))
        XCTAssertThrowsError(try OriginMediaSelection.match(OriginFixture.receipt(), attempt: attempt, nowMs: V4vFixture.time + 60_000))
        let raw = V4vFixture.edit(V4vFixture.origin(state: "live"), ["connectionAttemptId": .string(OriginFixture.nonce), "freshMedia": V4vFixture.edit(V4vFixture.fresh(), ["hasAudio": .bool(false)])])
        let receipt = try V4vSessionReceipt(V4vFixture.session(origin: raw))
        XCTAssertThrowsError(try OriginMediaSelection.match(receipt, attempt: attempt, nowMs: V4vFixture.time))
    }
    func testPostStatusMayAdvancePartsButCannotSwitchEpochOrInit() throws {
        let scope = try OriginFixture.scope()
        func selection(_ part: UInt64, _ initialization: UInt64 = 0, _ epoch: UInt64 = 1) throws -> OriginMediaSelection {
            try .match(OriginFixture.receipt(part: part, initialization: initialization, epoch: epoch), attempt: scope, nowMs: V4vFixture.time)
        }
        let before = try selection(2)
        XCTAssertTrue(before.stillCurrent(try selection(8)))
        XCTAssertFalse(before.stillCurrent(try selection(1)))
        XCTAssertFalse(before.stillCurrent(try selection(3, 1)))
        XCTAssertFalse(before.stillCurrent(try selection(3, 0, 2)))
    }
    func testPlaybackPrefixReplacesLiveDirectoryWithoutBearerOrQuery() throws {
        let chosen = try OriginMediaSelection.match(OriginFixture.receipt(), attempt: OriginFixture.scope(), nowMs: V4vFixture.time)
        for prefix in ["/live/", "/media/live/", "/origin/live/"] {
            let profile = try V4vConnectorProfile(apiOrigin: "https://api.example.invalid", playbackOrigin: "https://media.example.invalid", playbackPrefix: prefix)
            let resources = try OriginApprovedResources(profile: profile, media: chosen)
            XCTAssertEqual(resources.part.absoluteString, "https://media.example.invalid" + prefix + V4vFixture.event + "/abc_part2.mp4")
            let request = try resources.request(resources.part, timeout: 1)
            XCTAssertEqual(request.httpMethod, "GET"); XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
            XCTAssertNil(request.value(forHTTPHeaderField: "Cookie")); XCTAssertFalse(request.httpShouldHandleCookies)
            XCTAssertNil(request.url?.query); XCTAssertNil(request.httpBody)
            XCTAssertThrowsError(try resources.request(URL(string: "https://evil.example.invalid/x")!, timeout: 1))
        }
    }
    func testResponseAndAppendBoundsRejectRedirectsAndInflation() throws {
        var body = try OriginMediaBody(limit: 4); try body.append(Data([1, 2, 3, 4]))
        XCTAssertThrowsError(try body.append(Data([5]))); XCTAssertEqual(body.bytes.count, 4)
        XCTAssertThrowsError(try OriginMediaBody.response(status: 302, length: 1, mime: "video/mp4", limit: 4))
        XCTAssertThrowsError(try OriginMediaBody.response(status: 200, length: 5, mime: "video/mp4", limit: 4))
        XCTAssertThrowsError(try OriginMediaBody.response(status: 200, length: 1, mime: "text/html", limit: 4))
        XCTAssertNoThrow(try OriginMediaBody.response(status: 200, length: -1, mime: "application/mp4", limit: 4))
    }
    func testCoalescedPCMCountsNativeFramesAndExactTiming() throws {
        var pcm = OriginPCMProgress()
        try pcm.accept(frames: 7104, bytes: 14208, bytesPerFrame: 2, rate: 48000, first: .zero, last: CMTime(value: 7103, timescale: 48000), firstDuration: CMTime(value: 1, timescale: 48000), lastDuration: CMTime(value: 1, timescale: 48000))
        XCTAssertEqual(pcm.frames, 7104); XCTAssertEqual(pcm.outputs, 1)
        XCTAssertThrowsError(try pcm.accept(frames: 1024, bytes: 2048, bytesPerFrame: 2, rate: 48000, first: .zero, last: CMTime(value: 1023, timescale: 48000), firstDuration: CMTime(value: 1, timescale: 48000), lastDuration: CMTime(value: 1, timescale: 48000)))
    }
    func testInflatedPCMDurationAndBytesAreNotProgress() {
        for variant in 0..<3 {
            var pcm = OriginPCMProgress()
            XCTAssertThrowsError(try pcm.accept(frames: 2048, bytes: variant == 0 ? 2 : 4096, bytesPerFrame: 2, rate: 48000, first: .zero, last: CMTime(value: variant == 1 ? 4095 : 2047, timescale: 48000), firstDuration: CMTime(value: 1, timescale: 48000), lastDuration: CMTime(value: variant == 2 ? 2 : 1, timescale: 48000)))
        }
    }
    func testConfirmationUsesCapturedSDKFieldsAndRefusesFenceOrExpiry() throws {
        let clock = OriginTestClock(), gate = try OriginFixture.gate(clock), captured = try OriginFixture.scope()
        var applied: OriginAttemptIdentity?
        try gate.confirm(captured, beforeNs: .max, clock: { clock.base }) { value, _ in applied = value }
        XCTAssertEqual(applied?.sdkGeneration, 4); XCTAssertEqual(applied?.sdkAttempt, 5)
        XCTAssertThrowsError(try gate.confirm(captured, beforeNs: .max, clock: { clock.base + 60_000_000_000 }) { _, _ in XCTFail("Expired confirmation") })
        gate.invalidate()
        XCTAssertThrowsError(try gate.confirm(captured, beforeNs: .max, clock: { clock.base }) { _, _ in XCTFail("Fenced confirmation") })
    }
    func testHeldConfirmationGateResamplesTimeBeforeApplyingCapturedSDKProof() throws {
        let clock = OriginTestClock(), gate = try OriginFixture.gate(clock), captured = try OriginFixture.scope()
        let held = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0), waiting = DispatchSemaphore(value: 0)
        let instant = OriginTestBox(clock.base), applied = OriginTestBox(0)
        let holder = expectation(description: "holder released"), waiter = expectation(description: "expired waiter refused")
        DispatchQueue.global().async {
            do {
                try gate.confirm(captured, beforeNs: .max, clock: { clock.base }) { _, _ in
                    held.signal(); XCTAssertEqual(release.wait(timeout: .now() + 2), .success)
                }
            } catch { XCTFail("Holder failed: \(error)") }
            holder.fulfill()
        }
        XCTAssertEqual(held.wait(timeout: .now() + 1), .success)
        DispatchQueue.global().async {
            waiting.signal()
            do {
                try gate.confirm(captured, beforeNs: clock.base + 5_000_000_000, clock: { instant.value }) { _, _ in applied.update { $0 += 1 } }
                XCTFail("Proof passed its original observation/permit deadline")
            } catch { }
            waiter.fulfill()
        }
        XCTAssertEqual(waiting.wait(timeout: .now() + 1), .success)
        instant.value = clock.base + 5_000_000_000
        release.signal()
        wait(for: [holder, waiter], timeout: 2)
        XCTAssertEqual(applied.value, 0)
    }
    func testSuccessorCannotUseOldScopeOrReopenClosedGate() throws {
        let clock = OriginTestClock(), gate = try OriginFixture.gate(clock), old = try OriginFixture.scope()
        let next = try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 2, token: 4, sdkGeneration: 4, sdkAttempt: 6), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), nonce: String(repeating: "d", count: 32))
        let authority = try OriginCurrentAuthority(attempt: next, grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: clock.base + 60_000_000_000)
        try gate.replace(authority)
        XCTAssertThrowsError(try gate.confirm(old, beforeNs: .max, clock: { clock.base }) { _, _ in XCTFail("Old SDK scope") })
        gate.close(); XCTAssertThrowsError(try gate.replace(authority))
    }
    func testNativeDecoderRefusesEmptyInputAndCloseBeforeAdmission() {
        let decoder = OriginMediaDecoder(), callback = expectation(description: "empty")
        decoder.decode(Data(), deadlineNs: DispatchTime.now().uptimeNanoseconds + 1_000_000) { result in
            originAssertFailure(result); callback.fulfill()
        }
        wait(for: [callback], timeout: 1); XCTAssertTrue(decoder.cleanupComplete)
        decoder.close(); let closed = expectation(description: "closed")
        decoder.decode(Data([1]), deadlineNs: DispatchTime.now().uptimeNanoseconds + 1_000_000) { result in
            originAssertFailure(result); closed.fulfill()
        }
        wait(for: [closed], timeout: 1)
    }
    func testEngineIssuesOnlyFreshExactSessionPermitCappedByBearer() throws {
        final class Store: V4vJournalStore { var value: V4vJSON?; func load() -> V4vJSON? { value }; func save(_ next: V4vJSON) { value = next } }
        let store = Store(), engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: {})
        XCTAssertThrowsError(try engine.observationStatusRequest(clock: V4vFixture.clock()))
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 1, clock: V4vFixture.clock())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: V4vFixture.clock())
        let permit = try engine.observationStatusRequest(clock: V4vFixture.clock())
        XCTAssertEqual(permit.call.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)")
        XCTAssertEqual(permit.expiresAtNs, 1_000_000_000)
        XCTAssertFalse(String(reflecting: permit).contains("generated-fixture-bearer"))
        try engine.requestEnd(clock: V4vFixture.clock())
        XCTAssertThrowsError(try engine.observationStatusRequest(clock: V4vFixture.clock()))
    }
    func testLateOriginStatusAfterRefreshCannotFenceOrDiscardCurrentGrant() throws {
        final class Store: V4vJournalStore { var value: V4vJSON?; func load() -> V4vJSON? { value }; func save(_ next: V4vJSON) { value = next } }
        final class Control: OriginStatusControlExecuting {
            let issued = DispatchSemaphore(value: 0)
            let pending = OriginTestBox<((Result<V4vJSON, V4vFailure>) -> Void)?>(nil)
            let closed = OriginTestBox(false)
            var cleanupComplete: Bool { closed.value }
            func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) { pending.value = completion; issued.signal() }
            func close() { closed.value = true }
        }
        let fences = OriginTestBox(0), store = Store(), queue = DispatchQueue(label: "origin.status.refresh.regression")
        let engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: { fences.update { $0 += 1 } })
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: V4vFixture.clock())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: V4vFixture.clock())
        let client = Control(), done = expectation(description: "stale read refused")
        let status = try V4vOriginStatusHTTP(profile: V4vFixture.profile(), engine: engine, owner: queue, clock: { V4vFixture.clock() }, client: client)
        status.fresh(try OriginFixture.scope()) { answer in
            if case .success = answer { XCTFail("Old revision must not become observation authority") }
            done.fulfill()
        }
        XCTAssertEqual(client.issued.wait(timeout: .now() + 1), .success)
        try queue.sync {
            try engine.stageRefresh(key: String(repeating: "e", count: 32), clock: V4vFixture.clock())
            try engine.applyResponse(V4vFixture.answer(V4vFixture.session(2), grant: V4vFixture.grant(2)), for: XCTUnwrap(engine.pending), clock: V4vFixture.clock())
        }
        let reply = try XCTUnwrap(client.pending.value)
        reply(.success(V4vFixture.answer(grant: nil)))
        wait(for: [done], timeout: 1)
        try queue.sync { XCTAssertEqual(try engine.captureGrant(clock: V4vFixture.clock()).identity.revision, 2) }
        XCTAssertEqual(fences.value, 0)
        status.close(); XCTAssertTrue(status.cleanupComplete)
    }
}

private final class OriginFakeStatus: OriginSessionStatusFetching {
    let pending = OriginTestBox<[(Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void]>([])
    let closed = OriginTestBox(false)
    var cleanupComplete: Bool { closed.value }
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) { pending.update { $0.append(completion) } }
    func close() { closed.value = true }
    func reply(_ index: Int, _ receipt: V4vSessionReceipt, expiry: UInt64 = UInt64.max) { pending.value[index](.success(OriginStatusSnapshot(receipt: receipt, expiresAtNs: expiry))) }
}
private final class OriginFakeMedia: OriginMediaFetching {
    let closed = OriginTestBox(false)
    let calls = OriginTestBox(0)
    var cleanupComplete: Bool { closed.value }
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void) { calls.update { $0 += 1 }; completion(.success(Data([1]))) }
    func close() { closed.value = true }
}
private final class OriginFakeDecoder: OriginMediaDecoding {
    let callbacks = OriginTestBox<[(Result<OriginDecodedMedia, OriginObservationFailure>) -> Void]>([])
    let cleaned = OriginTestBox(true)
    var cleanupComplete: Bool { cleaned.value }
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) { callbacks.update { $0.append(completion) } }
    func close() {}
    func reply(_ result: Result<OriginDecodedMedia, OriginObservationFailure> = .success(OriginDecodedMedia(videoOutputs: 2, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: true))) { callbacks.value[0](result) }
}

final class OriginObservationPipelineTests: XCTestCase {
    private func until(_ predicate: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let end = Date().addingTimeInterval(1.5)
        while Date() < end && !predicate() { Thread.sleep(forTimeInterval: 0.001) }
        XCTAssertTrue(predicate(), file: file, line: line)
    }
    private final class Harness {
        let clock = OriginTestClock(), status = OriginFakeStatus(), media = OriginFakeMedia(), decoder = OriginFakeDecoder()
        let confirmations = OriginTestBox<[OriginAttemptIdentity]>([])
        let gate: OriginConfirmationGate
        let observer: V4vOriginObserver
        init() throws {
            gate = try OriginFixture.gate(clock)
            let values = confirmations
            observer = try V4vOriginObserver(attempt: OriginFixture.scope(), profile: V4vFixture.profile(), gate: gate, status: status, media: media, decoder: decoder, clock: clock.now, confirm: { value, _ in values.update { $0.append(value) } })
        }
    }
    func testMetadataDoesNotConfirmBeforeNativeDecodeAndFreshPostStatus() throws {
        let h = try Harness(), done = expectation(description: "confirmed")
        h.observer.start { result in originAssertSuccess(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; XCTAssertEqual(h.confirmations.value.count, 0)
        h.decoder.reply(); until { h.status.pending.value.count == 2 }; XCTAssertEqual(h.confirmations.value.count, 0)
        h.status.reply(1, try OriginFixture.receipt(part: 5))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 1)
        XCTAssertEqual(h.confirmations.value.first?.sdkAttempt, 5); XCTAssertTrue(h.observer.cleanupComplete)
    }
    func testStopWinsBeforeLateNativeCompletion() throws {
        let h = try Harness(), done = expectation(description: "cancelled")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.observer.cancel(); h.decoder.reply()
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testOldObserverCancellationCannotInvalidateSuccessorAuthority() throws {
        let h = try Harness(), done = expectation(description: "old observer cancelled")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }
        let successor = try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 2, token: 4, sdkGeneration: 4, sdkAttempt: 6), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), nonce: String(repeating: "d", count: 32))
        try h.gate.replace(OriginCurrentAuthority(attempt: successor, grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: h.clock.base + 60_000_000_000))
        h.observer.cancel()
        h.status.reply(0, try OriginFixture.receipt())
        wait(for: [done], timeout: 1)
        XCTAssertTrue(h.gate.permits(successor, nowNs: h.clock.now().monotonicNs))
        XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testTransportFenceWinsAfterDecodeBeforePostStatus() throws {
        let h = try Harness(), done = expectation(description: "fenced")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply(); until { h.status.pending.value.count == 2 }
        h.gate.invalidate(); h.status.reply(1, try OriginFixture.receipt(part: 5))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testPostDecodeEpochReplacementCannotConfirmOldMedia() throws {
        let h = try Harness(), done = expectation(description: "old epoch")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply(); until { h.status.pending.value.count == 2 }
        h.status.reply(1, try OriginFixture.receipt(part: 5, epoch: 2))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testExpiredOriginalBearerPermitCannotBeExtendedByLaterStatus() throws {
        let h = try Harness(), done = expectation(description: "expired permit")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt(), expiry: h.clock.base + 1_000_000_000)
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply(); until { h.status.pending.value.count == 2 }
        h.clock.offset.value = 2_000_000_000; h.status.reply(1, try OriginFixture.receipt(part: 5))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testNativeDecodeFailureLeavesUnconfirmed() throws {
        let h = try Harness(), done = expectation(description: "decode refused")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply(.failure(.decode))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.count, 0); XCTAssertEqual(h.status.pending.value.count, 1)
    }
    func testAvailabilityRetryKeepsOneFetchAndOneDecoder() throws {
        let h = try Harness(), done = expectation(description: "late availability")
        h.observer.start { result in originAssertSuccess(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.pending.value[0](.failure(.busy))
        until { h.status.pending.value.count == 2 }; h.status.reply(1, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply(); until { h.status.pending.value.count == 3 }
        h.status.reply(2, try OriginFixture.receipt(part: 5)); wait(for: [done], timeout: 1)
        XCTAssertEqual(h.media.calls.value, 1); XCTAssertEqual(h.decoder.callbacks.value.count, 1)
    }
    func testUncertainNativeCleanupNeverConfirmsOrClaimsReleased() throws {
        let h = try Harness(), done = expectation(description: "retained native owner")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.cleaned.value = false; h.decoder.reply(.failure(.deadline))
        wait(for: [done], timeout: 3); XCTAssertEqual(h.confirmations.value.count, 0); XCTAssertFalse(h.observer.cleanupComplete)
    }
}
