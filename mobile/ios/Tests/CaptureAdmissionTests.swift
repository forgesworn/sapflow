import XCTest
@testable import SapflowBroadcaster

final class CaptureAdmissionTests: XCTestCase {
    private func committedDropRun(_ gate: CaptureAdmission) throws -> CaptureAdmission.MediaRun {
        let preparation = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(preparation)); XCTAssertTrue(gate.beginPermissions(preparation)); XCTAssertTrue(gate.prepared(preparation))
        let start = try XCTUnwrap(gate.reserveStart(preparation)), run = try XCTUnwrap(gate.reserveMediaRun(start))
        XCTAssertTrue(gate.commitMediaRun(run, start: start, nowNs: 1, broadcasting: false))
        return run
    }
    func testNativeDropQueuedBeforeStopStillLatchesExactClosingRun() throws {
        let gate = CaptureAdmission(), run = try committedDropRun(gate)
        let token = try XCTUnwrap(gate.mediaToken(run, kind: .raw))
        _ = gate.stop(nowNs: 2)
        XCTAssertNotEqual(gate.allowsInput(token), .allow)
        XCTAssertTrue(gate.nativeDrop(token)); XCTAssertTrue(gate.mediaHasFault(run))
        XCTAssertFalse(gate.commitRecordingClosure(run))
    }
    func testOldNativeDropCannotFaultSuccessorAfterActualRunClose() throws {
        let gate = CaptureAdmission(), old = try committedDropRun(gate)
        let token = try XCTUnwrap(gate.mediaToken(old, kind: .raw)), stopped = gate.stop(nowNs: 2)
        gate.mediaRunClosed(old); XCTAssertTrue(gate.completeStop(stopped, cleanupComplete: true))
        let next = try committedDropRun(gate)
        XCTAssertFalse(gate.nativeDrop(token)); XCTAssertFalse(gate.mediaHasFault(next))
    }
    func testNormalStopCancellationPreservesSingleExactRecordingDecision() throws {
        let gate = CaptureAdmission(), run = try committedDropRun(gate)
        _ = gate.stop(nowNs: 2)
        XCTAssertTrue(gate.hasCommittedMediaRun(run)); XCTAssertFalse(gate.mediaHasFault(run))
        XCTAssertTrue(gate.commitRecordingClosure(run)); XCTAssertFalse(gate.commitRecordingClosure(run))
    }
    func testUncommittedRunCannotGrantRecordingDecision() throws {
        let gate = CaptureAdmission(), preparation = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(preparation)); XCTAssertTrue(gate.beginPermissions(preparation)); XCTAssertTrue(gate.prepared(preparation))
        let start = try XCTUnwrap(gate.reserveStart(preparation)), run = try XCTUnwrap(gate.reserveMediaRun(start))
        XCTAssertFalse(gate.hasCommittedMediaRun(run)); XCTAssertFalse(gate.commitRecordingClosure(run))
    }
    func testSameStartOldRecordingTicketCannotBorrowSuccessorRecorderBinding() throws {
        let gate = CaptureAdmission(), prep = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(prep)); XCTAssertTrue(gate.beginPermissions(prep)); XCTAssertTrue(gate.prepared(prep))
        let start = try XCTUnwrap(gate.reserveStart(prep)), admission = RecordingCaptureAdmission()
        let old = try XCTUnwrap(admission.begin(.segment(start))), oldRecorder = NSObject()
        XCTAssertTrue(admission.bindRecorder(old, recorder: oldRecorder))
        XCTAssertTrue(admission.closed(old, cleanupComplete: true))
        let current = try XCTUnwrap(admission.begin(.segment(start))), currentRecorder = NSObject()
        XCTAssertNotEqual(old, current)
        XCTAssertFalse(admission.bindRecorder(old, recorder: currentRecorder))
        XCTAssertTrue(admission.bindRecorder(current, recorder: currentRecorder))
        admission.cancel(prep)
        XCTAssertTrue(admission.ownsRecorder(current, recorder: currentRecorder))
        XCTAssertFalse(admission.ownsRecorder(old, recorder: currentRecorder))
        XCTAssertFalse(admission.ownsRecorder(current, recorder: oldRecorder))
    }
    func testRecordingLossLatchDoesNotTreatOrdinaryStopAsFailure() throws {
        let gate = CaptureAdmission(), run = try committedDropRun(gate), loss = CaptureRecordingLoss()
        _ = gate.stop(nowNs: 2)
        loss.mark(); XCTAssertFalse(loss.commit(admission: gate, run: run))
        XCTAssertFalse(gate.mediaHasFault(run))
    }

    func testFixedOrientationChoiceIsImmutableAcrossEveryPreparedAndCleanupPhase() throws {
        let gate = CaptureAdmission()
        XCTAssertTrue(gate.selectOrientationChoice(.landscapeTopLeft))
        let ticket = try XCTUnwrap(gate.reservePreparation())
        XCTAssertEqual(ticket.orientationChoice, .landscapeTopLeft)
        XCTAssertEqual(ticket.initialOrientation, .landscapeRight)
        XCTAssertFalse(gate.selectOrientationChoice(.portrait))
        XCTAssertNil(gate.observeOrientation(.portrait))
        XCTAssertTrue(gate.attached(ticket))
        XCTAssertEqual(gate.orientationAtAttachment(ticket), .landscapeRight)
        XCTAssertTrue(gate.beginPermissions(ticket))
        XCTAssertFalse(gate.selectOrientationChoice(.portrait)); XCTAssertNil(gate.observeOrientation(.landscapeLeft))
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertFalse(gate.selectOrientationChoice(.portrait))
        let start = try XCTUnwrap(gate.reserveStart(ticket))
        XCTAssertFalse(gate.selectOrientationChoice(.portrait)); XCTAssertNil(gate.observeOrientation(.portraitUpsideDown))
        XCTAssertTrue(gate.started(start)); XCTAssertNil(gate.beginRotation(ticket))
        XCTAssertNil(gate.observeOrientation(.portrait))
        let stopped = gate.stop()
        XCTAssertFalse(gate.selectOrientationChoice(.portrait))
        XCTAssertFalse(gate.completeStop(stopped, cleanupComplete: false))
        XCTAssertFalse(gate.selectOrientationChoice(.portrait))
        XCTAssertTrue(gate.completeStop(stopped, cleanupComplete: true))
        XCTAssertTrue(gate.selectOrientationChoice(.landscapeTopRight))
        let next = try XCTUnwrap(gate.reservePreparation())
        XCTAssertEqual(next.initialOrientation, .landscapeLeft)
        XCTAssertEqual(ticket.initialOrientation, .landscapeRight)
    }
    func testIdleAutoObservationInitialisesReservedShowEvenWhenItsQueuedWorkIsStale() throws {
        let gate = CaptureAdmission()
        // Model.updateOrientation observes synchronously, then reserve wins
        // before the native control queue consumes that idle observation.
        let idleEvent = try XCTUnwrap(gate.observeOrientation(.landscapeLeft))
        let ticket = try XCTUnwrap(gate.reservePreparation())
        XCTAssertEqual(ticket.orientationChoice, .automatic)
        XCTAssertEqual(ticket.initialOrientation, .landscapeLeft)
        XCTAssertFalse(gate.permitsOrientation(idleEvent))
        XCTAssertTrue(gate.attached(ticket))
        var plan = OrientationRestartPlan()
        plan.prepare(try XCTUnwrap(gate.orientationAtAttachment(ticket)))
        XCTAssertEqual(plan.desired, .landscapeLeft)
        XCTAssertTrue(gate.beginPermissions(ticket))
        let permissionEvent = try XCTUnwrap(gate.observeOrientation(.portraitUpsideDown))
        XCTAssertTrue(gate.permitsOrientation(permissionEvent))
        XCTAssertNil(plan.request(permissionEvent.orientation, running: false))
        XCTAssertEqual(plan.desired, .portraitUpsideDown)
        XCTAssertTrue(gate.prepared(ticket)); let start = try XCTUnwrap(gate.reserveStart(ticket))
        let startingEvent = try XCTUnwrap(gate.observeOrientation(.landscapeRight))
        XCTAssertTrue(gate.permitsOrientation(startingEvent))
        XCTAssertTrue(gate.started(start))
        XCTAssertTrue(gate.permitsOrientation(startingEvent))
        XCTAssertNotNil(plan.request(startingEvent.orientation, running: true))
        XCTAssertNotNil(gate.beginRotation(ticket))
    }
    func testLatestAutoPoseAtAttachmentAndNewerObservationWinWithoutFixedSuccessorMutation() throws {
        let gate = CaptureAdmission()
        let a = try XCTUnwrap(gate.reservePreparation())
        let earlier = try XCTUnwrap(gate.observeOrientation(.landscapeLeft))
        let later = try XCTUnwrap(gate.observeOrientation(.landscapeRight))
        XCTAssertFalse(gate.permitsOrientation(earlier)); XCTAssertTrue(gate.permitsOrientation(later))
        XCTAssertTrue(gate.attached(a)); XCTAssertEqual(gate.orientationAtAttachment(a), .landscapeRight)
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        XCTAssertFalse(gate.permitsOrientation(later))
        XCTAssertTrue(gate.selectOrientationChoice(.portrait))
        let b = try XCTUnwrap(gate.reservePreparation())
        XCTAssertTrue(gate.attached(b)); XCTAssertEqual(gate.orientationAtAttachment(b), .portrait)
        XCTAssertFalse(gate.permitsOrientation(later)); XCTAssertNil(gate.orientationAtAttachment(a))
        XCTAssertNil(gate.observeOrientation(.landscapeRight))
    }
    func testAudioOnlyIgnoresSelectedFixedOrientationAndForegroundFencesChoice() throws {
        let gate = CaptureAdmission()
        XCTAssertTrue(gate.selectOrientationChoice(.landscapeTopLeft))
        let audio = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertEqual(audio.orientationChoice, .automatic); XCTAssertNil(audio.initialOrientation)
        XCTAssertTrue(gate.attached(audio)); XCTAssertNil(gate.orientationAtAttachment(audio))
        XCTAssertNil(gate.observeOrientation(.portrait))
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        _ = gate.visibilityChanged(foreground: false)
        XCTAssertFalse(gate.selectOrientationChoice(.portrait)); XCTAssertNil(gate.observeOrientation(.portrait))
        _ = gate.visibilityChanged(foreground: true)
        let video = try XCTUnwrap(gate.reservePreparation())
        XCTAssertEqual(video.orientationChoice, .landscapeTopLeft)
    }
    func testChoosingAutoAgainInvalidatesQueuedIdlePoseAndStopFencesLateWork() throws {
        let gate = CaptureAdmission()
        let old = try XCTUnwrap(gate.observeOrientation(.landscapeLeft))
        XCTAssertTrue(gate.selectOrientationChoice(.portrait))
        XCTAssertTrue(gate.selectOrientationChoice(.automatic))
        XCTAssertFalse(gate.permitsOrientation(old))
        let ticket = try XCTUnwrap(gate.reservePreparation())
        let event = try XCTUnwrap(gate.observeOrientation(.portrait))
        XCTAssertTrue(gate.permitsOrientation(event))
        _ = gate.stop()
        XCTAssertFalse(gate.permitsOrientation(event)); XCTAssertNil(gate.observeOrientation(.landscapeRight))
        XCTAssertFalse(gate.prepared(ticket))
    }
    func testElapsedBeginsOnlyAtFirstCommittedStartAndFreezesExplicitStop() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertNil(gate.elapsed(ticket, nowNs: 50).captureNs)
        XCTAssertTrue(gate.prepared(ticket)); let start = gate.reserveStart(ticket)!
        XCTAssertNil(gate.elapsed(ticket, nowNs: 90).captureNs)
        XCTAssertTrue(gate.started(start, nowNs: 100))
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 150).captureNs, 50)
        XCTAssertFalse(gate.started(start, nowNs: 160))
        _ = gate.stop(nowNs: 200); _ = gate.stop(nowNs: 300)
        let stopped = gate.elapsed(ticket, nowNs: 1000)
        XCTAssertEqual(stopped.captureNs, 100); XCTAssertEqual(stopped.broadcastNs, 100)
        XCTAssertFalse(stopped.captureRunning); XCTAssertFalse(stopped.broadcastRunning)
    }
    func testRotationStartRetainsLogicalCaptureAndBroadcastAnchors() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertTrue(gate.started(gate.reserveStart(ticket)!, nowNs: 100))
        let child = gate.beginRotation(ticket)!
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 500).captureNs, 400)
        XCTAssertTrue(gate.started(gate.reserveRotationStart(child)!, nowNs: 1000))
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 1200).captureNs, 1100)
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 1200).broadcastNs, 1100)
    }
    func testDestinationEndFreezesOnlyBroadcastAndNeverRenewsItOnRotation() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertTrue(gate.started(gate.reserveStart(ticket)!, nowNs: 100))
        gate.broadcastEnded(ticket, nowNs: 150); gate.broadcastEnded(ticket, nowNs: 180)
        let child = gate.beginRotation(ticket)!
        XCTAssertTrue(gate.started(gate.reserveRotationStart(child)!, nowNs: 200))
        let value = gate.elapsed(ticket, nowNs: 300)
        XCTAssertEqual(value.captureNs, 200); XCTAssertTrue(value.captureRunning)
        XCTAssertEqual(value.broadcastNs, 50); XCTAssertFalse(value.broadcastRunning)
    }
    func testStopWinningAtCommitBoundaryCannotStartOrReviveElapsed() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.prepared(ticket)); let queued = gate.reserveStart(ticket)!
        _ = gate.stop(nowNs: 100)
        XCTAssertFalse(gate.started(queued, nowNs: 101))
        XCTAssertNil(gate.elapsed(ticket, nowNs: 200).captureNs)
        let other = CaptureAdmission(), admitted = preparing(other)
        XCTAssertTrue(other.prepared(admitted))
        XCTAssertTrue(other.started(other.reserveStart(admitted)!, nowNs: 100))
        _ = other.stop(nowNs: 101)
        XCTAssertEqual(other.elapsed(admitted, nowNs: 1000).captureNs, 1)
    }
    func testEndBeforeFirstStartCannotManufactureBroadcastClock() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        gate.broadcastEnded(ticket, nowNs: 50)
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertTrue(gate.started(gate.reserveStart(ticket)!, nowNs: 100))
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 200).captureNs, 100)
        XCTAssertNil(gate.elapsed(ticket, nowNs: 200).broadcastNs)
        XCTAssertFalse(gate.elapsed(ticket, nowNs: 200).broadcastRunning)
    }
    func testLocalOnlyClockHasNoBroadcastAndClockRollbackDoesNotUnderflow() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertTrue(gate.started(gate.reserveStart(ticket)!, nowNs: 100, broadcasting: false))
        XCTAssertNil(gate.elapsed(ticket, nowNs: 99).captureNs)
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 200).captureNs, 100)
        XCTAssertNil(gate.elapsed(ticket, nowNs: 200).broadcastNs)
        _ = gate.stop(nowNs: 90)
        XCTAssertNil(gate.elapsed(ticket, nowNs: UInt64.max).captureNs)
    }
    func testRetiredDestinationEndCannotFreezeSuccessorClocks() {
        let gate = CaptureAdmission(), old = preparing(gate)
        XCTAssertTrue(gate.prepared(old)); XCTAssertTrue(gate.started(gate.reserveStart(old)!, nowNs: 100))
        XCTAssertTrue(gate.completeStop(gate.stop(nowNs: 200), cleanupComplete: true))
        let next = preparing(gate); XCTAssertTrue(gate.prepared(next))
        XCTAssertTrue(gate.started(gate.reserveStart(next)!, nowNs: 1000))
        gate.broadcastEnded(old, nowNs: 1001)
        XCTAssertEqual(gate.elapsed(next, nowNs: 1010).broadcastNs, 10)
        XCTAssertTrue(gate.elapsed(next, nowNs: 1010).broadcastRunning)
        XCTAssertNil(gate.elapsed(old, nowNs: 1010).captureNs)
    }
    func testDefaultPreparationPreserves720AndReservesProfileSelectionSynchronously() {
        let gate = CaptureAdmission()
        XCTAssertTrue(gate.canChooseCaptureProfile)
        let ticket = gate.reservePreparation()!
        XCTAssertEqual(ticket.profile, .hd720p30)
        XCTAssertFalse(gate.canChooseCaptureProfile)
        XCTAssertNil(gate.reservePreparation(profile: .sd480p30))
        XCTAssertEqual(gate.currentPreparation?.profile, .hd720p30)
    }
    func testImmutableProfileSurvivesPreparationStartAndRepeatedRotation() {
        for profile in CaptureIntent.allCases.filter(\.hasVideo) {
            let gate = CaptureAdmission(), ticket = gate.reservePreparation(profile: profile)!
            XCTAssertEqual(ticket.profile, profile)
            XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
            XCTAssertTrue(gate.prepared(ticket)); XCTAssertEqual(gate.preparedTicket?.profile, profile)
            let start = gate.reserveStart(ticket)!
            XCTAssertEqual(start.preparation.profile, profile); XCTAssertTrue(gate.started(start))
            for _ in 0..<3 {
                let child = gate.beginRotation(ticket)!
                XCTAssertEqual(child.preparation.profile, profile)
                let restart = gate.reserveRotationStart(child)!
                XCTAssertEqual(restart.preparation.profile, profile)
                XCTAssertTrue(gate.started(restart)); XCTAssertFalse(gate.canChooseCaptureProfile)
                XCTAssertNil(gate.reservePreparation(profile: .defaultProfile))
            }
            XCTAssertEqual(gate.currentPreparation?.profile, profile)
        }
    }
    func testAudioOnlyIntentHasNoRotationEpochAndStopFencesItsLateStart() throws {
        let gate = CaptureAdmission()
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertEqual(ticket.profile, .audioOnly48kMono)
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        XCTAssertTrue(gate.prepared(ticket))
        let start = try XCTUnwrap(gate.reserveStart(ticket))
        XCTAssertTrue(gate.started(start, nowNs: 100))
        XCTAssertNil(gate.beginRotation(ticket)); XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertEqual(gate.elapsed(ticket, nowNs: 200).captureNs, 100)
        let stop = gate.stop(nowNs: 300)
        XCTAssertFalse(gate.started(start)); XCTAssertNil(gate.reservePreparation(profile: .hd720p30))
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        let successor = try XCTUnwrap(gate.reservePreparation(profile: .hd720p30))
        XCTAssertFalse(gate.prepared(ticket)); XCTAssertFalse(gate.started(start))
        XCTAssertNil(gate.beginRotation(ticket)); XCTAssertEqual(gate.currentPreparation, successor)
    }
    func testAudioOnlyStopDuringPermissionCannotPrepareOrStartLater() throws {
        let gate = CaptureAdmission()
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        let stop = gate.stop()
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertNil(gate.beginRotation(ticket))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
    }
    func testOldProfileCallbacksCannotAdoptStartOrRotateNewProfile() {
        let gate = CaptureAdmission(), old = gate.reservePreparation(profile: .sd480p30)!
        XCTAssertTrue(gate.attached(old)); XCTAssertTrue(gate.beginPermissions(old)); XCTAssertTrue(gate.prepared(old))
        let oldStart = gate.reserveStart(old)!
        XCTAssertTrue(gate.started(oldStart)); let oldRotation = gate.beginRotation(old)!
        let stop = gate.stop(); XCTAssertFalse(gate.canChooseCaptureProfile)
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true)); XCTAssertTrue(gate.canChooseCaptureProfile)
        let next = gate.reservePreparation(profile: .fullHD1080p30)!
        XCTAssertFalse(gate.attached(old)); XCTAssertFalse(gate.prepared(old)); XCTAssertFalse(gate.started(oldStart))
        XCTAssertNil(gate.reserveRotationStart(oldRotation)); XCTAssertNil(gate.beginRotation(old))
        XCTAssertFalse(gate.failed(old, cleanupComplete: true)); XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertEqual(gate.currentPreparation, next); XCTAssertEqual(next.profile, .fullHD1080p30)
        XCTAssertTrue(gate.attached(next)); XCTAssertTrue(gate.beginPermissions(next)); XCTAssertTrue(gate.prepared(next))
        XCTAssertTrue(gate.started(gate.reserveStart(next)!))
        XCTAssertEqual(gate.currentPreparation?.profile, .fullHD1080p30)
    }
    func testProfileSelectionCannotBypassStoppedFactoryOrUncertainCleanup() {
        let gate = CaptureAdmission(), old = gate.reservePreparation(profile: .sd480p30)!, stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertFalse(gate.canChooseCaptureProfile); XCTAssertNil(gate.reservePreparation(profile: .hd720p30))
        XCTAssertFalse(gate.candidateClosed(old, cleanupComplete: true))
        XCTAssertFalse(gate.canChooseCaptureProfile)
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertTrue(gate.canChooseCaptureProfile)
        let next = gate.reservePreparation(profile: .hd720p30)!
        XCTAssertEqual(next.profile, .hd720p30); XCTAssertFalse(gate.canChooseCaptureProfile)
    }
    func testStopWinningAfterRotationStartCannotReopenItsProfile() {
        let gate = CaptureAdmission(), ticket = gate.reservePreparation(profile: .fullHD1080p30)!
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket)); XCTAssertTrue(gate.prepared(ticket))
        XCTAssertTrue(gate.started(gate.reserveStart(ticket)!))
        let restart = gate.reserveRotationStart(gate.beginRotation(ticket)!)!
        let stop = gate.stop()
        XCTAssertFalse(gate.started(restart)); XCTAssertFalse(gate.canChooseCaptureProfile)
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertFalse(gate.started(restart)); XCTAssertTrue(gate.canChooseCaptureProfile)
        XCTAssertEqual(gate.reservePreparation()?.profile, .hd720p30)
    }
    func testEndRetiresEveryInitialPreparationPhaseBeforeStartCommits() {
        for stage in 0...4 {
            let gate = CaptureAdmission(), ticket = gate.reservePreparation()!
            if stage >= 1 { XCTAssertTrue(gate.attached(ticket)) }
            if stage >= 2 { XCTAssertTrue(gate.beginPermissions(ticket)) }
            if stage >= 3 { XCTAssertTrue(gate.prepared(ticket)) }
            let start = stage == 4 ? gate.reserveStart(ticket) : nil
            let stop = gate.stopIfPending(ticket)!
            XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertNil(gate.reserveStart(ticket))
            if let start { XCTAssertFalse(gate.started(start)) }
            if stage == 0 {
                XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true))
                XCTAssertTrue(gate.candidateClosed(ticket, cleanupComplete: true))
            } else { XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true)) }
            XCTAssertNotNil(gate.reservePreparation())
        }
    }
    func testEndWinningBeforeStartedRequiresAssignedRunCleanup() {
        let gate = CaptureAdmission(), intent = preparing(gate)
        XCTAssertTrue(gate.prepared(intent)); let start = gate.reserveStart(intent)!
        let end = gate.stopIfPending(intent)!
        XCTAssertFalse(gate.started(start))
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(end, cleanupComplete: true))
    }
    func testStartedWinningBeforeEndPreservesIndependentRecording() {
        let gate = CaptureAdmission(), intent = running(gate)
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertEqual(gate.currentPreparation, intent); XCTAssertNil(gate.reservePreparation())
    }
    func testEndPreservesExistingRecordingThroughRotationAndItsStartingPhase() {
        let gate = CaptureAdmission(), intent = running(gate)
        let child = gate.beginRotation(intent)!
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertTrue(gate.permitsRotation(child))
        let restart = gate.reserveRotationStart(child)!
        XCTAssertNil(gate.stopIfPending(intent)); XCTAssertTrue(gate.permitsStart(restart))
        XCTAssertTrue(gate.started(restart)); XCTAssertEqual(gate.currentPreparation, intent)
    }
    func testOldEndCannotRetireOrFenceSuccessorPreparation() {
        let gate = CaptureAdmission(), old = preparing(gate)
        let stopped = gate.stopIfPending(old)!
        XCTAssertTrue(gate.completeStop(stopped, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertNil(gate.stopIfPending(old)); XCTAssertFalse(gate.permitsStop(stopped))
        XCTAssertTrue(gate.permitsPreparation(next))
    }
    func testRepeatedEndJoinsFactoryAndActualCandidateCleanupBarrier() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!
        let end = gate.stopIfPending(factory)!
        XCTAssertEqual(gate.stopIfPending(factory), end)
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertEqual(gate.stopIfPending(factory), end)
        XCTAssertTrue(gate.candidateClosed(factory, cleanupComplete: true)); XCTAssertNil(gate.stopIfPending(factory))
    }
    func testLateCandidateCannotReleaseEndWithUnconfirmedNativeCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, end = gate.stopIfPending(factory)!
        XCTAssertFalse(gate.completeStop(end, cleanupComplete: false))
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(end, cleanupComplete: true)); XCTAssertNotNil(gate.reservePreparation())
    }
    func testPreparationFailureCannotRetireCommittedRecordingIntent() {
        let gate = CaptureAdmission(), intent = running(gate)
        XCTAssertFalse(gate.failed(intent, cleanupComplete: true)); XCTAssertEqual(gate.currentPhase, .running)
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = preparing(gate); XCTAssertNotNil(gate.stopIfPending(next))
    }
    private func preparing(_ gate: CaptureAdmission) -> CaptureAdmission.Preparation {
        let ticket = gate.reservePreparation()!
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        return ticket
    }
    private func running(_ gate: CaptureAdmission) -> CaptureAdmission.Preparation {
        let ticket = preparing(gate); XCTAssertTrue(gate.prepared(ticket))
        XCTAssertTrue(gate.started(gate.reserveStart(ticket)!)); return ticket
    }
    func testStopBeforePermissionResponseCannotReprepareEvenAfterCleanupFinishes() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        let stop = gate.stop()
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
        XCTAssertNil(gate.preparedTicket); XCTAssertNotNil(gate.reservePreparation())
    }
    func testStopBetweenPermissionResponseAndActorDispatchInvalidatesContinuation() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertTrue(gate.permitsPreparation(ticket)) // first permission answer
        _ = gate.stop() // second answer/control task has not been delivered
        XCTAssertFalse(gate.permitsPreparation(ticket)); XCTAssertFalse(gate.prepared(ticket))
    }
    func testDuplicatePrepareAndPermissionRequestsAreRejectedBeforeQueueing() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertNil(gate.reservePreparation()); XCTAssertFalse(gate.beginPermissions(ticket))
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertNil(gate.reservePreparation())
    }
    func testDestinationMustBeTheExactCurrentReservationBeforePermissions() {
        let gate = CaptureAdmission(), old = gate.reservePreparation()!
        XCTAssertFalse(gate.beginPermissions(old))
        XCTAssertFalse(gate.candidateClosed(old, cleanupComplete: true))
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = gate.reservePreparation()!
        XCTAssertFalse(gate.attached(old)); XCTAssertFalse(gate.beginPermissions(old))
        XCTAssertTrue(gate.attached(next)); XCTAssertTrue(gate.beginPermissions(next))
    }
    func testQueuedStartCannotWinSynchronousStop() {
        let gate = CaptureAdmission(), prepared = preparing(gate)
        XCTAssertTrue(gate.prepared(prepared)); let start = gate.reserveStart(prepared)!
        XCTAssertTrue(gate.permitsStart(start)); _ = gate.stop()
        XCTAssertFalse(gate.permitsStart(start)); XCTAssertFalse(gate.started(start))
    }
    func testOldStartApprovalCannotStartNewPreparation() {
        let gate = CaptureAdmission(), old = preparing(gate)
        XCTAssertTrue(gate.prepared(old)); let oldStart = gate.reserveStart(old)!
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        let next = preparing(gate); XCTAssertTrue(gate.prepared(next))
        let newStart = gate.reserveStart(next)!
        XCTAssertFalse(gate.started(oldStart)); XCTAssertTrue(gate.started(newStart))
    }
    func testStartConsumesOnlyItsPreparedDestinationOnce() {
        let gate = CaptureAdmission(), ticket = preparing(gate)
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.prepared(ticket))
        let start = gate.reserveStart(ticket)!
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.started(start))
        XCTAssertFalse(gate.started(start)); XCTAssertNil(gate.preparedTicket)
    }
    func testHealthyRotationIsChildOfSameIntentAndRequiresFreshStartApproval() {
        let gate = CaptureAdmission(), intent = running(gate)
        let rotation = gate.beginRotation(intent)!
        XCTAssertEqual(gate.beginRotation(intent), rotation)
        XCTAssertEqual(rotation.preparation, intent); XCTAssertTrue(gate.permitsRotation(rotation))
        let restart = gate.reserveRotationStart(rotation)!
        XCTAssertFalse(gate.permitsRotation(rotation)); XCTAssertNil(gate.reserveRotationStart(rotation))
        XCTAssertEqual(restart.preparation, intent); XCTAssertTrue(gate.started(restart))
        XCTAssertEqual(gate.currentPreparation, intent)
    }
    func testStopFencesRotationBeforeQueuedOrientationCancelOrOldCleanup() {
        let gate = CaptureAdmission(), intent = running(gate)
        let rotation = gate.beginRotation(intent)!, stop = gate.stop()
        XCTAssertFalse(gate.permitsRotation(rotation)); XCTAssertNil(gate.reserveRotationStart(rotation))
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNil(gate.reserveRotationStart(rotation)); XCTAssertNil(gate.currentPreparation)
    }
    func testStopAfterRotationApprovalStillRejectsDelayedRestart() {
        let gate = CaptureAdmission(), intent = running(gate)
        let start = gate.reserveRotationStart(gate.beginRotation(intent)!)!
        _ = gate.stop(); XCTAssertFalse(gate.started(start))
    }
    func testFailedCleanupRetainsCapacityUntilActualMatchingOwnerCompletes() {
        let gate = CaptureAdmission(); _ = running(gate); let stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertEqual(gate.currentPhase, .retained); XCTAssertNil(gate.reservePreparation())
        XCTAssertEqual(gate.stop(), stop); XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertEqual(gate.currentPhase, .idle); XCTAssertNotNil(gate.reservePreparation())
    }
    func testOldCleanupCannotUnlockNewOwnerAndDuplicateStopIsCoalesced() {
        let gate = CaptureAdmission(); _ = preparing(gate); let old = gate.stop()
        XCTAssertEqual(gate.stop(), old); XCTAssertTrue(gate.completeStop(old, cleanupComplete: true))
        _ = preparing(gate); let current = gate.stop()
        XCTAssertNotEqual(current, old); XCTAssertFalse(gate.completeStop(old, cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation()); XCTAssertTrue(gate.completeStop(current, cleanupComplete: true))
    }
    func testFactoryAndPermissionFailuresRetireOnlyExactIntent() {
        let gate = CaptureAdmission(), old = gate.reservePreparation()!
        XCTAssertTrue(gate.failed(old, cleanupComplete: true)) // no sink was constructed
        let next = preparing(gate)
        XCTAssertFalse(gate.failed(old, cleanupComplete: true))
        XCTAssertTrue(gate.permitsPreparation(next))
        XCTAssertTrue(gate.failed(next, cleanupComplete: true)) // denied permission/native cleanup done
        XCTAssertNotNil(gate.reservePreparation())
    }
    func testLateFailureCannotBypassStopBarrierAndUnknownConfigurationCleanupBlocks() {
        let gate = CaptureAdmission(), ticket = preparing(gate), stop = gate.stop()
        XCTAssertFalse(gate.failed(ticket, cleanupComplete: true)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertFalse(gate.failed(next, cleanupComplete: false)); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
    }
    func testIdleStopHoldsQueueBarrierAndNeverStartsCapture() {
        let gate = CaptureAdmission(), stop = gate.stop()
        XCTAssertNil(stop.preparation); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNil(gate.currentPreparation); XCTAssertNotNil(gate.reservePreparation())
    }
    func testDelayedDuplicateStopContinuationCannotResampleAndStopSuccessor() {
        let gate = CaptureAdmission(); _ = preparing(gate)
        let stopA = gate.stop(), duplicateA = gate.stop()
        var touched: [String] = []
        // This is the exact guard used before the Engine's queued Stop and
        // interruption continuations mutate native state or sample another Stop.
        let delayed = {
            guard gate.permitsStop(duplicateA) else { return }
            touched.append("stopped"); _ = gate.stop()
        }
        XCTAssertTrue(gate.completeStop(stopA, cleanupComplete: true))
        let next = preparing(gate)
        delayed()
        XCTAssertTrue(touched.isEmpty); XCTAssertTrue(gate.permitsPreparation(next))
        XCTAssertEqual(gate.currentPhase, .permissions)
    }
    func testStopWaitsForFactoryDeliveryAndActualRejectedCandidateCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: true)) // old ingress is already closed
        XCTAssertNil(gate.reservePreparation()); XCTAssertTrue(gate.awaitsCandidate(factory))
        // A's factory returns only now. Stop has not released the reservation.
        XCTAssertFalse(gate.attached(factory)); XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: false))
        XCTAssertEqual(gate.currentPhase, .retained); XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.candidateClosed(factory, cleanupComplete: true))
        let next = preparing(gate)
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true))
        XCTAssertTrue(gate.permitsPreparation(next))
    }
    func testFactoryThrowDuringStopDoesNotBypassOutstandingNativeCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true)) // factory threw, no candidate
        XCTAssertNil(gate.reservePreparation())
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
        XCTAssertNotNil(gate.reservePreparation())
    }
    func testActualCandidateCloseCannotReleaseFailedIngressCleanup() {
        let gate = CaptureAdmission(), factory = gate.reservePreparation()!, stop = gate.stop()
        XCTAssertFalse(gate.completeStop(stop, cleanupComplete: false))
        XCTAssertFalse(gate.candidateClosed(factory, cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation()); XCTAssertEqual(gate.currentPhase, .retained)
        XCTAssertTrue(gate.completeStop(stop, cleanupComplete: true))
    }
    func testTicketExhaustionNeverWrapsIntoStaleApproval() {
        let gate = CaptureAdmission(initialSerial: UInt64.max - 1), ticket = gate.reservePreparation()!
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket)); XCTAssertTrue(gate.prepared(ticket))
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertTrue(gate.completeStop(gate.stop(), cleanupComplete: true))
        XCTAssertNil(gate.reservePreparation())
    }
}
