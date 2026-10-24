import AVFoundation

/// File work has its own queue and never blocks capture or publishing. Already
/// encoded H.264/AAC goes directly to passthrough inputs: no second encode.
final class CompressedRecorder {
    private let queue: DispatchQueue
    private let budget = EncodedWorkBudget()
    private let terminalLock = NSLock()
    private var terminalAdmission = false
    private var terminalObserver: (() -> Void)?
    private var terminalObserverBound = false
    private var writer: AVAssetWriter?
    private let receipt: RecordingCatalogue.Writer
    private let hasVideo: Bool
    private let captureBound: Bool
    private let loss = CaptureRecordingLoss()
    private var processedSamples: UInt64 = 0 // Existing recording FIFO only.
    private var video: AVAssetWriterInput?
    private var audio: AVAssetWriterInput?
    private var staged: [(unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation)] = []
    private var avAudioTail: (unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation)?
    private var avVideoShift = CMTime.zero
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
    private var audioGenerations = AudioRecordingTiming.Ledger()
    private var audioFileExtent: AudioRecordingTiming.FileExtent?
    private var audioFirstTime: CMTime?
    private var audioSessionStart: CMTime?
    private var audioUnits: UInt64 = 0
    private var storedAudioUnits: UInt64 = 0
    private var audioHeadWritten = false
    struct Completion {
        let fileURL: URL?
        let finalised: Bool
        let cleanupComplete: Bool
        let failure: Error?
    }

    @discardableResult func observeTerminalAdmission(_ closed: @escaping () -> Void) -> Bool {
        terminalLock.lock()
        guard !terminalObserverBound else { terminalLock.unlock(); return false }
        terminalObserverBound = true
        let reportNow = terminalAdmission
        if !reportNow { terminalObserver = closed }
        terminalLock.unlock()
        if reportNow { closed() }
        return true
    }
    /// A failed branch closes its real queue before notifying the logical show.
    /// Ordinary part finish, including Auto, is not a terminal branch detach.
    private func closeFailedAdmission() {
        loss.mark() // Synchronous, before deferred failure notification/IO.
        budget.close()
        terminalLock.lock()
        guard !terminalAdmission else { terminalLock.unlock(); return }
        terminalAdmission = true
        let observer = terminalObserver
        terminalObserver = nil
        terminalLock.unlock()
        observer?()
    }

    /// Called only on the explicit recording IO owner, after its durable WRITING
    /// receipt. The caller closes the receipt if AVAssetWriter creation throws.
    init(receipt: RecordingCatalogue.Writer, queue: DispatchQueue, hasVideo: Bool = true, captureBound: Bool = false, onFailure: @escaping (Error) -> Void) throws {
        self.receipt = receipt; self.queue = queue; self.hasVideo = hasVideo; self.captureBound = captureBound; self.onFailure = onFailure
        if captureBound { try loss.bindLegacy(receipt) }
        guard hasVideo ? receipt.identity.container != .m4a : receipt.identity.container == .m4a else {
            throw CaptureFailure(message: "Recording container does not match its immutable tracks")
        }
        let created = try AVAssetWriter(outputURL: receipt.fileURL, fileType: hasVideo ? .mov : .m4a)
        created.movieTimeScale = 48_000
        created.movieFragmentInterval = CMTime(seconds: 1, preferredTimescale: 600)
        created.shouldOptimizeForNetworkUse = false
        writer = created
    }

    @discardableResult func offer(_ unit: CompressedUnit) -> EncodedWorkBudget.Reservation? {
        let admitted = budget.enqueue(bytes: unit.byteCount, on: queue) { reservation in
            guard !self.finishing, self.failure == nil, let writer = self.writer else { self.budget.release(reservation); return }
            guard !unit.isVideo || self.hasVideo else {
                self.budget.release(reservation)
                self.fail(CaptureFailure(message: "Video does not match this audio-only recording")); return
            }
            do {
                if !self.hasVideo { try self.offerAudio(unit, reservation: reservation) }
                else { try self.offerAudioVideo(unit, reservation: reservation, writer: writer) }
                guard self.processedSamples < UInt64.max else { throw AudioRecordingTiming.Failure.overflow }
                self.processedSamples += 1
            } catch { self.fail(error) }
        }
        if admitted == nil {
            let first = budget.close(enqueueOn: queue, firstOnly: true) {
                self.fail(CaptureFailure(message: "Recording backlog exceeded its 8 MiB / 128 sample / 2 s bound"))
            }
            if first { closeFailedAdmission() }
        }
        return admitted
    }

