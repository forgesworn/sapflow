import Foundation
import Sapflow

/// Closed concrete selection, frozen before CaptureRun/native construction.
/// Legacy remains the product default; SDK stored inputs are always incomplete.
final class CaptureRecording {
    enum BackendSelection: Equatable { case legacy, sdkCheckpointed }
    typealias Completion = CompressedRecorder.Completion
    private enum Backend { case legacy(CompressedRecorder), sdk(SDKCaptureRecorder) }
    private let backend: Backend
    init(legacy: CompressedRecorder) { backend = .legacy(legacy) }
    private init(sdk: SDKCaptureRecorder) { backend = .sdk(sdk) }
    static func open(catalogue: RecordingCatalogue, identity: RecordingCatalogue.Identity,
                     queue: DispatchQueue, selection: BackendSelection = .legacy,
                     onFailure: @escaping (Error) -> Void) throws -> CaptureRecording {
        switch selection {
        case .legacy:
            let receipt = try catalogue.begin(identity: identity)
            do {
                let recorder = try CompressedRecorder(receipt: receipt, queue: queue,
                    hasVideo: CaptureIntent(rawValue: identity.profileID)?.hasVideo ?? true, onFailure: onFailure)
                return CaptureRecording(legacy: recorder)
            } catch {
                let original = error
                receipt.cancel()
                do { try receipt.ownersClosed() }
                catch { receipt.retainUncertainCleanup(); throw error }
                throw original
            }
        case .sdkCheckpointed:
            // Real SDK creation precedes actual AppendWriter creation inside
            // open. All following adapter initialisation is nonthrowing.
            let owner = try RecordingCheckpointOwner.open(catalogue: catalogue, identity: identity,
                mediaEpochNs: 0, maxFragmentBytes: 4 * 1024 * 1024, maxFragmentMs: 500)
            return CaptureRecording(sdk: SDKCaptureRecorder(owner: owner, queue: queue, onFailure: onFailure))
        }
    }
    var audioReceiptMode: NativeAudioEncoder.ReceiptMode {
        switch backend { case .legacy: return .legacy; case .sdk: return .fullGenerationLedger }
    }
    @discardableResult func observeTerminalAdmission(_ closed: @escaping () -> Void) -> Bool {
        switch backend {
        case let .legacy(value): return value.observeTerminalAdmission(closed)
        case let .sdk(value): return value.observeTerminalAdmission(closed)
        }
    }
    @discardableResult func offer(_ unit: CompressedUnit, validated: RustEncodedIngress.ValidatedUnit) -> EncodedWorkBudget.Reservation? {
        switch backend {
        case let .legacy(value): return value.offer(unit)
        case let .sdk(value): return value.offer(sample: validated.sample, bytes: validated.payloadBytes)
        }
    }
    func audioTiming(_ event: AudioRecordingTiming.Event) {
        switch backend { case let .legacy(value): value.audioTiming(event); case let .sdk(value): value.audioTiming(event) }
    }
    func markIncomplete(_ error: Error) {
        switch backend { case let .legacy(value): value.markIncomplete(error); case let .sdk(value): value.markIncomplete(error) }
    }
    func checkStorage(profile: CaptureIntent, _ completion: @escaping (Result<Void, Error>) -> Void) {
        switch backend {
        case let .legacy(value): value.checkStorage(profile: profile, completion)
        case let .sdk(value): value.checkStorage(completion)
        }
    }
    func finish(incomplete: Bool = false, _ completion: @escaping (Completion) -> Void) {
        switch backend {
        case let .legacy(value): value.finish(incomplete: incomplete, completion)
        case let .sdk(value): value.finish(incomplete: incomplete, completion)
        }
    }
}

