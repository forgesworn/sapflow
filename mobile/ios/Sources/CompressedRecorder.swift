import AVFoundation

/// File work has its own queue and never blocks capture or publishing. Already
/// encoded H.264/AAC goes directly to passthrough inputs: no second encode.
final class CompressedRecorder {
    private let queue: DispatchQueue
    private let budget = EncodedWorkBudget()
    private var writer: AVAssetWriter?
    private let receipt: RecordingCatalogue.Writer
    private var video: AVAssetWriterInput?
    private var audio: AVAssetWriterInput?
    private var staged: [(unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation)] = []
    private var finishing = false
    private var failure: Error?
    private let onFailure: (Error) -> Void
    private var hasAcceptedMedia = false
    private var endTime = CMTime.zero
    private var completion: ((Completion) -> Void)?
    private var joinedCompletion: ((Completion) -> Void)?
    private var completed: Completion?
    struct Completion {
        let fileURL: URL?
        let finalised: Bool
        let cleanupComplete: Bool
        let failure: Error?
    }

    /// Called only on the explicit recording IO owner, after its durable WRITING
    /// receipt. The caller closes the receipt if AVAssetWriter creation throws.
    init(receipt: RecordingCatalogue.Writer, queue: DispatchQueue, onFailure: @escaping (Error) -> Void) throws {
        self.receipt = receipt; self.queue = queue; self.onFailure = onFailure
        let created = try AVAssetWriter(outputURL: receipt.fileURL, fileType: .mov)
        created.movieFragmentInterval = CMTime(seconds: 1, preferredTimescale: 600)
        created.shouldOptimizeForNetworkUse = false
        writer = created
    }

    func offer(_ unit: CompressedUnit) {
        guard let reservation = budget.reserve(bytes: unit.byteCount) else {
            if budget.close() { queue.async { self.fail(CaptureFailure(message: "Recording backlog exceeded its 8 MiB / 128 sample / 2 s bound")) } }
            return
        }
        queue.async {
            guard !self.finishing, self.failure == nil, let writer = self.writer else { self.budget.release(reservation); return }
            do {
                if writer.status == .unknown {
                    self.staged.append((unit, reservation))
                    if unit.isVideo && self.video == nil {
                        self.video = try self.makeInput(type: .video, unit: unit)
                    } else if !unit.isVideo && self.audio == nil {
                        self.audio = try self.makeInput(type: .audio, unit: unit)
                    }
                    guard self.video != nil && self.audio != nil else { return }
                    guard writer.startWriting() else { throw writer.error ?? CaptureFailure(message: "Recording could not start") }
                    writer.startSession(atSourceTime: .zero)
                    let initial = self.staged
                    self.staged.removeAll(keepingCapacity: false)
                    defer { for pending in initial { self.budget.release(pending.reservation) } }
                    for pending in initial { try self.append(pending.unit) }
                } else {
                    defer { self.budget.release(reservation) }
                    try self.append(unit)
                }
            } catch { self.fail(error) }
        }
    }