    private func offerAudioVideo(_ unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation, writer: AVAssetWriter) throws {
        var retained = false
        defer { if !retained { budget.release(reservation) } }
        if !unit.isVideo {
            guard audioLeading != nil, CMSampleBufferGetNumSamples(unit.buffer) == 1,
                  unit.duration == CMTime(value: 1024, timescale: 48_000), audioUnits < UInt64(Int64.max / 1024) else {
                throw AudioRecordingTiming.Failure.inconsistent
            }
            if let first = audioFirstTime {
                guard unit.time == CMTimeAdd(first, CMTime(value: Int64(audioUnits * 1024), timescale: 48_000)) else {
                    throw AudioRecordingTiming.Failure.inconsistent
                }
            } else { audioFirstTime = unit.time }
            audioUnits += 1
        }
        if writer.status == .unknown {
            staged.append((unit, reservation)); retained = true
            if unit.isVideo && video == nil { video = try makeInput(type: .video, unit: unit) }
            else if !unit.isVideo && audio == nil { audio = try makeInput(type: .audio, unit: unit) }
            guard video != nil && audio != nil, let leading = audioLeading,
                  audioUnits > UInt64(try AudioRecordingTiming.headUnits(leading)) else { return }
            try startAudioVideo(final: false, trailing: 0)
        } else if unit.isVideo { try append(unit) }
        else {
            try flushAudioVideoTail(trailing: 0)
            avAudioTail = (unit, reservation); retained = true
        }
    }

    /// Keep the final AAC access unit until actual EOF supplies its tail trim.
    /// A short/faulted initial prefix stays owned until the finish attempt.
    private func startAudioVideo(final: Bool, trailing: UInt32) throws {
        guard let writer, writer.status == .unknown, !staged.isEmpty else { return }
        let initial = staged
        let audioHead = Array(initial.lazy.filter { !$0.unit.isVideo }.map { $0.unit })
        let leading = audioLeading ?? 0
        let count = try AudioRecordingTiming.headUnits(leading)
        let fullHead = audioLeading != nil && audioHead.count >= count
        if !fullHead || video == nil || audio == nil {
            // A container may preserve an incomplete encoded prefix even when
            // no valid AAC presentation or second track became available.
            guard final else { throw AudioRecordingTiming.Failure.inconsistent }
            if failure == nil { fail(CaptureFailure(message: "Recording stopped before its native A/V head completed")) }
        }
        let head = fullHead ? Array(audioHead.prefix(count)) : []
        let grouped = fullHead ? try makeAudioHead(head, leading: leading,
            trailing: final && audioHead.count == count ? trailing : 0) : nil
        guard writer.startWriting() else { throw writer.error ?? CaptureFailure(message: "Recording could not start") }
        avVideoShift = fullHead ? CMTime(value: Int64(leading), timescale: 48_000) : .zero
        audioSessionStart = avVideoShift
        writer.startSession(atSourceTime: avVideoShift)
        staged.removeAll(keepingCapacity: false)
        var transferredTailIndex: Int?
        let lastAudioIndex = initial.lastIndex(where: { !$0.unit.isVideo })
        defer { for (index, pending) in initial.enumerated() where index != transferredTailIndex { budget.release(pending.reservation) } }
        var skipped = 0
        for (index, pending) in initial.enumerated() {
            if pending.unit.isVideo { try append(pending.unit) }
            else if fullHead && skipped < count {
                if skipped == 0, let grouped {
                    try append(CompressedUnit(buffer: grouped, isVideo: false, keyframe: false,
                        time: head[0].time, duration: CMTime(value: Int64(count * 1024), timescale: 48_000), configuration: head[0].configuration))
                }
                skipped += 1
            } else if fullHead && index == lastAudioIndex {
                avAudioTail = pending; transferredTailIndex = index
            } else { try append(pending.unit) }
        }
        audioHeadWritten = fullHead
    }

    private func flushAudioVideoTail(trailing: UInt32) throws {
        guard let pending = avAudioTail else { return }
        let copied = try audioCopy(pending.unit, trailing: trailing)
        try append(CompressedUnit(buffer: copied, isVideo: false, keyframe: false, time: pending.unit.time,
            duration: pending.unit.duration, configuration: pending.unit.configuration))
        avAudioTail = nil; budget.release(pending.reservation)
    }

