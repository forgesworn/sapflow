import XCTest
@testable import SapflowBroadcaster

final class CapturePrivacyAdmissionTests: XCTestCase {
    private struct Show {
        let gate = CaptureAdmission()
        let prep: CaptureAdmission.Preparation
        let start: CaptureAdmission.Start
        let run: CaptureAdmission.MediaRun
        init(video: Bool = false, standby: Bool = false, publishing: Bool = true, commit: Bool = true) throws {
            XCTAssertTrue(gate.selectInitialStandby(standby))
            prep = try XCTUnwrap(gate.reservePreparation(profile: video ? .defaultProfile : .audioOnly48kMono))
            XCTAssertTrue(gate.attached(prep)); XCTAssertTrue(gate.beginPermissions(prep)); XCTAssertTrue(gate.prepared(prep))
            start = try XCTUnwrap(gate.reserveStart(prep))
            run = try XCTUnwrap(gate.reserveMediaRun(start, publishing: publishing))
            if commit { XCTAssertTrue(gate.commitMediaRun(run, start: start, nowNs: 0, broadcasting: publishing)) }
        }
        func token(_ kind: CaptureAdmission.MediaKind) throws -> CaptureAdmission.MediaToken { try XCTUnwrap(gate.mediaToken(run, kind: kind)) }
        func enter(now: UInt64 = 1, generation: UInt64 = 2) throws -> CaptureAdmission.PrivacyRequest {
            let request = try XCTUnwrap(gate.requestStandby(preparation: prep, nowNs: now))
            XCTAssertTrue(gate.zeroOwnerReady(request, generation: generation, retiredGeneration: generation - 1, nowNs: now))
            return request
        }
        @discardableResult func confirm(now: UInt64 = 2, generation: UInt64 = 2, sinks: CaptureAdmission.SinkMask = .both) throws -> Bool {
            let zero = try token(.zero)
            _ = gate.recordBoundary(.init(token: zero, pts: 0, duration: 1024, codedIdr: false, audioGeneration: generation, acceptedSinks: sinks), nowNs: now)
            if prep.profile.hasVideo {
                _ = gate.recordBoundary(.init(token: try token(.slate), pts: 0, duration: 3000, codedIdr: true, audioGeneration: nil, acceptedSinks: sinks), nowNs: now)
            }
            return gate.recordBoundary(.init(token: zero, pts: 1024, duration: 1024, codedIdr: false, audioGeneration: generation, acceptedSinks: sinks), nowNs: now)
        }
    }
    func testInitialStandbyFreezesBeforePrepareAndRequiresExactCommittedStart() throws {
        let s = try Show(standby: true, commit: false)
        XCTAssertTrue(s.prep.initialStandby)
        XCTAssertFalse(s.gate.selectInitialStandby(false))
        XCTAssertEqual(s.gate.allowsInput(try s.token(.raw), nowNs: 0), .stopped)
        XCTAssertEqual(s.gate.allowsInput(try s.token(.zero), nowNs: 0), .stopped)
        XCTAssertTrue(s.gate.commitMediaRun(s.run, start: s.start, nowNs: 1, broadcasting: true))
        XCTAssertEqual(s.gate.allowsInput(try s.token(.zero), nowNs: 1), .allow)
        XCTAssertEqual(s.gate.allowsInput(try s.token(.raw), nowNs: 1), .superseded)
    }
    func testUrgentStandbyFencesCapturedRawBeforeRouteWaitAndReplayReturn() throws {
        let s = try Show(); let queuedRaw = try s.token(.raw)
        XCTAssertEqual(s.gate.allowsInput(queuedRaw), .allow)
        _ = try s.enter()
        XCTAssertEqual(s.gate.allowsInput(queuedRaw, nowNs: 2), .superseded)
        XCTAssertEqual(s.gate.outputAdmission(queuedRaw, nowNs: 2), .acceptedPrefix)
        XCTAssertFalse(s.gate.recordBoundary(.init(token: queuedRaw, pts: 0, duration: 1024, codedIdr: false, audioGeneration: 1, acceptedSinks: .both), nowNs: 2))
    }
    func testOnlyActualCodedIdrAndFreshContiguousZeroProofConfirmBothBranches() throws {
        let s = try Show(video: true); _ = try s.enter()
        let zero = try s.token(.zero); let slate = try s.token(.slate)
        XCTAssertFalse(s.gate.recordBoundary(.init(token: slate, pts: 0, duration: 3000, codedIdr: false, audioGeneration: nil, acceptedSinks: .both), nowNs: 2))
        XCTAssertFalse(s.gate.recordBoundary(.init(token: zero, pts: 0, duration: 1024, codedIdr: false, audioGeneration: 1, acceptedSinks: .both), nowNs: 2))
        XCTAssertFalse(try s.confirm(sinks: .recording))
        XCTAssertEqual(s.gate.privacyView?.phase, .enteringStandby)
        XCTAssertTrue(try s.confirm())
        XCTAssertEqual(s.gate.privacyView?.phase, .standby)
    }
    func testDuplicateOrDiscontinuousAudioDoesNotInflateBoundary() throws {
        let s = try Show(); _ = try s.enter(); let zero = try s.token(.zero)
        func receipt(_ pts: Int64) -> CaptureAdmission.MediaBoundary { .init(token: zero, pts: pts, duration: 1024, codedIdr: false, audioGeneration: 2, acceptedSinks: .both) }
        XCTAssertFalse(s.gate.recordBoundary(receipt(0), nowNs: 2))
        XCTAssertFalse(s.gate.recordBoundary(receipt(0), nowNs: 2))
        XCTAssertFalse(s.gate.recordBoundary(receipt(2048), nowNs: 2))
        XCTAssertEqual(s.gate.privacyView?.phase, .enteringStandby)
        XCTAssertTrue(s.gate.recordBoundary(receipt(1024), nowNs: 2))
    }
    func testResumeCannotBypassUnknownRetirementAndUrgentStandbySupersedesReadiness() throws {
        let s = try Show(); let first = try s.enter()
        XCTAssertNil(s.gate.requestResume(preparation: s.prep, expectedRevision: first.revision, nowNs: 2))
        XCTAssertTrue(try s.confirm())
        let resume = try XCTUnwrap(s.gate.requestResume(preparation: s.prep, expectedRun: s.run, expectedRevision: first.revision, nowNs: 3))
        let oldRaw = try s.token(.raw)
        let urgent = try XCTUnwrap(s.gate.requestStandby(preparation: s.prep, nowNs: 4))
        XCTAssertEqual(urgent.deadlineNs, resume.deadlineNs)
        XCTAssertEqual(s.gate.commitResume(resume, nowNs: 5), .superseded)
        XCTAssertEqual(s.gate.allowsInput(oldRaw, nowNs: 5), .superseded)
        XCTAssertEqual(s.gate.privacyView?.desired, .standby)
    }
    func testExplicitResumeChangesRevisionAndOldRawCannotBorrowNewNativeOwner() throws {
        let s = try Show(); let oldRaw = try s.token(.raw); let request = try s.enter(); XCTAssertTrue(try s.confirm())
        let resume = try XCTUnwrap(s.gate.requestResume(preparation: s.prep, expectedRevision: request.revision, nowNs: 3))
        XCTAssertEqual(s.gate.commitResume(resume, nowNs: 4), .allow)
        XCTAssertEqual(s.gate.allowsInput(try s.token(.raw), nowNs: 4), .allow)
        XCTAssertEqual(s.gate.allowsInput(oldRaw, nowNs: 4), .superseded)
        XCTAssertNil(s.gate.requestResume(preparation: s.prep, expectedRevision: request.revision, nowNs: 5))
    }
    func testRepeatsDoNotExtendOriginalDeadlineAndWithheldTimerStillFails() throws {
        let s = try Show(); let request = try s.enter(now: 10)
        XCTAssertEqual(s.gate.requestStandby(preparation: s.prep, nowNs: 1_000)?.deadlineNs, request.deadlineNs)
        XCTAssertEqual(s.gate.allowsInput(try s.token(.zero), nowNs: try XCTUnwrap(request.deadlineNs)), .failed)
        XCTAssertEqual(s.gate.privacyView?.phase, .failed)
        XCTAssertEqual(s.gate.allowsInput(try s.token(.raw)), .failed)
    }
    func testStopIsStickyButAlreadySubmittedNativeTailIsNotFalseFailure() throws {
        let s = try Show(); _ = try s.enter(); let zero = try s.token(.zero)
        _ = s.gate.stop(nowNs: 2)
        XCTAssertEqual(s.gate.allowsInput(zero, nowNs: 2), .stopped)
        XCTAssertEqual(s.gate.outputAdmission(zero, nowNs: 2), .acceptedPrefix)
        XCTAssertFalse(s.gate.mediaHasFault(s.run))
        XCTAssertNil(s.gate.requestStandby(preparation: s.prep, nowNs: 3))
        XCTAssertNil(s.gate.requestResume(preparation: s.prep, expectedRevision: zero.revision, nowNs: 3))
    }
    func testPublisherEndDuringHeldAutoCleanupPersistsAndLateOldDetachCannotAffectChild() throws {
        let s = try Show(video: true); let publishing = try XCTUnwrap(s.gate.sinkToken(run: s.run, kind: .publishing))
        let rotation = try XCTUnwrap(s.gate.beginRotation(s.prep, nowNs: 10))
        XCTAssertTrue(s.gate.sinkTerminated(publishing)) // Actual End while old native cleanup is held.
        s.gate.broadcastEnded(s.prep, nowNs: 11)
        _ = s.gate.requestStandby(preparation: s.prep, nowNs: 12)
        s.gate.mediaRunClosed(s.run)
        let start = try XCTUnwrap(s.gate.reserveRotationStart(rotation))
        let child = try XCTUnwrap(s.gate.reserveMediaRun(start))
        XCTAssertEqual(s.gate.requiredSinkMask(run: child), .recording)
        XCTAssertFalse(s.gate.sinkTerminated(publishing))
        XCTAssertTrue(s.gate.commitMediaRun(child, start: start, nowNs: 13, broadcasting: false))
        XCTAssertEqual(s.gate.privacyView?.desired, .standby)
        XCTAssertEqual(s.gate.allowsInput(try XCTUnwrap(s.gate.mediaToken(child, kind: .raw)), nowNs: 13), .superseded)
    }
    func testRecordingOnlyCanConfirmButNoRemainingSinksCannotVacuouslyConfirm() throws {
        let s = try Show(publishing: false); _ = try s.enter()
        XCTAssertTrue(try s.confirm(sinks: .recording))
        let lost = try Show(publishing: false); _ = try lost.enter()
        XCTAssertTrue(lost.gate.sinkTerminated(try XCTUnwrap(lost.gate.sinkToken(run: lost.run, kind: .recording))))
        XCTAssertEqual(lost.gate.allowsInput(try lost.token(.zero), nowNs: 2), .failed)
        XCTAssertFalse(try lost.confirm(sinks: []))
        XCTAssertEqual(lost.gate.privacyView?.phase, .failed)
    }
    func testForeignAdmissionWithSameCountersCannotActOnAnyNativeOrSinkOwner() throws {
        let a = try Show(); let b = try Show()
        XCTAssertNotEqual(a.prep, b.prep); XCTAssertNotEqual(a.run, b.run)
        let foreign = try a.token(.raw)
        XCTAssertEqual(b.gate.allowsInput(foreign), .superseded)
        XCTAssertFalse(b.gate.nativeFault(foreign))
        XCTAssertFalse(b.gate.sinkTerminated(try XCTUnwrap(a.gate.sinkToken(run: a.run, kind: .recording))))
        XCTAssertNil(b.gate.requestStandby(preparation: a.prep, nowNs: 1))
        XCTAssertFalse(b.gate.recordBoundary(.init(token: foreign, pts: 0, duration: 1024, codedIdr: true, audioGeneration: 2, acceptedSinks: .both), nowNs: 1))
    }
    func testProductionRouteWaitRechecksTheOriginalTokenOutsideTheAdmissionLock() throws {
        let s = try Show(); let token = try s.token(.raw)
        let result = s.gate.rawAfterWait(token) {
            // A held native route finishes after the original user gesture.
            // This must not deadlock and must not grant the captured old token.
            _ = s.gate.requestStandby(preparation: s.prep, nowNs: 1)
            return true
        }
        XCTAssertEqual(result, .superseded)
        XCTAssertEqual(s.gate.outputAdmission(token), .acceptedPrefix)
    }
    func testPrestartSourceMutationAllowsNeitherRawNorReplacementAndKeepsOriginalBudget() throws {
        let gate = CaptureAdmission()
        let prep = try XCTUnwrap(gate.reservePreparation())
        XCTAssertTrue(gate.attached(prep)); XCTAssertTrue(gate.beginPermissions(prep))
        let source = try XCTUnwrap(gate.initialCameraSource(canvas: .init(width: 720, height: 1280)))
        _ = gate.cameraFrame(source, width: 720, height: 1280, nowNs: 0)
        XCTAssertTrue(gate.prepared(prep))
        let change = try XCTUnwrap(gate.reserveCameraSwitch(.front, source: source, nowNs: 1))
        let privacy = try XCTUnwrap(gate.requestStandby(preparation: prep, nowNs: 5))
        XCTAssertEqual(privacy.deadlineNs, change.deadlineNs)
        XCTAssertTrue(gate.parkPrivacySources(prep))
        let start = try XCTUnwrap(gate.reserveStart(prep))
        let run = try XCTUnwrap(gate.reserveMediaRun(start))
        XCTAssertEqual(gate.allowsInput(try XCTUnwrap(gate.mediaToken(run, kind: .slate)), nowNs: 6), .stopped)
        XCTAssertEqual(gate.allowsInput(try XCTUnwrap(gate.mediaToken(run, kind: .raw)), nowNs: 6), .stopped)
        XCTAssertFalse(gate.commitMediaRun(run, start: start, nowNs: change.deadlineNs, broadcasting: true))
        XCTAssertTrue(gate.mediaHasFault(run))
        XCTAssertNil(gate.elapsed(prep, nowNs: change.deadlineNs).captureNs)
    }
    func testConfirmedStandbyAutoChildGetsANewBoundNotAnExpiredOldConfirmationDeadline() throws {
        let s = try Show(video: true); _ = try s.enter(); XCTAssertTrue(try s.confirm())
        let oldRaw = try s.token(.raw)
        let rotation = try XCTUnwrap(s.gate.beginRotation(s.prep, nowNs: 20_000_000_000))
        s.gate.mediaRunClosed(s.run)
        let start = try XCTUnwrap(s.gate.reserveRotationStart(rotation))
        let child = try XCTUnwrap(s.gate.reserveMediaRun(start))
        XCTAssertTrue(s.gate.commitMediaRun(child, start: start, nowNs: 20_000_000_001, broadcasting: true))
        XCTAssertEqual(s.gate.privacyView?.request?.deadlineNs, 26_000_000_000)
        XCTAssertEqual(s.gate.allowsInput(try XCTUnwrap(s.gate.mediaToken(child, kind: .zero)), nowNs: 20_000_000_002), .allow)
        XCTAssertEqual(s.gate.outputAdmission(oldRaw), .superseded)
    }
    func testGenuineNativeFaultRemainsStickyEvenWhenOrdinaryStopWonFirst() throws {
        let s = try Show(); let submitted = try s.token(.raw)
        _ = s.gate.stop(nowNs: 1)
        XCTAssertTrue(s.gate.nativeFault(submitted))
        XCTAssertTrue(s.gate.mediaHasFault(s.run))
        XCTAssertEqual(s.gate.outputAdmission(submitted), .failed)
        XCTAssertFalse(s.gate.nativeFault(submitted))
    }
    func testQueuedPrivacyFailureCannotDefeatStopOrNewerStandby() throws {
        let stopped = try Show(); let old = try stopped.enter()
        let queued = { stopped.gate.claimPrivacyFailure(old, nowNs: 4) }
        _ = stopped.gate.stop(nowNs: 3)
        XCTAssertNil(queued()); XCTAssertFalse(stopped.gate.mediaHasFault(stopped.run))
        let newer = try Show(); let first = try newer.enter(); XCTAssertTrue(try newer.confirm())
        let resume = try XCTUnwrap(newer.gate.requestResume(preparation: newer.prep, expectedRevision: first.revision, nowNs: 3))
        let held = { newer.gate.claimPrivacyFailure(resume, nowNs: 5) }
        let urgent = try XCTUnwrap(newer.gate.requestStandby(preparation: newer.prep, nowNs: 4))
        XCTAssertNil(held()); XCTAssertFalse(newer.gate.mediaHasFault(newer.run))
        XCTAssertEqual(newer.gate.privacyView?.request, urgent)
    }
    func testCurrentPrivacyFailureClaimWinsAtomicallyAndRemainsStickyThroughStop() throws {
        let s = try Show(); let request = try s.enter()
        guard case .media(s.run)? = s.gate.claimPrivacyFailure(request, nowNs: 2) else { return XCTFail("Current failure was not claimed") }
        _ = s.gate.stop(nowNs: 3)
        XCTAssertTrue(s.gate.mediaHasFault(s.run))
        XCTAssertEqual(s.gate.outputAdmission(try s.token(.zero)), .failed)
        XCTAssertNil(s.gate.claimPrivacyFailure(request, nowNs: 4))
    }
    func testActualRouteFaultBeforePrivacyCloseIsStickyButLateRetiredEventIsIgnored() throws {
        for faultFirst in [true, false] {
            let s = try Show(); let token = try s.token(.raw)
            let option = try XCTUnwrap(AudioInputOption(id: "builtin", name: "Built-in", kind: .builtIn))
            var actual: String? = "builtin"; var withheldDiagnostics = 0
            let route = try AudioInputRouting(owner: .init(preparation: s.prep, epoch: 1), pin: AudioInputPin(requestedID: nil),
                available: [option], read: { actual }, failure: { withheldDiagnostics += 1 }, changed: { _ in })
            XCTAssertTrue(route.verify())
            if faultFirst { actual = nil; route.routeChanged() }
            XCTAssertEqual(CapturePrivacyRouteRetirement.close(route, admission: s.gate, token: token), faultFirst)
            if !faultFirst { actual = nil; route.routeChanged() }
            XCTAssertEqual(withheldDiagnostics, faultFirst ? 1 : 0)
            XCTAssertEqual(s.gate.mediaHasFault(s.run), faultFirst)
            XCTAssertFalse(route.permitsPCM())
        }
    }

}
