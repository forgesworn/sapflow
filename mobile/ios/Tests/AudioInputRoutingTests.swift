import XCTest
@testable import SapflowBroadcaster

final class AudioInputRoutingTests: XCTestCase {
    private let a = AudioInputOption(id: "a", name: "Built-in", kind: .builtIn)!
    private let b = AudioInputOption(id: "b", name: "USB", kind: .usb)!
    private func owner(_ admission: CaptureAdmission, epoch: UInt64) -> AudioInputOwner {
        AudioInputOwner(preparation: admission.currentPreparation!, epoch: epoch)
    }
    func testPreferredIsNotActualProofAndMismatchFailsOnce() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        var actual: String? = "b"; var failures = 0
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: "a"), available: [a,b], read: { actual }, failure: { failures += 1 }, changed: { _ in })
        XCTAssertFalse(route.permitsPCM()); XCTAssertFalse(route.verify())
        actual = "a"; XCTAssertFalse(route.verify()); XCTAssertEqual(failures, 1)
    }
    func testDefaultPinSurvivesRotationAndOldCallbackCannotRepin() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let pin = AudioInputPin(requestedID: nil); var actual: String? = "a"
        let old = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: pin, available: [a,b], read: { actual }, failure: {}, changed: { _ in })
        XCTAssertTrue(old.verify()); old.close()
        actual = "b"; var failures = 0
        let next = try AudioInputRouting(owner: owner(admission, epoch: 2), pin: pin, available: [a,b], read: { actual }, failure: { failures += 1 }, changed: { _ in })
        XCTAssertFalse(next.verify()); XCTAssertFalse(old.verify()); XCTAssertEqual(pin.resolvedID, "a"); XCTAssertEqual(failures, 1)
    }
    func testSameInputOutputOnlyEventAndDisconnectBeforePCM() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        var actual: String? = "a"; var observed = 0
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a], read: { actual }, failure: {}, changed: { _ in observed += 1 })
        XCTAssertTrue(route.verify()); XCTAssertTrue(route.verify()); XCTAssertEqual(observed, 2)
        actual = nil; XCTAssertFalse(route.verify()); XCTAssertFalse(route.permitsPCM())
    }
    func testClosedEpochCannotStopNewPreparation() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        var actual: String? = "a"; var failures = 0
        let old = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a], read: { actual }, failure: { failures += 1 }, changed: { _ in })
        XCTAssertTrue(old.verify()); old.close(); actual = nil
        XCTAssertFalse(old.verify()); XCTAssertEqual(failures, 0)
        XCTAssertTrue(try AudioInputRouting(owner: owner(admission, epoch: 2), pin: AudioInputPin(requestedID: nil), available: [b], read: { "b" }, failure: {}, changed: { _ in }).verify())
    }
    func testVideoPrepareAndRotationConfigureBeforeAttachingRoute() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let pin = AudioInputPin(requestedID: nil)
        var prior: AudioInputRouting?
        for epoch: UInt64 in [1, 2] {
            var configured = false; var started = false
            let route = try AudioInputRouting.activate(configure: {
                // Video configuration can retire only a previous native owner.
                prior?.close(); configured = true
            }, attach: {
                XCTAssertTrue(configured)
                return try AudioInputRouting(owner: self.owner(admission, epoch: epoch), pin: pin,
                    available: [self.a], read: { XCTAssertTrue(started); return "a" }, failure: {}, changed: { _ in })
            }, start: { started = true })
            XCTAssertTrue(route.permitsPCM()); prior = route
        }
        XCTAssertEqual(pin.resolvedID, "a")
    }

    func testLatchedFaultSurvivesStopBeforeQueuedNotification() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        var actual: String? = "a"; var queued: (() -> Void)?; var delivered = false
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil),
            available: [a], read: { actual }, failure: { queued = { delivered = true } }, changed: { _ in })
        XCTAssertTrue(route.verify()); actual = nil; XCTAssertFalse(route.verify())
        XCTAssertFalse(delivered)
        // Engine consumes this result before deciding whether it may rotate or
        // finalise, independently of an empty sample queue/late notification.
        XCTAssertTrue(route.close()); XCTAssertFalse(route.permitsPCM())
        queued?(); XCTAssertTrue(delivered)
    }

    func testCloseDuringNativeReadRejectsLateObservation() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0), finished = DispatchSemaphore(value: 0)
        let pin = AudioInputPin(requestedID: nil)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: pin, available: [a], read: {
            entered.signal(); _ = resume.wait(timeout: .now() + 2); return "a"
        }, failure: {}, changed: { _ in XCTFail("Closed owner adopted late input") })
        DispatchQueue.global().async { XCTAssertFalse(route.verify()); finished.signal() }
        XCTAssertEqual(entered.wait(timeout: .now() + 2), .success)
        let closed = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { route.close(); closed.signal() }
        let closeBeforeRelease = closed.wait(timeout: .now() + 1)
        resume.signal(); XCTAssertEqual(closeBeforeRelease, .success)
        XCTAssertEqual(finished.wait(timeout: .now() + 2), .success)
        XCTAssertNil(pin.resolvedID); XCTAssertFalse(route.permitsPCM())
    }
    func testNewerNativeVerificationWinsOverOlderRead() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0), finished = DispatchSemaphore(value: 0)
        let reads = RoutingReadCounter(); let pin = AudioInputPin(requestedID: nil)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: pin, available: [a,b], read: {
            let number = reads.next()
            if number == 2 { entered.signal(); _ = resume.wait(timeout: .now() + 2); return "b" }
            return "a"
        }, failure: { XCTFail("Old read must be discarded without tainting the newer input") }, changed: { _ in })
        XCTAssertTrue(route.verify())
        DispatchQueue.global().async { XCTAssertFalse(route.verify()); finished.signal() }
        XCTAssertEqual(entered.wait(timeout: .now() + 2), .success)
        XCTAssertTrue(route.verify()); resume.signal()
        XCTAssertEqual(finished.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(pin.resolvedID, "a"); XCTAssertTrue(route.permitsPCM())
    }

    func testActivationCoalescesNotificationBeforePinningLatestInput() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 1, id: "a"); let pin = AudioInputPin(requestedID: nil)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: pin, available: [a,b],
            read: { probe.read() }, failure: { XCTFail("Stable activation must not fail") }, changed: { _ in })
        let complete = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            do { _ = try AudioInputRouting.activate(configure: {}, attach: { route }, start: {}) }
            catch { XCTFail("Stable activation unexpectedly failed: \(error)") }
            complete.signal()
        }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        probe.set("b"); route.routeChanged(); XCTAssertNil(pin.resolvedID)
        probe.resume.signal(); XCTAssertEqual(complete.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(pin.resolvedID, "b"); XCTAssertTrue(route.permitsPCM())
    }
    func testActivationCannotAdmitUnavailablePendingRecheck() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 1, id: "a"); let pin = AudioInputPin(requestedID: nil)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: pin, available: [a],
            read: { probe.read() }, failure: {}, changed: { _ in XCTFail("No stable actual input") })
        let complete = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            do { _ = try AudioInputRouting.activate(configure: {}, attach: { route }, start: {}); XCTFail("Unavailable input was admitted") }
            catch {}
            complete.signal()
        }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        probe.set(nil); route.routeChanged(); probe.resume.signal()
        XCTAssertEqual(complete.wait(timeout: .now() + 2), .success)
        XCTAssertNil(pin.resolvedID); XCTAssertFalse(route.permitsPCM())
    }
    private func transition(_ result: String?, stop: Bool = false, timeout: Bool = false) throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 2, id: "a"); let failures = RoutingReadCounter()
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a],
            read: { probe.read() }, failure: { _ = failures.next() }, changed: { _ in })
        XCTAssertTrue(route.verify()); probe.set(result)
        let eventDone = DispatchSemaphore(value: 0), mediaDone = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { route.routeChanged(); eventDone.signal() }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        XCTAssertFalse(route.permitsPCM()) // Closed before native lookup completes.
        DispatchQueue.global().async {
            XCTAssertEqual(route.awaitPCM(), !stop && !timeout && result != nil)
            mediaDone.signal()
        }
        if stop { XCTAssertFalse(route.close()) }
        else if !timeout { probe.resume.signal() }
        XCTAssertEqual(mediaDone.wait(timeout: .now() + 2), .success)
        if stop || timeout { probe.resume.signal() }
        XCTAssertEqual(eventDone.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(route.permitsPCM(), !stop && !timeout && result != nil)
        if timeout { XCTAssertTrue(route.failureDescription.contains("250 ms")) }
        if timeout || result == nil { XCTAssertTrue(route.close()) }
        else { XCTAssertFalse(route.close()) }
        XCTAssertEqual(failures.count, !stop && (timeout || result == nil) ? 1 : 0)
    }
    func testHeldSameInputEventWakesPCMWithoutLoss() throws { try transition("a") }
    func testHeldRouteEventStopWakesCleanly() throws { try transition("a", stop: true) }
    func testHeldRouteEventFaultWakesAndStaysClosed() throws { try transition(nil) }
    func testHeldRouteEventTimeoutRejectsLateSuccessOnce() throws { try transition("a", timeout: true) }

    func testEventReservationGatesBeforeNativeReadHandoff() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let handoffs = RoutingReadCounter(), nativeReads = RoutingReadCounter()
        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0), done = DispatchSemaphore(value: 0)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a],
            read: { _ = nativeReads.next(); return "a" }, failure: {}, changed: { _ in }, beforeRead: {
                if handoffs.next() == 2 { entered.signal(); _ = resume.wait(timeout: .now() + 2) }
            })
        XCTAssertTrue(route.verify())
        DispatchQueue.global().async { route.routeChanged(); done.signal() }
        XCTAssertEqual(entered.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(nativeReads.count, 1); XCTAssertFalse(route.permitsPCM())
        resume.signal(); XCTAssertEqual(done.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(nativeReads.count, 2); XCTAssertTrue(route.permitsPCM())
    }

    func testSameInputSettlementRefreshesInitiallyDiscardedActiveLabel() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 2, id: "a"); let deliveries = RoutingDeliveries()
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a],
            read: { probe.read() }, failure: {}, changed: { deliveries.push($0) })
        XCTAssertTrue(route.verify()); var label: String?
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { route.routeChanged(); done.signal() }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        if let id = deliveries.pop(), route.permitsPCM() { label = id }
        XCTAssertNil(label)
        probe.resume.signal(); XCTAssertEqual(done.wait(timeout: .now() + 2), .success)
        if let id = deliveries.pop(), route.permitsPCM() { label = id }
        XCTAssertEqual(label, "a")
    }
    func testPostDeadlineSettlementBeforeWaiterReacquiresCannotReopen() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 2, id: "a"), clock = RoutingClock()
        let failures = RoutingReadCounter()
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a],
            read: { probe.read() }, failure: { _ = failures.next() }, changed: { _ in }, now: { clock.read() })
        XCTAssertTrue(route.verify())
        let eventDone = DispatchSemaphore(value: 0), mediaDone = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { route.routeChanged(); eventDone.signal() }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        DispatchQueue.global().async { XCTAssertFalse(route.awaitPCM()); mediaDone.signal() }
        // The waiter has captured its original zero-based deadline and is just
        // about to release the condition lock. Settlement now happens at 300 ms.
        XCTAssertEqual(clock.waitRegistered.wait(timeout: .now() + 2), .success)
        clock.advance(300_000_000); probe.resume.signal()
        XCTAssertEqual(eventDone.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(mediaDone.wait(timeout: .now() + 2), .success)
        XCTAssertFalse(route.permitsPCM()); XCTAssertEqual(failures.count, 1)
        XCTAssertTrue(route.failureDescription.contains("250 ms"))
    }

    func testWaiterDeadlineSurvivesAnotherEventBeforeWakeReacquires() throws {
        let admission = CaptureAdmission(); _ = admission.reservePreparation()
        let probe = HeldRouteRead(hold: 2, id: "a"), clock = RoutingClock(), failures = RoutingReadCounter()
        let woke = DispatchSemaphore(value: 0), resumeWaiter = DispatchSemaphore(value: 0)
        let route = try AudioInputRouting(owner: owner(admission, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a],
            read: { probe.read() }, failure: { _ = failures.next() }, changed: { _ in },
            afterWait: { woke.signal(); _ = resumeWaiter.wait(timeout: .now() + 2) }, now: { clock.read() })
        XCTAssertTrue(route.verify())
        let eventDone = DispatchSemaphore(value: 0), mediaDone = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { route.routeChanged(); eventDone.signal() }
        XCTAssertEqual(probe.entered.wait(timeout: .now() + 2), .success)
        DispatchQueue.global().async { XCTAssertFalse(route.awaitPCM()); mediaDone.signal() }
        XCTAssertEqual(clock.waitRegistered.wait(timeout: .now() + 2), .success)
        probe.resume.signal()
        XCTAssertEqual(eventDone.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(woke.wait(timeout: .now() + 2), .success)
        clock.advance(300_000_000); route.routeChanged()
        // The old waiter is still registered outside the lock; event2 must
        // inherit its deadline and cannot briefly reopen the fast media path.
        XCTAssertFalse(route.permitsPCM()); XCTAssertEqual(failures.count, 1)
        resumeWaiter.signal(); XCTAssertEqual(mediaDone.wait(timeout: .now() + 2), .success)
    }

    private func mediaOwner() throws -> (CaptureAdmission, CaptureAdmission.MediaRun, CaptureAdmission.MediaToken) {
        let admission = CaptureAdmission()
        let prep = try XCTUnwrap(admission.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(admission.attached(prep)); XCTAssertTrue(admission.beginPermissions(prep)); XCTAssertTrue(admission.prepared(prep))
        let start = try XCTUnwrap(admission.reserveStart(prep))
        let run = try XCTUnwrap(admission.reserveMediaRun(start))
        XCTAssertTrue(admission.commitMediaRun(run, start: start, nowNs: 0, broadcasting: false))
        return (admission, run, try XCTUnwrap(admission.mediaToken(run, kind: .raw)))
    }
    func testActualRouteFaultFencesBoundRunBeforeWithheldDiagnostic() throws {
        let (admission, run, token) = try mediaOwner(); var actual: String? = "a"
        var queuedDiagnostic: (() -> Void)?; var delivered = false
        let route = try AudioInputRouting(owner: .init(preparation: run.preparation, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a], read: { actual }, failure: { queuedDiagnostic = { delivered = true } }, changed: { _ in })
        XCTAssertTrue(route.verify())
        XCTAssertTrue(route.observeNativeFailure(run: run) { _ = admission.nativeFault(token) })
        actual = nil; route.routeChanged()
        XCTAssertFalse(delivered); XCTAssertNotNil(queuedDiagnostic)
        XCTAssertTrue(admission.mediaHasFault(run)); XCTAssertEqual(admission.allowsInput(token), .failed)
        queuedDiagnostic?(); XCTAssertTrue(delivered)
        XCTAssertTrue(route.close())
    }
    func testFaultBeforeNativeBindingReplaysOnceButClosedOldRouteCannotBindSuccessor() throws {
        let (admission, run, token) = try mediaOwner(); var actual: String? = "a", receipts = 0
        let route = try AudioInputRouting(owner: .init(preparation: run.preparation, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a], read: { actual }, failure: {}, changed: { _ in })
        XCTAssertTrue(route.verify()); actual = nil; route.routeChanged()
        XCTAssertTrue(route.observeNativeFailure(run: run) { receipts += 1; _ = admission.nativeFault(token) })
        XCTAssertTrue(route.observeNativeFailure(run: run) { receipts += 100 })
        XCTAssertEqual(receipts, 1); XCTAssertTrue(admission.mediaHasFault(run))
        _ = route.close()
        let (other, next, raw) = try mediaOwner()
        XCTAssertFalse(route.observeNativeFailure(run: next) { _ = other.nativeFault(raw) })
        route.routeChanged(); XCTAssertFalse(other.mediaHasFault(next))
    }
    func testIntentionalNativeRouteCloseWinsBeforeLateEventWithoutFaultingRun() throws {
        let (admission, run, token) = try mediaOwner(); var actual: String? = "a", faults = 0
        let route = try AudioInputRouting(owner: .init(preparation: run.preparation, epoch: 1), pin: AudioInputPin(requestedID: nil), available: [a], read: { actual }, failure: { faults += 1 }, changed: { _ in })
        XCTAssertTrue(route.verify()); XCTAssertTrue(route.observeNativeFailure(run: run) { _ = admission.nativeFault(token) })
        XCTAssertFalse(route.close()); actual = nil; route.routeChanged()
        XCTAssertEqual(faults, 0); XCTAssertFalse(admission.mediaHasFault(run))
    }

}

private final class RoutingReadCounter {
    private let lock = NSLock(); private var value = 0
    func next() -> Int { lock.lock(); defer { lock.unlock() }; value += 1; return value }
    var count: Int { lock.lock(); defer { lock.unlock() }; return value }
}

private final class HeldRouteRead {
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    private let lock = NSLock(); private var calls = 0; private var id: String?; private let hold: Int
    init(hold: Int, id: String?) { self.hold = hold; self.id = id }
    func set(_ id: String?) { lock.lock(); self.id = id; lock.unlock() }
    func read() -> String? {
        lock.lock(); calls += 1; let shouldHold = calls == hold; let result = id; lock.unlock()
        if shouldHold { entered.signal(); _ = resume.wait(timeout: .now() + 2) }
        return result
    }
}

private final class RoutingDeliveries {
    private let lock = NSLock(); private var values: [String] = []
    func push(_ value: String) { lock.lock(); values.append(value); lock.unlock() }
    func pop() -> String? { lock.lock(); defer { lock.unlock() }; return values.isEmpty ? nil : values.removeFirst() }
}
private final class RoutingClock {
    let waitRegistered = DispatchSemaphore(value: 0)
    private let lock = NSLock(); private var value: UInt64 = 0; private var calls = 0
    func advance(_ value: UInt64) { lock.lock(); self.value = value; lock.unlock() }
    func read() -> UInt64 {
        lock.lock(); calls += 1; let notify = calls == 3; let snapshot = value; lock.unlock()
        if notify { waitRegistered.signal() }
        return snapshot
    }

}
