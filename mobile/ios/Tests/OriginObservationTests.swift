import XCTest
import CoreMedia
import AudioToolbox
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
    static func scope(expectedMedia: OriginExpectedMedia = .audioVideo) throws -> V4vObservedAttempt {
        try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 2, token: 3, sdkGeneration: 4, sdkAttempt: 5), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), nonce: nonce, expectedMedia: expectedMedia)
    }
    static func receipt(part: UInt64 = 2, initialization: UInt64 = 0, epoch: UInt64 = 1, nonce: String = nonce, expectedMedia: OriginExpectedMedia = .audioVideo) throws -> V4vSessionReceipt {
        let fresh = V4vFixture.edit(V4vFixture.fresh(part, initId: initialization), ["connectionEpoch": .integer(epoch), "hasVideo": .bool(expectedMedia.hasVideo)])
        let origin = V4vFixture.edit(V4vFixture.origin(state: "live"), ["connectionEpoch": .integer(epoch), "connectionAttemptId": .string(nonce), "freshMedia": fresh])
        return try V4vSessionReceipt(V4vFixture.session(origin: origin))
    }
    static func gate(_ clock: OriginTestClock, expectedMedia: OriginExpectedMedia = .audioVideo) throws -> OriginConfirmationGate {
        let gate = OriginConfirmationGate()
        try gate.stage(OriginCurrentAuthority(attempt: scope(expectedMedia: expectedMedia), grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: clock.base + 60_000_000_000), clock: { clock.base })
        XCTAssertTrue(try gate.activate(scope(expectedMedia: expectedMedia), clock: { clock.base }))
        return gate
    }
}

