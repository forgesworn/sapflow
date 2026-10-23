import Foundation
import Sapflow

/// One bounded owner validates each native compressed unit once, then forwards
/// legacy native buffers or the SAME Rust sample to recording, and the same
/// validated sample owner to transport. No second sample construction/copy.
/// Terminal publishing closes only that branch; validation can keep recording.
final class IngressWorker {
    private let queue: DispatchQueue
    private let budget = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
    private let state = NSLock()
    let ingress: RustEncodedIngress
    private let run: CaptureAdmission.MediaRun
    private let admission: CaptureAdmission
    private let recorder: CaptureRecording
    private let publishingToken: CaptureAdmission.SinkToken
    private let recordingToken: CaptureAdmission.SinkToken
    private let fallbackToken: CaptureAdmission.MediaToken
    private let boundary: (CaptureAdmission.MediaBoundary) -> Void
    private let terminal: (CaptureAdmission.SinkToken) -> Void
    private let mediaFailure: (Error) -> Void
    private let failure: (Error) -> Void
    private let statistics: (UInt64, String) -> Void
    private var publishingOpen: Bool
    /// This real SDK queue is the independently bounded publishing branch.
    /// Only the ingress owner calls it; push/pop retains the payload Arc and
    /// does not repeat codec validation or copy compressed/native media bytes.
    private let publishingQueue: NativeSampleFanout?
    private var nativePublishingCloseAttempted = false // Encoded owner only.
    private var nativePublishingCloseFailure: Error?
    private var recordingOpen: Bool
    private var failed = false
    private var lastStatistics = 0.0 // Encoded queue only.
    private struct FinishJoin {
        var recording: ((CompressedRecorder.Completion) -> Void)?
        var publishing: ((Result<Void, Error>) -> Void)?
    }
    private var finishStarted = false // All finish state belongs to queue.
    private var recordingResult: CompressedRecorder.Completion?
    private var publishingResult: Result<Void, Error>?
    private var finishJoins: [FinishJoin] = []

    /// Existing tools/callers retain the exact legacy writer and queue path.
    convenience init(run: CaptureAdmission.MediaRun, ingress: RustEncodedIngress, recorder: CompressedRecorder,
         admission: CaptureAdmission, boundary: @escaping (CaptureAdmission.MediaBoundary) -> Void,
         terminal: @escaping (CaptureAdmission.SinkToken) -> Void, mediaFailure: @escaping (Error) -> Void,
         failure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void,
         publishingQueueBytes: UInt64 = 4 * 1024 * 1024,
         queue: DispatchQueue = DispatchQueue(label: "sapflow.encoded-ingress", qos: .userInitiated)) throws {
        try self.init(run: run, ingress: ingress, recorder: CaptureRecording(legacy: recorder), admission: admission,
            boundary: boundary, terminal: terminal, mediaFailure: mediaFailure, failure: failure,
            statistics: statistics, publishingQueueBytes: publishingQueueBytes, queue: queue)
    }
    init(run: CaptureAdmission.MediaRun, ingress: RustEncodedIngress, recorder: CaptureRecording,
         admission: CaptureAdmission, boundary: @escaping (CaptureAdmission.MediaBoundary) -> Void,
         terminal: @escaping (CaptureAdmission.SinkToken) -> Void, mediaFailure: @escaping (Error) -> Void,
         failure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void,
         publishingQueueBytes: UInt64 = 4 * 1024 * 1024,
         queue: DispatchQueue = DispatchQueue(label: "sapflow.encoded-ingress", qos: .userInitiated)) throws {
        guard let publishing = admission.sinkToken(run: run, kind: .publishing),
              let recording = admission.sinkToken(run: run, kind: .recording),
              let raw = admission.mediaToken(run, kind: .raw) else {
            throw CaptureFailure(message: "Encoded output ownership was retired before construction")
        }
        self.queue = queue
        self.run = run; self.ingress = ingress; self.recorder = recorder; self.admission = admission
        publishingToken = publishing; recordingToken = recording; fallbackToken = raw
        self.boundary = boundary; self.terminal = terminal; self.mediaFailure = mediaFailure
        self.failure = failure; self.statistics = statistics
        let required = admission.requiredSinkMask(run: run)
        publishingOpen = required.contains(.publishing) && ingress.destination != nil
        recordingOpen = required.contains(.recording)
        if publishingOpen {
            let branch = try NativeSampleFanout(publishingBytes: publishingQueueBytes,
                recordingBytes: 1, maxMediaMs: 1000, hasVideo: run.preparation.profile.hasVideo)
            try branch.closeRecordingAdmission()
            publishingQueue = branch
        } else { publishingQueue = nil }
        guard recorder.observeTerminalAdmission({ [weak self] in self?.closeRecordingAdmission() }) else {
            throw CaptureFailure(message: "Recording output already has an encoded owner")
        }
        if required.contains(.publishing) && ingress.destination == nil {
            _ = closePublishingAdmission()
            terminal(publishingToken)
        }
    }

