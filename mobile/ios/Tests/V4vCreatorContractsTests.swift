import XCTest
@testable import SapflowBroadcaster

// Generated JSON only, following V4V 15353b1 server routes and publicNativeSession.
final class V4vCreatorContractsTests: XCTestCase {
    private func row(_ id: String = "event-creator", _ fields: [String: V4vJSON] = [:]) -> V4vJSON {
        V4vFixture.edit(.object(["id": .string(id)]), fields)
    }
    private func answer(_ rows: [V4vJSON]) -> V4vJSON { .object(["ok": .bool(true), "events": .array(rows)]) }
    private func engine(_ store: V4vMemoryJournal, age: UInt64 = 86_400) throws -> V4vSessionEngine {
        let engine = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: {})
        try engine.installAuthenticatedSession(pubkey: String(repeating: "d", count: 64), token: "synthetic-creator-bearer", maxAgeSeconds: age, clock: V4vFixture.clock())
        return engine
    }
    private func prepared(_ engine: V4vSessionEngine) throws {
        try engine.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: V4vFixture.clock())
        let operation = try XCTUnwrap(engine.pending)
        try engine.applyResponse(V4vFixture.answer(), for: operation, clock: V4vFixture.clock())
    }
    func testDisplayProjectionPreservesUnicodeAndIgnoresPresentationAuthority() throws {
        let value = row("event-creator", ["title": .string("日本語 🎵"), "status": .string("scheduled"), "startsAt": .string("2026-10-21T18:00:00.000Z"), "endsAt": .string(""),
                                          "role": .string("owner"), "canManage": .bool(true), "stream": .object(["password": .string("synthetic-extension-not-retained")])])
        let list = try V4vCreatorEvents(answer([value]))
        XCTAssertEqual(list.events.first?.id, "event-creator")
        XCTAssertEqual(list.events.first?.title, "日本語 🎵")
        XCTAssertEqual(list.events.first?.statusText, "scheduled")
        XCTAssertEqual(list.events.first?.startsAtText, "2026-10-21T18:00:00.000Z")
        XCTAssertEqual(list.events.first?.endsAtText, "")
        XCTAssertFalse(String(reflecting: list).contains("synthetic-extension"))
        XCTAssertNil(try V4vCreatorEvents(answer([row()])).events.first?.title)
        XCTAssertNil(try V4vCreatorEvents(answer([row("event-creator", ["title": .null])])).events.first?.title)
    }
    func testRowCountAndDuplicateIdentityFailWholeResponseWithoutTruncation() throws {
        let rows = (0..<128).map { row("event-creator-\($0)") }
        XCTAssertEqual(try V4vCreatorEvents(answer(rows)).events.count, 128)
        XCTAssertThrowsError(try V4vCreatorEvents(answer(rows + [row("event-overflow")])))
        XCTAssertThrowsError(try V4vCreatorEvents(answer([row(), row()])))
        XCTAssertTrue(try V4vCreatorEvents(answer([])).events.isEmpty)
        for id in ["event-x/../other", "event-Creator", "event-x\n", "event-%61"] {
            XCTAssertThrowsError(try V4vCreatorEvents(answer([row(id)])))
        }
    }
    func testOptionalDisplayTypesAndByteBoundsAreStrict() throws {
        XCTAssertNoThrow(try V4vCreatorEvents(answer([row("event-creator", ["title": .string(String(repeating: "🎵", count: 160))])])))
        for fields: [String: V4vJSON] in [["title": .string(String(repeating: "x", count: 641))], ["title": .bool(true)], ["title": .string("raw\ncontrol")],
                                         ["status": .string(String(repeating: "s", count: 21))], ["status": .integer(1)],
                                         ["startsAt": .string(String(repeating: "t", count: 41))], ["endsAt": .bool(false)]] {
            XCTAssertThrowsError(try V4vCreatorEvents(answer([row("event-creator", fields)])))
        }
    }
    func testEscapedDuplicateKeysRejectedBeforeDisplayProjection() throws {
        for json in [#"{"ok":true,"events":[{"id":"event-a","\u0069d":"event-b"}]}"#,
                     #"{"ok":true,"events":[{"id":"event-a","extension":{"role":1,"\u0072ole":2}}]}"#,
                     #"{"ok":true,"\u006fk":true,"events":[]}"#] {
            XCTAssertThrowsError(try V4vCreatorEvents(bytes: Data(json.utf8)))
        }
    }
    func testWholeBodyUtf8AndDepthUseExistingStrictParser() throws {
        let small = #"{"ok":true,"events":[]}"#
        let exact = Data((small + String(repeating: " ", count: V4vJSON.maxBody - small.utf8.count)).utf8)
        XCTAssertTrue(try V4vCreatorEvents(bytes: exact).events.isEmpty)
        XCTAssertThrowsError(try V4vCreatorEvents(bytes: exact + Data([32])))
        XCTAssertThrowsError(try V4vCreatorEvents(bytes: Data([0xff])))
        let deep = #"{"ok":true,"events":[{"id":"event-a","extension":"# + String(repeating: "[", count: 16) + "0" + String(repeating: "]", count: 16) + "}]}"
        XCTAssertThrowsError(try V4vCreatorEvents(bytes: Data(deep.utf8)))
    }
    func testResponseAndRowShapeCannotCoerceAuthorityOrErrors() throws {
        for value: V4vJSON in [.array([]), .object(["ok": .bool(false), "events": .array([])]), .object(["ok": .integer(1), "events": .array([])]),
                               .object(["ok": .bool(true), "events": .bool(true)]), .object(["ok": .bool(true), "events": .array([]), "grant": .null]),
                               answer([.string("event-creator")]), answer([.object([:])])] {
            XCTAssertThrowsError(try V4vCreatorEvents(value))
        }
    }
    func testCurrentCollectionUsesExactEmptyOrExistingFullReceiptOnly() throws {
        let empty: V4vJSON = .object(["contractVersion": .integer(1), "eventId": .string(V4vFixture.event), "eventGeneration": .integer(0), "sessionId": .null])
        let current: (V4vJSON) -> V4vJSON = { .object(["ok": .bool(true), "session": $0]) }
        XCTAssertNil(try V4vCurrentSession(current(empty), event: V4vFixture.event).receipt)
        XCTAssertEqual(try V4vCurrentSession(V4vFixture.answer(grant: nil), event: V4vFixture.event).receipt?.identity.sessionId, V4vFixture.sid)
        for changes: [String: V4vJSON] in [["eventGeneration": .bool(false)], ["eventGeneration": .number("0e0")], ["eventGeneration": .integer(1)], ["revision": .integer(1)]] {
            XCTAssertThrowsError(try V4vCurrentSession(current(V4vFixture.edit(empty, changes)), event: V4vFixture.event))
        }
        XCTAssertThrowsError(try V4vCurrentSession(current(empty), event: "event-other"))
        XCTAssertThrowsError(try V4vCurrentSession(current(V4vFixture.edit(V4vFixture.session(), ["expiresAtUnixMs": .bool(true)])), event: V4vFixture.event))
    }
    func testExactSessionStatusBindsEventSessionGenerationAndRevision() throws {
        let requested = try V4vIdentity(V4vFixture.identity())
        XCTAssertEqual(try V4vCreatorSessionStatus(V4vFixture.answer(grant: nil), requested: requested).receipt.identity, requested)
        XCTAssertEqual(try V4vCreatorSessionStatus(V4vFixture.answer(V4vFixture.session(2), grant: nil), requested: requested).receipt.identity.revision, 2)
        for identity in [try V4vIdentity(eventId: "event-other", generation: 1, sessionId: V4vFixture.sid, revision: 1),
                         try V4vIdentity(eventId: V4vFixture.event, generation: 2, sessionId: V4vFixture.sid, revision: 1),
                         try V4vIdentity(eventId: V4vFixture.event, generation: 1, sessionId: String(repeating: "c", count: 32), revision: 1),
                         try V4vIdentity(eventId: V4vFixture.event, generation: 1, sessionId: V4vFixture.sid, revision: 2)] {
            XCTAssertThrowsError(try V4vCreatorSessionStatus(V4vFixture.answer(grant: nil), requested: identity))
        }
        XCTAssertThrowsError(try V4vCreatorSessionStatus(V4vFixture.answer(), requested: requested))
        XCTAssertThrowsError(try V4vCreatorSessionStatus(V4vFixture.answer(V4vFixture.edit(V4vFixture.session(), ["phase": .string("unknown")]), grant: nil), requested: requested))
    }
    func testFixedAuthenticatedRoutesDoNotPersistOrCreateSession() throws {
        let store = V4vMemoryJournal(), engine = try engine(store)
        let saved = store.value, saves = store.saves
        let calls = [try engine.creatorEventsRequest(clock: V4vFixture.clock()), try engine.currentRequest(event: V4vFixture.event, clock: V4vFixture.clock())]
        for (call, path) in zip(calls, ["/api/live-events", "/api/live-events/\(V4vFixture.event)/native-session"]) {
            let request = try call.urlRequest(profile: V4vFixture.profile())
            XCTAssertEqual(request.httpMethod, "GET"); XCTAssertNil(request.httpBody)
            XCTAssertEqual(request.url?.path, path); XCTAssertNil(request.url?.query)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer synthetic-creator-bearer")
            XCTAssertFalse(request.httpShouldHandleCookies)
            XCTAssertFalse(String(reflecting: call).contains("synthetic-creator-bearer"))
        }
        XCTAssertThrowsError(try engine.currentRequest(event: "event-x?admin=1", clock: V4vFixture.clock()))
        XCTAssertThrowsError(try engine.sessionStatusRequest(clock: V4vFixture.clock()))
        XCTAssertEqual(store.value, saved); XCTAssertEqual(store.saves, saves); XCTAssertNil(engine.session)
    }
    func testReadsPreserveUncertainRefreshAndTerminalEndWithoutAdoption() throws {
        let store = V4vMemoryJournal(), engine = try engine(store); try prepared(engine)
        try engine.stageRefresh(key: String(repeating: "c", count: 32), clock: V4vFixture.clock())
        try engine.requestEnd(clock: V4vFixture.clock())
        let saved = store.value, operation = engine.pending, session = engine.session
        _ = try engine.creatorEventsRequest(clock: V4vFixture.clock())
        _ = try engine.currentRequest(event: "event-other", clock: V4vFixture.clock())
        let call = try engine.sessionStatusRequest(clock: V4vFixture.clock())
        XCTAssertEqual(call.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)")
        _ = try V4vCreatorSessionStatus(V4vFixture.answer(V4vFixture.session(2), grant: nil), requested: try XCTUnwrap(session).identity)
        XCTAssertEqual(engine.pending, operation); XCTAssertEqual(engine.session, session); XCTAssertEqual(store.value, saved)
        XCTAssertTrue(engine.terminalIntent); XCTAssertThrowsError(try engine.captureGrant(clock: V4vFixture.clock()))
    }
    func testReadResultsCannotRestorePublishingAuthorityAfterRestart() throws {
        let store = V4vMemoryJournal(), original = try engine(store); try prepared(original)
        let restored = try V4vSessionEngine(profile: V4vFixture.profile(), store: store, clock: V4vFixture.clock(), fencePublisher: {})
        let saved = store.value
        _ = try restored.creatorEventsRequest(clock: V4vFixture.clock())
        _ = try restored.currentRequest(event: V4vFixture.event, clock: V4vFixture.clock())
        _ = try restored.sessionStatusRequest(clock: V4vFixture.clock())
        _ = try V4vCurrentSession(V4vFixture.answer(grant: nil), event: V4vFixture.event)
        _ = try V4vCreatorSessionStatus(V4vFixture.answer(V4vFixture.session(2), grant: nil), requested: try XCTUnwrap(restored.session).identity)
        XCTAssertEqual(restored.session?.identity.revision, 1)
        XCTAssertEqual(store.value, saved); XCTAssertNil(restored.pending)
        XCTAssertThrowsError(try restored.captureGrant(clock: V4vFixture.clock()))
    }
    func testReadsRequireUnexpiredAuthenticationAndTickCannotReopenGrant() throws {
        let empty = try V4vSessionEngine(profile: V4vFixture.profile(), store: V4vMemoryJournal(), clock: V4vFixture.clock(), fencePublisher: {})
        XCTAssertThrowsError(try empty.creatorEventsRequest(clock: V4vFixture.clock()))
        XCTAssertThrowsError(try empty.currentRequest(event: V4vFixture.event, clock: V4vFixture.clock()))
        XCTAssertThrowsError(try empty.sessionStatusRequest(clock: V4vFixture.clock()))
        let store = V4vMemoryJournal(), engine = try engine(store, age: 1)
        XCTAssertNoThrow(try engine.creatorEventsRequest(clock: V4vFixture.clock(999)))
        XCTAssertThrowsError(try engine.currentRequest(event: V4vFixture.event, clock: V4vFixture.clock(1_000)))
        XCTAssertThrowsError(try engine.creatorEventsRequest(clock: V4vFixture.clock(1_000)))
        let liveStore = V4vMemoryJournal(), live = try self.engine(liveStore); try prepared(live)
        _ = try live.sessionStatusRequest(clock: V4vFixture.clock(60_000))
        XCTAssertThrowsError(try live.captureGrant(clock: V4vFixture.clock(60_000)))
        XCTAssertThrowsError(try live.creatorEventsRequest(clock: V4vFixture.clock(59_999)))
        XCTAssertThrowsError(try live.currentRequest(event: V4vFixture.event, clock: V4vFixture.clock(60_000)))
        live.close(); XCTAssertThrowsError(try live.sessionStatusRequest(clock: V4vFixture.clock(60_000)))
    }
}
