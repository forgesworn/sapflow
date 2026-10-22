import AVFoundation

/// File work has its own queue and never blocks capture or publishing. Already
/// encoded H.264/AAC goes directly to passthrough inputs: no second encode.
final class CompressedRecorder {
    private let queue: DispatchQueue
    private let budget = EncodedWorkBudget()
    private var writer: AVAssetWriter?
    private let receipt: RecordingCatalogue.Writer
    private let hasVideo: Bool
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
    private var audioLeading: UInt32?
    private var audioExtent: AudioRecordingTiming.Extent?
    private var audioFirstTime: CMTime?
    private var audioSessionStart: CMTime?
    private var audioUnits: UInt64 = 0
    private var audioHeadWritten = false
    struct Completion {
        let fileURL: URL?
        let finalised: Bool
        let cleanupComplete: Bool
        let failure: Error?
    }

    /// Called only on the explicit recording IO owner, after its durable WRITING
    /// receipt. The caller closes the receipt if AVAssetWriter creation throws.
    init(receipt: RecordingCatalogue.Writer, queue: DispatchQueue, hasVideo: Bool = true, onFailure: @escaping (Error) -> Void) throws {
        self.receipt = receipt; self.queue = queue; self.hasVideo = hasVideo; self.onFailure = onFailure
        guard hasVideo ? receipt.identity.container != .m4a : receipt.identity.container == .m4a else {
            throw CaptureFailure(message: "Recording container does not match its immutable tracks")
        }
        let created = try AVAssetWriter(outputURL: receipt.fileURL, fileType: hasVideo ? .mov : .m4a)
        if !hasVideo { created.movieTimeScale = 48_000 }
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
            guard !unit.isVideo || self.hasVideo else {
                self.budget.release(reservation)
                self.fail(CaptureFailure(message: "Video does not match this audio-only recording")); return
            }
            if !self.hasVideo {
                do { try self.offerAudio(unit, reservation: reservation) } catch { self.fail(error) }
                return
            }
            do {
                if writer.status == .unknown {
                    self.staged.append((unit, reservation))
                    if unit.isVideo && self.video == nil {
                        self.video = try self.makeInput(type: .video, unit: unit)
                    } else if !unit.isVideo && self.audio == nil {
                        self.audio = try self.makeInput(type: .audio, unit: unit)
                    }
                    guard (!self.hasVideo || self.video != nil) && self.audio != nil else { return }
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

    /// Cold metadata events are enqueued before first AAC and after codec EOF.
    /// No publishing buffer or timestamp is modified by recording trims.
    func audioTiming(_ event: AudioRecordingTiming.Event) {
        guard !hasVideo else { return }
        queue.async {
            guard !self.finishing else { return }
            switch event {
            case let .began(leading):
                do {
                    _ = try AudioRecordingTiming.headUnits(leading)
                    guard self.audioLeading == nil else { throw AudioRecordingTiming.Failure.inconsistent }
                    self.audioLeading = leading
                } catch { self.fail(error) }
            case let .ended(extent):
                guard self.audioExtent == nil else { self.fail(AudioRecordingTiming.Failure.inconsistent); return }
                self.audioExtent = extent
            }
        }
    }

    private func offerAudio(_ unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation) throws {
        var retained = false
        defer { if !retained { budget.release(reservation) } }
        guard let leading = audioLeading, !unit.isVideo,
              case let .aac(asc) = unit.configuration, asc == Data([0x11, 0x88]),
              CMSampleBufferGetNumSamples(unit.buffer) == 1,
              unit.duration == CMTime(value: 1024, timescale: 48_000), unit.time.isNumeric else {
            throw CaptureFailure(message: "Audio recording requires exact native AAC timing")
        }
        if let first = audioFirstTime {
            guard audioUnits < UInt64(Int64.max / 1024),
                  CMTimeCompare(unit.time, CMTimeAdd(first, CMTime(value: Int64(audioUnits * 1024), timescale: 48_000))) == 0 else {
                throw CaptureFailure(message: "Audio recording cadence changed")
            }
        } else { audioFirstTime = unit.time }
        if audio == nil { audio = try makeInput(type: .audio, unit: unit) }
        audioUnits += 1
        staged.append((unit, reservation)); retained = true
        let count = try AudioRecordingTiming.headUnits(leading)
        if !audioHeadWritten && staged.count > count { try appendAudioHead(count: count, trailing: 0) }
        if audioHeadWritten && staged.count > 1 { try appendAudioPending(trailing: 0) }
    }

    private func audioCopy(_ unit: CompressedUnit, trailing: UInt32) throws -> CMSampleBuffer {
        if trailing == 0 { return unit.buffer }
        var copied: CMSampleBuffer?
        var timing = CMSampleTimingInfo(duration: unit.duration, presentationTimeStamp: unit.time, decodeTimeStamp: unit.time)
        guard CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: unit.buffer,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &copied) == noErr, let copied else {
            throw CaptureFailure(message: "Audio recording tail metadata failed")
        }
        CMSetAttachment(copied, key: kCMSampleBufferAttachmentKey_TrimDurationAtEnd,
            value: CMTimeCopyAsDictionary(CMTime(value: Int64(trailing), timescale: 48_000), allocator: kCFAllocatorDefault)!,
            attachmentMode: kCMAttachmentMode_ShouldPropagate)
        return copied
    }

    private func appendAudioHead(count: Int, trailing: UInt32) throws {
        guard !audioHeadWritten, (1...8).contains(count), staged.count >= count,
              let leading = audioLeading, let first = audioFirstTime, let writer else { throw AudioRecordingTiming.Failure.inconsistent }
        guard Int64(count * 1024) > Int64(leading) + Int64(trailing) else { throw AudioRecordingTiming.Failure.inconsistent }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateEmpty(allocator: kCFAllocatorDefault, capacity: UInt32(count), flags: 0, blockBufferOut: &block) == noErr,
              let block else { throw CaptureFailure(message: "Audio recording head reference failed") }
        var sizes: [Int] = [], timings: [CMSampleTimingInfo] = []
        for pending in staged.prefix(count) {
            let unit = pending.unit
            guard let bytes = CMSampleBufferGetDataBuffer(unit.buffer),
                  CMBlockBufferAppendBufferReference(block, targetBBuf: bytes, offsetToData: 0, dataLength: unit.byteCount, flags: 0) == noErr else {
                throw CaptureFailure(message: "Audio recording payload reference failed")
            }
            sizes.append(unit.byteCount)
            timings.append(CMSampleTimingInfo(duration: unit.duration, presentationTimeStamp: unit.time, decodeTimeStamp: unit.time))
        }
        var combined: CMSampleBuffer?
        let status = sizes.withUnsafeBufferPointer { size in timings.withUnsafeBufferPointer { timing in
            CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block,
                formatDescription: CMSampleBufferGetFormatDescription(staged[0].unit.buffer), sampleCount: count,
                sampleTimingEntryCount: count, sampleTimingArray: timing.baseAddress!, sampleSizeEntryCount: count,
                sampleSizeArray: size.baseAddress!, sampleBufferOut: &combined)
        } }
        guard status == noErr, let combined else { throw CaptureFailure(message: "Audio recording grouped header failed") }
        CMSetAttachment(combined, key: kCMSampleBufferAttachmentKey_TrimDurationAtStart,
            value: CMTimeCopyAsDictionary(CMTime(value: Int64(leading), timescale: 48_000), allocator: kCFAllocatorDefault)!,
            attachmentMode: kCMAttachmentMode_ShouldPropagate)
        if trailing > 0 {
            CMSetAttachment(combined, key: kCMSampleBufferAttachmentKey_TrimDurationAtEnd,
                value: CMTimeCopyAsDictionary(CMTime(value: Int64(trailing), timescale: 48_000), allocator: kCFAllocatorDefault)!,
                attachmentMode: kCMAttachmentMode_ShouldPropagate)
        }
        if writer.status == .unknown {
            guard writer.startWriting() else { throw writer.error ?? CaptureFailure(message: "Audio recording could not start") }
            audioSessionStart = CMTimeAdd(first, CMTime(value: Int64(leading), timescale: 48_000))
            writer.startSession(atSourceTime: audioSessionStart!)
        }
        try appendAudioBuffer(combined)
        for pending in staged.prefix(count) { budget.release(pending.reservation) }
        staged.removeFirst(count); audioHeadWritten = true
    }

    private func appendAudioPending(trailing: UInt32) throws {
        guard let pending = staged.first else { return }
        try appendAudioBuffer(audioCopy(pending.unit, trailing: trailing))
        budget.release(pending.reservation); staged.removeFirst()
    }

    private func appendAudioBuffer(_ sample: CMSampleBuffer) throws {
        guard let start = audioSessionStart else { throw AudioRecordingTiming.Failure.inconsistent }
        let end = CMTimeSubtract(CMTimeAdd(CMSampleBufferGetOutputPresentationTimeStamp(sample), CMSampleBufferGetOutputDuration(sample)), start)
        guard end.isNumeric, end.seconds.isFinite, end.seconds > 0,
              let writer, writer.status == .writing, let audio, audio.isReadyForMoreMediaData, audio.append(sample) else {
            throw writer?.error ?? CaptureFailure(message: "Audio recording storage cannot keep up")
        }
        hasAcceptedMedia = true
        if CMTimeCompare(end, endTime) > 0 { endTime = end }
    }

    private func flushAudioRecording(complete: Bool) throws {
        guard !staged.isEmpty else { return }
        guard let leading = audioLeading else { throw AudioRecordingTiming.Failure.inconsistent }
        var trailing: UInt32 = 0
        if complete {
            guard let extent = audioExtent else { throw CaptureFailure(message: "Native AAC finish timing unavailable") }
            _ = try AudioRecordingTiming.validFrames(extent, observedUnits: audioUnits, leadingFrames: leading)
            trailing = extent.trailingFrames
        }
        if !audioHeadWritten { try appendAudioHead(count: staged.count, trailing: trailing) }
        else { try appendAudioPending(trailing: trailing) }
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
        // The audio-only tail is a bounded accepted prefix, retained until its
        // actual finish attempt. Closing the budget prevents further offers.
        if hasVideo {
            for pending in staged { budget.release(pending.reservation) }
            staged.removeAll()
        }
        onFailure(error)
    }
    func markIncomplete(_ error: Error) { queue.async { self.fail(error) } }
    func checkStorage(profile: CaptureIntent, _ completion: @escaping (Result<Void, Error>) -> Void) {
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
            if !self.hasVideo {
                do { try self.flushAudioRecording(complete: self.failure == nil) }
                catch { self.fail(error); try? self.flushAudioRecording(complete: false) }
            }
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
                if error == nil { error = CaptureFailure(message: "No finalised recording was captured") }
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
