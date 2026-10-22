import XCTest
@testable import SapflowBroadcaster

private final class ManualImportStore: ManualDestinationProfileStoring {
    var loads = 0, secretReads = 0, saves = 0, deletes = 0
    var failed = false
    var beforeSecret: (() -> Void)?
    var profiles = [SavedDestinationProfile(id: UUID(), name: "Saved", endpoint: "rtmps://stream.example.invalid/live", allowPlaintext: false, transport: "rtmp")]
    func load() throws -> [SavedDestinationProfile] { loads += 1; if failed { throw DestinationError.storage }; return profiles }
    func key(for id: UUID) throws -> String { secretReads += 1; beforeSecret?(); if failed { throw DestinationError.keychain }; return "saved-key" }
    func srtSecret(for id: UUID) throws -> (streamID: String, passphrase: String) { secretReads += 1; return ("saved-id", "saved-passphrase") }
    func resolve(expected: SavedDestinationProfile) throws -> DestinationProfileSecret {
        secretReads += 1; beforeSecret?(); if failed { throw DestinationError.keychain }
        return expected.transport == "srt" ? DestinationProfileSecret(streamKey: "", streamID: "saved-id", passphrase: "saved-passphrase") :
            DestinationProfileSecret(streamKey: "saved-key", streamID: "", passphrase: "")
    }
    func save(id: UUID?, name: String, destination: ManualDestination, allowPlaintext: Bool) throws -> UUID { saves += 1; return id ?? UUID() }
    func saveSrt(id: UUID?, name: String, destination: ManualSrtDestination, allowPlaintext: Bool) throws -> UUID { saves += 1; return id ?? UUID() }
    func delete(_ id: UUID) throws { deletes += 1 }
}
private final class ManualSecretPort: DestinationSecretStoring {
    var values: [UUID: Data] = [:]
    var writes = 0
    func read(_ id: UUID) throws -> Data { guard let data = values[id] else { throw DestinationError.keychain }; return data }
    func write(_ data: Data, for id: UUID) throws { writes += 1; values[id] = data }
    func delete(_ id: UUID) throws { values.removeValue(forKey: id) }
}
private final class ManualImportFixture {
    var available = true, jobs: [() -> Void] = [], replies: [() -> Void] = []
    let store = ManualImportStore()
    var settings: ManualDestinationSettings!
    var directory: URL
    init(reader: ManualDestinationFileReader = ManualDestinationFileReader()) throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-manual-import-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        settings = ManualDestinationSettings(store: store, available: { [unowned self] in self.available }, reader: reader,
            work: { [unowned self] in self.jobs.append($0) }, delivery: { [unowned self] in self.replies.append($0) })
    }
    deinit { try? FileManager.default.removeItem(at: directory) }
    func file(_ data: Data = ManualDestinationImportTests.document()) throws -> URL {
        let url = directory.appendingPathComponent(UUID().uuidString + ".json"); try data.write(to: url); return url
    }
    func receive(_ data: Data = ManualDestinationImportTests.document()) throws -> ManualDestinationSettings.Ticket {
        let ticket = try XCTUnwrap(settings.beginImport()); settings.received(ticket, file: .success(try file(data))); return ticket
    }
    func finishRead() { let work = jobs.removeFirst(); work(); let reply = replies.removeFirst(); reply() }
}
final class ManualDestinationImportTests: XCTestCase {
    static func document(_ edits: [String: Any] = [:]) -> Data {
        var fields: [String: Any] = ["version": 1, "kind": "sapflow-manual-destination", "name": " Example ", "transport": "rtmp", "endpoint": "rtmps://stream.example.invalid/private-app", "streamKey": "private-key", "streamID": "", "passphrase": "", "allowPlaintext": false]
        for (key, value) in edits { fields[key] = value }
        return (try? JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])) ?? Data()
    }
    func testSharedSyntheticContractVectors() throws {
        struct Vector: Decodable { let id: String, accepted: Bool, json: String }
        struct Corpus: Decodable { let version: Int, cases: [Vector] }
        let fixture = ProcessInfo.processInfo.environment["SAPFLOW_MANUAL_PROFILE_VECTORS"].map { URL(fileURLWithPath: $0) } ??
            Bundle(for: ManualDestinationImportTests.self).url(forResource: "manual-destination-v1", withExtension: "json")
        let corpus = try JSONDecoder().decode(Corpus.self, from: Data(contentsOf: XCTUnwrap(fixture)))
        XCTAssertEqual(corpus.version, 1); XCTAssertGreaterThanOrEqual(corpus.cases.count, 42)
        for vector in corpus.cases {
            if vector.accepted { XCTAssertNoThrow(try ImportedManualDestination(Data(vector.json.utf8)), vector.id) }
            else { XCTAssertThrowsError(try ImportedManualDestination(Data(vector.json.utf8)), vector.id) }
        }
    }
    func testStrictSyntaxLexicalVersionAndDecodedDuplicateKeys() {
        let source = String(decoding: Self.document(), as: UTF8.self)
        for value in ["1.0", "1e0", "true", "\"1\""] {
            XCTAssertThrowsError(try ImportedManualDestination(Data(source.replacingOccurrences(of: "\"version\":1", with: "\"version\":" + value).utf8)))
        }
        XCTAssertThrowsError(try ImportedManualDestination(Data((source.dropLast() + ",\"\\u006eame\":\"other\"}").utf8)))
        XCTAssertThrowsError(try ImportedManualDestination(Data([0xff, 0xfe])))
        XCTAssertThrowsError(try ImportedManualDestination(Self.document(["name": String(repeating: "a", count: 49)])))
    }
    func testExactWhitespaceAndUnicodeControlValidation() throws {
        XCTAssertEqual(try ImportedManualDestination(Self.document()).name, "Example")
        for control in ["\u{0085}", "\u{202e}", "\u{200d}", "\n"] {
            for field in ["endpoint", "streamKey", "name"] {
                let original = field == "endpoint" ? "rtmps://stream.example.invalid/live" : "example"
                XCTAssertThrowsError(try ImportedManualDestination(Self.document([field: original + control])))
            }
            XCTAssertThrowsError(try ImportedManualDestination(Self.document(["transport": "srt", "endpoint": "srt://192.0.2.1:9000", "streamKey": "", "streamID": "id" + control, "passphrase": "generated-secret"])))
        }
        for endpoint in [" rtmps://stream.example.invalid/live", "rtmps://stream.example.invalid/live "] {
            XCTAssertThrowsError(try ImportedManualDestination(Self.document(["endpoint": endpoint])))
        }
        XCTAssertThrowsError(try ImportedManualDestination(Self.document(["name": "\nExample"])))
    }
    func testRedactedReviewAndExplicitApplyNeverPersist() throws {
        let f = try ManualImportFixture(); let ticket = try f.receive(Self.document(["name": "private-name-token"])); f.finishRead()
        let review = try XCTUnwrap(f.settings.review)
        XCTAssertEqual(review.peer, "stream.example.invalid:443")
        XCTAssertFalse(String(describing: review).contains("private-app")); XCTAssertFalse(String(describing: review).contains("private-key"))
        XCTAssertFalse(String(describing: review).contains("private-name-token"))
        XCTAssertFalse(String(describing: try ImportedManualDestination(Self.document())).contains("private-key"))
        XCTAssertEqual(f.settings.endpoint, ""); XCTAssertEqual(f.store.saves, 0)
        XCTAssertTrue(f.settings.apply(ticket, plaintextConfirmed: false))
        XCTAssertEqual(f.settings.streamKey, "private-key"); XCTAssertEqual(f.store.saves, 0)
        XCTAssertEqual(f.settings.profileName, "", "Imported names must not become cleartext form labels")
        XCTAssertFalse(f.settings.apply(ticket, plaintextConfirmed: true))
    }
    func testPlaintextFileCannotProvideConsentAndSecureFileCannotEnableIt() throws {
        let f = try ManualImportFixture()
        var ticket = try f.receive(Self.document(["endpoint": "rtmp://stream.example.invalid/live", "allowPlaintext": true])); f.finishRead()
        XCTAssertFalse(f.settings.apply(ticket, plaintextConfirmed: false)); XCTAssertEqual(f.settings.endpoint, "")
        XCTAssertTrue(f.settings.apply(ticket, plaintextConfirmed: true)); XCTAssertTrue(f.settings.allowPlaintext)
        ticket = try f.receive(Self.document(["allowPlaintext": true])); f.finishRead()
        XCTAssertTrue(f.settings.apply(ticket, plaintextConfirmed: false)); XCTAssertFalse(f.settings.allowPlaintext)
    }
    func testSrtImportClearsOtherProtocolAndRequiresIndependentPlaintextPermission() throws {
        let f = try ManualImportFixture(); f.settings.streamKey = "old"
        let ticket = try f.receive(Self.document(["transport": "srt", "endpoint": "srt://192.0.2.1:9000", "streamKey": "", "passphrase": "", "streamID": "id", "allowPlaintext": true])); f.finishRead()
        XCTAssertFalse(f.settings.apply(ticket, plaintextConfirmed: false)); XCTAssertTrue(f.settings.apply(ticket, plaintextConfirmed: true))
        XCTAssertEqual(f.settings.streamKey, ""); XCTAssertTrue(f.settings.srtPlaintext); XCTAssertFalse(f.settings.allowPlaintext)
    }
    func testCancelledPlaintextReviewCannotAuthoriseSuccessor() throws {
        let f = try ManualImportFixture()
        let data = Self.document(["endpoint": "rtmp://stream.example.invalid/live", "allowPlaintext": true])
        let a = try f.receive(data); f.finishRead(); f.settings.cancelReview(a)
        let b = try f.receive(data); f.finishRead()
        XCTAssertNotEqual(a, b)
        XCTAssertFalse(f.settings.apply(a, plaintextConfirmed: true))
        XCTAssertFalse(f.settings.apply(b, plaintextConfirmed: false))
        XCTAssertEqual(f.settings.endpoint, ""); XCTAssertFalse(f.settings.allowPlaintext)
        XCTAssertTrue(f.settings.apply(b, plaintextConfirmed: true))
    }
    func testDismissalBeforePickerReplyPreservesOriginalTicketAndLateARejectsB() throws {
        let f = try ManualImportFixture(); let a = try XCTUnwrap(f.settings.beginImport())
        f.settings.pickerDismissed(a); f.settings.received(a, file: .success(try f.file())); f.finishRead()
        XCTAssertNotNil(f.settings.review)
        f.settings.cancelReview(a); let b = try XCTUnwrap(f.settings.beginImport())
        f.settings.received(a, file: .success(try f.file()))
        f.settings.pickerDismissed(a)
        XCTAssertEqual(f.settings.presentation, b); XCTAssertTrue(f.settings.pickerVisible); XCTAssertTrue(f.jobs.isEmpty)
    }
    func testEditDuringReadFencesReplyAndCapacityWaitsForActualClose() throws {
        let f = try ManualImportFixture(); _ = try f.receive()
        f.settings.endpoint = "new"
        XCTAssertNil(f.settings.beginImport()); XCTAssertTrue(f.settings.reading)
        f.finishRead(); XCTAssertNil(f.settings.review); XCTAssertEqual(f.settings.endpoint, "new")
        XCTAssertNotNil(f.settings.beginImport())
    }
    func testCaptureAndModeForegroundInvalidationRejectReadAndApply() throws {
        let f = try ManualImportFixture(); var ticket = try f.receive()
        f.available = false; f.settings.captureChanged(); f.finishRead()
        XCTAssertNil(f.settings.review); XCTAssertFalse(f.settings.apply(ticket, plaintextConfirmed: true))
        f.available = true; ticket = try f.receive(); f.finishRead()
        f.settings.invalidate() // model mode/foreground/Prepare calls the same cold fence
        XCTAssertFalse(f.settings.apply(ticket, plaintextConfirmed: true)); XCTAssertEqual(f.settings.endpoint, "")
    }
    func testPreparedAvailabilityBlocksAllProfileActionsAtModelBoundary() throws {
        let f = try ManualImportFixture(); let id = try XCTUnwrap(f.store.profiles.first?.id)
        f.settings.selectedProfile = id; f.settings.endpoint = "rtmps://stream.example.invalid/live"; f.settings.streamKey = "key"
        f.available = false
        f.settings.loadProfile(id); f.settings.saveProfile(); f.settings.deleteProfile()
        XCTAssertEqual(f.store.secretReads, 0); XCTAssertEqual(f.store.saves, 0); XCTAssertEqual(f.store.deletes, 0)
        XCTAssertNil(f.settings.beginImport())
    }
    func testAvailabilityChangedDuringSecretReadCannotMutateForm() throws {
        let f = try ManualImportFixture(); f.settings.endpoint = "unchanged"
        f.store.beforeSecret = { f.available = false }
        f.settings.loadProfile(f.store.profiles.first?.id)
        f.store.beforeSecret = nil
        XCTAssertEqual(f.settings.endpoint, "unchanged")
    }
    func testFailedLoadLeavesAllFieldsAndSecretsUnchanged() throws {
        let f = try ManualImportFixture(); f.settings.streamKey = "keep"; f.settings.endpoint = "keep-endpoint"; f.store.failed = true
        f.settings.loadProfile(f.store.profiles.first?.id)
        XCTAssertEqual(f.settings.streamKey, "keep"); XCTAssertEqual(f.settings.endpoint, "keep-endpoint")
        XCTAssertFalse(f.settings.error?.contains("keep") ?? true)
    }
    func testReaderBoundsEmptyOversizedAndMalformedFile() throws {
        let f = try ManualImportFixture()
        for data in [Data(), Data(repeating: 32, count: 16_385), Data([0xff])] {
            _ = try f.receive(data); f.finishRead(); XCTAssertNil(f.settings.review)
        }
        XCTAssertFalse(f.settings.reading)
    }
    func testSecurityScopeOutlivesReadAndEndsOnlyAfterClose() throws {
        var events: [String] = []
        let reader = ManualDestinationFileReader(access: { _ in events.append("access"); return { events.append("release") } }, close: { handle in events.append("close"); try handle.close() })
        let f = try ManualImportFixture(reader: reader); _ = try f.receive()
        XCTAssertTrue(events.isEmpty)
        let job = f.jobs.removeFirst(); job()
        XCTAssertEqual(events, ["access", "close", "release"]); XCTAssertTrue(f.settings.reading)
        f.replies.removeFirst()(); XCTAssertFalse(f.settings.reading)
    }
    func testUncertainCloseNeverReleasesScopeOrAdmitsAnotherRead() throws {
        var accesses = 0, releases = 0
        // Close the generated handle, then report an ambiguous close result.
        // No actual OS/security resource is leaked by this negative fixture.
        let reader = ManualDestinationFileReader(access: { _ in accesses += 1; return { releases += 1 } }, close: { handle in
            try handle.close(); throw ManualImportError.unavailable
        })
        let f = try ManualImportFixture()
        let url = try f.file()
        let first = reader.read(url)
        XCTAssertFalse(first.cleanupComplete); XCTAssertEqual(accesses, 1); XCTAssertEqual(releases, 0)
        if case .success = first.result { XCTFail("An uncertain close cannot expose an import") }
        let second = reader.read(url)
        XCTAssertFalse(second.cleanupComplete); XCTAssertEqual(accesses, 1); XCTAssertEqual(releases, 0)
    }
    func testCorruptMetadataCannotSilentlyResetOrDeleteSavedSecrets() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let corrupt = Data("not json".utf8); defaults.set(corrupt, forKey: "sapflow.manual-destinations.v1")
        let store = DestinationProfileStore(defaults: defaults)
        XCTAssertThrowsError(try store.load())
        XCTAssertThrowsError(try store.save(id: nil, name: "New", destination: ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "key", allowPlaintext: false), allowPlaintext: false))
        XCTAssertThrowsError(try store.delete(UUID()))
        XCTAssertEqual(defaults.data(forKey: "sapflow.manual-destinations.v1"), corrupt)
    }
    func testDuplicateMetadataIdentitiesAndWrongStorageTypeRefuse() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let profile = SavedDestinationProfile(id: UUID(), name: "Saved", endpoint: "rtmps://stream.example.invalid/live", allowPlaintext: false)
        defaults.set(try JSONEncoder().encode([profile, profile]), forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try DestinationProfileStore(defaults: defaults).load())
        defaults.set("unexpected", forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try DestinationProfileStore(defaults: defaults).load())
    }
    func testInterruptedProtectedSaveCannotPairNewSecretWithOldEndpoint() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let first = try ManualDestination(endpoint: "rtmps://first.example.invalid/live", streamKey: "first-key", allowPlaintext: false)
        let id = try store.save(id: nil, name: "First", destination: first, allowPlaintext: false)
        let oldMetadata = try XCTUnwrap(defaults.data(forKey: "sapflow.manual-destinations.v1"))
        let second = try ManualDestination(endpoint: "rtmps://second.example.invalid/live", streamKey: "second-key", allowPlaintext: false)
        _ = try store.save(id: id, name: "Second", destination: second, allowPlaintext: false)
        let committed = try XCTUnwrap(defaults.data(forKey: "sapflow.manual-destinations.v1"))
        XCTAssertEqual(try store.key(for: id), "second-key")
        // Simulate termination after the protected write but before metadata.
        defaults.set(oldMetadata, forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try store.key(for: id))
        let settings = ManualDestinationSettings(store: store, available: { true })
        settings.endpoint = "unchanged"; settings.streamKey = "unchanged-key"; settings.loadProfile(id)
        XCTAssertEqual(settings.endpoint, "unchanged"); XCTAssertEqual(settings.streamKey, "unchanged-key")
        defaults.set(committed, forKey: "sapflow.manual-destinations.v1")
        XCTAssertEqual(try store.key(for: id), "second-key")
    }
    func testProtectedSecretBindsTransportPlaintextAndIdentityButNotName() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let destination = try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "key", allowPlaintext: false)
        let id = try store.save(id: nil, name: "Original", destination: destination, allowPlaintext: false)
        let original = try XCTUnwrap(store.load().first)
        var changed = original; changed.allowPlaintext = true
        defaults.set(try JSONEncoder().encode([changed]), forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try store.key(for: id))
        changed = original; changed.transport = "srt"
        defaults.set(try JSONEncoder().encode([changed]), forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try store.srtSecret(for: id))
        let different = SavedDestinationProfile(id: UUID(), name: original.name, endpoint: original.endpoint, allowPlaintext: original.allowPlaintext, transport: original.transport)
        secrets.values[different.id] = secrets.values[id]
        defaults.set(try JSONEncoder().encode([different]), forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try store.key(for: different.id))
        changed = original; changed.name = "Renamed"
        defaults.set(try JSONEncoder().encode([changed]), forKey: "sapflow.manual-destinations.v1")
        XCTAssertEqual(try store.key(for: id), "key")
    }
    func testCachedProfileCannotBorrowValidSuccessorSecretForItsOldPeer() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let a = try ManualDestination(endpoint: "rtmps://first.example.invalid/live", streamKey: "first-key", allowPlaintext: false)
        let id = try store.save(id: nil, name: "A", destination: a, allowPlaintext: false)
        let settings = ManualDestinationSettings(store: store, available: { true })
        XCTAssertEqual(settings.profiles.first?.endpoint, a.endpoint)
        let b = try ManualDestination(endpoint: "rtmps://second.example.invalid/live", streamKey: "second-key", allowPlaintext: false)
        _ = try store.save(id: id, name: "B", destination: b, allowPlaintext: false)
        XCTAssertEqual(try store.key(for: id), "second-key", "The new protected pair is valid on its own")
        settings.endpoint = "unchanged"; settings.streamKey = "unchanged-key"
        settings.loadProfile(id)
        XCTAssertEqual(settings.endpoint, "unchanged"); XCTAssertEqual(settings.streamKey, "unchanged-key")
        XCTAssertNotNil(settings.error)
    }
    func testLegacyRawValuesRemainReadableAndExplicitSaveUpgradesThem() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let profile = SavedDestinationProfile(id: UUID(), name: "Legacy", endpoint: "rtmps://stream.example.invalid/live", allowPlaintext: false)
        defaults.set(try JSONEncoder().encode([profile]), forKey: "sapflow.manual-destinations.v1")
        secrets.values[profile.id] = Data("legacy-key".utf8)
        XCTAssertEqual(try store.key(for: profile.id), "legacy-key")
        _ = try store.save(id: profile.id, name: profile.name, destination: ManualDestination(endpoint: profile.endpoint, streamKey: "legacy-key", allowPlaintext: false), allowPlaintext: false)
        XCTAssertTrue(try XCTUnwrap(secrets.values[profile.id]).starts(with: Data("SAPFLOW-MANUAL-DESTINATION:1\n".utf8)))
        XCTAssertEqual(try store.key(for: profile.id), "legacy-key")
    }
    func testBoundSrtSecretAndLargeRtmpKeyRoundTrip() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let srt = try ManualSrtDestination(endpoint: "srt://192.0.2.1:9000", streamID: "generated-id", passphrase: "generated-secret", allowPlaintext: false)
        let id = try store.saveSrt(id: nil, name: "SRT", destination: srt, allowPlaintext: false)
        XCTAssertEqual(try store.srtSecret(for: id).streamID, srt.streamID)
        XCTAssertEqual(try store.srtSecret(for: id).passphrase, srt.passphrase)
        let key = String(repeating: "\\", count: 4096)
        let rtmp = try ManualDestination(endpoint: "rtmps://stream.example.invalid/" + String(repeating: "a", count: 4000), streamKey: key, allowPlaintext: false)
        let large = try store.save(id: nil, name: "Large", destination: rtmp, allowPlaintext: false)
        XCTAssertEqual(try store.key(for: large), key)
        XCTAssertGreaterThan(try XCTUnwrap(secrets.values[large]).count, 4096)
    }
    func testLocalGraphemeByteLimitAndMetadataCapPrecedeSecretMutation() throws {
        let suite = "sapflow-manual-store-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let secrets = ManualSecretPort(), store = DestinationProfileStore(defaults: defaults, secrets: secrets)
        let destination = try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "key", allowPlaintext: false)
        XCTAssertThrowsError(try store.save(id: nil, name: "a" + String(repeating: "\u{0301}", count: 200), destination: destination, allowPlaintext: false))
        XCTAssertEqual(secrets.writes, 0)
        let endpoint = "rtmps://x.invalid/" + String(repeating: "/", count: 4070) + "a"
        let profiles = (0..<15).map { SavedDestinationProfile(id: UUID(), name: "Saved\($0)", endpoint: endpoint, allowPlaintext: false, transport: "rtmp") }
        let bytes = try JSONEncoder().encode(profiles)
        XCTAssertLessThanOrEqual(bytes.count, 128 * 1024)
        defaults.set(bytes, forKey: "sapflow.manual-destinations.v1")
        XCTAssertThrowsError(try store.save(id: nil, name: "New", destination: ManualDestination(endpoint: endpoint, streamKey: "key", allowPlaintext: false), allowPlaintext: false))
        XCTAssertEqual(secrets.writes, 0); XCTAssertEqual(defaults.data(forKey: "sapflow.manual-destinations.v1"), bytes)
    }
    func testFiniteDeterministicByteMutationPreservesRedactedErrors() throws {
        let original = Array(Self.document()); var state: UInt64 = 0x51a7
        for index in 0..<1024 {
            state = state &* 6364136223846793005 &+ 1442695040888963407
            var bytes = original; let offset = Int(state % UInt64(bytes.count)); bytes[offset] = UInt8(truncatingIfNeeded: state >> 24)
            do { _ = try ImportedManualDestination(Data(bytes)) }
            catch { XCTAssertEqual(error.localizedDescription, ManualImportError.invalidFile.localizedDescription, "mutation \(index)") }
        }
    }
}
