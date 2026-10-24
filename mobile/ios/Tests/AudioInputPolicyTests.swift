import XCTest
@testable import SapflowBroadcaster

final class AudioInputPolicyTests: XCTestCase {
    private func input(_ id: String, _ kind: AudioInputOption.Kind = .usb, name: String = "Microphone") -> AudioInputOption {
        AudioInputOption(id: id, name: name, kind: kind)!
    }
    func testDescriptorsBoundWorkAndNeverUseNamesAsIdentity() {
        XCTAssertNil(AudioInputOption(id: "", name: "Mic", kind: .usb))
        XCTAssertNil(AudioInputOption(id: "a\n", name: "Mic", kind: .usb))
        XCTAssertNil(AudioInputOption(id: "a\u{202e}", name: "Mic", kind: .usb))
        XCTAssertNil(AudioInputOption(id: "a\u{e0001}", name: "Mic", kind: .usb))
        XCTAssertEqual(input("usb", name: "A\u{202e}B\u{e0001}").name, "AB")
        XCTAssertFalse(CaptureAdmission().selectAudioInput("a\u{202e}"))
        XCTAssertNil(AudioInputOption(id: String(repeating: "é", count: 129), name: "Mic", kind: .usb))
        XCTAssertNotNil(AudioInputOption(id: String(repeating: "é", count: 128), name: "Mic", kind: .usb))
        XCTAssertNil(AudioInputOption(id: "usb", name: String(repeating: "x", count: 1025), kind: .usb))
        XCTAssertEqual(input("usb", name: "A\nB" + String(repeating: "x", count: 100)).name.count, 80)
        XCTAssertEqual(input("usb", name: "A\nB").name, "AB")
        XCTAssertEqual(AudioInputOption.inventory((0..<100).map { input("\($0)") }).count, 32)
        XCTAssertEqual(AudioInputOption.inventory([input("a"), input("a"), input("b")]).map(\.id), ["a", "b"])
    }
    func testSystemDefaultRequiresActualCurrentOwnerObservation() {
        let policy = AudioInputPolicy(owner: 1, available: [input("built-in", .builtIn)])!
        XCTAssertNil(policy.requestedID)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
        XCTAssertEqual(policy.observe(owner: 1, id: "built-in"), .accepted)
        XCTAssertEqual(policy.observedID(owner: 1), "built-in")
        XCTAssertTrue(policy.permitsMedia(owner: 1))
        XCTAssertEqual(policy.observe(owner: 1, id: "built-in"), .accepted)
    }
    func testUnavailableAndInvalidExplicitChoiceCannotCreatePolicy() {
        XCTAssertNil(AudioInputPolicy(owner: 1, requestedID: "missing", available: [input("usb")]))
        XCTAssertNil(AudioInputPolicy(owner: 1, requestedID: "", available: []))
        let policy = AudioInputPolicy(owner: 1, available: [])!
        XCTAssertEqual(policy.observe(owner: 1, id: "unknown"), .failed)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
    }
    func testMatchingNameOrSetterIntentCannotReplaceActualID() {
        let policy = AudioInputPolicy(owner: 1, requestedID: "usb", available: [input("usb"), input("built-in", .builtIn)])!
        XCTAssertEqual(policy.observe(owner: 1, id: "built-in"), .failed)
        XCTAssertEqual(policy.observe(owner: 1, id: "usb"), .ignored)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
        XCTAssertNil(policy.observedID(owner: 1))
    }
    func testDisconnectClosesMediaWithoutFallbackOrRepeatedFailure() {
        let policy = AudioInputPolicy(owner: 1, requestedID: "usb", available: [input("usb")])!
        XCTAssertEqual(policy.observe(owner: 1, id: "usb"), .accepted)
        XCTAssertEqual(policy.observe(owner: 1, id: nil), .failed)
        XCTAssertEqual(policy.observe(owner: 1, id: nil), .ignored)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
    }
    func testDefaultDoesNotSilentlyAdoptAnotherAvailableInput() {
        let policy = AudioInputPolicy(owner: 1, available: [input("wired", .wired), input("usb")])!
        XCTAssertEqual(policy.observe(owner: 1, id: "wired"), .accepted)
        XCTAssertEqual(policy.observe(owner: 1, id: "usb"), .failed)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
    }
    func testStaleObservationAndCloseCannotPoisonCurrentOwner() {
        let policy = AudioInputPolicy(owner: 2, requestedID: "usb", available: [input("usb")])!
        XCTAssertEqual(policy.observe(owner: 1, id: nil), .stale)
        policy.close(owner: 1)
        XCTAssertEqual(policy.observe(owner: 2, id: "usb"), .accepted)
        XCTAssertTrue(policy.permitsMedia(owner: 2))
        XCTAssertFalse(policy.permitsMedia(owner: 1))
        XCTAssertNil(policy.observedID(owner: 1))
    }
    func testClosedOwnerCannotBeRevivedByLateObservation() {
        let policy = AudioInputPolicy(owner: 1, available: [input("usb")])!
        policy.close(owner: 1)
        XCTAssertEqual(policy.observe(owner: 1, id: "usb"), .ignored)
        XCTAssertFalse(policy.permitsMedia(owner: 1))
    }
    func testPrepareFreezesChoiceAndActualCleanupFencesSuccessor() {
        let admission = CaptureAdmission()
        XCTAssertTrue(admission.selectAudioInput("usb-a"))
        let a = admission.reservePreparation()!
        XCTAssertEqual(a.audioInputID, "usb-a")
        XCTAssertFalse(admission.selectAudioInput("usb-b"))
        XCTAssertTrue(admission.attached(a))
        let stopped = admission.stop()
        XCTAssertFalse(admission.completeStop(stopped, cleanupComplete: false))
        XCTAssertNil(admission.reservePreparation())
        XCTAssertFalse(admission.selectAudioInput(nil))
        XCTAssertTrue(admission.completeStop(stopped, cleanupComplete: true))
        XCTAssertTrue(admission.selectAudioInput("usb-b"))
        let b = admission.reservePreparation()!
        XCTAssertEqual(b.audioInputID, "usb-b")
        struct Owner: Equatable { let preparation: CaptureAdmission.Preparation; let epoch: Int }
        let current = Owner(preparation: b, epoch: 2)
        let policy = AudioInputPolicy(owner: current, requestedID: "usb-b", available: [input("usb-b")])!
        XCTAssertEqual(policy.observe(owner: Owner(preparation: a, epoch: 1), id: nil), .stale)
        XCTAssertEqual(policy.observe(owner: current, id: "usb-b"), .accepted)
        XCTAssertEqual(admission.currentPreparation, b)
    }
    func testDefaultCompatibilityAndRotationRetainFrozenInput() {
        let admission = CaptureAdmission()
        let a = admission.reservePreparation()!
        XCTAssertNil(a.audioInputID)
        XCTAssertTrue(admission.attached(a)); XCTAssertTrue(admission.beginPermissions(a)); XCTAssertTrue(admission.prepared(a))
        let start = admission.reserveStart(a)!
        XCTAssertTrue(admission.started(start))
        let rotation = admission.beginRotation(a)!
        let next = admission.reserveRotationStart(rotation)!
        XCTAssertEqual(next.preparation.audioInputID, a.audioInputID)
        XCTAssertFalse(admission.selectAudioInput("usb"))
        struct Owner: Equatable { let preparation: CaptureAdmission.Preparation; let epoch: Int }
        let current = Owner(preparation: a, epoch: 2)
        let policy = AudioInputPolicy(owner: current, available: [input("usb")])!
        let old = Owner(preparation: a, epoch: 1)
        XCTAssertEqual(policy.observe(owner: old, id: nil), .stale)
        policy.close(owner: old)
        XCTAssertEqual(policy.observe(owner: current, id: "usb"), .accepted)
        XCTAssertTrue(policy.permitsMedia(owner: current))
        XCTAssertTrue(admission.permitsStart(next), "old native epoch must not mutate capture admission")
    }
}
