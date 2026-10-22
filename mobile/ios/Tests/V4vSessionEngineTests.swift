import XCTest
@testable import SapflowBroadcaster

// Generated wire fixtures following V4V native-broadcaster-sessions.md, not
// captured account, camera, phone or server data. TEST-NET address is not used.
enum V4vFixture {
    static let time: UInt64 = 1_000_000_000
    static let event = "event-foundation"
    static let sid = String(repeating: "a", count: 32)
    static let key = String(repeating: "b", count: 32)
    static func clock(_ elapsed: UInt64 = 0, wall: UInt64? = nil) -> V4vClock { V4vClock(wallMs: wall ?? time + elapsed, monotonicNs: elapsed * 1_000_000) }
    static func profile() throws -> V4vConnectorProfile { try V4vConnectorProfile(apiOrigin: "https://api.example.invalid", playbackOrigin: "https://media.example.invalid", playbackPrefix: "/origin/") }
    static func identity(_ revision: UInt64 = 1, generation: UInt64 = 1, sid: String = sid) -> [String: V4vJSON] {
        ["contractVersion": .integer(1), "eventId": .string(event), "eventGeneration": .integer(generation), "sessionId": .string(sid), "revision": .integer(revision)]
    }
    static func origin(_ revision: UInt64 = 1, state: String = "prepared", recording: String = "never-started", generation: UInt64 = 1, sid: String = sid) -> V4vJSON {
        var f = identity(revision, generation: generation, sid: sid)
        f.merge(["expiresAtUnixMs": .integer(time + 60_000), "connectionEpoch": .integer(state == "live" ? 1 : 0), "state": .string(state), "admission": .string(state == "revoked" ? "denied" : "open"), "protocol": .string(state == "live" ? "active" : "none"), "recording": .string(recording), "callbacks": .string("disabled")]) { _, b in b }
        return .object(f)
    }
    static func session(_ revision: UInt64 = 1, phase: String = "active", expiry: UInt64 = time + 60_000, origin: V4vJSON? = nil, complete: Bool = false, generation: UInt64 = 1, sid: String = sid) -> V4vJSON {
        var f = identity(revision, generation: generation, sid: sid)
        f.merge(["expiresAtUnixMs": .integer(expiry), "refreshAfterUnixMs": .integer(expiry - 10_000), "reconnectGraceMs": .integer(90_000), "phase": .string(phase), "origin": origin ?? edit(Self.origin(revision, generation: generation, sid: sid), ["expiresAtUnixMs": .integer(expiry)]), "endComplete": .bool(complete)]) { _, b in b }
        return .object(f)
    }
    static func grant(_ revision: UInt64 = 1, expiry: UInt64 = time + 60_000, generation: UInt64 = 1, sid: String = sid) -> V4vJSON {
        var f = identity(revision, generation: generation, sid: sid)
        f.merge(["destination": .object(["kind": .string("v4v"), "connector": .string("sapflow"), "unlimited": .bool(true), "watermark": .bool(false), "subscriptionRequired": .bool(false)]),
                 "transport": .string("srt"), "endpoint": .string("srt://192.0.2.10:9000"), "streamId": .string("publish:live/\(event):\(sid):" + String(repeating: "P", count: 43)),
                 "encryption": .object(["cipher": .string("aes256-ctr"), "passphrase": .string(String(repeating: "Q", count: 43))]),
                 "expiresAtUnixMs": .integer(expiry), "refreshAfterUnixMs": .integer(expiry - 10_000), "reconnectGraceMs": .integer(90_000),
                 "limits": .object(["maxPublishBps": .integer(5_000_000), "maxQueueBytes": .integer(4_194_304)])]) { _, b in b }
        return .object(f)
    }
    static func answer(_ session: V4vJSON = session(), grant: V4vJSON? = grant()) -> V4vJSON {
        var f: [String: V4vJSON] = ["ok": .bool(true), "session": session]; f["grant"] = grant; return .object(f)
    }
    static func edit(_ json: V4vJSON, _ changes: [String: V4vJSON]) -> V4vJSON {
        guard case var .object(f) = json else { preconditionFailure("Test fixture must be object") }; f.merge(changes) { _, b in b }; return .object(f)
    }
    static func fresh(_ part: UInt64 = 2, initId: UInt64 = 0) -> V4vJSON {
        .object(["connectionEpoch": .integer(1), "partId": .integer(part), "initializationId": .integer(initId), "initPath": .string("/live/\(event)/abc_\(initId == 0 ? "init" : "init\(initId)").mp4"), "partPath": .string("/live/\(event)/abc_part\(part).mp4"), "hasVideo": .bool(true), "hasAudio": .bool(true)])
    }
}