/// Uses the EXISTING recordingIO queue and admission budget. Owns only real
/// validated MediaSample handles, no encoder/protocol injection/native buffer.
private final class SDKCaptureRecorder {
    private let owner: RecordingCheckpointOwner
    private let queue: DispatchQueue
    private let budget = EncodedWorkBudget()
    private let onFailure: (Error) -> Void
    private let terminalLock = NSLock()
    private var terminalAdmission = false
    private var terminalObserverBound = false
    private var terminalObserver: (() -> Void)?
    private var failure: Error?
    private var fileEOF = false
    private var finishing = false
    private var completed: CaptureRecording.Completion?
    private var joinedCompletion: ((CaptureRecording.Completion) -> Void)?
    private var retainedSelf: SDKCaptureRecorder?
    init(owner: RecordingCheckpointOwner, queue: DispatchQueue, onFailure: @escaping (Error) -> Void) {
        self.owner = owner; self.queue = queue; self.onFailure = onFailure; retainedSelf = self
    }
    func observeTerminalAdmission(_ closed: @escaping () -> Void) -> Bool {
        terminalLock.lock()
        guard !terminalObserverBound else { terminalLock.unlock(); return false }
        terminalObserverBound = true
        let now = terminalAdmission
        if !now { terminalObserver = closed }
        terminalLock.unlock(); if now { closed() }; return true
    }
    private func closeFailedAdmission() {
        budget.close()
        terminalLock.lock()
        guard !terminalAdmission else { terminalLock.unlock(); return }
        terminalAdmission = true
        let observer = terminalObserver; terminalObserver = nil
        terminalLock.unlock(); observer?()
    }
    private func fail(_ error: Error) {
        guard failure == nil else { return }
        failure = error; closeFailedAdmission(); onFailure(error)
    }
    @discardableResult func offer(sample: MediaSample, bytes: Int) -> EncodedWorkBudget.Reservation? {
        let admitted = budget.enqueue(bytes: bytes, on: queue) { reservation in
            defer { self.budget.release(reservation) }
            guard !self.finishing, self.failure == nil else { return }
            do { try self.owner.push(sample: sample) } catch { self.fail(error) }
        }
        if admitted == nil {
            let first = budget.close(enqueueOn: queue, firstOnly: true) {
                self.fail(CaptureFailure(message: "SDK recording backlog exceeded its 8 MiB / 128 sample / 2 s admission bound"))
            }
            if first { closeFailedAdmission() }
        }
        return admitted
    }
    func audioTiming(_ event: AudioRecordingTiming.Event) {
        let admitted = budget.enqueue(bytes: 1, on: queue) { reservation in
            defer { self.budget.release(reservation) }
            guard !self.finishing, self.failure == nil else { return }
            do {
                switch event {
                case let .began(leading): try self.owner.beginAudio(firstLeadingFrames: leading)
                case let .generationEnded(value):
                    try self.owner.generationEnded(NativeAudioGenerationReceipt(generation: value.generation,
                        inputFrames: value.inputFrames, rawFrames: value.rawFrames, zeroFrames: value.zeroFrames,
                        inputRate: value.inputRate, encodedFrames: value.encodedFrames, leadingFrames: value.leadingFrames,
                        trailingFrames: value.trailingFrames, sourceMapping: value.sourceMapping == .floor ? .floor : .nearest))
                case let .endedGenerations(value):
                    try self.owner.completeAudio(NativeAudioFileReceipt(generationCount: value.generationCount,
                        sourceFramesAt48k: value.sourceFramesAt48k, intentionalStandbyFrames: value.intentionalStandbyFrames,
                        encodedFrames: value.encodedFrames, firstLeadingFrames: value.leadingFrames, lastTrailingFrames: value.trailingFrames))
                    self.fileEOF = true
                case .ended: throw CaptureFailure(message: "SDK recording requires actual full generation/file receipts")
                case .failed: throw CaptureFailure(message: "Native AAC generation did not close cleanly")
                }
            } catch { self.fail(error) }
        }
        if admitted == nil {
            let first = budget.close(enqueueOn: queue, firstOnly: true) {
                self.fail(CaptureFailure(message: "SDK recording EOF admission was exhausted"))
            }
            if first { closeFailedAdmission() }
        }
    }
    func markIncomplete(_ error: Error) {
        closeFailedAdmission(); budget.close(enqueueOn: queue) { self.fail(error) }
    }
    func checkStorage(_ completion: @escaping (Result<Void, Error>) -> Void) {
        queue.async {
            guard !self.finishing, self.completed == nil else { completion(.failure(RecordingCheckpointOwner.Failure.closed)); return }
            do { try self.owner.checkStorage(); completion(.success(())) }
            catch { self.fail(error); completion(.failure(error)) }
        }
    }
    func finish(incomplete: Bool, _ completion: @escaping (CaptureRecording.Completion) -> Void) {
        budget.close(enqueueOn: queue) {
            if let result = self.completed { completion(result); return }
            guard !self.finishing else { if self.joinedCompletion == nil { self.joinedCompletion = completion }; return }
            self.finishing = true
            if incomplete && self.failure == nil { self.failure = CaptureFailure(message: "Recording ended with missing encoded media") }
            if !self.fileEOF && self.failure == nil { self.failure = CaptureFailure(message: "Native AAC file EOF receipt is unavailable") }
            let outcome: NativeRecordingCompletion = self.failure == nil && self.fileEOF ? .complete : .incompletePrefix
            do { _ = try self.owner.finishStoredInputs(outcome: outcome) }
            catch {
                if self.failure == nil { self.failure = error }
                // A pre-terminal SDK failure may still checked-close incomplete.
                // A retained/unknown file close rejects here without retrying FD.
                do { _ = try self.owner.closeIncomplete() }
                catch { if self.failure == nil { self.failure = error } }
            }
            let result = CaptureRecording.Completion(fileURL: nil, finalised: false,
                cleanupComplete: self.owner.cleanupComplete, failure: self.failure)
            self.completed = result
            let joined = self.joinedCompletion; self.joinedCompletion = nil
            if result.cleanupComplete { self.retainedSelf = nil }
            completion(result); joined?(result)
        }
    }
}