    private func makeInput(type: AVMediaType, unit: CompressedUnit) throws -> AVAssetWriterInput {
        guard let writer else { throw CaptureFailure(message: "Recording writer is closed") }
        let input = AVAssetWriterInput(mediaType: type, outputSettings: nil, sourceFormatHint: CMSampleBufferGetFormatDescription(unit.buffer))
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw CaptureFailure(message: "Recording cannot accept the encoded format") }
        writer.add(input)
        return input
    }
    private func append(_ unit: CompressedUnit) throws {
        let input = unit.isVideo ? video : audio
        guard let writer, let input, writer.status == .writing, input.isReadyForMoreMediaData, input.append(unit.buffer) else {
            throw writer?.error ?? CaptureFailure(message: "Recording storage cannot keep up; recording stopped")
        }
        hasAcceptedMedia = true
        let end = CMTimeAdd(unit.time, unit.duration)
        guard end.isNumeric, end.seconds >= 0, end.seconds.isFinite else {
            throw CaptureFailure(message: "Recording sample extent is invalid")
        }
        if CMTimeCompare(end, endTime) > 0 { endTime = end }
    }
    private func fail(_ error: Error) {
        guard failure == nil else { return }
        failure = error
        budget.close()
        for pending in staged { budget.release(pending.reservation) }
        staged.removeAll()
        onFailure(error)
    }
    func markIncomplete(_ error: Error) { queue.async { self.fail(error) } }
    func checkStorage(profile: CaptureProfile, _ completion: @escaping (Result<Void, Error>) -> Void) {
        queue.async {
            do {
                let available = try self.receipt.fileURL.deletingLastPathComponent().resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]).volumeAvailableCapacityForImportantUsage ?? 0
                guard available >= 0, RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: UInt64(available)) else { throw CaptureFailure(message: "Recording storage is below its required reserve") }
                completion(.success(()))
            } catch { self.fail(error); completion(.failure(error)) }
        }
    }
    /// One recording-only failure may begin finish before capture Stop. Retain
    /// at most those two listeners; all further duplicates coalesce. Completed
    /// results replay without repeating native/file IO.
    func finish(incomplete: Bool = false, _ completion: @escaping (Completion) -> Void) {
        budget.close()
        queue.async {
            if let completed = self.completed { completion(completed); return }
            guard !self.finishing else {
                if self.joinedCompletion == nil { self.joinedCompletion = completion }
                return
            }
            self.finishing = true; self.completion = completion
            if incomplete && self.failure == nil { self.failure = CaptureFailure(message: "Recording ended with missing encoded media") }
            for pending in self.staged { self.budget.release(pending.reservation) }
            self.staged.removeAll()
            guard let writer = self.writer else {
                self.finishNativeWriter(containerCompleted: false); return
            }
            if writer.status == .writing && self.hasAcceptedMedia {
                // All appends use this same queue. Preserve a positive accepted
                // prefix: cancelWriting would block and delete the original.
                self.video?.markAsFinished(); self.audio?.markAsFinished()
                writer.finishWriting { self.queue.async { self.finishNativeWriter(containerCompleted: self.writer?.status == .completed) } }
            } else {
                // Public API permits cancellation only after appends have
                // returned; it can delete this never-started/empty output.
                if writer.status == .unknown || writer.status == .writing { writer.cancelWriting() }
                self.finishNativeWriter(containerCompleted: writer.status == .completed && self.hasAcceptedMedia)
            }
        }
    }
    private func finishNativeWriter(containerCompleted: Bool) {
        let nativeFailure = writer?.error
        video = nil; audio = nil; writer = nil
        // The preceding native callback/cancel queue turn may still have a
        // local strong writer reference. Its return precedes this metadata turn.
        queue.async { self.finishReceipt(containerCompleted: containerCompleted, nativeFailure: nativeFailure) }
    }
    private func finishReceipt(containerCompleted: Bool, nativeFailure: Error?) {
        var error = failure ?? nativeFailure
        let finalised = containerCompleted && error == nil
        var saved: URL?
        do {
            if containerCompleted {
                let milliseconds = endTime.seconds * 1_000
                guard milliseconds.isFinite, milliseconds >= 0, milliseconds < Double(Int64.max) else {
                    throw CaptureFailure(message: "Recording duration is outside its supported bound")
                }
                try receipt.complete(durationMs: Int64(milliseconds), outcome: finalised ? .finalised : .incomplete)
                saved = receipt.fileURL
            } else {
                receipt.cancel()
                try receipt.ownersClosed()
                if error == nil { error = CaptureFailure(message: "No finalised audio/video recording was captured") }
            }
        } catch let persistenceError {
            receipt.retainUncertainCleanup()
            if error == nil { error = persistenceError }
        }
        let result = Completion(fileURL: saved, finalised: finalised && receipt.cleanupComplete,
                                cleanupComplete: receipt.cleanupComplete, failure: error)
        completed = result
        let reply = completion; let joined = joinedCompletion
        completion = nil; joinedCompletion = nil
        reply?(result); joined?(result)
    }
}