    private func permitsOutput(_ token: CaptureAdmission.MediaToken) -> Bool {
        guard token.run == run else { return false }
        switch admission.outputAdmission(token) {
        case .current, .acceptedPrefix: return true
        case .superseded, .failed: return false
        }
    }

    @discardableResult func offer(_ unit: CompressedUnit, token: CaptureAdmission.MediaToken,
                                 audioGeneration: UInt64? = nil) -> Bool {
        guard permitsOutput(token) else { return false }
        let validKind = token.kind == .raw || (unit.isVideo ? token.kind == .slate : token.kind == .zero)
        guard validKind, unit.isVideo || audioGeneration.map({ $0 > 0 }) == true else {
            failMedia(token, CaptureFailure(message: "Encoded output lost its original native provenance")); return false
        }
        let reservation = budget.enqueue(bytes: unit.byteCount, on: queue) { ticket in
            defer { self.budget.release(ticket) }
            guard self.permitsOutput(token) else { return }
            self.state.lock(); let failed = self.failed; self.state.unlock()
            guard !failed else { return }
            let validated: RustEncodedIngress.ValidatedUnit
            do { validated = try self.ingress.validate(unit) }
            catch { self.failMedia(token, error); return }

            var accepted: CaptureAdmission.SinkMask = []
            self.state.lock(); let records = self.recordingOpen; self.state.unlock()
            if records, self.recorder.offer(unit, validated: validated) != nil { accepted.insert(.recording) }

            // A void destination offer may drop during recovery. Confirmation
            // instead requires this exact validated sample's real local SDK
            // publishing admission, followed by its actual dequeue. At most
            // one sample is retained here: every successful push is popped
            // before another push, inside the existing ingress reservation.
            self.state.lock(); let publishes = self.publishingOpen; self.state.unlock()
            if publishes, let branch = self.publishingQueue {
                do {
                    let outcome = try branch.push(sample: validated.sample, nowNs: DispatchTime.now().uptimeNanoseconds)
                    if outcome.publishingQueued {
                        guard let sample = try branch.popPublishing(nowNs: DispatchTime.now().uptimeNanoseconds) else {
                            throw CaptureFailure(message: "Accepted publishing media expired before its bounded dequeue")
                        }
                        // End can win while Rust executes. Its irreversible
                        // Swift fence prevents a new receipt after that close.
                        self.state.lock(); let current = self.publishingOpen; self.state.unlock()
                        if current {
                            do {
                                try self.ingress.publish(validated, sample: sample)
                                accepted.insert(.publishing)
                            } catch { self.failPublishing(error) }
                        }
                    }
                } catch let error as BindingError {
                    // The exclusively owned SDK rejected continuity, clock or
                    // ownership, rather than a recoverable network attempt.
                    self.failMedia(token, error)
                } catch { self.failPublishing(error) }
            }
            if token.kind != .raw, !accepted.isEmpty {
                self.boundary(CaptureAdmission.MediaBoundary(token: token, pts: validated.pts,
                    duration: validated.duration, codedIdr: validated.codedIdr,
                    audioGeneration: audioGeneration, acceptedSinks: accepted))
            }
            let now = ProcessInfo.processInfo.systemUptime
            if now - self.lastStatistics >= 0.5 {
                self.lastStatistics = now
                self.statistics(self.ingress.accepted, self.ingress.status)
            }
        }
        guard reservation != nil else {
            // A late offer after ordinary finish is cancellation, not a new
            // media fault. Exhaustion closes the same real queue irreversibly.
            let first = budget.close(enqueueOn: queue, firstOnly: true) {}
            if first { failMedia(token, CaptureFailure(message: "Encoded SDK ingress exceeded its bounded backlog")) }
            return false
        }
        return true
    }

    /// Codec timing events share the unit FIFO, including generation EOF. The
    /// recorder cannot observe a completed generation before its original AUs.
    func audioTiming(_ event: AudioRecordingTiming.Event) {
        let reservation = budget.enqueue(bytes: 1, on: queue) { ticket in
            defer { self.budget.release(ticket) }
            self.state.lock(); let records = self.recordingOpen; self.state.unlock()
            if records { self.recorder.audioTiming(event) }
        }
        if reservation == nil {
            let first = budget.close(enqueueOn: queue, firstOnly: true) {}
            if first { failMedia(fallbackToken, CaptureFailure(message: "Encoded timing queue exceeded its bounded backlog")) }
        }
    }