final class OriginObservationPolicyTests: XCTestCase {
    func testInjectedSleepSizedAuthorityJumpRefusesHTTPAndDecoderBeforeNativeWork() throws {
        let base: UInt64 = 1_000_000, deadline = base + 3_000_000_000
        let afterSleep = base + 120_000_000_000
        let decoder = OriginMediaDecoder(now: { afterSleep })
        var decoded = false
        decoder.decode(Data([1]), expectedMedia: .audioOnly48kMono, deadlineNs: deadline) { result in
            decoded = true; originAssertFailure(result)
        }
        XCTAssertTrue(decoded); XCTAssertTrue(decoder.cleanupComplete); decoder.close()
        let selected = try OriginMediaSelection.match(OriginFixture.receipt(), attempt: OriginFixture.scope(), nowMs: V4vFixture.time)
        let resources = try OriginApprovedResources(profile: V4vFixture.profile(), media: selected)
        let http = OriginMediaHTTP(now: { afterSleep })
        let refused = expectation(description: "expired before creating a network task")
        let closed = expectation(description: "real URLSession invalidation")
        http.whenCleanupComplete { closed.fulfill() }
        http.fetch(resources, deadlineNs: deadline) { result in
            guard case .failure(.deadline) = result else { XCTFail("Suspended deadline must not extend"); refused.fulfill(); return }
            refused.fulfill()
        }
        wait(for: [refused], timeout: 2); http.close(); wait(for: [closed], timeout: 2)
    }
    func testAudioOnlyNativeDescriptionRefusesContradictoryProfileAndFormat() throws {
        for flags in [UInt32(0), 2] {
            XCTAssertNoThrow(try OriginAACDescription.requireAudioOnly(formatID: kAudioFormatMPEG4AAC, objectFlags: flags, rate: 48000, channels: 1, framesPerPacket: 1024))
        }
        for flags in [UInt32(1), 3, 4, 5, .max] {
            XCTAssertThrowsError(try OriginAACDescription.requireAudioOnly(formatID: kAudioFormatMPEG4AAC, objectFlags: flags, rate: 48000, channels: 1, framesPerPacket: 1024))
        }
        let malformed: [(UInt32, Double, UInt32, UInt32)] = [(kAudioFormatLinearPCM, 48000, 1, 1024), (kAudioFormatMPEG4AAC, 44100, 1, 1024), (kAudioFormatMPEG4AAC, 48000, 2, 1024), (kAudioFormatMPEG4AAC, 48000, 1, 960)]
        for (format, rate, channels, frames) in malformed {
            XCTAssertThrowsError(try OriginAACDescription.requireAudioOnly(formatID: format, objectFlags: 0, rate: rate, channels: channels, framesPerPacket: frames))
        }
    }
    func testAudioOnlySelectionRequiresExactCapturedExpectationAndNonce() throws {
        let audio = try OriginFixture.scope(expectedMedia: .audioOnly48kMono), video = try OriginFixture.scope()
        XCTAssertNotEqual(audio, video)
        XCTAssertNoThrow(try OriginMediaSelection.match(OriginFixture.receipt(expectedMedia: .audioOnly48kMono), attempt: audio, nowMs: V4vFixture.time))
        XCTAssertThrowsError(try OriginMediaSelection.match(OriginFixture.receipt(), attempt: audio, nowMs: V4vFixture.time))
        XCTAssertThrowsError(try OriginMediaSelection.match(OriginFixture.receipt(expectedMedia: .audioOnly48kMono), attempt: video, nowMs: V4vFixture.time))
        XCTAssertThrowsError(try OriginMediaSelection.match(OriginFixture.receipt(nonce: String(repeating: "d", count: 32), expectedMedia: .audioOnly48kMono), attempt: audio, nowMs: V4vFixture.time))
        let clock = OriginTestClock(), gate = try OriginFixture.gate(clock, expectedMedia: .audioOnly48kMono)
        XCTAssertThrowsError(try gate.confirm(video, beforeNs: .max, clock: { clock.base }) { _, _ in XCTFail("Topology changed under same local scope") })
        XCTAssertNoThrow(try gate.confirm(audio, beforeNs: .max, clock: { clock.base }) { _, _ in })
    }
    func testAudioOnlyProofCountsPCMWithoutFabricatingVideoOrHardware() {
        let audio = OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: false)
        XCTAssertTrue(audio.sufficient(for: .audioOnly48kMono)); XCTAssertFalse(audio.sufficient)
        XCTAssertFalse(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 2047, pcmOutputs: 1, hardwareVideo: false).sufficient(for: .audioOnly48kMono))
        XCTAssertFalse(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: true).sufficient(for: .audioOnly48kMono))
        XCTAssertFalse(OriginDecodedMedia(videoOutputs: 2, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: true).sufficient(for: .audioOnly48kMono))
        XCTAssertFalse(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 0, hardwareVideo: false).sufficient(for: .audioOnly48kMono))
    }
    func testStatusCompositionRejectsAnotherProfileBeforeAllocatingClient() throws {
        final class Store: V4vJournalStore { func load() -> V4vJSON? { nil }; func save(_ next: V4vJSON) {} }
        final class NeverClient: OriginStatusControlExecuting {
            var cleanupComplete: Bool { true }
            func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) { XCTFail("Mismatched profile executed a bearer request") }
            func close() {}
        }
        let profile = try V4vFixture.profile(), queue = DispatchQueue(label: "origin.profile.binding")
        let engine = try V4vSessionEngine(profile: profile, store: Store(), clock: V4vFixture.clock(), fencePublisher: {})
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: V4vFixture.clock())
        var clients = 0
        for candidate in [
            try V4vConnectorProfile(apiOrigin: "https://another.example.invalid", playbackOrigin: profile.playbackOrigin, playbackPrefix: profile.playbackPrefix),
            try V4vConnectorProfile(apiOrigin: profile.apiOrigin, playbackOrigin: "https://another.example.invalid", playbackPrefix: profile.playbackPrefix),
            try V4vConnectorProfile(apiOrigin: profile.apiOrigin, playbackOrigin: profile.playbackOrigin, playbackPrefix: "/another/live/")
        ] {
            XCTAssertThrowsError(try V4vOriginStatusHTTP(profile: candidate, engine: engine, owner: queue, clock: { V4vFixture.clock() }, makeClient: { _ in clients += 1; return NeverClient() }))
        }
        XCTAssertEqual(clients, 0)
        XCTAssertNoThrow(try engine.requireProfile(profile))
    }
    func testScopedSRTLimitsConvertBitsAndNeverEnlargeManualCaps() throws {
        let scoped = try SrtPublishingLimits.scoped(maxQueueBytes: 65_536, maxPublishBps: 4_000_007)
        XCTAssertEqual(scoped.queueBytes, 65_536); XCTAssertEqual(scoped.bandwidthBytesPerSecond, 500_000)
        XCTAssertEqual(try SrtPublishingLimits.scoped(maxQueueBytes: 4_194_304, maxPublishBps: .max), .manual)
        XCTAssertThrowsError(try SrtPublishingLimits.scoped(maxQueueBytes: 1499, maxPublishBps: 4_000_000))
        XCTAssertThrowsError(try SrtPublishingLimits.scoped(maxQueueBytes: 4_194_305, maxPublishBps: 4_000_000))
        XCTAssertThrowsError(try SrtPublishingLimits.scoped(maxQueueBytes: 1500, maxPublishBps: 3_999_999))
        let low = try SrtPublishingLimits.scoped(maxQueueBytes: 65_536, maxPublishBps: 1_000_007, captureProfile: .audioVideo(.sd480p30))
        XCTAssertEqual(low.bandwidthBytesPerSecond, 125_000); XCTAssertEqual(low.queueBytes, 65_536)
        XCTAssertThrowsError(try SrtPublishingLimits.scoped(maxQueueBytes: 65_536, maxPublishBps: 999_999, captureProfile: .audioVideo(.sd480p30)))
        XCTAssertThrowsError(try SrtPublishingLimits.scoped(maxQueueBytes: 65_536, maxPublishBps: 7_999_999, captureProfile: .audioVideo(.fullHD1080p30)))
        XCTAssertEqual(try SrtPublishingLimits.scoped(maxQueueBytes: 65_536, maxPublishBps: 8_000_007, captureProfile: .audioVideo(.fullHD1080p30)).bandwidthBytesPerSecond, 1_000_000)
    }
    func testPublishingExpiryUsesEffectiveClockAfterWallRollback() throws {
        final class Store: V4vJournalStore { var value: V4vJSON?; func load() -> V4vJSON? { value }; func save(_ next: V4vJSON) { value = next } }
        let store = Store(), fences = OriginTestBox(0)
        let engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: { fences.update { $0 += 1 } })
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: V4vFixture.clock())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: V4vFixture.clock())
        let permit = try engine.publishingGrant(clock: V4vFixture.clock(50_000, wall: 1))
        XCTAssertEqual(permit.expiresAtNs, 60_000_000_000)
        XCTAssertFalse(String(reflecting: permit).contains("generated-fixture-bearer"))
        XCTAssertThrowsError(try engine.publishingGrant(clock: V4vFixture.clock(60_000, wall: 1)))
        XCTAssertGreaterThan(fences.value, 0)
    }
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
        XCTAssertNoThrow(try OriginMediaBody.response(status: 200, length: 4, mime: "audio/mp4", limit: 4))
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
    private func pcmBlock(_ progress: inout OriginPCMProgress, count: Int = 1024, start: CMTime = .zero) throws {
        let frame = CMTime(value: 1, timescale: 48000)
        try progress.accept(frames: count, bytes: count * 2, bytesPerFrame: 2, rate: 48000,
            first: start, last: CMTimeAdd(start, CMTime(value: Int64(count - 1), timescale: 48000)),
            firstDuration: frame, lastDuration: frame)
    }
    func testAudioOnlyPCMRequiresExactRationalContiguityOrOneCoalescedExtent() throws {
        var split = OriginPCMProgress(requireContiguous: true)
        try pcmBlock(&split)
        // Equivalent rational times must compare equal without Double rounding.
        try pcmBlock(&split, start: CMTime(value: 2048, timescale: 96000))
        XCTAssertEqual(split.frames, 2048); XCTAssertEqual(split.outputs, 2)
        var coalesced = OriginPCMProgress(requireContiguous: true)
        try pcmBlock(&coalesced, count: 7104)
        XCTAssertEqual(coalesced.frames, 7104); XCTAssertEqual(coalesced.outputs, 1)
    }
    func testAudioOnlyPCMGapsOverlapsAndRepeatsCannotAccumulateProof() throws {
        // No one-frame quantisation allowance: native CMTime is rational.
        for nextStart: Int64 in [0, 1023, 1025, 1072, 48000] {
            var pcm = OriginPCMProgress(requireContiguous: true)
            try pcmBlock(&pcm)
            XCTAssertThrowsError(try pcmBlock(&pcm, start: CMTime(value: nextStart, timescale: 48000)))
            XCTAssertEqual(pcm.frames, 1024); XCTAssertEqual(pcm.outputs, 1)
            // A refused block must not advance or poison the accepted extent.
            try pcmBlock(&pcm, start: CMTime(value: 1024, timescale: 48000))
            XCTAssertEqual(pcm.frames, 2048)
        }
    }
    func testLegacyAVPCMRetainsItsNonoverlapOnlyContract() throws {
        var pcm = OriginPCMProgress()
        try pcmBlock(&pcm)
        try pcmBlock(&pcm, start: CMTime(value: 2048, timescale: 48000))
        XCTAssertEqual(pcm.frames, 2048)
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
        try gate.stage(authority, clock: { clock.base })
        XCTAssertTrue(gate.activate(next, clock: { clock.base }))
        XCTAssertThrowsError(try gate.confirm(old, beforeNs: .max, clock: { clock.base }) { _, _ in XCTFail("Old SDK scope") })
        gate.close(); XCTAssertThrowsError(try gate.stage(authority, clock: { clock.base }))
    }
    func testStagedAttemptNeedsStartupAndCannotActivateAfterFailure() throws {
        let clock = OriginTestClock(), gate = OriginConfirmationGate(), scope = try OriginFixture.scope()
        let authority = try OriginCurrentAuthority(attempt: scope, grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: clock.base + 60_000_000_000)
        try gate.stage(authority, clock: { clock.base })
        XCTAssertFalse(gate.permits(scope, nowNs: clock.base))
        gate.invalidate(attempt: scope)
        XCTAssertFalse(gate.activate(scope, clock: { clock.base }))
        XCTAssertThrowsError(try gate.stage(authority, clock: { clock.base }))
        XCTAssertThrowsError(try gate.confirm(scope, beforeNs: .max, clock: { clock.base }) { _, _ in XCTFail("Failed startup reopened") })
    }
    func testClosedCodecEpochRefusesQueuedStageButPreservesLaterEpoch() throws {
        let clock = OriginTestClock(), gate = OriginConfirmationGate(), old = try OriginFixture.scope()
        let grant = try V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time)
        gate.closeEpoch(2)
        XCTAssertThrowsError(try gate.stage(OriginCurrentAuthority(attempt: old, grant: grant, expiresAtNs: clock.base + 60_000_000_000), clock: { clock.base }))
        let next = try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 3, token: 4, sdkGeneration: 4, sdkAttempt: 6), grant: grant, nonce: String(repeating: "d", count: 32))
        try gate.stage(OriginCurrentAuthority(attempt: next, grant: grant, expiresAtNs: clock.base + 60_000_000_000), clock: { clock.base })
        XCTAssertTrue(gate.activate(next, clock: { clock.base }))
        gate.closeEpoch(2)
        XCTAssertTrue(gate.permits(next, nowNs: clock.base))
    }
    func testGrantRefreshPreservesStagingAndCannotReviveInvalidatedAttempt() throws {
        let clock = OriginTestClock(), gate = OriginConfirmationGate(), scope = try OriginFixture.scope()
        let original = try OriginCurrentAuthority(attempt: scope, grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: clock.base + 60_000_000_000)
        let newer = try OriginCurrentAuthority(attempt: scope, grant: V4vGrant(V4vFixture.grant(2, expiry: V4vFixture.time + 120_000), nowMs: V4vFixture.time), expiresAtNs: clock.base + 120_000_000_000)
        try gate.stage(original, clock: { clock.base })
        try gate.refresh(newer, clock: { clock.base })
        XCTAssertFalse(gate.permits(scope, nowNs: clock.base))
        XCTAssertTrue(gate.activate(scope, clock: { clock.base }))
        XCTAssertTrue(gate.permits(scope, nowNs: clock.base + 70_000_000_000))
        gate.invalidate(attempt: scope)
        XCTAssertThrowsError(try gate.refresh(newer, clock: { clock.base }))
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
            var onClosed: (() -> Void)?
            var cleanupComplete: Bool { closed.value }
            func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) { pending.value = completion; issued.signal() }
            func close() { closed.value = true; onClosed?() }
        }
        let fences = OriginTestBox(0), store = Store(), queue = DispatchQueue(label: "origin.status.refresh.regression")
        let engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: { fences.update { $0 += 1 } })
        try engine.installAuthenticatedSession(pubkey: String(repeating: "a", count: 64), token: "generated-fixture-bearer", maxAgeSeconds: 60, clock: V4vFixture.clock())
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        try engine.applyResponse(V4vFixture.answer(), for: XCTUnwrap(engine.pending), clock: V4vFixture.clock())
        let client = Control(), done = expectation(description: "stale read refused")
        let status = try V4vOriginStatusHTTP(profile: V4vFixture.profile(), engine: engine, owner: queue, clock: { V4vFixture.clock() }, makeClient: { client.onClosed = $0; return client })
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
    var closesImmediately = true
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { closed.value }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) { pending.update { $0.append(completion) } }
    func close() { if closesImmediately { completeCleanup() } }
    func completeCleanup() { closed.value = true; cleanup.complete() }
    func reply(_ index: Int, _ receipt: V4vSessionReceipt, expiry: UInt64 = UInt64.max) { pending.value[index](.success(OriginStatusSnapshot(receipt: receipt, expiresAtNs: expiry))) }
}
private final class OriginFakeMedia: OriginMediaFetching {
    let closed = OriginTestBox(false)
    let calls = OriginTestBox(0)
    var closesImmediately = true
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { closed.value }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void) { calls.update { $0 += 1 }; completion(.success(Data([1]))) }
    func close() { if closesImmediately { completeCleanup() } }
    func completeCleanup() { closed.value = true; cleanup.complete() }
}
private final class OriginFakeDecoder: OriginMediaDecoding {
    let callbacks = OriginTestBox<[(Result<OriginDecodedMedia, OriginObservationFailure>) -> Void]>([])
    let cleaned = OriginTestBox(true)
    let expectations = OriginTestBox<[OriginExpectedMedia]>([])
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { cleaned.value }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) { callbacks.update { $0.append(completion) } }
    func decode(_ data: Data, expectedMedia: OriginExpectedMedia, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) {
        expectations.update { $0.append(expectedMedia) }; callbacks.update { $0.append(completion) }
    }
    func close() { if cleaned.value { cleanup.complete() } }
    func completeCleanup() { cleaned.value = true; cleanup.complete() }
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
        init(expectedMedia: OriginExpectedMedia = .audioVideo) throws {
            gate = try OriginFixture.gate(clock, expectedMedia: expectedMedia)
            let values = confirmations
            observer = try V4vOriginObserver(attempt: OriginFixture.scope(expectedMedia: expectedMedia), profile: V4vFixture.profile(), gate: gate, status: status, media: media, decoder: decoder, clock: clock.now, confirm: { value, _ in values.update { $0.append(value) } })
        }
    }
    func testAudioOnlyRequiresActualPCMAndFreshMatchingPostStatus() throws {
        let h = try Harness(expectedMedia: .audioOnly48kMono), done = expectation(description: "PCM confirmed")
        h.observer.start { result in originAssertSuccess(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt(expectedMedia: .audioOnly48kMono))
        until { h.decoder.callbacks.value.count == 1 }
        XCTAssertEqual(h.decoder.expectations.value, [.audioOnly48kMono]); XCTAssertTrue(h.confirmations.value.isEmpty)
        h.decoder.reply(.success(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: false)))
        until { h.status.pending.value.count == 2 }; XCTAssertTrue(h.confirmations.value.isEmpty)
        h.status.reply(1, try OriginFixture.receipt(part: 5, expectedMedia: .audioOnly48kMono))
        wait(for: [done], timeout: 1); XCTAssertEqual(h.confirmations.value.first?.sdkAttempt, 5)
        XCTAssertEqual(h.confirmations.value.count, 1); XCTAssertTrue(h.observer.cleanupComplete)
    }
    func testAudioOnlyRejectsTopologyOrNonceChangeAfterPCM() throws {
        for topologyChange in [false, true] {
            let h = try Harness(expectedMedia: .audioOnly48kMono), done = expectation(description: "changed media refused")
            h.observer.start { result in originAssertFailure(result); done.fulfill() }
            until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt(expectedMedia: .audioOnly48kMono))
            until { h.decoder.callbacks.value.count == 1 }
            h.decoder.reply(.success(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: false)))
            until { h.status.pending.value.count == 2 }
            h.status.reply(1, try OriginFixture.receipt(part: 5, nonce: topologyChange ? OriginFixture.nonce : String(repeating: "d", count: 32), expectedMedia: topologyChange ? .audioVideo : .audioOnly48kMono))
            wait(for: [done], timeout: 1); XCTAssertTrue(h.confirmations.value.isEmpty)
        }
    }
    func testAudioOnlyStopAndExpiryRefuseLatePCM() throws {
        for stop in [true, false] {
            let h = try Harness(expectedMedia: .audioOnly48kMono), done = expectation(description: "cold fence refused PCM")
            h.observer.start { result in originAssertFailure(result); done.fulfill() }
            until { h.status.pending.value.count == 1 }
            h.status.reply(0, try OriginFixture.receipt(expectedMedia: .audioOnly48kMono), expiry: h.clock.base + 1_000_000_000)
            until { h.decoder.callbacks.value.count == 1 }
            if stop { h.observer.cancel() } else { h.clock.offset.value = 2_000_000_000 }
            h.decoder.reply(.success(OriginDecodedMedia(videoOutputs: 0, pcmFrames: 7104, pcmOutputs: 1, hardwareVideo: false)))
            wait(for: [done], timeout: 1); XCTAssertTrue(h.confirmations.value.isEmpty)
            XCTAssertEqual(h.status.pending.value.count, 1)
        }
    }
    func testAudioOnlyCannotUseVideoProofAsFallback() throws {
        let h = try Harness(expectedMedia: .audioOnly48kMono), done = expectation(description: "AV proof refused")
        h.observer.start { result in originAssertFailure(result); done.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt(expectedMedia: .audioOnly48kMono))
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply()
        wait(for: [done], timeout: 1); XCTAssertTrue(h.confirmations.value.isEmpty)
        XCTAssertEqual(h.status.pending.value.count, 1)
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
        try h.gate.stage(OriginCurrentAuthority(attempt: successor, grant: V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time), expiresAtNs: h.clock.base + 60_000_000_000), clock: { h.clock.base })
        XCTAssertTrue(h.gate.activate(successor, clock: { h.clock.base }))
        h.observer.cancel()
        h.status.reply(0, try OriginFixture.receipt())
        wait(for: [done], timeout: 1)
        XCTAssertTrue(h.gate.permits(successor, nowNs: h.clock.now().monotonicNs))
        XCTAssertEqual(h.confirmations.value.count, 0)
    }
    func testQueuedObservationCannotRestartAnExpiredOriginalDeadline() throws {
        let h = try Harness(), done = expectation(description: "original deadline expired")
        h.observer.start(deadlineNs: h.clock.base) { result in originAssertFailure(result); done.fulfill() }
        wait(for: [done], timeout: 1)
        XCTAssertTrue(h.status.pending.value.isEmpty)
        XCTAssertTrue(h.confirmations.value.isEmpty)
        XCTAssertTrue(h.observer.cleanupComplete)
    }
    func testActualLateCleanupNotifiesRetainedSlotWithoutAdoptingFailedProof() throws {
        let h = try Harness(), failed = expectation(description: "cleanup failure reported"), cleaned = expectation(description: "actual late cleanup")
        let reports = OriginTestBox(0)
        h.decoder.cleaned.value = false
        h.observer.whenCleanupComplete { cleaned.fulfill() }
        h.observer.start { result in originAssertFailure(result); reports.update { $0 += 1 }; failed.fulfill() }
        until { h.status.pending.value.count == 1 }; h.status.reply(0, try OriginFixture.receipt())
        until { h.decoder.callbacks.value.count == 1 }; h.decoder.reply()
        wait(for: [failed], timeout: 3)
        XCTAssertFalse(h.observer.cleanupComplete)
        XCTAssertEqual(h.confirmations.value.count, 0)
        h.decoder.completeCleanup()
        wait(for: [cleaned], timeout: 1)
        XCTAssertTrue(h.observer.cleanupComplete)
        XCTAssertEqual(reports.value, 1)
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

private final class OriginSlotCandidate {
    let scope: V4vObservedAttempt
    let gate: OriginConfirmationGate
    let clock: OriginTestClock
    let status = OriginFakeStatus(), media = OriginFakeMedia(), decoder = OriginFakeDecoder()
    let builds = OriginTestBox(0)
    let results = OriginTestBox<[Result<OriginDecodedMedia, OriginObservationFailure>]>([])
    let confirmations = OriginTestBox(0)
    init(token: UInt64, gate: OriginConfirmationGate, clock: OriginTestClock, destination: UInt64 = 1) throws {
        self.gate = gate; self.clock = clock
        let grant = try V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time)
        scope = try V4vObservedAttempt(local: OriginAttemptIdentity(destination: destination, codecEpoch: 2, token: token, sdkGeneration: 4, sdkAttempt: token + 2), grant: grant, nonce: String(format: "%032llx", token))
        try gate.stage(OriginCurrentAuthority(attempt: scope, grant: grant, expiresAtNs: clock.base + 60_000_000_000), clock: { clock.now().monotonicNs })
        XCTAssertTrue(gate.activate(scope, clock: { clock.now().monotonicNs }))
    }
    func request(owner: DispatchQueue, deadlineNs: UInt64? = nil, refuseFactory: Bool = false) -> V4vObservationRequest {
        V4vObservationRequest(attempt: scope, gate: gate, deadlineNs: deadlineNs ?? clock.base + 8_000_000_000, make: {
            self.builds.update { $0 += 1 }
            if refuseFactory { throw OriginObservationFailure.unavailable }
            return try V4vOriginObserver(attempt: self.scope, profile: V4vFixture.profile(), gate: self.gate,
                status: self.status, media: self.media, decoder: self.decoder, owner: owner, clock: self.clock.now,
                confirm: { _, _ in self.confirmations.update { $0 += 1 } })
        }, completed: { result in self.results.update { $0.append(result) } })
    }
}