final class V4vTypedContractsTests: XCTestCase {
    func testGrantBindsScopeSeparateSecretsAndConfiguredLimits() throws {
        let valid = try V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time)
        XCTAssertEqual(valid.graceMs, 90_000); XCTAssertEqual(valid.maxQueueBytes, 4_194_304)
        XCTAssertFalse(String(reflecting: valid).contains(valid.passphrase))
        for changed in [V4vFixture.edit(valid.raw, ["sessionId": .string(String(repeating: "c", count: 32))]),
                        V4vFixture.edit(valid.raw, ["expiresAtUnixMs": .integer(V4vFixture.time + 900_001)]),
                        V4vFixture.edit(valid.raw, ["transport": .string("rtmp")]),
                        V4vFixture.edit(valid.raw, ["encryption": .object(["cipher": .string("aes256-ctr"), "passphrase": .string(String(repeating: "P", count: 43))])])] {
            XCTAssertThrowsError(try V4vGrant(changed, nowMs: V4vFixture.time))
        }
        XCTAssertNoThrow(try V4vGrant(V4vFixture.grant(expiry: V4vFixture.time + 900_000), nowMs: V4vFixture.time))
        XCTAssertNoThrow(try V4vGrant(V4vFixture.edit(valid.raw, ["reconnectGraceMs": .integer(0)]), nowMs: V4vFixture.time))
    }
    func testFreshMediaRequiresExactCanonicalIdentityAndPaths() throws {
        XCTAssertNoThrow(try V4vFreshMedia(V4vFixture.fresh(), event: V4vFixture.event, epoch: 1))
        for changes: [String: V4vJSON] in [["partPath": .string("https://evil.example.invalid/x")], ["initPath": .string("/live/\(V4vFixture.event)/abc_init0.mp4")], ["partPath": .string("/live/\(V4vFixture.event)/abc_part02.mp4")], ["partPath": .string("/live/\(V4vFixture.event)/def_part2.mp4")], ["partPath": .string("/live/\(V4vFixture.event)/abc_part2.mp4?x")], ["hasVideo": .bool(false), "hasAudio": .bool(false)], ["connectionEpoch": .integer(2)], ["decoded": .bool(true)]] {
            XCTAssertThrowsError(try V4vFreshMedia(V4vFixture.edit(V4vFixture.fresh(), changes), event: V4vFixture.event, epoch: 1))
        }
    }
    func testOriginOptionalMetadataAndActiveFences() throws {
        let old = try V4vOriginReceipt(V4vFixture.origin())
        XCTAssertNil(old.attempt); XCTAssertNil(old.freshMedia)
        let live = V4vFixture.edit(V4vFixture.origin(state: "live"), ["connectionAttemptId": .string(V4vFixture.key), "freshMedia": V4vFixture.fresh()])
        XCTAssertNotNil(try V4vOriginReceipt(live).freshMedia)
        for changes: [String: V4vJSON] in [["state": .string("held")], ["admission": .string("denied")], ["protocol": .string("stopping")], ["connectionEpoch": .integer(0)]] {
            XCTAssertThrowsError(try V4vOriginReceipt(V4vFixture.edit(live, changes)))
        }
        let null = try V4vOriginReceipt(V4vFixture.edit(V4vFixture.origin(), ["freshMedia": .null, "connectionAttemptId": .null]))
        XCTAssertNil(null.freshMedia); XCTAssertNil(null.attempt)
    }
    func testEndCompleteCannotHideIncompleteRecordingOrForeignOrigin() throws {
        let origin = V4vFixture.origin(state: "revoked", recording: "finalised")
        XCTAssertTrue(try V4vSessionReceipt(V4vFixture.session(phase: "ended", origin: origin, complete: true)).endComplete)
        XCTAssertThrowsError(try V4vSessionReceipt(V4vFixture.session(phase: "ended", origin: V4vFixture.edit(origin, ["recording": .string("incomplete")]), complete: true)))
        XCTAssertThrowsError(try V4vSessionReceipt(V4vFixture.session(origin: V4vFixture.edit(origin, ["sessionId": .string(String(repeating: "c", count: 32))]))))
    }
    func testOperationsValidateAllFieldsAndRecreateExactSavedRequest() throws {
        let op = try V4vOperation(kind: .refresh, event: V4vFixture.event, session: V4vFixture.sid, body: .object(["revision": .integer(1), "idempotencyKey": .string(V4vFixture.key)]))
        XCTAssertEqual(try V4vOperation(journal: V4vJSON.parse(op.journal.encoded())), op)
        XCTAssertEqual(op.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)/refresh")
        XCTAssertThrowsError(try V4vOperation(kind: .end, event: V4vFixture.event, session: V4vFixture.sid, body: V4vFixture.edit(op.body, ["revision": .number("1e0")])))
        XCTAssertThrowsError(try V4vOperation(kind: .end, event: V4vFixture.event, session: V4vFixture.sid, body: V4vFixture.edit(op.body, ["extra": .bool(true)])))
    }
    func testCurrentCollectionValidatesZeroPreconditionAndExactEvent() throws {
        let empty: V4vJSON = .object(["contractVersion": .integer(1), "eventId": .string(V4vFixture.event), "eventGeneration": .integer(0), "sessionId": .null])
        let response: V4vJSON = .object(["ok": .bool(true), "session": empty])
        XCTAssertEqual(try V4vCurrentSession(response, event: V4vFixture.event).generation, 0)
        XCTAssertEqual(try V4vCurrentSession(V4vFixture.answer(grant: nil), event: V4vFixture.event).generation, 1)
        XCTAssertThrowsError(try V4vCurrentSession(response, event: "event-other"))
        XCTAssertThrowsError(try V4vCurrentSession(.object(["ok": .bool(true), "session": V4vFixture.edit(empty, ["eventGeneration": .integer(1)])]), event: V4vFixture.event))
    }
    func testExactOriginExpiryAndNeverIssuedTerminalException() throws {
        let mismatch = V4vFixture.edit(V4vFixture.origin(), ["expiresAtUnixMs": .integer(V4vFixture.time - 1)])
        XCTAssertThrowsError(try V4vSessionReceipt(V4vFixture.session(origin: mismatch)))
        let tombstone = V4vFixture.edit(V4vFixture.origin(state: "revoked"), ["expiresAtUnixMs": .integer(0)])
        let receipt = try V4vSessionReceipt(V4vFixture.session(phase: "ended", origin: tombstone, complete: true))
        XCTAssertTrue(receipt.endComplete); XCTAssertFalse(receipt.permitsGrant)
        XCTAssertEqual(receipt.origin?.expiresMs, 0)
        XCTAssertThrowsError(try V4vOriginReceipt(V4vFixture.edit(tombstone, ["admission": .string("open")])))
    }
}

