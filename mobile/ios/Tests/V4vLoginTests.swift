import XCTest
@testable import SapflowBroadcaster

private let generatedPersona = String(repeating: "a", count: 64)
private func generatedChallenge() throws -> V4vLoginChallenge {
    try V4vLoginChallenge(.object(["ok": .bool(true), "enabled": .bool(true), "registration": .string("closed"), "appName": .string("Generated test"), "challenge": .string(String(repeating: "b", count: 64)), "relayUrl": .string("wss://unused.example.invalid")]))
}
private func generatedProof(_ t: V4vLoginTemplate) throws -> Data {
    // Structural fixture ONLY. Native canonical ID/signature verification has
    // separate actual Rust generated-key tests, not this injected fake signer.
    try V4vJSON.object(["id": .string(String(repeating: "c", count: 64)), "pubkey": .string(t.persona), "created_at": .integer(t.createdAt), "kind": .integer(21236), "content": .string(""), "sig": .string(String(repeating: "d", count: 128)), "tags": .array([.array([.string("challenge"), .string(t.challenge.challenge)]), .array([.string("origin"), .string(t.origin)]), .array([.string("app"), .string(t.challenge.appName)])])]).encoded()
}
private func generatedLogin() -> V4vJSON { .object(["ok": .bool(true), "pubkey": .string(generatedPersona), "registered": .bool(false), "operator": .bool(false), "sessionToken": .string("generated-token-not-an-account"), "sessionTokenMaxAgeSec": .integer(86400)]) }