    @discardableResult func closePublishingAdmission() -> CaptureAdmission.SinkToken? {
        state.lock()
        guard publishingOpen else { state.unlock(); return nil }
        publishingOpen = false
        state.unlock()
        // The scalar fence is immediate; the separate native-close receipt
        // follows accepted work on its actual SDK owner. No FFI under lock.
        queue.async {
            if self.closeNativePublishing() { self.terminal(self.publishingToken) }
        }
        return publishingToken
    }
    @discardableResult private func closeNativePublishing() -> Bool {
        if nativePublishingCloseAttempted { return nativePublishingCloseFailure == nil }
        nativePublishingCloseAttempted = true
        do { try publishingQueue?.closePublishingAdmission(); return true }
        catch {
            nativePublishingCloseFailure = error
            // An ambiguous SDK close is never a successful native receipt or
            // permission to create another queue. Preserve the failed show.
            failMedia(fallbackToken, error)
            return false
        }
    }
    private func closeRecordingAdmission() {
        state.lock()
        guard recordingOpen else { state.unlock(); return }
        recordingOpen = false
        state.unlock()
        terminal(recordingToken)
    }

    @discardableResult func startPublishing(activate: () -> Void = {}) -> Bool {
        defer { activate() }
        state.lock(); let open = publishingOpen; state.unlock()
        guard open else { return false }
        do { try ingress.destination?.start(); return true }
        catch { failPublishing(error); return false }
    }
    private func failPublishing(_ error: Error) {
        guard closePublishingAdmission() != nil else { return }
        ingress.destination?.endDestination()
        failure(error)
    }
    private func failMedia(_ token: CaptureAdmission.MediaToken, _ error: Error) {
        guard token.run == run else { return }
        state.lock()
        guard !failed else { state.unlock(); return }
        failed = true
        state.unlock()
        budget.close()
        _ = admission.nativeFault(token)
        recorder.markIncomplete(error)
        _ = closePublishingAdmission()
        ingress.destination?.endDestination()
        mediaFailure(error)
    }
    func finish(_ completion: @escaping (Result<Void, Error>) -> Void) {
        finish(recording: { _ in }, publishing: completion)
    }
    /// Join validation/forwarding before closing the recorder's own queue.
    /// Its finish starts independently of transport drain or remote handshake.
    func finish(recording: @escaping (CompressedRecorder.Completion) -> Void,
                publishing: @escaping (Result<Void, Error>) -> Void) {
        budget.close(enqueueOn: queue) {
            if let result = self.recordingResult { recording(result) }
            if let result = self.publishingResult { publishing(result) }
            if self.recordingResult == nil || self.publishingResult == nil {
                guard self.finishJoins.count < 8 else {
                    let error = CaptureFailure(message: "Encoded cleanup join capacity is exhausted")
                    if self.recordingResult == nil { recording(CompressedRecorder.Completion(fileURL: nil,
                        finalised: false, cleanupComplete: false, failure: error)) }
                    if self.publishingResult == nil { publishing(.failure(error)) }
                    return
                }
                self.finishJoins.append(FinishJoin(recording: self.recordingResult == nil ? recording : nil,
                    publishing: self.publishingResult == nil ? publishing : nil))
            }
            guard !self.finishStarted else { return }
            self.finishStarted = true
            _ = self.closeNativePublishing()
            self.recorder.finish { result in self.queue.async {
                guard self.recordingResult == nil else { return }
                self.recordingResult = result
                let callbacks = self.finishJoins.compactMap(\.recording)
                for i in self.finishJoins.indices { self.finishJoins[i].recording = nil }
                self.finishJoins.removeAll { $0.recording == nil && $0.publishing == nil }
                for callback in callbacks { callback(result) }
            } }
            let published: (Result<Void, Error>) -> Void = { result in self.queue.async {
                guard self.publishingResult == nil else { return }
                let result = self.nativePublishingCloseFailure.map { Result<Void, Error>.failure($0) } ?? result
                self.publishingResult = result
                let callbacks = self.finishJoins.compactMap(\.publishing)
                for i in self.finishJoins.indices { self.finishJoins[i].publishing = nil }
                self.finishJoins.removeAll { $0.recording == nil && $0.publishing == nil }
                for callback in callbacks { callback(result) }
            } }
            if let destination = self.ingress.destination { destination.finishEpoch(completion: published) }
            else { published(.success(())) }
        }
    }
}