final class V4vMemoryJournal: V4vJournalStore {
    var value: V4vJSON?
    var fails = false
    var loadsFail = false
    var saves = 0
    func load() throws -> V4vJSON? { if loadsFail { throw V4vFailure.journal }; return value }
    func save(_ document: V4vJSON) throws { saves += 1; if fails { throw V4vFailure.journal }; value = document }
}

final class V4vSessionEngineTests: XCTestCase {
    private func engine(_ store: V4vMemoryJournal, clock: V4vClock = V4vFixture.clock(), fence: @escaping () -> Void = {}) throws -> V4vSessionEngine {
        try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: clock, fencePublisher: fence)
    }
    private func login(_ e: V4vSessionEngine) throws { try e.installAuthenticatedSession(pubkey: String(repeating: "d", count: 64), token: "synthetic-test-bearer", maxAgeSeconds: 86_400, clock: V4vFixture.clock()) }
    @discardableResult private func prepared(_ e: V4vSessionEngine) throws -> V4vOperation {
        try login(e); try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let op = try XCTUnwrap(e.pending)
        try e.applyResponse(V4vFixture.answer(), for: op, clock: V4vFixture.clock()); return op
    }
    func testSmallPositiveGrantRetainsAuthorityWithoutChoosingCaptureQuality() throws {
        let e = try engine(V4vMemoryJournal()); try login(e)
        try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let grant = V4vFixture.edit(V4vFixture.grant(), ["limits": .object(["maxPublishBps": .integer(1_000_000), "maxQueueBytes": .integer(65_536)])])
        try e.applyResponse(V4vFixture.answer(grant: grant), for: XCTUnwrap(e.pending), clock: V4vFixture.clock())
        XCTAssertNil(e.pending)
        XCTAssertEqual(try e.captureGrant(clock: V4vFixture.clock()).maxPublishBps, 1_000_000)
        XCTAssertEqual(try e.publishingGrant(clock: V4vFixture.clock()).grant.maxQueueBytes, 65_536)
        XCTAssertThrowsError(try V4vGrant(V4vFixture.edit(grant, ["limits": .object(["maxPublishBps": .integer(0), "maxQueueBytes": .integer(65_536)])]), nowMs: V4vFixture.time))
    }
    func testPrepareIsDurableBeforeAnyRequestAndFailurePoisonsAdmission() throws {
        let store = V4vMemoryJournal(); var fences = 0; let e = try engine(store, fence: { fences += 1 }); try login(e)
        store.fails = true
        XCTAssertThrowsError(try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock()))
        XCTAssertNil(e.pending); XCTAssertGreaterThan(fences, 0)
        store.fails = false
        XCTAssertThrowsError(try e.pendingRequest(clock: V4vFixture.clock()))
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
    }
    func testLostResponseRestartRetainsExactRequestAndDoesNotRecoverGrantFromStatus() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); try login(e)
        try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let original = try XCTUnwrap(e.pendingRequest(clock: V4vFixture.clock()))
        let restored = try engine(store)
        XCTAssertEqual(try restored.pendingRequest(clock: V4vFixture.clock())?.operation.bytes, original.operation.bytes)
        try restored.applyResponse(V4vFixture.answer(), for: original.operation, clock: V4vFixture.clock())
        XCTAssertNoThrow(try restored.captureGrant(clock: V4vFixture.clock()))
        let another = try engine(store)
        XCTAssertThrowsError(try another.captureGrant(clock: V4vFixture.clock()))
        try another.applyStatus(V4vFixture.answer(grant: nil), clock: V4vFixture.clock())
        XCTAssertThrowsError(try another.captureGrant(clock: V4vFixture.clock()))
        try another.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock())
        let op = try XCTUnwrap(another.pending)
        try another.applyResponse(V4vFixture.answer(V4vFixture.session(2), grant: V4vFixture.grant(2)), for: op, clock: V4vFixture.clock())
        XCTAssertNoThrow(try another.captureGrant(clock: V4vFixture.clock()))
    }
    func testEndDuringUncertainRefreshFencesBeforeSaveAndWaitsExactAcknowledgedRevision() throws {
        let store = V4vMemoryJournal(); var fences = 0; let e = try engine(store, fence: { fences += 1 }); try prepared(e)
        try e.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock())
        let refresh = try XCTUnwrap(e.pending)
        try e.requestEnd(clock: V4vFixture.clock())
        XCTAssertGreaterThan(fences, 0); XCTAssertEqual(e.pending, refresh)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
        try e.applyResponse(V4vFixture.answer(V4vFixture.session(2), grant: V4vFixture.grant(2)), for: refresh, clock: V4vFixture.clock())
        XCTAssertNil(e.pending); XCTAssertTrue(e.terminalIntent)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
        try e.continueEnd(key: String(repeating: "e", count: 32), clock: V4vFixture.clock())
        XCTAssertEqual(e.pending?.kind, .end)
        let fields = try XCTUnwrap(e.pending).body.object(required: ["revision", "idempotencyKey"])
        XCTAssertEqual(try fields.uint("revision"), 2)
        let restored = try engine(store)
        XCTAssertTrue(restored.terminalIntent); XCTAssertEqual(restored.pending, e.pending)
    }
    func testRemoteEndReadCommitsTerminalOnlyScopeAtomicallyAndSurvivesRestart() throws {
        let store = V4vMemoryJournal(); var fences = 0
        let e = try engine(store, fence: { fences += 1 }); try login(e)
        let requested = try V4vIdentity(V4vFixture.identity())
        let read = try e.remoteEndStatusRequest(identity: requested, clock: V4vFixture.clock())
        XCTAssertEqual(read.call.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)")
        XCTAssertNil(e.session); XCTAssertFalse(e.terminalIntent)
        try e.commitRemoteEnd(V4vFixture.answer(V4vFixture.session(2), grant: nil), for: read, clock: V4vFixture.clock(1))
        XCTAssertGreaterThan(fences, 0); XCTAssertTrue(e.terminalIntent)
        XCTAssertEqual(e.session?.identity.revision, 2); XCTAssertNil(e.pending)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock(1)))
        XCTAssertThrowsError(try e.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock(1)))
        let saved = try XCTUnwrap(store.value).object(required: ["version", "apiOrigin", "clockFloorMs", "terminalIntent"],
                                                      optional: ["pubkey", "bearer", "bearerExpiresMs", "session"])
        XCTAssertTrue(try saved.flag("terminalIntent")); XCTAssertNotNil(saved["session"])
        XCTAssertNil(saved["grant"]); XCTAssertNil(saved["grantPolicy"]); XCTAssertNil(saved["grantExpiryMs"])

        let restored = try engine(store, clock: V4vFixture.clock(1))
        XCTAssertTrue(restored.terminalIntent); XCTAssertEqual(restored.session?.identity.revision, 2); XCTAssertNil(restored.pending)
        try restored.continueEnd(key: String(repeating: "e", count: 32), clock: V4vFixture.clock(1))
        XCTAssertEqual(restored.pending?.kind, .end)
        let body = try XCTUnwrap(restored.pending).body.object(required: ["revision", "idempotencyKey"])
        XCTAssertEqual(try body.uint("revision"), 2)
    }
    func testRemoteEndRejectsLateWrongOrNonactiveStatusWithoutAdoption() throws {
        func fresh() throws -> (V4vMemoryJournal, V4vSessionEngine, V4vRemoteEndRead) {
            let store = V4vMemoryJournal(), e = try engine(store); try login(e)
            let read = try e.remoteEndStatusRequest(identity: V4vIdentity(V4vFixture.identity()), clock: V4vFixture.clock())
            return (store, e, read)
        }
        do {
            let (store, e, read) = try fresh(); let saved = store.value
            XCTAssertThrowsError(try e.commitRemoteEnd(V4vFixture.answer(V4vFixture.session(phase: "preparing"), grant: nil), for: read, clock: V4vFixture.clock(1)))
            XCTAssertNil(e.session); XCTAssertFalse(e.terminalIntent); XCTAssertEqual(store.value, saved)
        }
        do {
            let (store, e, read) = try fresh(); let saved = store.value
            let other = V4vFixture.session(generation: 2, sid: String(repeating: "c", count: 32))
            XCTAssertThrowsError(try e.commitRemoteEnd(V4vFixture.answer(other, grant: nil), for: read, clock: V4vFixture.clock(1)))
            XCTAssertNil(e.session); XCTAssertFalse(e.terminalIntent); XCTAssertEqual(store.value, saved)
        }
        do {
            let (store, e, read) = try fresh(); let saved = store.value
            XCTAssertThrowsError(try e.commitRemoteEnd(V4vFixture.answer(grant: nil), for: read, clock: V4vFixture.clock(8_001)))
            XCTAssertNil(e.session); XCTAssertFalse(e.terminalIntent); XCTAssertEqual(store.value, saved)
        }
    }
    func testRemoteEndSaveFailureCannotExposeNonterminalSession() throws {
        let store = V4vMemoryJournal(); var fences = 0
        let e = try engine(store, fence: { fences += 1 }); try login(e)
        let read = try e.remoteEndStatusRequest(identity: V4vIdentity(V4vFixture.identity()), clock: V4vFixture.clock())
        let saved = store.value; store.fails = true
        XCTAssertThrowsError(try e.commitRemoteEnd(V4vFixture.answer(V4vFixture.session(2), grant: nil), for: read, clock: V4vFixture.clock(1)))
        XCTAssertGreaterThan(fences, 0); XCTAssertNil(e.session); XCTAssertFalse(e.terminalIntent)
        store.fails = false; XCTAssertEqual(store.value, saved)
        let restored = try engine(store, clock: V4vFixture.clock(1))
        XCTAssertNil(restored.session); XCTAssertFalse(restored.terminalIntent)
    }
    func testExpiredOwnerCannotBeRevivedByLateRefreshAndImmutablePinSurvivesGrantRemoval() throws {
        let store = V4vMemoryJournal(); var fences = 0; let e = try engine(store, fence: { fences += 1 }); try prepared(e)
        try e.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock(50_000))
        let op = try XCTUnwrap(e.pending)
        try e.tick(clock: V4vFixture.clock(60_000)); XCTAssertGreaterThan(fences, 0)
        let changed = V4vFixture.edit(V4vFixture.grant(2, expiry: V4vFixture.time + 120_000), ["endpoint": .string("srt://192.0.2.11:9000")])
        XCTAssertThrowsError(try e.applyResponse(V4vFixture.answer(V4vFixture.session(2, expiry: V4vFixture.time + 120_000), grant: changed), for: op, clock: V4vFixture.clock(60_001)))
        XCTAssertEqual(e.pending, op)
        try e.applyResponse(V4vFixture.answer(V4vFixture.session(2, expiry: V4vFixture.time + 120_000), grant: V4vFixture.grant(2, expiry: V4vFixture.time + 120_000)), for: op, clock: V4vFixture.clock(60_002))
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock(60_003)))
        XCTAssertThrowsError(try engine(store, clock: V4vFixture.clock(60_003)).captureGrant(clock: V4vFixture.clock(60_003)))
    }
    func testWallRollbackCannotExtendExpiryAndMonotonicRollbackFences() throws {
        let store = V4vMemoryJournal(); var fences = 0; let e = try engine(store, fence: { fences += 1 }); try prepared(e)
        XCTAssertNoThrow(try e.captureGrant(clock: V4vFixture.clock(59_000, wall: 1)))
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock(60_000, wall: 1)))
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock(59_999)))
        XCTAssertGreaterThan(fences, 0)
    }
    func testStatusCannotClearPendingAndTerminalStatusNeverRecoversCredentials() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); try prepared(e)
        try e.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock())
        let op = e.pending
        try e.applyStatus(V4vFixture.answer(V4vFixture.session(2), grant: nil), clock: V4vFixture.clock())
        XCTAssertEqual(e.pending, op); XCTAssertEqual(e.session?.identity.revision, 1)
        try e.applyStatus(V4vFixture.answer(V4vFixture.session(2, phase: "ended", origin: V4vFixture.origin(2, state: "revoked", recording: "finalised"), complete: true), grant: nil), clock: V4vFixture.clock())
        XCTAssertTrue(e.terminalIntent); XCTAssertEqual(e.pending, op)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
    }
    func testFailedResponseCommitLeavesDurableOperationForExactRetry() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); try login(e)
        try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let op = try XCTUnwrap(e.pending); store.fails = true
        XCTAssertThrowsError(try e.applyResponse(V4vFixture.answer(), for: op, clock: V4vFixture.clock()))
        store.fails = false
        let restored = try engine(store)
        XCTAssertEqual(restored.pending, op); XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
        try restored.applyResponse(V4vFixture.answer(), for: op, clock: V4vFixture.clock())
        XCTAssertNoThrow(try restored.captureGrant(clock: V4vFixture.clock()))
    }
    func testStaleResponseCannotMutateLaterSessionOrClosedOwner() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); let op = try prepared(e)
        XCTAssertThrowsError(try e.applyResponse(V4vFixture.answer(), for: op, clock: V4vFixture.clock()))
        e.close()
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
        XCTAssertThrowsError(try e.stageRefresh(key: V4vFixture.key, clock: V4vFixture.clock()))
    }
    func testMissingCorruptForeignJournalAndAccountSwitchFailClosed() throws {
        let store = V4vMemoryJournal(); store.loadsFail = true
        XCTAssertThrowsError(try engine(store)); store.loadsFail = false
        store.value = .object(["version": .integer(1)])
        XCTAssertThrowsError(try engine(store)); store.value = nil
        let e = try engine(store); try prepared(e)
        XCTAssertThrowsError(try e.installAuthenticatedSession(pubkey: String(repeating: "e", count: 64), token: "another-synthetic-token", maxAgeSeconds: 100, clock: V4vFixture.clock()))
        let foreign = try V4vConnectorProfile(apiOrigin: "https://other.example.invalid", playbackOrigin: "https://media.example.invalid", playbackPrefix: "/")
        XCTAssertThrowsError(try V4vSessionEngine(profile: foreign, store: store, clock: V4vFixture.clock(), fencePublisher: {}))
    }
    func testIncompleteEndNeedsExplicitAcknowledgementThenSeparateReplacementPrepare() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); try prepared(e)
        try e.requestEnd(clock: V4vFixture.clock())
        XCTAssertThrowsError(try e.stageAcknowledgement(key: V4vFixture.key, clock: V4vFixture.clock()))
        try e.continueEnd(key: String(repeating: "c", count: 32), clock: V4vFixture.clock())
        let incompleteOrigin = V4vFixture.origin(2, state: "revoked", recording: "incomplete")
        let ended = V4vFixture.session(2, phase: "ended", origin: incompleteOrigin)
        try e.applyResponse(V4vFixture.answer(ended, grant: nil), for: XCTUnwrap(e.pending), clock: V4vFixture.clock())
        XCTAssertFalse(try XCTUnwrap(e.session).endComplete)
        XCTAssertThrowsError(try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 1, key: V4vFixture.key, clock: V4vFixture.clock()))
        try e.stageAcknowledgement(key: String(repeating: "e", count: 32), clock: V4vFixture.clock())
        let ack = try XCTUnwrap(e.pending)
        let recovery: V4vJSON = .object(["state": .string("acknowledged"), "acknowledgementKey": .string(String(repeating: "e", count: 32)), "eventGeneration": .integer(1), "revision": .integer(2), "explicitPrepareRequired": .bool(true), "origin": incompleteOrigin])
        try e.applyResponse(V4vFixture.answer(V4vFixture.edit(ended, ["recovery": recovery]), grant: nil), for: ack, clock: V4vFixture.clock())
        XCTAssertNil(e.pending); XCTAssertTrue(e.terminalIntent)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
        try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 1, key: String(repeating: "f", count: 32), replacingAcknowledged: true, clock: V4vFixture.clock())
        let replacement = try XCTUnwrap(e.pending)
        let old = try replacement.body.object(required: ["expectedEventGeneration", "idempotencyKey"], optional: ["replaces"]).need("replaces").object(required: ["sessionId", "revision", "acknowledgementKey"])
        XCTAssertEqual(try old.text("sessionId"), V4vFixture.sid)
        let successor = String(repeating: "c", count: 32)
        try e.applyResponse(V4vFixture.answer(V4vFixture.session(generation: 2, sid: successor), grant: V4vFixture.grant(generation: 2, sid: successor)), for: replacement, clock: V4vFixture.clock())
        XCTAssertEqual(try e.captureGrant(clock: V4vFixture.clock()).identity.generation, 2)
        XCTAssertThrowsError(try e.applyResponse(V4vFixture.answer(ended, grant: nil), for: ack, clock: V4vFixture.clock()))
    }
    func testContradictoryOriginCannotAdmitGrantOrLeaveOldCaptureAdmission() throws {
        let cases: [V4vJSON] = [
            V4vFixture.origin(state: "inactive"),
            V4vFixture.edit(V4vFixture.origin(), ["admission": .string("denied")]),
            V4vFixture.edit(V4vFixture.origin(), ["state": .string("expired")]),
            V4vFixture.edit(V4vFixture.origin(), ["expiresAtUnixMs": .integer(V4vFixture.time - 1)]),
            V4vFixture.edit(V4vFixture.origin(), ["expiresAtUnixMs": .integer(V4vFixture.time + 61_000)])
        ]
        for origin in cases {
            let answer = V4vFixture.answer(V4vFixture.session(origin: origin))
            let firstStore = V4vMemoryJournal(), e = try engine(firstStore)
            try login(e); try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
            let pending = try XCTUnwrap(e.pending)
            XCTAssertThrowsError(try e.applyResponse(answer, for: pending, clock: V4vFixture.clock()))
            XCTAssertEqual(e.pending, pending)
            XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))

            var fences = 0
            let other = try engine(V4vMemoryJournal(), fence: { fences += 1 }); try prepared(other)
            let status = V4vFixture.answer(V4vFixture.session(origin: origin), grant: nil)
            // Some contradictions reject the entire receipt, others are valid
            // status metadata but cannot supply an active publishing grant.
            do { try other.applyStatus(status, clock: V4vFixture.clock()) } catch {}
            XCTAssertThrowsError(try other.captureGrant(clock: V4vFixture.clock()))
            XCTAssertGreaterThan(fences, 0); XCTAssertNil(other.pending)
        }
    }
    func testExplicitNullGrantAcknowledgesOperationWithoutPublishingAuthority() throws {
        let store = V4vMemoryJournal(); let e = try engine(store); try login(e)
        try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let pending = try XCTUnwrap(e.pending)
        try e.applyResponse(V4vFixture.answer(grant: .null), for: pending, clock: V4vFixture.clock())
        XCTAssertNil(e.pending); XCTAssertNotNil(e.session)
        XCTAssertThrowsError(try e.captureGrant(clock: V4vFixture.clock()))
    }
}