final class OriginObservationSlotTests: XCTestCase {
    private func until(_ predicate: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let end = Date().addingTimeInterval(1.5)
        while Date() < end && !predicate() { Thread.sleep(forTimeInterval: 0.001) }
        XCTAssertTrue(predicate(), file: file, line: line)
    }
    private func decodeStarted(_ candidate: OriginSlotCandidate, slot: V4vObservationSlot, owner: DispatchQueue) throws {
        owner.sync { slot.submit(candidate.request(owner: owner)) }
        until { candidate.status.pending.value.count == 1 }
        candidate.status.reply(0, try OriginFixture.receipt(nonce: candidate.scope.nonce))
        until { candidate.decoder.callbacks.value.count == 1 }
    }
    func testCloseOfCleanSlotIsImmediateAndPermanentlyRejectsRequests() throws {
        let owner = DispatchQueue(label: "origin.slot.close.clean"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let calls = OriginTestBox(0), candidate = try OriginSlotCandidate(token: 3, gate: gate, clock: clock)
        owner.sync {
            slot.close { calls.update { $0 += 1 } }
            XCTAssertEqual(calls.value, 1)
            slot.close { calls.update { $0 += 1 } }
            slot.submit(candidate.request(owner: owner))
        }
        XCTAssertEqual(calls.value, 2)
        XCTAssertEqual(candidate.builds.value, 0)
        XCTAssertEqual(candidate.results.value.count, 1)
        XCTAssertFalse(gate.permits(candidate.scope, nowNs: clock.base))
    }
    func testCloseWaitsForNativeAndBothLateIOOwnersAndRejectsSuccessor() throws {
        let owner = DispatchQueue(label: "origin.slot.close.owners"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let old = try OriginSlotCandidate(token: 3, gate: gate, clock: clock)
        old.decoder.cleaned.value = false
        old.status.closesImmediately = false; old.media.closesImmediately = false
        try decodeStarted(old, slot: slot, owner: owner)
        let next = try OriginSlotCandidate(token: 4, gate: gate, clock: clock)
        let calls = OriginTestBox(0), closed = expectation(description: "all actual owners closed")
        owner.sync {
            slot.submit(next.request(owner: owner))
            slot.close { calls.update { $0 += 1 }; closed.fulfill() }
            slot.close { calls.update { $0 += 100 } }
        }
        XCTAssertEqual(next.results.value.count, 1); XCTAssertEqual(next.builds.value, 0)
        old.decoder.reply(); old.status.reply(0, try OriginFixture.receipt())
        old.decoder.completeCleanup(); owner.sync {}
        XCTAssertEqual(calls.value, 0)
        old.status.completeCleanup(); owner.sync {}
        XCTAssertEqual(calls.value, 0)
        old.media.completeCleanup()
        wait(for: [closed], timeout: 1)
        XCTAssertEqual(calls.value, 1); XCTAssertEqual(old.confirmations.value, 0)
        XCTAssertEqual(old.results.value.count, 1)
        owner.sync { slot.submit(next.request(owner: owner)); slot.close { calls.update { $0 += 1 } } }
        XCTAssertEqual(next.builds.value, 0); XCTAssertEqual(calls.value, 2)
    }
    func testSemanticCleanupTimeoutDoesNotCompleteCloseOrReleaseSlot() throws {
        let owner = DispatchQueue(label: "origin.slot.close.retention"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        var slot: V4vObservationSlot? = V4vObservationSlot(owner: owner, clock: clock.now)
        weak var retained = slot
        let candidate = try OriginSlotCandidate(token: 3, gate: gate, clock: clock)
        candidate.decoder.cleaned.value = false
        try decodeStarted(candidate, slot: slot!, owner: owner)
        let calls = OriginTestBox(0), closed = expectation(description: "late actual cleanup")
        owner.sync { slot!.close { calls.update { $0 += 1 }; closed.fulfill() } }
        slot = nil
        until { candidate.status.closed.value }
        let semanticTimeout = expectation(description: "semantic cleanup failure")
        owner.asyncAfter(deadline: .now() + 2.1) { semanticTimeout.fulfill() }
        wait(for: [semanticTimeout], timeout: 3)
        XCTAssertEqual(candidate.results.value.count, 1)
        if case .failure(.cleanup) = candidate.results.value.first {} else { XCTFail("Expected cleanup failure") }
        XCTAssertEqual(calls.value, 0); XCTAssertNotNil(retained)
        candidate.decoder.reply(); candidate.decoder.completeCleanup()
        wait(for: [closed], timeout: 1)
        until { retained == nil }
        XCTAssertEqual(calls.value, 1); XCTAssertEqual(candidate.confirmations.value, 0)
    }
    func testCoalescedLatestSuccessorWaitsForActualCleanupAfterFailure() throws {
        let owner = DispatchQueue(label: "origin.slot.coalesce"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let old = try OriginSlotCandidate(token: 3, gate: gate, clock: clock); old.decoder.cleaned.value = false
        try decodeStarted(old, slot: slot, owner: owner)
        old.decoder.reply(.failure(.decode))
        let skipped = try OriginSlotCandidate(token: 4, gate: gate, clock: clock)
        owner.sync { slot.submit(skipped.request(owner: owner)) }
        let latest = try OriginSlotCandidate(token: 5, gate: gate, clock: clock)
        owner.sync { slot.submit(latest.request(owner: owner)) }
        let failure = expectation(description: "semantic failure before actual cleanup")
        owner.asyncAfter(deadline: .now() + 2.1) { failure.fulfill() }
        wait(for: [failure], timeout: 3)
        XCTAssertEqual(old.results.value.count, 1)
        XCTAssertEqual(skipped.builds.value, 0); XCTAssertEqual(latest.builds.value, 0)
        old.decoder.completeCleanup()
        until { latest.builds.value == 1 }
        XCTAssertEqual(skipped.builds.value, 0); XCTAssertEqual(old.confirmations.value, 0)
        gate.close(); owner.sync { slot.cancelDestination(1) }
        until { latest.results.value.count == 1 }
    }
    func testOriginalPendingDeadlineCannotRestartAfterOldNativeCleanup() throws {
        let owner = DispatchQueue(label: "origin.slot.expiry"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let old = try OriginSlotCandidate(token: 3, gate: gate, clock: clock); old.decoder.cleaned.value = false
        try decodeStarted(old, slot: slot, owner: owner)
        let next = try OriginSlotCandidate(token: 4, gate: gate, clock: clock)
        owner.sync { slot.submit(next.request(owner: owner)) }
        clock.offset.value = 9_000_000_000; old.decoder.completeCleanup()
        until { next.results.value.count == 1 }
        XCTAssertEqual(next.builds.value, 0)
        if case let .failure(reason) = next.results.value[0] { XCTAssertEqual(reason, .deadline) } else { XCTFail("Expired pending probe succeeded") }
        XCTAssertEqual(old.confirmations.value, 0); gate.close()
    }
    func testDestinationEndRemovesItsPendingScopeBeforeLateCleanup() throws {
        let owner = DispatchQueue(label: "origin.slot.end"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let old = try OriginSlotCandidate(token: 3, gate: gate, clock: clock); old.decoder.cleaned.value = false
        try decodeStarted(old, slot: slot, owner: owner)
        let next = try OriginSlotCandidate(token: 4, gate: gate, clock: clock)
        owner.sync { slot.submit(next.request(owner: owner)); slot.cancelDestination(1) }
        gate.close(); old.decoder.completeCleanup()
        until { old.results.value.count == 1 }
        XCTAssertEqual(next.results.value.count, 1); XCTAssertEqual(next.builds.value, 0)
    }
    func testThrowingFactoryDoesNotOccupySlotOrPreventExplicitSuccessor() throws {
        let owner = DispatchQueue(label: "origin.slot.factory"), clock = OriginTestClock(), gate = OriginConfirmationGate()
        let slot = V4vObservationSlot(owner: owner, clock: clock.now)
        let refused = try OriginSlotCandidate(token: 3, gate: gate, clock: clock)
        owner.sync { slot.submit(refused.request(owner: owner, refuseFactory: true)) }
        XCTAssertEqual(refused.results.value.count, 1)
        let next = try OriginSlotCandidate(token: 4, gate: gate, clock: clock)
        owner.sync { slot.submit(next.request(owner: owner)) }
        until { next.status.pending.value.count == 1 }; XCTAssertEqual(next.builds.value, 1)
        gate.close(); owner.sync { slot.cancelDestination(1) }
        until { next.results.value.count == 1 }
    }
    func testActualCleanupNotificationSupportsOneLateListener() {
        let signal = OriginCleanupNotification(), calls = OriginTestBox(0)
        signal.complete(); signal.observe { calls.update { $0 += 1 } }
        signal.complete(); signal.observe { calls.update { $0 += 10 } }
        XCTAssertEqual(calls.value, 1)
    }
}
