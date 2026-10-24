import XCTest
import CoreMedia
import CoreVideo
@testable import SapflowBroadcaster

final class CameraControlPolicyTests: XCTestCase {
    typealias Policy = CameraControlPolicy<Int>
    let canvas = Policy.Canvas(width: 720, height: 1280)
    private func ready() throws -> (Policy, Policy.Source) {
        var policy = Policy(); policy.prepare(1, initial: .rear)
        let source = try XCTUnwrap(policy.initial(1, canvas: canvas))
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: 1), .accepted(first: true))
        return (policy, source)
    }
    func testSourceRequiresFirstValidFrameForNativeControls() throws {
        var policy = Policy(); policy.prepare(1, initial: .front)
        let source = try XCTUnwrap(policy.initial(1, canvas: canvas))
        XCTAssertFalse(policy.permitsControls(source))
        XCTAssertEqual(source.position, .front)
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: 1), .accepted(first: true))
        XCTAssertTrue(policy.permitsControls(source))
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: 2), .accepted(first: false))
    }
    func testSwitchRetiresOldRawSourceAndRequiresNewFrame() throws {
        var (policy, rear) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        XCTAssertFalse(policy.permitsControls(rear))
        let front = try XCTUnwrap(policy.retire(request, nowNs: 11))
        XCTAssertEqual(front.canvas, rear.canvas)
        XCTAssertEqual(front.owner, rear.owner)
        XCTAssertNotEqual(front.serial, rear.serial)
        XCTAssertEqual(policy.frame(rear, width: 720, height: 1280, nowNs: 12), .stale)
        XCTAssertFalse(policy.permitsControls(front))
        XCTAssertEqual(policy.frame(front, width: 720, height: 1280, nowNs: 12), .accepted(first: true))
        XCTAssertTrue(policy.permitsControls(front))
        XCTAssertNil(policy.pending)
    }
    func testFrontPersistsAcrossCodecOrientationEpoch() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        let source = try XCTUnwrap(policy.retire(request, nowNs: 11))
        _ = policy.frame(source, width: 720, height: 1280, nowNs: 12)
        let rotated = try XCTUnwrap(policy.initial(1, canvas: .init(width: 1280, height: 720)))
        XCTAssertEqual(rotated.position, .front)
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: 13), .stale)
        XCTAssertEqual(policy.frame(rotated, width: 1280, height: 720, nowNs: 14), .accepted(first: true))
        policy.close(); policy.prepare(2, initial: .rear)
        XCTAssertEqual(try XCTUnwrap(policy.initial(2, canvas: canvas)).position, .rear)
    }
    func testPreflightFailureKeepsCurrentSourceAndNoFallback() throws {
        var (policy, source) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        enum Failure: Error { case unsupported }
        var retired = false; var replaced = false
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { throw Failure.unsupported }, retire: {
            retired = true; return policy.retire(request, nowNs: 11)
        }, replace: { (_: Int, _: Policy.Source) in replaced = true }))
        XCTAssertFalse(retired); XCTAssertFalse(replaced)
        XCTAssertTrue(policy.reject(request)); XCTAssertTrue(policy.permitsControls(source))
        XCTAssertEqual(policy.desired, .rear)
    }
    func testStopDuringPreflightPreventsNativeReplacement() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        var replacements = 0
        let changed = try CameraSourceTransaction.perform(preflight: { policy.close(); return 42 }, retire: {
            policy.retire(request, nowNs: 11)
        }, replace: { _, _ in replacements += 1 })
        XCTAssertFalse(changed); XCTAssertEqual(replacements, 0)
        XCTAssertNil(policy.source)
    }
    func testStopAfterRetirementBeforeNativeReplacementWins() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        var replacements = 0
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            let result = policy.retire(request, nowNs: 11); policy.close(); return result
        }, isCurrent: { policy.source == $0 }, replace: { _, _ in replacements += 1 }))
        XCTAssertEqual(replacements, 0)
    }
    func testFailedReplacementCannotResumeRetiredRawSource() throws {
        var (policy, old) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        enum Failure: Error { case configuration }
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            policy.retire(request, nowNs: 11)
        }, replace: { _, _ in throw Failure.configuration }))
        XCTAssertTrue(policy.fail(request)); XCTAssertFalse(policy.reject(request))
        XCTAssertEqual(policy.frame(old, width: 720, height: 1280, nowNs: 12), .stale)
        XCTAssertTrue(policy.failed)
    }
    func testOriginalSixSecondDeadlineCannotBeExtendedByGesture() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        XCTAssertNil(policy.reserveSwitch(1, position: .front, nowNs: 100))
        let source = try XCTUnwrap(policy.retire(request, nowNs: 11))
        XCTAssertEqual(request.deadlineNs, 6_000_000_010)
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: request.deadlineNs), .failed)
        XCTAssertEqual(policy.frame(source, width: 720, height: 1280, nowNs: request.deadlineNs + 1), .stale)
        XCTAssertFalse(policy.permitsControls(source))
    }
    func testRollbackOverflowAndExpiredPreflightRefuseRetirement() throws {
        var (policy, old) = try ready()
        XCTAssertNil(policy.reserveSwitch(1, position: .front, nowNs: UInt64.max))
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        XCTAssertNil(policy.retire(request, nowNs: 9))
        XCTAssertNil(policy.retire(request, nowNs: request.deadlineNs))
        XCTAssertTrue(policy.reject(request)); XCTAssertTrue(policy.permitsControls(old))
    }
    func testWrongCanvasFailsClosedWithoutChangingTimestamps() throws {
        var (policy, source) = try ready()
        XCTAssertEqual(policy.frame(source, width: 1280, height: 720, nowNs: 2), .failed)
        XCTAssertFalse(policy.permitsControls(source))
        // Camera policy never owns or rewrites sample timestamps or media epoch.
        XCTAssertEqual(source.canvas, canvas)
    }
    func testStaleShowCannotControlOrConfirmSuccessor() throws {
        var (policy, a) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        policy.close(); policy.prepare(2, initial: .rear)
        let b = try XCTUnwrap(policy.initial(2, canvas: canvas))
        XCTAssertNil(policy.retire(request, nowNs: 11)); XCTAssertFalse(policy.fail(request))
        XCTAssertEqual(policy.frame(a, width: 720, height: 1280, nowNs: 12), .stale)
        XCTAssertFalse(policy.permitsControls(a))
        XCTAssertEqual(policy.frame(b, width: 720, height: 1280, nowNs: 12), .accepted(first: true))
    }
    func testCapabilitiesRejectNonFiniteOutsideAndUnsupportedControls() {
        var caps = CameraCapabilities(minimumZoom: 1, maximumZoom: 4, minimumBias: -2, maximumBias: 2)
        XCTAssertTrue(caps.accepts(.zoom(4))); XCTAssertTrue(caps.accepts(.bias(-2)))
        for value in [Double.nan, .infinity, -.infinity, 0, 4.01] { XCTAssertFalse(caps.accepts(.zoom(value))) }
        XCTAssertFalse(caps.accepts(.focus(.locked))); XCTAssertFalse(caps.accepts(.whiteBalance(.automatic)))
        XCTAssertFalse(caps.accepts(.exposurePoint(.init(x: 0.5, y: 0.5))))
        caps.minimumZoom = .nan; XCTAssertFalse(caps.accepts(.zoom(1)))
    }
    func testPointControlsAndNativeModeCapabilitiesAreIndependent() {
        let caps = CameraCapabilities(focusAuto: true, focusLock: true, focusPoint: true,
                                      exposureAuto: true, exposureLock: false, exposurePoint: true,
                                      whiteBalanceAuto: true, whiteBalanceLock: false)
        XCTAssertTrue(caps.accepts(.focusPoint(.init(x: 0, y: 1))))
        XCTAssertFalse(caps.accepts(.focusPoint(.init(x: .nan, y: 0))))
        XCTAssertFalse(caps.accepts(.exposurePoint(.init(x: 1.01, y: 0))))
        XCTAssertTrue(caps.accepts(.focus(.locked))); XCTAssertFalse(caps.accepts(.exposure(.locked)))
        XCTAssertTrue(caps.accepts(.whiteBalance(.automatic))); XCTAssertFalse(caps.accepts(.whiteBalance(.locked)))
    }
    func testMailboxBoundsAndCoalescesSourceAndControlFamilies() {
        let box = CameraCommandMailbox<Int>()
        XCTAssertTrue(box.offer(1, .zoom(1)))
        for index in 1...1000 { XCTAssertFalse(box.offer(1, .zoom(Double(index)))) }
        XCTAssertFalse(box.offer(1, .focus(.locked)))
        XCTAssertFalse(box.offer(2, .focus(.automatic)))
        XCTAssertFalse(box.offer(1, .bias(1)))
        let batch = box.take(); XCTAssertEqual(batch.count, 3)
        XCTAssertEqual(batch[0].command, .zoom(1000)); XCTAssertEqual(batch[1].source, 2)
        XCTAssertTrue(box.take().isEmpty); XCTAssertFalse(box.offer(2, .zoom(1)))
        XCTAssertEqual(box.complete(batch[0])?.schedule, true)
        XCTAssertEqual(box.take().first?.source, 2)
    }
    func testFrontMirrorsOnlyPreviewPolicy() {
        XCTAssertTrue(CameraPosition.front.previewMirrored); XCTAssertFalse(CameraPosition.rear.previewMirrored)
        XCTAssertFalse(CameraPosition.front.encodedMirrored); XCTAssertFalse(CameraPosition.rear.encodedMirrored)
    }
    func testSupersededNativeCompletionCannotConfirmNewerRequest() throws {
        let box = CameraCommandMailbox<Int>()
        XCTAssertTrue(box.offer(1, .bias(1))); let a = try XCTUnwrap(box.take().first)
        XCTAssertFalse(box.offer(1, .bias(2))); XCTAssertTrue(box.take().isEmpty)
        let old = try XCTUnwrap(box.complete(a)); XCTAssertFalse(old.confirmed); XCTAssertTrue(old.schedule)
        let b = try XCTUnwrap(box.take().first)
        XCTAssertTrue(box.pending(1)); XCTAssertEqual(box.complete(b)?.confirmed, true); XCTAssertFalse(box.pending(1))
        XCTAssertTrue(box.offer(1, .zoom(2))); let c = try XCTUnwrap(box.take().first)
        box.invalidate(); XCTAssertNil(box.complete(c)); XCTAssertFalse(box.pending(1))
    }
    func testVideoTimelineRejectsResetDuplicateAndInvalidWithoutRebase() {
        var timeline = CameraVideoTimeline()
        XCTAssertTrue(timeline.accept(CMTime(value: 1, timescale: 30)))
        XCTAssertTrue(timeline.accept(CMTime(value: 2, timescale: 30)))
        XCTAssertFalse(timeline.accept(CMTime(value: 2, timescale: 30)))
        XCTAssertFalse(timeline.accept(.zero)); XCTAssertFalse(timeline.accept(.invalid))
        XCTAssertTrue(timeline.accept(CMTime(value: 3, timescale: 30)))
    }
    func testHeldNativeCompletionBoundsThousandMoreSliderGestures() throws {
        let box = CameraCommandMailbox<Int>()
        XCTAssertTrue(box.offer(1, .bias(0))); let first = try XCTUnwrap(box.take().first)
        for index in 1...1000 { XCTAssertFalse(box.offer(1, .bias(Double(index)))) }
        XCTAssertTrue(box.take().isEmpty)
        let finished = try XCTUnwrap(box.complete(first)); XCTAssertFalse(finished.confirmed); XCTAssertTrue(finished.schedule)
        let next = box.take(); XCTAssertEqual(next.count, 1); XCTAssertEqual(next.first?.command, .bias(1000))
    }
    func testProductionSampleBarrierOrdersOldAcceptedPrefixBeforeRetirement() throws {
        var (policy, old) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        let samples = DispatchQueue(label: "test.camera-samples")
        final class Trace { var values: [String] = [] }
        let trace = Trace()
        samples.async { trace.values.append("old accepted prefix") }
        let next = try CameraSourceTransaction.onSampleOwner(queue: samples, retire: {
            trace.values.append("retired"); return policy.retire(request, nowNs: 11)
        }, requestKeyframe: { _ in trace.values.append("IDR requested") })
        XCTAssertNotNil(next)
        XCTAssertEqual(trace.values, ["old accepted prefix", "retired", "IDR requested"])
        XCTAssertEqual(policy.frame(old, width: 720, height: 1280, nowNs: 12), .stale)
    }
    func testRealAdmissionStopCanWinWhileNativePreflightIsBlocked() throws {
        let (gate, preparation, source) = try preparedGate()
        _ = gate.cameraFrame(source, width: 720, height: 1280, nowNs: 1)
        let request = try XCTUnwrap(gate.reserveCameraSwitch(.front, source: source, nowNs: 2))
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let done = expectation(description: "native preflight returned")
        final class ResultBox {
            private let lock = NSLock()
            private var replacements = 0
            func replaced() { lock.lock(); replacements += 1; lock.unlock() }
            var count: Int { lock.lock(); defer { lock.unlock() }; return replacements }
        }
        let result = ResultBox()
        DispatchQueue(label: "test.camera-control").async {
            do {
                let changed = try CameraSourceTransaction.perform(preflight: { entered.signal(); _ = release.wait(timeout: .now() + 2); return 42 },
                    retire: { gate.retireCameraSwitch(request, nowNs: 3) },
                    isCurrent: { gate.permitsCameraFrame($0) }, replace: { _, _ in result.replaced() })
                XCTAssertFalse(changed)
            } catch { XCTFail("Unexpected native preflight error") }
            done.fulfill()
        }
        XCTAssertEqual(entered.wait(timeout: .now() + 2), .success)
        XCTAssertNotNil(gate.stop(preparation: preparation)); release.signal()
        wait(for: [done], timeout: 3)
        XCTAssertEqual(result.count, 0); XCTAssertFalse(gate.permitsCameraFrame(source))
    }
    private func generatedSample(pixelFormat: OSType) throws -> CMSampleBuffer {
        var pixel: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 2, 2, pixelFormat, nil, &pixel), kCVReturnSuccess)
        let image = try XCTUnwrap(pixel)
        var format: CMVideoFormatDescription?
        XCTAssertEqual(CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image, formatDescriptionOut: &format), noErr)
        var sample: CMSampleBuffer?
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 30), presentationTimeStamp: .zero, decodeTimeStamp: .invalid)
        XCTAssertEqual(CMSampleBufferCreateReadyWithImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image,
            formatDescription: try XCTUnwrap(format), sampleTiming: &timing, sampleBufferOut: &sample), noErr)
        return try XCTUnwrap(sample)
    }
    func testFirstFrameRequiresActualReadyNativeYUVBuffer() throws {
        let sample = try generatedSample(pixelFormat: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
        XCTAssertEqual(CameraFrameGeometry.read(sample)?.width, 2)
        XCTAssertEqual(CameraFrameGeometry.read(sample)?.height, 2)
        CMSampleBufferInvalidate(sample)
        XCTAssertNil(CameraFrameGeometry.read(sample))
    }
    func testWrongNativePixelFormatCannotConfirmSource() throws {
        let sample = try generatedSample(pixelFormat: kCVPixelFormatType_32BGRA)
        XCTAssertNil(CameraFrameGeometry.read(sample))
    }
    func testSwitchSealUsesNominalOldDurationAndHalfSecondTrueGap() {
        var timeline = CameraVideoTimeline()
        let old = CMTime(value: 30, timescale: 30), duration = CMTime(value: 1, timescale: 30)
        XCTAssertTrue(timeline.accept(old)); XCTAssertTrue(timeline.beginSourceSwitch(sealedDuration: duration))
        XCTAssertEqual(CMTimeAdd(old, duration), CMTime(value: 31, timescale: 30))
        let next = CMTime(value: 45, timescale: 30)
        XCTAssertTrue(timeline.accept(next)); XCTAssertTrue(timeline.accept(CMTime(value: 46, timescale: 30)))
        XCTAssertFalse(timeline.accept(next)) // true PTS highwater was not reset
    }
    func testSourceGapAtTwoSecondsAndOverlapRefuseWithoutRetime() {
        for delta in [CMTime(value: 2, timescale: 1), CMTime(value: 3, timescale: 1), CMTime(value: 1, timescale: 100)] {
            var timeline = CameraVideoTimeline()
            XCTAssertTrue(timeline.accept(CMTime(value: 1, timescale: 1)))
            XCTAssertTrue(timeline.beginSourceSwitch(sealedDuration: CMTime(value: 1, timescale: 30)))
            XCTAssertFalse(timeline.accept(CMTimeAdd(CMTime(value: 1, timescale: 1), delta)))
        }
    }
    func testWithheldControlTimerCannotKeepAudioOpenPastSwitchDeadline() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        _ = try XCTUnwrap(policy.retire(request, nowNs: 11))
        XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs - 1), .allowed)
        XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs), .failed)
        XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs + 1), .stale)
        XCTAssertEqual(policy.media(2, nowNs: request.deadlineNs + 1), .stale)
        XCTAssertTrue(policy.failed)
    }
    func testExactCameraFrameFaultAlsoClosesAudioScalarAdmission() throws {
        var (policy, source) = try ready()
        XCTAssertEqual(policy.frame(source, width: 1, height: 1, nowNs: 1), .failed)
        XCTAssertEqual(policy.media(1, nowNs: 2), .stale)
        policy.close(); policy.prepare(2, initial: .front)
        XCTAssertEqual(policy.media(1, nowNs: 3), .stale)
        XCTAssertEqual(policy.media(2, nowNs: 3), .allowed)
    }
    func testExpiredAfterIrreversibleSealCannotResumeOldCamera() throws {
        var (policy, _) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        var started = false; var replaced = false
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            started = true // old producer paused and nominal pending frame sealed
            return policy.retire(request, nowNs: request.deadlineNs) // native flush withheld until original deadline
        }, retirementStarted: { started }, replace: { _, _ in replaced = true })) { error in
            XCTAssertTrue(error is CameraSourceTransaction.Failure)
        }
        XCTAssertTrue(started); XCTAssertFalse(replaced)
        // The production catch routes this exact irreversible failure to Stop,
        // rather than treating it as an unsupported preflight and resuming old.
        XCTAssertTrue(policy.fail(request)); XCTAssertTrue(policy.failed)
        XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs), .stale)
    }
    func testScalarRetirementAndAudioDeadlinePrecedeHeldNativeSeal() throws {
        var (policy, old) = try ready()
        let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
        let samples = DispatchQueue(label: "test.camera-held-seal")
        var started = false; var replaced = false
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            try CameraSourceTransaction.onSampleOwner(queue: samples, retire: {
                let source = policy.retire(request, nowNs: 11); started = source != nil; return source
            }, seal: { _ in
                XCTAssertTrue(policy.retired)
                XCTAssertEqual(policy.frame(old, width: 720, height: 1280, nowNs: 12), .stale)
                // Simulates a native flush that withheld control until the
                // original deadline. Audio's production scalar gate is closed
                // before that flush returns, even without the timer callback.
                XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs), .failed)
            }, requestKeyframe: { _ in })
        }, isCurrent: { policy.source == $0 && !policy.failed && policy.permits(request, nowNs: request.deadlineNs) },
           retirementStarted: { started }, replace: { _, _ in replaced = true }))
        XCTAssertFalse(replaced); XCTAssertTrue(policy.failed)
        XCTAssertEqual(policy.media(1, nowNs: request.deadlineNs + 1), .stale)
    }
    func testPostSealFinalDeadlinePredicateWithTimelySuccessControl() throws {
        for expires in [false, true] {
            var (policy, _) = try ready()
            let request = try XCTUnwrap(policy.reserveSwitch(1, position: .front, nowNs: 10))
            let samples = DispatchQueue(label: "test.camera-seal-final-check")
            var started = false; var replaced = false; var now: UInt64 = 11
            let perform = {
                try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
                    try CameraSourceTransaction.onSampleOwner(queue: samples, retire: {
                        let source = policy.retire(request, nowNs: now); started = source != nil; return source
                    }, seal: { _ in now = expires ? request.deadlineNs : request.deadlineNs - 1 }, requestKeyframe: { _ in })
                }, isCurrent: { policy.source == $0 && !policy.failed && policy.permits(request, nowNs: now) },
                   retirementStarted: { started }, replace: { _, _ in replaced = true })
            }
            if expires { XCTAssertThrowsError(try perform()); XCTAssertFalse(replaced) }
            else { XCTAssertTrue(try perform()); XCTAssertTrue(replaced) }
            XCTAssertTrue(started); XCTAssertFalse(policy.failed) // no audio/media call pre-failed the expiry case
            XCTAssertFalse(policy.confirmed) // graph mutation alone never confirms the source
        }
    }
    func testRejectedNaNControlReleasesOperationIdentityAndCapacity() throws {
        let invalid: [CameraCommand] = [.zoom(.nan), .bias(.nan), .focusPoint(.init(x: .nan, y: 0)), .exposurePoint(.init(x: 0, y: .nan))]
        let caps = CameraCapabilities(focusAuto: true, focusPoint: true, exposureAuto: true, exposurePoint: true)
        for command in invalid {
            let box = CameraCommandMailbox<Int>()
            XCTAssertTrue(box.offer(1, command)); let request = try XCTUnwrap(box.take().first)
            XCTAssertFalse(caps.accepts(request.command))
            XCTAssertEqual(box.complete(request)?.confirmed, true)
            XCTAssertFalse(box.pending(1))
            XCTAssertTrue(box.offer(1, command)) // rejected values cannot wedge a native slot
        }
    }
    private func runningGate() throws -> (CaptureAdmission, CaptureAdmission.Preparation, CaptureAdmission.CameraSource) {
        let (gate, preparation, source) = try preparedGate()
        _ = gate.cameraFrame(source, width: 720, height: 1280, nowNs: 1)
        let start = try XCTUnwrap(gate.reserveStart(preparation))
        XCTAssertTrue(gate.started(start, nowNs: 1, broadcasting: true))
        return (gate, preparation, source)
    }
    func testNormalStopDuringSuccessfulSealCannotTaintRecorderOnLateCancellation() throws {
        let (gate, preparation, old) = try runningGate()
        let request = try XCTUnwrap(gate.reserveCameraSwitch(.front, source: old, nowNs: 2))
        let samples = DispatchQueue(label: "test.camera-stop-during-seal")
        var retired = false; var ordinaryStop: CaptureAdmission.Stop?
        var marks = 0; var failureStops = 0
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            try CameraSourceTransaction.onSampleOwner(queue: samples, retire: {
                let source = gate.retireCameraSwitch(request, nowNs: 3); retired = source != nil; return source
            }, seal: { _ in
                ordinaryStop = gate.stop(preparation: preparation) // main Stop while otherwise successful native seal is held
            }, requestKeyframe: { _ in })
        }, isCurrent: { gate.permitsCameraFrame($0) }, retirementStarted: { retired }, replace: { _, _ in XCTFail("Stopped source replaced") }))
        XCTAssertFalse(CameraSourceTransaction.claimFailure(claim: { gate.stop(preparation: preparation) },
            markIncomplete: { marks += 1 }, stop: { _ in failureStops += 1 }))
        XCTAssertEqual(marks, 0); XCTAssertEqual(failureStops, 0)
        XCTAssertTrue(gate.permitsStop(try XCTUnwrap(ordinaryStop)))
        XCTAssertFalse(gate.canChooseCaptureProfile) // cancellation did not manufacture actual cleanup
    }
    func testControlFailureClaimBeforeRecorderMutationWinsAgainstLaterStop() throws {
        let (gate, preparation, source) = try runningGate()
        var marks = 0; var failureStop: CaptureAdmission.Stop?
        XCTAssertTrue(CameraSourceTransaction.claimFailure(claim: { gate.stop(preparation: preparation) }, markIncomplete: {
            // Production mark may wait for samples. An ordinary Stop arriving
            // during that wait cannot steal its exact already-claimed cleanup.
            XCTAssertNil(gate.stop(preparation: preparation)); marks += 1
            XCTAssertFalse(gate.permitsCameraFrame(source))
        }, stop: { failureStop = $0 }))
        XCTAssertEqual(marks, 1)
        XCTAssertTrue(gate.permitsStop(try XCTUnwrap(failureStop)))
        XCTAssertNil(gate.stop(preparation: preparation))
    }
    func testPreviouslyLatchedSampleFaultRemainsIncompleteWhenNormalStopWinsCleanup() throws {
        let (gate, preparation, source) = try runningGate()
        var marks = 0
        let fault = gate.cameraFrame(source, width: 1, height: 1, nowNs: 2)
        XCTAssertEqual(fault, .failed)
        if fault == .failed { marks += 1 } // exact sample-owned recorder fault already latched
        let ordinary = try XCTUnwrap(gate.stop(preparation: preparation))
        XCTAssertFalse(CameraSourceTransaction.claimFailure(claim: { gate.stop(preparation: preparation) },
            markIncomplete: { marks += 1 }, stop: { _ in XCTFail("Ordinary cleanup was replaced") }))
        XCTAssertEqual(marks, 1); XCTAssertTrue(gate.permitsStop(ordinary))
    }
    func testActualSealErrorAfterNormalStopStillLatchesExactMediaFault() throws {
        let (gate, preparation, old) = try runningGate()
        let request = try XCTUnwrap(gate.reserveCameraSwitch(.front, source: old, nowNs: 2))
        let samples = DispatchQueue(label: "test.camera-seal-error-stop")
        enum SealError: Error { case generatedNativeFailure }
        var ordinaryStop: CaptureAdmission.Stop?; var marks = 0; var retired = false
        XCTAssertThrowsError(try CameraSourceTransaction.perform(preflight: { 42 }, retire: {
            try CameraSourceTransaction.onSampleOwner(queue: samples, retire: {
                let source = gate.retireCameraSwitch(request, nowNs: 3); retired = source != nil; return source
            }, seal: { _ in
                ordinaryStop = gate.stop(preparation: preparation)
                throw SealError.generatedNativeFailure
            }, sealFailed: { source, error in
                XCTAssertEqual(source.owner, preparation); XCTAssertTrue(error is SealError)
                marks += 1 // production callback runs on samples, before the cold control catch
            }, requestKeyframe: { _ in XCTFail("Failed seal requested new frame") })
        }, isCurrent: { gate.permitsCameraFrame($0) }, retirementStarted: { retired }, replace: { _, _ in XCTFail("Failed seal replaced source") }))
        XCTAssertEqual(marks, 1)
        XCTAssertFalse(CameraSourceTransaction.claimFailure(claim: { gate.stop(preparation: preparation) },
            markIncomplete: { marks += 1 }, stop: { _ in XCTFail("Ordinary cleanup was replaced") }))
        XCTAssertEqual(marks, 1); XCTAssertTrue(gate.permitsStop(try XCTUnwrap(ordinaryStop)))
    }
    private func preparedGate(camera: CameraPosition = .rear) throws -> (CaptureAdmission, CaptureAdmission.Preparation, CaptureAdmission.CameraSource) {
        let gate = CaptureAdmission(); XCTAssertTrue(gate.selectCamera(camera))
        let ticket = try XCTUnwrap(gate.reservePreparation())
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        let source = try XCTUnwrap(gate.initialCameraSource(canvas: .init(width: 720, height: 1280)))
        XCTAssertTrue(gate.prepared(ticket))
        return (gate, ticket, source)
    }
    func testAdmissionFreezesCameraBeforePermissionAndStartRequiresFirstFrame() throws {
        let (gate, ticket, source) = try preparedGate(camera: .front)
        XCTAssertEqual(ticket.initialCamera, .front); XCTAssertFalse(gate.selectCamera(.rear))
        XCTAssertNil(gate.reserveStart(ticket)); XCTAssertNil(gate.cameraControlSource)
        XCTAssertEqual(gate.cameraFrame(source, width: 720, height: 1280, nowNs: 1), .accepted(first: true))
        XCTAssertEqual(gate.cameraControlSource, source); XCTAssertNotNil(gate.reserveStart(ticket))
    }
    func testAdmissionStopSynchronouslyFencesSourceCommandAndPendingSwitch() throws {
        let (gate, ticket, source) = try preparedGate()
        _ = gate.cameraFrame(source, width: 720, height: 1280, nowNs: 1)
        let request = try XCTUnwrap(gate.reserveCameraSwitch(.front, source: source, nowNs: 2))
        _ = gate.stop(preparation: ticket)
        XCTAssertFalse(gate.permitsCameraControls(source)); XCTAssertFalse(gate.permitsCameraFrame(source))
        XCTAssertNil(gate.retireCameraSwitch(request, nowNs: 3))
        XCTAssertEqual(gate.cameraFrame(source, width: 720, height: 1280, nowNs: 3), .stale)
    }
    func testStaleSourceCannotReserveSwitchOnNewEpochOfSamePreparation() throws {
        let (gate, preparation, old) = try preparedGate()
        _ = gate.cameraFrame(old, width: 720, height: 1280, nowNs: 1)
        let fresh = try XCTUnwrap(gate.initialCameraSource(canvas: .init(width: 1280, height: 720)))
        _ = gate.cameraFrame(fresh, width: 1280, height: 720, nowNs: 2)
        XCTAssertEqual(old.owner, preparation); XCTAssertEqual(fresh.owner, preparation)
        XCTAssertNil(gate.reserveCameraSwitch(.front, source: old, nowNs: 3))
        XCTAssertNotNil(gate.reserveCameraSwitch(.front, source: fresh, nowNs: 3))
    }
    func testDeferredAutoPoseResumesLatestAdmittedObservationWithoutNewSerial() throws {
        let (gate, _, source) = try preparedGate()
        _ = gate.cameraFrame(source, width: 720, height: 1280, nowNs: 1)
        let old = try XCTUnwrap(gate.observeOrientation(.landscapeLeft))
        // The owner processed old and deferred it. A newer sensor pose is
        // admitted synchronously but its queued handler has not executed yet.
        let newer = try XCTUnwrap(gate.observeOrientation(.landscapeRight))
        let resume = try XCTUnwrap(gate.latestOrientationObservation)
        XCTAssertEqual(resume.orientation, .landscapeRight)
        XCTAssertFalse(gate.permitsOrientation(old)); XCTAssertTrue(gate.permitsOrientation(resume))
        XCTAssertTrue(gate.permitsOrientation(newer)) // no fresh serial invalidated the real newest event
        let newest = try XCTUnwrap(gate.observeOrientation(.portrait))
        XCTAssertFalse(gate.permitsOrientation(resume)); XCTAssertTrue(gate.permitsOrientation(newest))
        XCTAssertEqual(gate.latestOrientationObservation?.orientation, .portrait)
    }
    func testAudioOnlyHasNoCameraSourceOrLiveControls() throws {
        let gate = CaptureAdmission(); XCTAssertTrue(gate.selectCamera(.front))
        let ticket = try XCTUnwrap(gate.reservePreparation(profile: .audioOnly48kMono))
        XCTAssertTrue(gate.attached(ticket)); XCTAssertTrue(gate.beginPermissions(ticket))
        XCTAssertNil(gate.initialCameraSource(canvas: .init(width: 720, height: 1280)))
        XCTAssertTrue(gate.prepared(ticket)); XCTAssertNil(gate.cameraControlSource)
        XCTAssertNotNil(gate.reserveStart(ticket))
    }
}