    /// Cold metadata events are enqueued before first AAC and after codec EOF.
    /// No publishing buffer or timestamp is modified by recording trims.
    func audioTiming(_ event: AudioRecordingTiming.Event) {
        if case .failed = event { closeFailedAdmission() }
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
                guard self.audioExtent == nil, self.audioFileExtent == nil, self.audioGenerations.extent == nil else { self.fail(AudioRecordingTiming.Failure.inconsistent); return }
                self.audioExtent = extent
            case let .generationEnded(extent):
                do {
                    guard self.audioExtent == nil, self.audioFileExtent == nil, let leading = self.audioLeading else { throw AudioRecordingTiming.Failure.inconsistent }
                    try self.audioGenerations.append(extent)
                    guard let file = self.audioGenerations.extent, file.leadingFrames == leading,
                          file.encodedFrames / 1024 == self.audioUnits else { throw AudioRecordingTiming.Failure.inconsistent }
                } catch { self.fail(error) }
            case let .endedGenerations(extent):
                guard self.audioExtent == nil, self.audioFileExtent == nil,
                      self.audioGenerations.extent == extent else { self.fail(AudioRecordingTiming.Failure.inconsistent); return }
                self.audioFileExtent = extent
            case .failed:
                self.fail(CaptureFailure(message: "Native AAC generation did not close cleanly"))
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
        let combined = try makeAudioHead(staged.prefix(count).map { $0.unit }, leading: leading, trailing: trailing)
        if writer.status == .unknown {
            guard writer.startWriting() else { throw writer.error ?? CaptureFailure(message: "Audio recording could not start") }
            audioSessionStart = CMTimeAdd(first, CMTime(value: Int64(leading), timescale: 48_000))
            writer.startSession(atSourceTime: audioSessionStart!)
        }
        try appendAudioBuffer(combined)
        for pending in staged.prefix(count) { budget.release(pending.reservation) }
        staged.removeFirst(count); audioHeadWritten = true
    }

