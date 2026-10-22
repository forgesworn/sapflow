import XCTest
@testable import SapflowBroadcaster

final class V4vFakeJournalKey: V4vJournalKeySource {
    var key: Data?
    var failed = false
    func load() throws -> Data? { if failed { throw V4vFailure.journal }; return key }
    func create() throws -> Data { guard key == nil, !failed else { throw V4vFailure.journal }; let value = Data(repeating: 42, count: 32); key = value; return value }
}

final class V4vAdaptersTests: XCTestCase {
    func testShowLeaseWritesWhileKeySourceIsLockedAndReleasesExactlyItsOwner() throws {
        let dir = try directory(), keys = V4vFakeJournalKey()
        let store = try V4vEncryptedJournal(directory: dir, profile: V4vFixture.profile(), keys: keys)
        try store.save(.object(["version": .integer(1)]))
        let a = UUID(), b = UUID()
        try store.acquireShowLease(owner: a)
        XCTAssertNoThrow(try store.acquireShowLease(owner: a))
        XCTAssertThrowsError(try store.acquireShowLease(owner: b))
        keys.failed = true
        store.releaseShowLease(owner: b)
        let document = V4vJSON.object(["generated-locked-refresh": .integer(2)])
        try store.save(document); XCTAssertEqual(try store.load(), document)
        store.releaseShowLease(owner: a)
        XCTAssertThrowsError(try store.save(.object(["after-cleanup": .integer(3)])))
        XCTAssertEqual(try Data(contentsOf: dir.appendingPathComponent("session.v1.sealed")).isEmpty, false)
    }
    func testLeaseCannotCreateOrRepairJournalAndIsNotRestoredIntoAnotherOwner() throws {
        let dir = try directory(), keys = V4vFakeJournalKey(), profile = try V4vFixture.profile()
        var store: V4vEncryptedJournal? = try V4vEncryptedJournal(directory: dir, profile: profile, keys: keys)
        XCTAssertThrowsError(try XCTUnwrap(store).acquireShowLease(owner: UUID()))
        XCTAssertNil(keys.key); XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: dir.path).isEmpty)
        try XCTUnwrap(store).save(.object(["generated": .bool(true)]))
        try XCTUnwrap(store).acquireShowLease(owner: UUID())
        store = nil; keys.failed = true
        let restored = try V4vEncryptedJournal(directory: dir, profile: profile, keys: keys)
        XCTAssertThrowsError(try restored.load())
        XCTAssertThrowsError(try restored.acquireShowLease(owner: UUID()))
    }
    func testOldLeaseReleaseCannotReleaseSuccessorLeaseOrUnlockSignerJournal() throws {
        let keys = V4vFakeJournalKey(), profile = try V4vFixture.profile()
        let publishing = try V4vEncryptedJournal(directory: directory(), profile: profile, keys: keys)
        try publishing.save(.object(["publish": .bool(true)]))
        // A separate generated key source reflects the separate production
        // signer Keychain service, without accessing any real Keychain.
        let signerKeys = V4vFakeJournalKey()
        let signer = try V4vEncryptedJournal(directory: directory(), profile: profile, keys: signerKeys)
        try signer.save(.object(["signer": .bool(true)]))
        let old = UUID(), current = UUID()
        try publishing.acquireShowLease(owner: old); publishing.releaseShowLease(owner: old)
        try publishing.acquireShowLease(owner: current); keys.failed = true; signerKeys.failed = true
        publishing.releaseShowLease(owner: old)
        XCTAssertNoThrow(try publishing.save(.object(["refresh": .integer(2)])))
        XCTAssertThrowsError(try signer.save(.object(["unexpected-sign-in": .bool(true)])))
        publishing.releaseShowLease(owner: current)
        XCTAssertThrowsError(try publishing.load())
    }
    private func directory() throws -> URL {
        // Only this test's generated files, never a user's Keychain/app files.
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-v4v-test-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        addTeardownBlock { try FileManager.default.removeItem(at: directory) }
        return directory
    }
    func testEncryptedJournalRoundTripAndPlaintextSecretsAreNotWritten() throws {
        let dir = try directory(), key = V4vFakeJournalKey(), profile = try V4vFixture.profile()
        var store: V4vEncryptedJournal? = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        XCTAssertNil(try XCTUnwrap(store).load())
        let document = V4vJSON.object(["synthetic-secret": .string("test-secret-not-account-data")])
        try XCTUnwrap(store).save(document)
        XCTAssertEqual(try XCTUnwrap(store).load(), document)
        let bytes = try Data(contentsOf: dir.appendingPathComponent("session.v1.sealed"))
        XCTAssertNil(bytes.range(of: Data("test-secret-not-account-data".utf8)))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: dir.path), ["session.v1.sealed"])
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: profile, keys: key))
        store = nil
        let reopened = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        XCTAssertEqual(try reopened.load(), document)
        try reopened.save(.object(["new": .bool(true)]))
        XCTAssertEqual(try reopened.load(), .object(["new": .bool(true)]))
    }
    func testMissingKeyOrMissingExistingFileCannotStartFresh() throws {
        let dir = try directory(), key = V4vFakeJournalKey(), profile = try V4vFixture.profile()
        var store: V4vEncryptedJournal? = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        try XCTUnwrap(store).save(.object(["test": .integer(1)]))
        store = nil
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: profile, keys: V4vFakeJournalKey()).load())
        try FileManager.default.removeItem(at: dir.appendingPathComponent("session.v1.sealed"))
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: profile, keys: key).load())
    }
    func testCorruptionProfileMismatchAndLockedKeyPoisonStoreWithoutRepair() throws {
        let dir = try directory(), key = V4vFakeJournalKey(), profile = try V4vFixture.profile()
        var store: V4vEncryptedJournal? = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        try XCTUnwrap(store).save(.object(["test": .integer(1)]))
        store = nil
        let foreign = try V4vConnectorProfile(apiOrigin: "https://foreign.example.invalid", playbackOrigin: "https://media.example.invalid", playbackPrefix: "/")
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: foreign, keys: key).load())
        store = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        key.failed = true
        XCTAssertThrowsError(try XCTUnwrap(store).load()); key.failed = false
        XCTAssertThrowsError(try XCTUnwrap(store).save(.object([:])))
        store = nil
        let path = dir.appendingPathComponent("session.v1.sealed")
        var bytes = try Data(contentsOf: path); bytes[bytes.count - 1] ^= 1; try bytes.write(to: path)
        let corrupt = try V4vEncryptedJournal(directory: dir, profile: profile, keys: key)
        XCTAssertThrowsError(try corrupt.load())
        XCTAssertThrowsError(try corrupt.save(.object([:])))
        XCTAssertEqual(try Data(contentsOf: path), bytes)
    }
    func testSymlinkAndOversizeFilesAreRejectedWithoutFollowingOrReplacing() throws {
        let dir = try directory(), target = dir.appendingPathComponent("unrelated-test-file"), profile = try V4vFixture.profile()
        try Data([1, 2, 3]).write(to: target)
        let path = dir.appendingPathComponent("session.v1.sealed")
        try FileManager.default.createSymbolicLink(at: path, withDestinationURL: target)
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: profile, keys: V4vFakeJournalKey()).load())
        XCTAssertEqual(try Data(contentsOf: target), Data([1, 2, 3]))
        try FileManager.default.removeItem(at: path)
        try Data(repeating: 0, count: 262_145).write(to: path)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path.path)
        XCTAssertThrowsError(try V4vEncryptedJournal(directory: dir, profile: profile, keys: V4vFakeJournalKey()).load())
    }
    func testActualRequestBuilderPinsProfileRouteAndDoesNotUseCookiesOrSecretsInDebug() throws {
        let op = try V4vOperation(kind: .end, event: V4vFixture.event, session: V4vFixture.sid, body: .object(["revision": .integer(1), "idempotencyKey": .string(V4vFixture.key)]))
        let call = V4vHTTPCall(V4vControlRequest(operation: op, bearer: "synthetic-test-bearer"))
        let request = try call.urlRequest(profile: V4vFixture.profile())
        XCTAssertEqual(request.url?.absoluteString, "https://api.example.invalid" + op.path)
        XCTAssertEqual(request.httpMethod, "POST"); XCTAssertEqual(request.httpBody, op.bytes)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer synthetic-test-bearer")
        XCTAssertEqual(request.timeoutInterval, 8)
        XCTAssertFalse(request.httpShouldHandleCookies)
        XCTAssertFalse(String(reflecting: call).contains("synthetic-test-bearer"))
        XCTAssertThrowsError(try V4vHTTPCall(V4vControlRequest(operation: op, bearer: "bad\r\nheader")).urlRequest(profile: V4vFixture.profile()))
        XCTAssertThrowsError(try V4vHTTPCall(current: "event-x/../../admin", bearer: "synthetic-test-bearer"))
    }
    func testResponseAccumulatorRejectsBoundBeforeAppendAndStatusBeforeBody() throws {
        var body = V4vResponseBody()
        try body.append(Data(repeating: 32, count: 65_536))
        XCTAssertThrowsError(try body.append(Data([32])))
        XCTAssertEqual(body.bytes.count, 65_536)
        XCTAssertNoThrow(try V4vResponseBody.check(status: 200, expectedLength: -1))
        for status in [301, 302, 307, 401, 403, 409, 429, 500] { XCTAssertThrowsError(try V4vResponseBody.check(status: status, expectedLength: -1)) }
        XCTAssertThrowsError(try V4vResponseBody.check(status: 200, expectedLength: 65_537))
    }
    func testActualClientCloseAndConcurrentInvalidCallsHaveNoNetworkOrLostCompletion() throws {
        let client = V4vControlClient(profile: try V4vFixture.profile())
        let op = try V4vOperation(kind: .end, event: V4vFixture.event, session: V4vFixture.sid, body: .object(["revision": .integer(1), "idempotencyKey": .string(V4vFixture.key)]))
        // Fails in the real builder BEFORE any URLSession task is created.
        let invalid = V4vHTTPCall(V4vControlRequest(operation: op, bearer: "bad\rheader"))
        let all = expectation(description: "bounded completions"); all.expectedFulfillmentCount = 100
        DispatchQueue.concurrentPerform(iterations: 100) { _ in
            client.execute(invalid) { result in
                if case .success = result { XCTFail("Invalid request must not execute") }
                all.fulfill()
            }
        }
        wait(for: [all], timeout: 3)
        client.close()
        let closed = expectation(description: "synchronous close admission")
        client.execute(invalid) { result in
            guard case .failure(.closed) = result else { XCTFail("Close must fence admission synchronously"); closed.fulfill(); return }
            closed.fulfill()
        }
        wait(for: [closed], timeout: 3)
    }
    func testDirectoryReplacementCannotBypassHeldJournalOwnership() throws {
        let parent = try directory(), app = parent.appendingPathComponent("app"), old = parent.appendingPathComponent("old")
        let profile = try V4vFixture.profile(), key = V4vFakeJournalKey()
        let journal = try V4vEncryptedJournal(directory: app, profile: profile, keys: key)
        try journal.save(.object(["test": .integer(1)]))
        try FileManager.default.moveItem(at: app, to: old)
        try FileManager.default.createDirectory(at: app, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        XCTAssertThrowsError(try journal.load())
        XCTAssertThrowsError(try journal.save(.object([:])))
        XCTAssertTrue(FileManager.default.fileExists(atPath: old.appendingPathComponent("session.v1.sealed").path))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: app.path), [])
    }
    func testJournalServiceConstructorUsesWholeStringWithoutAccessingKeychain() throws {
        XCTAssertNoThrow(try V4vKeychainJournalKey(service: "dev.example.journal"))
        XCTAssertThrowsError(try V4vKeychainJournalKey(service: "dev.example.journal\n"))
        XCTAssertThrowsError(try V4vKeychainJournalKey(service: "bad service"))
    }
}
