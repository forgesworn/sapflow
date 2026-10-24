import XCTest
@testable import SapflowBroadcaster

final class AudioShowLifecycleTests: XCTestCase {
    func testProductionStopActionRetiresOnlyTheWonManualShow() throws {
        let gate = CaptureAdmission(), selection = BroadcasterDestinationSelection { _ in }
        selection.select(.manual)
        let a = try running(gate); XCTAssertTrue(selection.reserve(a))
        var aEnds = 0, bEnds = 0
        XCTAssertTrue(selection.installManual(a) { aEnds += 1 })
        let action = AudioShowStopControl(preparation: a, admission: gate)
        var receipt: CaptureAdmission.Stop?
        func invoke() -> Bool {
            action.stop(capture: { receipt = $0.perform(on: gate); return receipt != nil },
                        retirePublisher: { selection.stop(preparation: $0) })
        }
        XCTAssertTrue(invoke()); XCTAssertEqual(aEnds, 1)
        let stopped = try XCTUnwrap(receipt)
        XCTAssertFalse(invoke()); XCTAssertEqual(aEnds, 1)
        XCTAssertTrue(gate.completeStop(stopped, cleanupComplete: true))
        let b = try running(gate); XCTAssertTrue(selection.reserve(b))
        XCTAssertTrue(selection.installManual(b) { bEnds += 1 })
        XCTAssertFalse(invoke()); XCTAssertEqual(bEnds, 0)
        XCTAssertEqual(gate.currentPreparation, b); XCTAssertEqual(selection.preparation, b)
    }
    func testInactiveAppearanceDoesNotRetirePermissionOrCommittedAudioOwner() throws {
        let gate = CaptureAdmission(); XCTAssertTrue(gate.selectBackgroundAudio(true))
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        let inactive = AudioShowLifecycle.sceneVisibility(active: false, background: false)
        XCTAssertNil(inactive)
        if let inactive { _ = gate.visibilityChanged(foreground: inactive) }
        XCTAssertTrue(gate.permitsPreparation(ticket))
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertTrue(gate.started(try XCTUnwrap(gate.reserveStart(ticket))))
        if let inactive { _ = gate.visibilityChanged(foreground: inactive) }
        XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertEqual(AudioShowLifecycle.sceneVisibility(active: true, background: false), true)
        XCTAssertEqual(AudioShowLifecycle.sceneVisibility(active: false, background: true), false)
    }
    private func prepared(_ gate: CaptureAdmission, profile: CaptureIntent = .audioOnly48kMono, optIn: Bool = true) throws -> CaptureAdmission.Preparation {
        XCTAssertTrue(gate.selectBackgroundAudio(optIn))
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: profile))
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket)); XCTAssertTrue(gate.prepared(ticket))
        return ticket
    }
    private func running(_ gate: CaptureAdmission, profile: CaptureIntent = .audioOnly48kMono, optIn: Bool = true) throws -> CaptureAdmission.Preparation {
        let ticket = try prepared(gate, profile: profile, optIn: optIn)
        XCTAssertTrue(gate.started(try XCTUnwrap(gate.reserveStart(ticket)), nowNs: 100))
        return ticket
    }
    func testEveryUncommittedPhaseFailsBackgroundAdmission() throws {
        for stage in 0...4 {
            let gate = CaptureAdmission(); XCTAssertTrue(gate.selectBackgroundAudio(true))
            let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
            if stage >= 1 { XCTAssertTrue(gate.attached(ticket)) }
            if stage >= 2 { XCTAssertTrue(gate.beginPermissions(ticket)) }
            if stage >= 3 { XCTAssertTrue(gate.prepared(ticket)) }
            let start = stage == 4 ? try XCTUnwrap(gate.reserveStart(ticket)) : nil
            let result = gate.visibilityChanged(foreground: false, nowNs: 200)
            XCTAssertNil(result.continuing); XCTAssertNotNil(result.stop)
            if let start { XCTAssertFalse(gate.started(start, nowNs: 201)) }
            XCTAssertNil(gate.reservePreparation(profile: .audioOnly48kMono))
        }
    }
    func testOnlyOptedInCommittedAudioContinuesAndNeverCreatesANewStart() throws {
        for profile in [CaptureIntent.audioOnly48kMono, .hd720p30] {
            for optIn in [false, true] {
                let gate = CaptureAdmission(), ticket = try running(gate, profile: profile, optIn: optIn)
                let result = gate.visibilityChanged(foreground: false, nowNs: 300)
                if optIn && !profile.hasVideo {
                    XCTAssertEqual(result.continuing, ticket); XCTAssertNil(result.stop)
                    XCTAssertEqual(gate.currentPhase, .running)
                    XCTAssertEqual(gate.visibilityChanged(foreground: false).continuing, ticket)
                } else { XCTAssertNotNil(result.stop); XCTAssertNil(result.continuing) }
                XCTAssertNil(gate.reserveStart(ticket)); XCTAssertNil(gate.reservePreparation())
            }
        }
    }
    func testOptInIsFrozenAtPrepareAndCannotBeAddedToAVideoShow() throws {
        let gate = CaptureAdmission(), ticket = try prepared(gate, optIn: false)
        XCTAssertFalse(gate.selectBackgroundAudio(true)); XCTAssertFalse(ticket.continuesAudioInBackground)
        let video = CaptureAdmission(), v = try prepared(video, profile: .hd720p30)
        XCTAssertFalse(v.continuesAudioInBackground)
    }
    func testAppStopCapturesShowAndRefusesSuccessorOrUnknownCleanup() throws {
        let gate = CaptureAdmission(), a = try running(gate), control = AudioShowStopControl(preparation: a, admission: gate)
        let foreign = CaptureAdmission(); _ = try running(foreign)
        XCTAssertNil(control.perform(on: foreign)); XCTAssertEqual(foreign.currentPhase, .running)
        XCTAssertEqual(control.capability, .appOnly)
        let stop = try XCTUnwrap(control.perform(on: gate))
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        let b = try running(gate)
        XCTAssertNil(control.perform(on: gate)); XCTAssertEqual(gate.currentPreparation, b)
        XCTAssertEqual(gate.currentPhase, .running)
    }
    func testBackgroundCancelRetainsCandidateUntilBothActualCleanupReceipts() throws {
        let gate = CaptureAdmission(); XCTAssertTrue(gate.selectBackgroundAudio(true))
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        let stop = try XCTUnwrap(gate.visibilityChanged(foreground: false).stop)
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertTrue(gate.ownsPreparation(ticket)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.candidateClosed(ticket, cleanupComplete: true))
        XCTAssertFalse(gate.ownsPreparation(ticket)); XCTAssertNil(gate.reservePreparation())
        _ = gate.visibilityChanged(foreground: true)
        XCTAssertEqual(gate.currentPhase, .idle); XCTAssertNotNil(gate.reservePreparation())
    }
    func testInterruptionAndMediaResetStopBackgroundShowWithoutResume() throws {
        for event in [AudioShowLifecycle.Interruption.audioBegan, .mediaServicesReset, .captureFailed] {
            let gate = CaptureAdmission(), ticket = try running(gate)
            XCTAssertEqual(gate.visibilityChanged(foreground: false).continuing, ticket)
            let stop = try XCTUnwrap(gate.interruption(event))
            XCTAssertNil(gate.backgroundContinuation)
            XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
            XCTAssertNil(gate.interruption(.audioEnded))
            XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
            _ = gate.visibilityChanged(foreground: true)
            XCTAssertEqual(gate.currentPhase, .idle); XCTAssertNil(gate.preparedTicket)
        }
    }
    func testLateInterruptionEndedCannotRetireANewShow() throws {
        let gate = CaptureAdmission(), a = try running(gate)
        let stop = try XCTUnwrap(gate.interruption(.audioBegan)); XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        let b = try running(gate)
        XCTAssertNotEqual(a, b); XCTAssertNil(gate.interruption(.audioEnded)); XCTAssertEqual(gate.currentPreparation, b)
    }
    func testAuthorityRelativeWakeDoesNotExtendOriginalDeadline() {
        XCTAssertEqual(AudioShowLifecycle.remaining(deadline: 100, now: 40), 60)
        XCTAssertEqual(AudioShowLifecycle.remaining(deadline: 100, now: 100), 0)
        XCTAssertEqual(AudioShowLifecycle.remaining(deadline: 100, now: 1000), 0)
        XCTAssertLessThanOrEqual(AudioShowAuthorityClock.nowNs(), AudioShowAuthorityClock.nowNs())
    }
}