private final class LoginSigner: V4vLoginSigner {
    var onConnect: (() -> Void)?
    var onSign: (() -> Void)?
    var cancelled = false
    func connect() throws -> (persona: String, credential: Data) { onConnect?(); return (generatedPersona, Data("generated-credential".utf8)) }
    func sign(_ t: V4vLoginTemplate) throws -> Data { onSign?(); return try generatedProof(t) }
    func cancel() { cancelled = true }
    func waitClosed() -> Bool { true }
}
private final class LoginHTTP: V4vLoginHTTP {
    let onClosed: () -> Void
    var calls: [V4vHTTPCall] = []
    var responder: ((Int, @escaping (Result<V4vJSON, V4vFailure>) -> Void) -> Void)?
    var delayClose = false
    init(_ closed: @escaping () -> Void) { onClosed = closed }
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) {
        calls.append(call)
        if let responder { responder(calls.count, completion) }
        else { completion(.success(calls.count == 1 ? try! V4vJSON.parse(Data(#"{"ok":true,"enabled":true,"registration":"closed","appName":"Generated test","challenge":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.utf8)) : generatedLogin())) }
    }
    func close() { if !delayClose { onClosed() } }
}

final class V4vLoginTests: XCTestCase {
    func testChallengeIsStrictBeforeFoundationNormalisation() throws {
        XCTAssertNoThrow(try generatedChallenge())
        for raw in [#"{"ok":true,"enabled":true,"enabled":true}"#, #"{"ok":true,"enabled":1,"registration":"closed","appName":"Test","challenge":"a"}"#] {
            XCTAssertThrowsError(try V4vLoginChallenge(V4vJSON.parse(Data(raw.utf8))))
        }
        let disabled = V4vJSON.object(["ok": .bool(true), "enabled": .bool(false), "registration": .string("closed"), "appName": .string("Test"), "challenge": .string("")])
        XCTAssertThrowsError(try V4vLoginChallenge(disabled))
    }
    func testOnlyExactFixedProofCanBuildLoginRequest() throws {
        let t = try V4vLoginTemplate(persona: generatedPersona, challenge: generatedChallenge(), profile: V4vFixture.profile(), createdAt: 1700000000)
        let proof = try t.checkedProof(generatedProof(t))
        let request = try V4vHTTPCall(login: proof).urlRequest(profile: V4vFixture.profile())
        XCTAssertEqual(request.url?.path, "/api/auth/nostr/login"); XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        XCTAssertFalse(request.httpShouldHandleCookies)
        var fields = try V4vJSON.parse(generatedProof(t)).object(required: ["id", "pubkey", "created_at", "kind", "content", "sig", "tags"])
        for (key, bad) in [("kind", V4vJSON.number("21236.0")), ("created_at", .bool(true)), ("content", .string("arbitrary sign request")), ("pubkey", .string(String(repeating: "e", count: 64))), ("tags", .array([]))] {
            let old = fields[key]; fields[key] = bad
            XCTAssertThrowsError(try t.checkedProof(V4vJSON.object(fields).encoded())); fields[key] = old
        }
    }
    func testLoginResultRequiresSamePersonaAndBoundedActualTokenLifetime() throws {
        XCTAssertNoThrow(try V4vLoginResult(generatedLogin(), persona: generatedPersona))
        XCTAssertThrowsError(try V4vLoginResult(generatedLogin(), persona: String(repeating: "e", count: 64)))
        var f = try generatedLogin().object(required: ["ok", "pubkey", "registered", "operator", "sessionToken", "sessionTokenMaxAgeSec"])
        for bad in [V4vJSON.bool(true), .number("8.64e4"), .integer(86401), .integer(0)] { f["sessionTokenMaxAgeSec"] = bad; XCTAssertThrowsError(try V4vLoginResult(.object(f), persona: generatedPersona)) }
        f["sessionTokenMaxAgeSec"] = .integer(1); f["sessionToken"] = .string("token\r\nheader")
        XCTAssertThrowsError(try V4vLoginResult(.object(f), persona: generatedPersona))
    }
    func testChallengeRequestCannotCarryAccountBearerOrFollowSuggestedRelay() throws {
        let call = V4vHTTPCall(loginChallenge: ())
        let request = try call.urlRequest(profile: V4vFixture.profile())
        XCTAssertEqual(request.url?.absoluteString, "https://api.example.invalid/api/auth/nostr/challenge")
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization")); XCTAssertNil(request.httpBody)
    }
    func testCredentialStoreRetainsExactClientBytesAndForgetDoesNotTouchPublishJournal() throws {
        let journal = V4vMemoryJournal(), store = V4vSignetCredentialStore(journal: journal)
        XCTAssertNil(try store.load()); let generated = Data("generated-client-credential".utf8)
        try store.save(generated); XCTAssertEqual(try store.load(), generated)
        try store.forget(); XCTAssertNil(try store.load())
        XCTAssertThrowsError(try store.save(Data(repeating: 0, count: 16385)))
    }
    func testExplicitLoginPersistsClientAndClosesBothOwnersBeforeAdoption() throws {
        let signer = LoginSigner(), journal = V4vMemoryJournal(), done = expectation(description: "closed then adopted")
        var http: LoginHTTP?, owner: V4vSignetLoginOwner!
        owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: journal), delivery: .main, makeSigner: { saved, _ in XCTAssertNil(saved); return signer }, makeHTTP: { http = LoginHTTP($0); return http! }, commit: { result in
            XCTAssertEqual(result.persona, generatedPersona); XCTAssertTrue(signer.cancelled); XCTAssertTrue(owner.cleanupComplete)
            XCTAssertEqual(try V4vSignetCredentialStore(journal: journal).load(), Data("generated-credential".utf8))
        })
        XCTAssertTrue(owner.start(restoring: false) { result in if case .failure = result { XCTFail("unexpected login failure") }; done.fulfill() })
        XCTAssertFalse(owner.start(restoring: false) { _ in XCTFail("duplicate") })
        wait(for: [done], timeout: 3); XCTAssertEqual(http?.calls.count, 2)
    }
    func testMissingRestoreCredentialDoesNotRepairOrCreateFreshIdentity() throws {
        let done = expectation(description: "fail closed")
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in XCTFail("must not pair"); return LoginSigner() }, makeHTTP: { LoginHTTP($0) }, commit: { _ in XCTFail("must not adopt") })
        XCTAssertTrue(owner.start(restoring: true) { result in if case .success = result { XCTFail("missing restore succeeded") }; done.fulfill() })
        wait(for: [done], timeout: 3)
    }
    func testCancelDuringConsentDiscardsLatePersonaAndNeverStartsHTTP() throws {
        let entered = expectation(description: "consent"), done = expectation(description: "cancelled")
        let gate = DispatchSemaphore(value: 0), signer = LoginSigner()
        signer.onConnect = { entered.fulfill(); _ = gate.wait(timeout: .now() + 2) }
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in signer }, makeHTTP: { _ in XCTFail("late HTTP"); return LoginHTTP({}) }, commit: { _ in XCTFail("late adoption") })
        XCTAssertTrue(owner.start(restoring: false) { _ in done.fulfill() })
        wait(for: [entered], timeout: 2); for _ in 0..<100 { owner.cancel() }; gate.signal(); wait(for: [done], timeout: 3)
        XCTAssertTrue(signer.cancelled); XCTAssertTrue(owner.cleanupComplete)
    }
    func testLateLoginReplyAfterCancelCannotAdoptToken() throws {
        let reached = expectation(description: "login"), done = expectation(description: "cancelled"), signer = LoginSigner()
        var reply: ((Result<V4vJSON, V4vFailure>) -> Void)?
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in signer }, makeHTTP: { closed in
            let http = LoginHTTP(closed); http.responder = { count, callback in
                if count == 1 { callback(.success(try! V4vJSON.parse(Data(#"{"ok":true,"enabled":true,"registration":"closed","appName":"Generated test","challenge":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.utf8)))) }
                else { reply = callback; reached.fulfill() }
            }; return http
        }, commit: { _ in XCTFail("late token adopted") })
        XCTAssertTrue(owner.start(restoring: false) { _ in done.fulfill() }); wait(for: [reached], timeout: 2)
        owner.cancel(); reply?(.success(generatedLogin())); wait(for: [done], timeout: 3)
    }
    func testHTTPInvalidationMustActuallyCompleteBeforeTokenAdoption() throws {
        let closing = expectation(description: "HTTP closing"), done = expectation(description: "actual closed")
        var held: LoginHTTP?, adopted = false
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in LoginSigner() }, makeHTTP: { closed in
            let http = LoginHTTP(closed); http.delayClose = true; held = http
            http.responder = { count, callback in
                if count == 1 { callback(.success(try! V4vJSON.parse(Data(#"{"ok":true,"enabled":true,"registration":"closed","appName":"Generated test","challenge":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.utf8)))) }
                else { callback(.success(generatedLogin())); closing.fulfill() }
            }; return http
        }, commit: { _ in adopted = true })
        XCTAssertTrue(owner.start(restoring: false) { _ in done.fulfill() })
        wait(for: [closing], timeout: 2)
        XCTAssertFalse(adopted); XCTAssertFalse(owner.cleanupComplete)
        held?.onClosed(); wait(for: [done], timeout: 3); XCTAssertTrue(adopted)
    }
    func testOriginalDeadlineAndClockRollbackDiscardDelayedChallenge() throws {
        for rollback in [false, true] {
            let reached = expectation(description: "challenge"), done = expectation(description: "deadline")
            var now: UInt64 = 100_000_000, callback: ((Result<V4vJSON, V4vFailure>) -> Void)?
            let signer = LoginSigner(); signer.onSign = { XCTFail("expired proof request") }
            let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in signer }, makeHTTP: { closed in
                let http = LoginHTTP(closed); http.responder = { _, reply in callback = reply; reached.fulfill() }; return http
            }, clock: { now }, commit: { _ in XCTFail("expired adoption") })
            XCTAssertTrue(owner.start(restoring: false, timeoutMs: 1000) { _ in done.fulfill() }); wait(for: [reached], timeout: 2)
            now = rollback ? 99_000_000 : 1_100_000_000
            callback?(.success(try V4vJSON.parse(Data(#"{"ok":true,"enabled":true,"registration":"closed","appName":"Generated test","challenge":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.utf8))))
            wait(for: [done], timeout: 3)
        }
    }
    func testCancellingDormantOwnerCannotReleaseAnotherActiveOwnersPermit() throws {
        let entered = expectation(description: "active consent"), done = expectation(description: "cancel")
        let gate = DispatchSemaphore(value: 0), signer = LoginSigner()
        signer.onConnect = { entered.fulfill(); _ = gate.wait(timeout: .now() + 2) }
        func make(_ signer: LoginSigner) throws -> V4vSignetLoginOwner {
            V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in signer }, makeHTTP: { LoginHTTP($0) }, commit: { _ in XCTFail("cancelled") })
        }
        let active = try make(signer), dormant = try make(LoginSigner()), successor = try make(LoginSigner())
        XCTAssertTrue(active.start(restoring: false) { _ in done.fulfill() }); wait(for: [entered], timeout: 2)
        dormant.cancel()
        XCTAssertFalse(successor.start(restoring: false) { _ in XCTFail("concurrent owner") })
        active.cancel(); gate.signal(); wait(for: [done], timeout: 3)
    }

    func testLateActualHTTPCloseAfterFailureReleasesCapacityWithoutLateAdoption() throws {
        let failure = expectation(description: "bounded cleanup failure"), successorDone = expectation(description: "successor cleaned")
        var held: LoginHTTP?, callbacks = 0
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in LoginSigner() }, makeHTTP: { closed in
            let http = LoginHTTP(closed); http.delayClose = true; held = http; return http
        }, commit: { _ in XCTFail("late token must not be adopted") })
        XCTAssertTrue(owner.start(restoring: false) { result in callbacks += 1; if case .success = result { XCTFail("cleanup was not confirmed") }; failure.fulfill() })
        wait(for: [failure], timeout: 4); XCTAssertFalse(owner.cleanupComplete)
        held?.onClosed()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            XCTAssertTrue(owner.cleanupComplete); XCTAssertEqual(callbacks, 1)
            let successor = V4vSignetLoginOwner(profile: try! V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in LoginSigner() }, makeHTTP: { LoginHTTP($0) }, commit: { _ in XCTFail("cancelled") })
            XCTAssertTrue(successor.start(restoring: false) { _ in successorDone.fulfill() }); successor.cancel()
        }
        wait(for: [successorDone], timeout: 3); XCTAssertEqual(callbacks, 1)
    }

    func testMintLifetimeSubtractsWholeRequestCleanupDeliveryAndIntegerSecondMargin() throws {
        let result = try V4vLoginResult(generatedLogin(), persona: generatedPersona)
        let trimmed = try result.remaining(postStartedNs: 10_000_000_000, nowNs: 12_200_000_000)
        XCTAssertEqual(trimmed.maxAgeSeconds, 86396)
        XCTAssertThrowsError(try result.remaining(postStartedNs: 10, nowNs: 9))
        XCTAssertEqual(try result.remaining(postStartedNs: 10, nowNs: 10).maxAgeSeconds, 86399)
    }
    func testShortRemoteTokenExpiresDuringActualCloseAndCannotGainFreshLifetime() throws {
        let closing = expectation(description: "close pending"), done = expectation(description: "expired token")
        var now: UInt64 = 10_000_000_000, held: LoginHTTP?
        let owner = V4vSignetLoginOwner(profile: try V4vFixture.profile(), credentials: V4vSignetCredentialStore(journal: V4vMemoryJournal()), delivery: .main, makeSigner: { _, _ in LoginSigner() }, makeHTTP: { closed in
            let http = LoginHTTP(closed); held = http; http.delayClose = true
            http.responder = { count, callback in
                if count == 1 { callback(.success(try! V4vJSON.parse(Data(#"{"ok":true,"enabled":true,"registration":"closed","appName":"Generated test","challenge":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#.utf8)))) }
                else {
                    var f = try! generatedLogin().object(required: ["ok", "pubkey", "registered", "operator", "sessionToken", "sessionTokenMaxAgeSec"])
                    f["sessionTokenMaxAgeSec"] = .integer(1); callback(.success(.object(f))); closing.fulfill()
                }
            }; return http
        }, clock: { now }, commit: { _ in XCTFail("expired token adopted") })
        XCTAssertTrue(owner.start(restoring: false) { result in if case .success = result { XCTFail("expired token") }; done.fulfill() })
        wait(for: [closing], timeout: 2); now = 11_100_000_000; held?.onClosed(); wait(for: [done], timeout: 3)
        XCTAssertTrue(owner.cleanupComplete)
    }

}
