import XCTest
@testable import SapflowBroadcaster

private final class InvitationStore: ManualDestinationProfileStoring {
    var saves = 0
    func load() throws -> [SavedDestinationProfile] { [] }
    func key(for id: UUID) throws -> String { throw DestinationError.keychain }
    func srtSecret(for id: UUID) throws -> (streamID: String, passphrase: String) { throw DestinationError.keychain }
    func resolve(expected: SavedDestinationProfile) throws -> DestinationProfileSecret { throw DestinationError.keychain }
    func save(id: UUID?, name: String, destination: ManualDestination, allowPlaintext: Bool) throws -> UUID { saves += 1; return UUID() }
    func saveSrt(id: UUID?, name: String, destination: ManualSrtDestination, allowPlaintext: Bool) throws -> UUID { saves += 1; return UUID() }
    func delete(_ id: UUID) throws { throw DestinationError.storage }
}
private final class InvitationModel {
    var idle = true, libraryReady = true, manualMode = false, creatorMode = false
    var jobs: [() -> Void] = [], replies: [() -> Void] = []
    let store = InvitationStore()
    lazy var settings = ManualDestinationSettings(store: store,
        available: { [unowned self] in self.idle && self.libraryReady && self.manualMode },
        invitationAvailable: { [unowned self] in self.idle && !self.creatorMode },
        invitationReviewAvailable: { [unowned self] in self.idle && self.libraryReady && !self.creatorMode },
        work: { [unowned self] in self.jobs.append($0) }, delivery: { [unowned self] in self.replies.append($0) })
    func review(_ ticket: ManualDestinationSettings.Ticket) -> Bool {
        settings.reviewInvitation(ticket) { manualMode = true; settings.modeChanged() }
    }
}
final class ManualDestinationEnvelopeTests: XCTestCase {
    private func document(plain: Bool = false) throws -> Data {
        try JSONSerialization.data(withJSONObject: ["version": 1, "kind": "sapflow-manual-destination", "name": "not-for-display-key",
            "transport": "rtmp", "endpoint": plain ? "rtmp://stream.example.invalid/private" : "rtmps://stream.example.invalid/private",
            "streamKey": "generated-secret", "streamID": "", "passphrase": "", "allowPlaintext": plain], options: [.sortedKeys])
    }
    private func link(_ data: Data) -> String {
        ManualDestinationEnvelope.prefix + data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    func testSharedGeneratedEnvelopeAndStrictSettingsCorpus() throws {
        struct Vector: Decodable { let id: String, accepted: Bool, text: String }
        struct Corpus: Decodable { let version: Int, cases: [Vector] }
        let fixture = ProcessInfo.processInfo.environment["SAPFLOW_MANUAL_LINK_VECTORS"].map { URL(fileURLWithPath: $0) } ??
            Bundle(for: Self.self).url(forResource: "manual-destination-link-v1", withExtension: "json")
        let corpus = try JSONDecoder().decode(Corpus.self, from: Data(contentsOf: XCTUnwrap(fixture)))
        XCTAssertEqual(corpus.version, 1); XCTAssertGreaterThanOrEqual(corpus.cases.count, 98)
        for vector in corpus.cases {
            if vector.accepted { XCTAssertNoThrow(try ImportedManualDestination(ManualDestinationEnvelope(vector.text).document), vector.id) }
            else { XCTAssertThrowsError(try ImportedManualDestination(ManualDestinationEnvelope(vector.text).document), vector.id) }
        }
    }
    func testCanonicalEnvelopeAndRedactedErrors() throws {
        let source = try document(), valid = link(source)
        XCTAssertEqual(try ManualDestinationEnvelope(valid).document, source)
        XCTAssertEqual(String(describing: try ManualDestinationEnvelope(valid)), "Manual destination link (redacted)")
        for text in [valid + "=", valid + "#x", " " + valid, valid + "\n", valid.replacingOccurrences(of: "sapflow-manual", with: "SAPFLOW-MANUAL"),
                     "sapflow-manual://import/v1#AB", "sapflow-manual://import/v1#A", "sapflow-manual://import/v1#AA+/"] {
            XCTAssertThrowsError(try ManualDestinationEnvelope(text)) { error in
                XCTAssertEqual(error.localizedDescription, ManualImportError.invalidLink.localizedDescription)
                XCTAssertFalse(error.localizedDescription.contains("generated-secret"))
            }
        }
    }
    func testExactDecodedAndURLByteBounds() throws {
        var exact = try document(); exact.append(Data(repeating: 32, count: 16_384 - exact.count))
        XCTAssertEqual(try ManualDestinationEnvelope(link(exact)).document.count, 16_384)
        XCTAssertNoThrow(try ImportedManualDestination(ManualDestinationEnvelope(link(exact)).document))
        exact.append(32); XCTAssertThrowsError(try ManualDestinationEnvelope(link(exact)))
        XCTAssertThrowsError(try ManualDestinationEnvelope(ManualDestinationEnvelope.prefix + String(repeating: "A", count: 24_577)))
    }
    func testColdLaunchInvitationWaitsForActiveAndReadyWithoutChangingRecordingMode() throws {
        let m = InvitationModel(); m.libraryReady = false
        XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
        let ticket = try XCTUnwrap(m.settings.invitation)
        // SwiftUI inactive supplies no visibility event; the first actual
        // active event must preserve the cold-launch invitation.
        m.settings.captureChanged()
        XCTAssertEqual(m.settings.invitation, ticket); XCTAssertFalse(m.review(ticket)); XCTAssertFalse(m.manualMode)
        m.settings.sceneChanged(active: true)
        XCTAssertEqual(m.settings.invitation, ticket); XCTAssertFalse(m.review(ticket))
        m.libraryReady = true; m.settings.captureChanged()
        XCTAssertTrue(m.review(ticket)); XCTAssertTrue(m.manualMode)
        XCTAssertEqual(m.settings.endpoint, ""); XCTAssertNil(m.settings.invitation)
        XCTAssertEqual(m.settings.review?.peer, "stream.example.invalid:443"); XCTAssertEqual(m.store.saves, 0)
        XCTAssertTrue(m.settings.apply(ticket, plaintextConfirmed: false))
        XCTAssertEqual(m.settings.streamKey, "generated-secret"); XCTAssertEqual(m.settings.profileName, "")
        XCTAssertEqual(m.store.saves, 0)
    }
    func testActualBackgroundBeforeFirstActiveRetiresColdInvitation() throws {
        let m = InvitationModel()
        XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
        let old = try XCTUnwrap(m.settings.invitation)
        m.settings.sceneChanged(active: false)
        XCTAssertNil(m.settings.invitation)
        m.settings.sceneChanged(active: true)
        XCTAssertNil(m.settings.invitation); XCTAssertFalse(m.review(old)); XCTAssertFalse(m.manualMode)
    }
    func testNewInvitationBeforeWarmActiveIsNotAutoReviewed() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true); m.settings.sceneChanged(active: false)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
        let ticket = try XCTUnwrap(m.settings.invitation)
        m.settings.sceneChanged(active: true)
        XCTAssertEqual(m.settings.invitation, ticket); XCTAssertNil(m.settings.review); XCTAssertFalse(m.manualMode)
    }
    func testEditsBackgroundModeAndCaptureRetireInvitation() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        for change in [{ m.settings.endpoint = "local edit" }, { m.settings.modeChanged() },
                       { m.settings.sceneChanged(active: false) }, { m.idle = false; m.settings.captureChanged() }] {
            m.idle = true; m.settings.sceneChanged(active: true)
            XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
            let ticket = try XCTUnwrap(m.settings.invitation); change()
            XCTAssertNil(m.settings.invitation); XCTAssertFalse(m.review(ticket)); XCTAssertNil(m.settings.review)
        }
    }
    func testConflictingIncomingInviteCannotReplaceCurrentOrLendConsent() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        let plain = link(try document(plain: true))
        XCTAssertTrue(m.settings.receiveInvitation(plain)); let a = try XCTUnwrap(m.settings.invitation)
        XCTAssertFalse(m.settings.receiveInvitation(link(try document())))
        XCTAssertEqual(m.settings.invitation, a); XCTAssertTrue(m.review(a))
        XCTAssertFalse(m.settings.receiveInvitation(link(try document())))
        XCTAssertEqual(m.settings.review?.ticket, a)
        XCTAssertFalse(m.settings.apply(a, plaintextConfirmed: false)); XCTAssertFalse(m.settings.allowPlaintext)
        m.settings.cancelReview(a)
        XCTAssertTrue(m.settings.receiveInvitation(plain)); let b = try XCTUnwrap(m.settings.invitation)
        m.settings.cancelInvitation(a); XCTAssertEqual(m.settings.invitation, b); XCTAssertFalse(m.review(a))
        XCTAssertTrue(m.review(b)); XCTAssertFalse(m.settings.apply(a, plaintextConfirmed: true))
        XCTAssertFalse(m.settings.apply(b, plaintextConfirmed: false)); XCTAssertTrue(m.settings.apply(b, plaintextConfirmed: true))
    }
    func testCancelledPlaintextReviewAndSecureSuccessorDoNotInheritConsent() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document(plain: true)))); let a = try XCTUnwrap(m.settings.invitation)
        XCTAssertTrue(m.review(a)); m.settings.cancelReview(a)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document()))); let b = try XCTUnwrap(m.settings.invitation)
        XCTAssertTrue(m.review(b)); XCTAssertFalse(m.settings.apply(a, plaintextConfirmed: true))
        XCTAssertTrue(m.settings.apply(b, plaintextConfirmed: false)); XCTAssertFalse(m.settings.allowPlaintext)
    }
    func testBusyCaptureAndCreatorModeRefuseEvenDuringInitialLibrarySetup() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        m.idle = false; XCTAssertFalse(m.settings.receiveInvitation(link(try document())))
        m.idle = true; m.creatorMode = true; m.libraryReady = false
        XCTAssertFalse(m.settings.receiveInvitation(link(try document())))
        m.creatorMode = false; XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
        let ticket = try XCTUnwrap(m.settings.invitation)
        m.creatorMode = true; m.settings.captureChanged()
        XCTAssertNil(m.settings.invitation); XCTAssertFalse(m.review(ticket))
        m.creatorMode = false
        XCTAssertFalse(m.settings.receiveInvitation("bunker://generated-not-authority"))
        XCTAssertEqual(m.settings.endpoint, ""); XCTAssertFalse(m.manualMode)
    }
    func testExactReviewRechecksActualAvailabilityAfterExplicitModeSelection() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document()))); let ticket = try XCTUnwrap(m.settings.invitation)
        XCTAssertFalse(m.settings.reviewInvitation(ticket) { m.manualMode = true; m.idle = false; m.settings.modeChanged() })
        XCTAssertNil(m.settings.review); XCTAssertEqual(m.settings.endpoint, "")
    }
    func testAnotherInvitationDuringSelectionIsRefusedWithoutDisplacingReview() throws {
        let m = InvitationModel(); m.settings.sceneChanged(active: true)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document()))); let a = try XCTUnwrap(m.settings.invitation)
        let next = link(try document(plain: true))
        XCTAssertTrue(m.settings.reviewInvitation(a) {
            m.manualMode = true
            XCTAssertFalse(m.settings.receiveInvitation(next))
            m.settings.modeChanged()
        })
        XCTAssertEqual(m.settings.review?.ticket, a); XCTAssertNil(m.settings.invitation)
        XCTAssertTrue(m.settings.apply(a, plaintextConfirmed: false)); XCTAssertFalse(m.settings.allowPlaintext)
    }
    func testLateActualFileReadCannotReplaceLinkAndReadRetainsCapacity() throws {
        let m = InvitationModel(); m.manualMode = true; m.settings.sceneChanged(active: true)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-link-generated-" + UUID().uuidString + ".json")
        try document().write(to: url); defer { try? FileManager.default.removeItem(at: url) }
        let fileTicket = try XCTUnwrap(m.settings.beginImport())
        XCTAssertFalse(m.settings.receiveInvitation(link(try document())))
        XCTAssertEqual(m.settings.presentation, fileTicket)
        m.settings.received(fileTicket, file: .success(url))
        XCTAssertTrue(m.settings.reading)
        XCTAssertFalse(m.settings.receiveInvitation(link(try document(plain: true))))
        XCTAssertNil(m.settings.invitation)
        m.jobs.removeFirst()(); m.replies.removeFirst()()
        XCTAssertFalse(m.settings.reading); XCTAssertEqual(m.settings.review?.ticket, fileTicket)
        m.settings.cancelReview(fileTicket)
        XCTAssertTrue(m.settings.receiveInvitation(link(try document())))
        let a = try XCTUnwrap(m.settings.invitation)
        m.settings.received(fileTicket, file: .success(url))
        XCTAssertEqual(m.settings.invitation, a); XCTAssertTrue(m.jobs.isEmpty)
    }
    func testOriginalAppSessionCannotApproveAnotherSessionInvitation() throws {
        let a = InvitationModel(), b = InvitationModel()
        a.settings.sceneChanged(active: true); b.settings.sceneChanged(active: true)
        let text = link(try document())
        XCTAssertTrue(a.settings.receiveInvitation(text)); XCTAssertTrue(b.settings.receiveInvitation(text))
        let old = try XCTUnwrap(a.settings.invitation), current = try XCTUnwrap(b.settings.invitation)
        XCTAssertNotEqual(old, current); XCTAssertFalse(b.review(old)); XCTAssertTrue(b.review(current))
    }
}