    /// The first AAC buffers carry their declared priming as one native group.
    /// This prevents the writer inventing preroll packets before passthrough
    /// audio. Payload references, original PTS and later AAC remain unchanged.
    private func makeAudioHead(_ units: [CompressedUnit], leading: UInt32, trailing: UInt32) throws -> CMSampleBuffer {
        let count = units.count
        guard (1...8).contains(count), units.allSatisfy({ !$0.isVideo &&
            CMSampleBufferGetNumSamples($0.buffer) == 1 && $0.duration == CMTime(value: 1024, timescale: 48_000) }) else {
            throw AudioRecordingTiming.Failure.inconsistent
        }
        guard Int64(count * 1024) > Int64(leading) + Int64(trailing) else { throw AudioRecordingTiming.Failure.inconsistent }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateEmpty(allocator: kCFAllocatorDefault, capacity: UInt32(count), flags: 0, blockBufferOut: &block) == noErr,
              let block else { throw CaptureFailure(message: "Audio recording head reference failed") }
        var sizes: [Int] = [], timings: [CMSampleTimingInfo] = []
        for unit in units {
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
                formatDescription: CMSampleBufferGetFormatDescription(units[0].buffer), sampleCount: count,
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
        return combined
    }

    private func appendAudioPending(trailing: UInt32) throws {
        guard let pending = staged.first else { return }
        try appendAudioBuffer(audioCopy(pending.unit, trailing: trailing))
        budget.release(pending.reservation); staged.removeFirst()
    }

    private func appendAudioBuffer(_ sample: CMSampleBuffer) throws {
        guard let start = audioSessionStart else { throw AudioRecordingTiming.Failure.inconsistent }
        let count = CMSampleBufferGetNumSamples(sample)
        guard count > 0, UInt64(count) <= UInt64(Int64.max / 1024) - storedAudioUnits else {
            throw AudioRecordingTiming.Failure.overflow
        }
        let end = CMTimeSubtract(CMTimeAdd(CMSampleBufferGetOutputPresentationTimeStamp(sample), CMSampleBufferGetOutputDuration(sample)), start)
        guard end.isNumeric, end.seconds.isFinite, end.seconds > 0,
              let writer, writer.status == .writing, let audio, audio.isReadyForMoreMediaData, audio.append(sample) else {
            throw writer?.error ?? CaptureFailure(message: "Audio recording storage cannot keep up")
        }
        storedAudioUnits += UInt64(count)
        hasAcceptedMedia = true
        if CMTimeCompare(end, endTime) > 0 { endTime = end }
    }

    private func flushAudioRecording(complete: Bool) throws {
        guard !staged.isEmpty else { return }
        guard let leading = audioLeading else { throw AudioRecordingTiming.Failure.inconsistent }
        var trailing: UInt32 = 0
        if complete {
            trailing = try finalAudioTrailing(leading: leading)
        }
        if !audioHeadWritten { try appendAudioHead(count: staged.count, trailing: trailing) }
        else { try appendAudioPending(trailing: trailing) }
    }

    private func finalAudioTrailing(leading: UInt32) throws -> UInt32 {
        if let extent = audioFileExtent {
            guard audioGenerations.extent == extent else { throw AudioRecordingTiming.Failure.inconsistent }
            _ = try AudioRecordingTiming.validFrames(extent, observedUnits: audioUnits, leadingFrames: leading)
            return extent.trailingFrames
        }
        guard let extent = audioExtent else { throw CaptureFailure(message: "Native AAC finish timing unavailable") }
        _ = try AudioRecordingTiming.validFrames(extent, observedUnits: audioUnits, leadingFrames: leading)
        return extent.trailingFrames
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
        var sample = unit.buffer
        if unit.isVideo && avVideoShift != .zero {
            // Audio's measured file priming is removed by the writer session.
            // Shift video metadata by that same file-only origin so its source
            // PTS stays aligned. The compressed payload and live output do not
            // change, and original camera gaps remain present.
            var copied: CMSampleBuffer?
            let shifted = CMTimeAdd(unit.time, avVideoShift)
            var timing = CMSampleTimingInfo(duration: unit.duration, presentationTimeStamp: shifted, decodeTimeStamp: shifted)
            guard CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: unit.buffer,
                sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &copied) == noErr, let copied else {
                throw CaptureFailure(message: "Video recording clock metadata failed")
            }
            sample = copied
        }
        let audioCount = unit.isVideo ? 0 : CMSampleBufferGetNumSamples(sample)
        guard audioCount >= 0, UInt64(audioCount) <= UInt64(Int64.max / 1024) - storedAudioUnits else {
            throw AudioRecordingTiming.Failure.overflow
        }
        guard let writer, let input, writer.status == .writing, input.isReadyForMoreMediaData, input.append(sample) else {
            throw writer?.error ?? CaptureFailure(message: "Recording storage cannot keep up; recording stopped")
        }
        storedAudioUnits += UInt64(audioCount)
        hasAcceptedMedia = true
        let end = CMTimeSubtract(CMTimeAdd(CMSampleBufferGetOutputPresentationTimeStamp(sample), CMSampleBufferGetOutputDuration(sample)), audioSessionStart ?? .zero)
        guard end.isNumeric, end.seconds >= 0, end.seconds.isFinite else {
            throw CaptureFailure(message: "Recording sample extent is invalid")
        }
        if CMTimeCompare(end, endTime) > 0 { endTime = end }
    }
    private func fail(_ error: Error) {
        guard failure == nil else { return }
        failure = error
        closeFailedAdmission()
        // Retain the bounded accepted head/tail until the actual finish attempt,
        // including an A/V show stopped before its AAC priming head completed.
        onFailure(error)
    }
    func markIncomplete(_ error: Error) {
        closeFailedAdmission()
        budget.close(enqueueOn: queue) { self.fail(error) }
    }
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
    func finish(incomplete: Bool = false, producers: CaptureRecordingClosure? = nil, recorder: AnyObject? = nil,
                _ completion: @escaping (Completion) -> Void) {
        if incomplete { closeFailedAdmission() }
        budget.close(enqueueOn: queue) {
            if let completed = self.completed { completion(completed); return }
            guard !self.finishing else {
                if self.joinedCompletion == nil { self.joinedCompletion = completion }
                return
            }
            self.finishing = true; self.completion = completion
            if incomplete && self.failure == nil { self.failure = CaptureFailure(message: "Recording ended with missing encoded media") }
            if self.hasVideo {
                var trailing: UInt32 = 0
                if self.failure == nil {
                    do {
                        guard let leading = self.audioLeading else { throw AudioRecordingTiming.Failure.inconsistent }
                        trailing = try self.finalAudioTrailing(leading: leading)
                    } catch { self.fail(error) }
                }
                do {
                    try self.startAudioVideo(final: true, trailing: trailing)
                    try self.flushAudioVideoTail(trailing: trailing)
                } catch { self.fail(error); try? self.flushAudioVideoTail(trailing: 0) }
            } else {
                do { try self.flushAudioRecording(complete: self.failure == nil) }
                catch { self.fail(error); try? self.flushAudioRecording(complete: false) }
            }
            for pending in self.staged { self.budget.release(pending.reservation) }
            self.staged.removeAll()
            if let tail = self.avAudioTail { self.budget.release(tail.reservation); self.avAudioTail = nil }
            guard let writer = self.writer else {
                self.finishNativeWriter(containerCompleted: false, producers: producers, recorder: recorder); return
            }
            if writer.status == .writing && self.hasAcceptedMedia {
                // All appends use this same queue. Preserve a positive accepted
                // prefix: cancelWriting would block and delete the original.
                self.video?.markAsFinished(); self.audio?.markAsFinished()
                writer.finishWriting { self.queue.async { self.finishNativeWriter(containerCompleted: self.writer?.status == .completed, producers: producers, recorder: recorder) } }
            } else {
                // Public API permits cancellation only after appends have
                // returned; it can delete this never-started/empty output.
                if writer.status == .unknown || writer.status == .writing { writer.cancelWriting() }
                self.finishNativeWriter(containerCompleted: writer.status == .completed && self.hasAcceptedMedia, producers: producers, recorder: recorder)
            }
        }
    }
    private func finishNativeWriter(containerCompleted: Bool, producers: CaptureRecordingClosure?, recorder: AnyObject?) {
        let nativeFailure = writer?.error
        video = nil; audio = nil; writer = nil
        // The preceding native callback/cancel queue turn may still have a
        // local strong writer reference. Its return precedes this metadata turn.
        queue.async { self.finishReceipt(containerCompleted: containerCompleted, nativeFailure: nativeFailure, producers: producers, recorder: recorder) }
    }
    private func finishReceipt(containerCompleted: Bool, nativeFailure: Error?, producers: CaptureRecordingClosure?, recorder: AnyObject?) {
        var error = failure ?? nativeFailure
        var finalised = containerCompleted && error == nil
        var saved: URL?
        do {
            if containerCompleted {
                if hasVideo, let leading = audioLeading,
                   storedAudioUnits > 0, storedAudioUnits * 1024 > UInt64(leading) {
                    do {
                        if finalised && storedAudioUnits != audioUnits { throw AudioRecordingTiming.Failure.inconsistent }
                        let trailing = finalised ? try finalAudioTrailing(leading: leading) : 0
                        let coded = storedAudioUnits * 1024
                        guard UInt64(leading) + UInt64(trailing) < coded else { throw AudioRecordingTiming.Failure.inconsistent }
                        let facts = QuickTimeAudioPriming.Facts(accessUnits: storedAudioUnits,
                            leadingFrames: leading, validFrames: coded - UInt64(leading) - UInt64(trailing))
                        try receipt.updateClosedMedia { file in
                            try QuickTimeAudioPrimingFile.finalise(file, facts: facts)
                        }
                    } catch let metadataError {
                        finalised = false
                        if error == nil { error = metadataError }
                    }
                }
                let milliseconds = endTime.seconds * 1_000
                guard milliseconds.isFinite, milliseconds >= 0, milliseconds < Double(Int64.max) else {
                    throw CaptureFailure(message: "Recording duration is outside its supported bound")
                }
                if finalised && captureBound {
                    if let producers, let recorder,
                       producers.closeLegacyRecorder(recorder, writer: receipt, loss: loss),
                       let permit = producers.consumeLegacy(writer: receipt, processedSamples: processedSamples) {
                        try receipt.complete(durationMs: Int64(milliseconds), capture: permit)
                    } else {
                        finalised = false
                        if error == nil { error = CaptureFailure(message: "Real producer/ingress closure or recording loss refused final publication") }
                        try receipt.complete(durationMs: Int64(milliseconds), outcome: .incomplete)
                    }
                } else {
                    try receipt.complete(durationMs: Int64(milliseconds), outcome: finalised ? .finalised : .incomplete)
                }
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
