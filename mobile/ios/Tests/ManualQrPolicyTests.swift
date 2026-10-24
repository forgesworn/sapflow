import XCTest
@testable import SapflowBroadcaster

final class ManualQrPolicyTests: XCTestCase {
    private func link(documentBytes: Int? = nil) -> String {
        var bytes = ManualDestinationImportTests.document()
        if let size = documentBytes, bytes.count < size { bytes.append(Data(repeating: 32, count: size - bytes.count)) }
        return ManualDestinationEnvelope.prefix + bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    func testCanonicalQrBoundDoesNotNarrowDeepLinks() throws {
        let inside = link(documentBytes: 1515), outside = link(documentBytes: 1516)
        // 27-byte prefix + canonical base64url cannot have total length2048:
        // its encoded suffix would have forbidden length mod4 == 1.
        XCTAssertEqual(inside.utf8.count, 2047); XCTAssertEqual(outside.utf8.count, 2049)
        XCTAssertTrue(ManualQrPolicy.validatedCandidate(inside))
        XCTAssertFalse(ManualQrPolicy.validatedCandidate(outside))
        XCTAssertNoThrow(try ImportedManualDestination(ManualDestinationEnvelope(outside).document))
        XCTAssertTrue(ManualQrPolicy.boundedASCII(String(repeating: "x", count: 2048)))
        XCTAssertFalse(ManualQrPolicy.boundedASCII(String(repeating: "x", count: 2049)))
        for text in ["", "https://example.invalid", inside + "=", "\n" + inside, "\u{202e}" + inside] {
            XCTAssertFalse(ManualQrPolicy.validatedCandidate(text))
        }
    }
    func testSharedLinkCorpusAlsoPassesThroughQrValidation() throws {
        struct Vector: Decodable { let id: String, accepted: Bool, text: String }
        struct Corpus: Decodable { let cases: [Vector] }
        let url = ProcessInfo.processInfo.environment["SAPFLOW_MANUAL_LINK_VECTORS"].map { URL(fileURLWithPath: $0) } ??
            Bundle(for: ManualQrPolicyTests.self).url(forResource: "manual-destination-link-v1", withExtension: "json")
        let corpus = try JSONDecoder().decode(Corpus.self, from: Data(contentsOf: XCTUnwrap(url)))
        XCTAssertGreaterThan(corpus.cases.count, 10)
        for vector in corpus.cases {
            XCTAssertEqual(ManualQrPolicy.validatedCandidate(vector.text), vector.accepted && vector.text.utf8.count <= 2048, vector.id)
        }
    }
    func testPermissionCancellationAndLateGrantCannotOpenOrLendCapacity() throws {
        let p = ManualQrPolicy(), gate = RecordingLibraryAdmission()
        let lease = try XCTUnwrap(gate.reserveCameraScan { true })
        let ticket = try XCTUnwrap(p.begin(now: 1))
        XCTAssertTrue(p.claimModal(ticket))
        var osReply: ((Bool) -> Void)?, uiReplies: [() -> Void] = [], opens = 0, replies = 0
        XCTAssertTrue(ManualQrPermissionRequest.begin(policy: p, ticket: ticket, now: 2,
            request: { osReply = $0 }, deliver: { uiReplies.append($0) }, reply: { _ in
                replies += 1
                if p.claimOpen(ticket, now: 5) { opens += 1 }
            }))
        p.cancel(ticket); p.modalClosed(ticket); p.nativeClosed(ticket, clean: true)
        XCTAssertNil(p.takeCompletion(ticket, now: 3))
        XCTAssertNil(gate.withCaptureReservation { true }); XCTAssertNil(p.begin(now: 3))
        osReply?(true)
        XCTAssertEqual(uiReplies.count, 1)
        XCTAssertNil(p.takeCompletion(ticket, now: 4)) // Main reply is still withheld.
        XCTAssertTrue(gate.owns(lease)); XCTAssertEqual(opens, 0)
        uiReplies.removeFirst()()
        XCTAssertEqual(replies, 1); XCTAssertEqual(opens, 0)
        XCTAssertNil(try XCTUnwrap(p.takeCompletion(ticket, now: 5)).delivery)
        _ = gate.finish(lease, cleanupComplete: true)
        let next = try XCTUnwrap(p.begin(now: 6))
        let nextLease = try XCTUnwrap(gate.reserveCameraScan { true })
        osReply?(true); uiReplies.removeFirst()() // A duplicate old reply cannot touch B.
        XCTAssertEqual(replies, 1); XCTAssertTrue(gate.owns(nextLease))
        XCTAssertFalse(p.claimOpen(ticket, now: 7)); XCTAssertTrue(p.claimModal(next))
        XCTAssertTrue(p.claimOpen(next, now: 7))
    }
    func testClaimedOpenWaitsForNativeMetadataAndModalReceipts() throws {
        let p = ManualQrPolicy(), ticket = try XCTUnwrap(p.begin(now: 0))
        XCTAssertTrue(p.claimModal(ticket))
        XCTAssertTrue(p.claimOpen(ticket, now: 1)); p.cancel(ticket)
        p.modalClosed(ticket); XCTAssertNil(p.takeCompletion(ticket, now: 10))
        XCTAssertNil(p.begin(now: 2))
        p.nativeClosed(ticket, clean: false)
        XCTAssertNil(p.takeCompletion(ticket, now: 10)); XCTAssertNil(p.begin(now: 3))
        p.nativeClosed(ticket, clean: true) // a real later cleanup receipt
        XCTAssertNotNil(p.takeCompletion(ticket, now: 10)); XCTAssertNil(p.takeCompletion(ticket, now: 10))
    }
    func testFirstCandidateClosesAdmissionButCannotDeliverBeforeCleanup() throws {
        let p = ManualQrPolicy(), ticket = try XCTUnwrap(p.begin(now: 0)), text = link()
        XCTAssertTrue(p.claimModal(ticket))
        XCTAssertTrue(p.claimOpen(ticket, now: 1))
        XCTAssertTrue(p.candidate(text, ticket: ticket, now: 2))
        XCTAssertFalse(p.candidate(text, ticket: ticket, now: 3)); XCTAssertFalse(p.permits(ticket, now: 3))
        p.nativeClosed(ticket, clean: true); XCTAssertNil(p.takeCompletion(ticket, now: 10))
        p.modalClosed(ticket)
        XCTAssertEqual(try XCTUnwrap(p.takeCompletion(ticket, now: 10)).delivery?.textForDelivery(nowNs: 10), text)
        let next = try XCTUnwrap(p.begin(now: 4))
        p.cancel(ticket); p.modalClosed(ticket); p.nativeClosed(ticket, clean: true)
        XCTAssertTrue(p.permits(next, now: 5)); XCTAssertNil(p.takeCompletion(ticket, now: 10))
    }
    func testOriginalDeadlineCannotBeExtendedOrReleaseNativeOwnership() throws {
        let p = ManualQrPolicy(), ticket = try XCTUnwrap(p.begin(now: 10))
        XCTAssertTrue(p.claimModal(ticket))
        XCTAssertTrue(p.claimOpen(ticket, now: 11))
        let expired = 10 + ManualQrPolicy.lifetimeNs
        XCTAssertFalse(p.permits(ticket, now: expired))
        XCTAssertFalse(p.candidate(link(), ticket: ticket, now: expired))
        XCTAssertTrue(p.expire(ticket, now: expired)); XCTAssertFalse(p.expire(ticket, now: expired + 1))
        p.modalClosed(ticket); XCTAssertNil(p.takeCompletion(ticket, now: 10)); XCTAssertNil(p.begin(now: expired + 2))
        p.nativeClosed(ticket, clean: true)
        XCTAssertNil(try XCTUnwrap(p.takeCompletion(ticket, now: 10)).delivery)
        let permission = try XCTUnwrap(p.begin(now: expired + 3))
        XCTAssertTrue(p.claimModal(permission))
        XCTAssertFalse(p.claimOpen(permission, now: expired + 3 + ManualQrPolicy.lifetimeNs))
    }
    func testCancelBeforeSheetCanBePresentedHasNoModalOrCameraOwner() throws {
        let p = ManualQrPolicy(), ticket = try XCTUnwrap(p.begin(now: 0))
        XCTAssertFalse(p.claimOpen(ticket, now: 1))
        p.cancel(ticket)
        XCTAssertFalse(p.claimModal(ticket)); XCTAssertFalse(p.claimOpen(ticket, now: 2))
        XCTAssertNotNil(p.takeCompletion(ticket, now: 10))
    }
    func testCandidateBeforeDeadlineCannotDeliverAfterHeldCleanup() throws {
        let p = ManualQrPolicy(), ticket = try XCTUnwrap(p.begin(now: 0))
        XCTAssertTrue(p.claimModal(ticket)); XCTAssertTrue(p.claimOpen(ticket, now: 1))
        XCTAssertTrue(p.candidate(link(), ticket: ticket, now: 2))
        p.modalClosed(ticket)
        XCTAssertNil(p.takeCompletion(ticket, now: ManualQrPolicy.lifetimeNs))
        XCTAssertNil(p.begin(now: ManualQrPolicy.lifetimeNs))
        p.nativeClosed(ticket, clean: true)
        XCTAssertNil(try XCTUnwrap(p.takeCompletion(ticket, now: ManualQrPolicy.lifetimeNs)).delivery)
        XCTAssertNotNil(p.begin(now: ManualQrPolicy.lifetimeNs + 1))
    }
    func testActualCleanupCompositionWaitsForCallbackBarrierAndRetainsUnknown() {
        var order: [String] = [], barrier: (() -> Void)?, completed: Bool?
        ManualQrCleanup.close(stop: { order.append("stop-after-configuration"); return true },
            detach: { order.append("detach") }, afterCallbacks: { barrier = $0 },
            release: { order.append("release") }, complete: { completed = $0 })
        XCTAssertEqual(order, ["stop-after-configuration", "detach"]); XCTAssertNil(completed)
        barrier?(); XCTAssertEqual(order.last, "release"); XCTAssertEqual(completed, true)
        order = []; barrier = nil; completed = nil
        ManualQrCleanup.close(stop: { false }, detach: {}, afterCallbacks: { barrier = $0 },
            release: { order.append("unsafe release") }, complete: { completed = $0 })
        barrier?(); XCTAssertTrue(order.isEmpty); XCTAssertEqual(completed, false)
    }
}
