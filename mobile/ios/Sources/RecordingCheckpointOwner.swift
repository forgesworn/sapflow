import Foundation
import CryptoKit
import CoreMedia
import Sapflow

/// SDK/file compound owner. Stored inputs alone remain incomplete; finalised
/// capture requires the concrete native producer/ingress authority at commit.
/// Every SDK/hasher/file operation shares one busy guard.
final class RecordingCheckpointOwner {
    enum Failure: Error, Equatable { case busy, closed, context, sequence, finalHeader }
    struct Frontier: Equatable {
        let owner: UUID
        let sequence: UInt32
        let fileEnd: UInt64
        let fragmentBytes: UInt64
        let audioUnits: UInt64
        let videoUnits: UInt64
        let audioStart: UInt64
        let firstLeadingFrames: UInt32
        let audioContainerEnd: UInt64
        let videoStart: UInt64?
        let videoEnd: UInt64?
        let completeAudioEOF: Bool
    }
    struct PrefixClosure { let bytes: UInt64; let deferredFinalHeader: Bool; let completeAudioEOF: Bool }
    private let owner = UUID()
    private let muxer: NativeRecordingMuxer
    private let file: RecordingCatalogue.AppendWriter
    private let bindings: RecordingCatalogue.AppendBindings
    private let epoch: UInt64
    private var initial: RecordingCatalogue.AppendWriter.InitialAppend?
    private var regionHasher = SHA256()
    private var regionBytes: UInt64 = 0
    private var installed: DecodedRecordingCheckpoint?
    private var deferredFinalHeader: Data?
    private var nativeFinishOutcome: NativeRecordingCompletion?
    private var installedToken: NativeRecordingCheckpoint?
    private var installedReceipt: RecordingCatalogue.AppendWriter.InstalledMetadataReceipt?
    private var outputDrainedAfterFinish = false
    private var earlierFailure = false
    private var cancelRequested = false
    private var storedInputsResult: StoredInputsClosure?
    private let lock = NSLock()
    private enum Phase { case active, closing, committing, retained, closed }
    private var phase = Phase.active
    private var busy = false
    private var admissionClosed = false
    private var progress: Frontier?
    private var frontierObserver: ((Frontier) -> Bool)?
    private var closedResult: PrefixClosure?
    private var retainedSelf: RecordingCheckpointOwner?

    /// Construct the real native object BEFORE creating any file owner. If a
    /// later binding check fails, explicit checked incomplete close is required.
    static func open(catalogue: RecordingCatalogue, identity: RecordingCatalogue.Identity,
                     mediaEpochNs: UInt64, maxFragmentBytes: UInt64, maxFragmentMs: UInt32) throws -> RecordingCheckpointOwner {
        guard let profile = CaptureIntent(rawValue: identity.profileID) else { throw Failure.context }
        let muxer = try NativeRecordingMuxer.withNativeAacTiming(hasVideo: profile.hasVideo, hasAudio: true,
            mediaEpochNs: mediaEpochNs, maxFragmentBytes: maxFragmentBytes, maxFragmentMs: maxFragmentMs)
        let file = try catalogue.beginAppend(identity: identity, payloadLimit: maxFragmentBytes)
        do {
            let bindings = try file.bindings()
            return RecordingCheckpointOwner(muxer: muxer, file: file, bindings: bindings, epoch: mediaEpochNs)
        } catch {
            let original = error
            // Any close failure supersedes construction failure and keeps the
            // actual child/root owner retained. No implicit ownersClosed bypass.
            _ = try file.closeIncomplete()
            throw original
        }
    }
    private init(muxer: NativeRecordingMuxer, file: RecordingCatalogue.AppendWriter,
                 bindings: RecordingCatalogue.AppendBindings, epoch: UInt64) {
        self.muxer = muxer; self.file = file; self.bindings = bindings; self.epoch = epoch
        // Native/metadata ownership also survives caller loss until explicit
        // checked closure, matching Catalogue's retained real append child.
        retainedSelf = self
    }
    var durableBudgetOwner: UUID { owner }
    /// One cold scalar consumer, bound before native input/any filesystem work.
    func observeInstalledFrontier(_ observer: @escaping (Frontier) -> Bool) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard phase == .active, !admissionClosed, !busy, progress == nil, frontierObserver == nil else { return false }
        frontierObserver = observer; return true
    }
    var frontier: Frontier? { lock.lock(); defer { lock.unlock() }; return progress }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return phase == .closed }
    /// True only after an actual concrete producer/ingress closure committed.
    var normalStopAvailable: Bool { lock.lock(); defer { lock.unlock() }; return storedInputsResult?.applicationAdmission == .completeCapture }
    /// Scalar fencing only, safe during a held real filesystem/native operation.
    func cancel() {
        lock.lock(); admissionClosed = true; cancelRequested = true; lock.unlock()
        file.cancel()
    }
    /// Cold recordingIO query, covered by the same compound operation guard.
    func checkStorage() throws { try perform { try file.checkStorage() } }
    func beginAudio(firstLeadingFrames: UInt32) throws {
        try perform { try muxer.beginNativeAudio(firstLeadingFrames: firstLeadingFrames); try drain() }
    }
    func push(sample: MediaSample) throws {
        // Reuse the existing validated handle. No codec/sample construction or
        // second per-access-unit validation is introduced here.
        try perform { try muxer.push(sample: sample); try drain() }
    }
    func generationEnded(_ receipt: NativeAudioGenerationReceipt) throws {
        try perform { try muxer.endNativeAudioGeneration(receipt: receipt); try drain() }
    }
    func completeAudio(_ receipt: NativeAudioFileReceipt) throws {
        try perform { try muxer.completeNativeAudio(receipt: receipt); try drain() }
    }
    /// Explicitly a stored PREFIX, never normal Stop/finalised presentation.
    /// FINAL_HEADER remains bounded deferred metadata, with no offset-zero IO.
    func finishPrefix(outcome: NativeRecordingCompletion) throws {
        try perform {
            guard nativeFinishOutcome == nil else { throw Failure.closed }
            try muxer.finishNative(outcome: outcome)
            nativeFinishOutcome = outcome
            try drain()
        }
    }
    enum StoredInputsScope: Equatable { case submittedSDKInputsOnly, captureProducerAndIngress }
    enum ApplicationAdmission: Equatable { case unknown, completeCapture }
    struct StoredInputsClosure {
        let scope: StoredInputsScope
        let applicationAdmission: ApplicationAdmission
        let sdkOutcome: NativeRecordingCompletion
        let bytes: UInt64
        let movieTimescale: UInt32
        let durationTicks: UInt64
        let durationMsCeil: UInt64
        let completeAudioEOF: Bool
        let cancelledBeforeCommit: Bool
        fileprivate init(outcome: NativeRecordingCompletion, bytes: UInt64,
                         extent: NativeRecordingPresentationExtent, cancelledBeforeCommit: Bool, admission: ApplicationAdmission = .unknown) {
            scope = admission == .completeCapture ? .captureProducerAndIngress : .submittedSDKInputsOnly; applicationAdmission = admission; sdkOutcome = outcome
            self.bytes = bytes; movieTimescale = extent.movieTimescale; durationTicks = extent.durationTicks
            durationMsCeil = extent.durationMsCeil; completeAudioEOF = extent.completeAudioEof
            self.cancelledBeforeCommit = cancelledBeforeCommit
        }
    }
    /// Complete only the inputs this private SDK owner actually accepted.
    /// This entry point has no capture authority. Even checked native EOF and
    /// installed FINAL_HEADER yield incomplete.
    func finishStoredInputs(outcome: NativeRecordingCompletion = .incompletePrefix) throws -> StoredInputsClosure {
        try finishInputs(outcome: outcome, capture: nil)
    }
    var fileURL: URL? {
        lock.lock()
        let finalised = phase == .closed && storedInputsResult?.applicationAdmission == .completeCapture
        lock.unlock()
        return finalised ? file.finalisedURL : nil
    }
    func finishCaptureInputs(_ capture: CaptureRecordingClosure) throws -> StoredInputsClosure {
        try finishInputs(outcome: .complete, capture: capture)
    }
    private func finishInputs(outcome: NativeRecordingCompletion, capture: CaptureRecordingClosure?) throws -> StoredInputsClosure {
        lock.lock()
        if let storedInputsResult {
            lock.unlock()
            guard storedInputsResult.sdkOutcome == outcome else { throw Failure.closed }
            return storedInputsResult
        }
        guard phase == .active, !admissionClosed else { lock.unlock(); throw Failure.closed }
        guard !busy else { lock.unlock(); throw Failure.busy }
        busy = true; lock.unlock()
        var terminalBegan = false
        defer { lock.lock(); busy = false; lock.unlock() }
        do {
            if let previous = nativeFinishOutcome {
                guard previous == outcome else { throw Failure.closed }
            } else {
                try muxer.finishNative(outcome: outcome)
                nativeFinishOutcome = outcome // Preserve the actual successful call.
            }
            try drain()
            guard let initial, let installed, let token = installedToken,
                  let installedReceipt, let header = deferredFinalHeader,
                  outputDrainedAfterFinish, !earlierFailure else { throw Failure.context }
            let info = installed.info(), facts = token.facts(), stats = try muxer.stats()
            let (accepted, overflow) = facts.audioUnits.addingReportingOverflow(facts.videoUnits)
            guard !overflow, facts == info.facts, stats.finished, stats.bufferedSamples == 0,
                  stats.bufferedBytes == 0, stats.queuedOutputBytes == 0,
                  stats.acceptedSamples == accepted, stats.fragments == UInt64(facts.sequence),
                  UInt64(header.count) == initial.bytes, header == installed.header(),
                  installedReceipt.sequence == facts.sequence, installedReceipt.end == info.committedOffset else { throw Failure.context }
            let extent = try token.presentationExtent() // Once, on exact installed token.
            guard extent.movieTimescale == 720_000, extent.completeAudioEof == facts.completeAudioEof,
                  let duration = Int64(exactly: extent.durationMsCeil),
                  outcome != .complete || facts.completeAudioEof else { throw Failure.context }
            try requireAdmission()
            lock.lock(); phase = .closing; admissionClosed = true; lock.unlock(); terminalBegan = true
            // Capture actual finish/drain/stats BEFORE cancellation clears state.
            try muxer.cancel()
            var cancelledBeforeCommit = false
            var captureAccepted = false
            let closed = try file.finishCaptureInputs(header: header, durationMs: duration, checkpoint: installedReceipt) {
                self.lock.lock()
                guard self.phase == .closing, self.busy else { self.lock.unlock(); throw Failure.closed }
                cancelledBeforeCommit = self.cancelRequested
                self.phase = .committing; self.lock.unlock()
                // Header close has returned; receipt IO/metadata closes remain.
                // Exact native/ingress closure is consumed only at this frontier.
                if !cancelledBeforeCommit, outcome == .complete, let capture, let frontier = self.frontier {
                    captureAccepted = capture.consume(owner: self, frontier: frontier)
                }
                return captureAccepted ? .finalised : .incomplete
            }
            let result = StoredInputsClosure(outcome: outcome, bytes: closed.bytes,
                extent: extent, cancelledBeforeCommit: cancelledBeforeCommit,
                admission: captureAccepted ? .completeCapture : .unknown)
            // Drop Rust objects outside the scalar lock, after actual file close.
            installedToken = nil; self.installedReceipt = nil
            lock.lock(); storedInputsResult = result; phase = .closed
            let retained = retainedSelf; retainedSelf = nil; lock.unlock()
            withExtendedLifetime(retained) {}
            return result
        } catch {
            earlierFailure = true
            lock.lock(); admissionClosed = true
            if terminalBegan { phase = .retained; retainedSelf = self }
            lock.unlock(); file.cancel(); throw error
        }
    }
    private func perform<T>(_ body: () throws -> T) throws -> T {
        lock.lock()
        guard phase == .active, !admissionClosed else { lock.unlock(); throw Failure.closed }
        guard !busy else { lock.unlock(); throw Failure.busy }
        busy = true; lock.unlock()
        defer { lock.lock(); busy = false; lock.unlock() }
        do { return try body() }
        catch {
            earlierFailure = true
            lock.lock(); admissionClosed = true; lock.unlock()
            file.cancel(); throw error
        }
    }
    private func requireAdmission() throws {
        lock.lock(); let allowed = phase == .active && !admissionClosed && busy; lock.unlock()
        guard allowed else { throw Failure.closed }
    }
    private func drain() throws {
        while true {
            try requireAdmission()
            guard let chunk = try muxer.takeNativeOutput() else {
                if nativeFinishOutcome != nil { outputDrainedAfterFinish = true }
                return
            }
            switch chunk.kind {
            case .initial:
                guard initial == nil, chunk.checkpoint == nil else { throw Failure.context }
                initial = try file.writeInitial(chunk.bytes)
            case .fragment:
                guard let token = chunk.checkpoint, let initial else { throw Failure.context }
                let (nextRegion, overflow) = regionBytes.addingReportingOverflow(UInt64(chunk.bytes.count))
                guard !overflow else { throw Failure.context }
                var preview = regionHasher
                preview.update(data: chunk.bytes) // THIS original fragment buffer, once.
                let digest = Data(preview.finalize())
                let encoded = try muxer.encodeCheckpoint(token: token, recordingBinding: bindings.recording,
                    sourceFileBinding: bindings.sourceFile, fragmentRegionSha256: digest)
                let info = encoded.info
                let (end, endOverflow) = initial.bytes.addingReportingOverflow(nextRegion)
                guard !endOverflow, info.mediaEpochNs == epoch,
                      info.initialHeaderLen == UInt32(initial.bytes), info.initialHeaderSha256 == initial.sha256,
                      info.recordingBinding == bindings.recording, info.sourceFileBinding == bindings.sourceFile,
                      info.fragmentRegionSha256 == digest, info.fragmentRegionBytes == nextRegion,
                      info.committedOffset == end else { throw Failure.context }
                let prior = installed
                let prepared = try decodeRecordingCheckpoint(bytes: encoded.bytes,
                    expectedRecordingBinding: bindings.recording, expectedSourceFileBinding: bindings.sourceFile)
                guard Self.equal(prepared.info(), info) else { throw Failure.context }
                if let prior { try validateRecordingCheckpointSuccessor(prior: prior, next: prepared) }
                else { guard info.facts.sequence == 1 else { throw Failure.sequence } }
                try requireAdmission()
                var readback: DecodedRecordingCheckpoint?
                let receipt = try file.appendCheckpointFragment(chunk.bytes, expectedEnd: end, sequence: info.facts.sequence,
                    checkpointBytes: encoded.bytes, initialLength: info.initialHeaderLen, initialHash: info.initialHeaderSha256,
                    expectedBindings: bindings) { bytes in
                        let decoded = try decodeRecordingCheckpoint(bytes: bytes,
                            expectedRecordingBinding: self.bindings.recording, expectedSourceFileBinding: self.bindings.sourceFile)
                        guard Self.equal(decoded.info(), info) else { throw Failure.context }
                        if let prior { try validateRecordingCheckpointSuccessor(prior: prior, next: decoded) }
                        readback = decoded // Private candidate only, no state/publication.
                    }
                guard let readback, file.confirms(receipt), receipt.sequence == info.facts.sequence,
                      receipt.end == end else { throw Failure.context }
                try requireAdmission()
                // Guard still covers these worker-only fields; ARC dropping an
                // older Rust object occurs OUTSIDE the scalar publication lock.
                regionHasher = preview; regionBytes = nextRegion; installed = readback
                installedToken = token; installedReceipt = receipt
                let next = Frontier(owner: owner, sequence: info.facts.sequence, fileEnd: end,
                    fragmentBytes: nextRegion, audioUnits: info.facts.audioUnits, videoUnits: info.facts.videoUnits,
                    audioStart: info.facts.audioStart, firstLeadingFrames: info.facts.firstLeadingFrames,
                    audioContainerEnd: info.facts.audioContainerEnd, videoStart: info.facts.videoStart, videoEnd: info.facts.videoEnd,
                    completeAudioEOF: info.facts.completeAudioEof)
                lock.lock()
                guard phase == .active, !admissionClosed, busy else { lock.unlock(); throw Failure.closed }
                // Cancellation and publication share the same cut. The callback
                // only updates bounded scalars; it MUST NOT call this owner/IO.
                guard frontierObserver?(next) ?? true else {
                    admissionClosed = true; lock.unlock(); throw Failure.context
                }
                progress = next; lock.unlock() // AFTER actual installed readback close.
            case .finalHeader:
                guard let initial, chunk.checkpoint == nil, deferredFinalHeader == nil,
                      chunk.bytes.count == Int(initial.bytes), chunk.bytes.count <= 65_536 else { throw Failure.finalHeader }
                // Remains deferred until explicit submitted-input storage finish.
                // Never append/hash this header or advance fragment sequence.
                deferredFinalHeader = chunk.bytes
            }
        }
    }
    private static func equal(_ decoded: DecodedRecordingCheckpointInfo, _ encoded: EncodedRecordingCheckpointInfo) -> Bool {
        decoded.facts == encoded.facts && decoded.mediaEpochNs == encoded.mediaEpochNs &&
            decoded.initialHeaderLen == encoded.initialHeaderLen && decoded.initialHeaderSha256 == encoded.initialHeaderSha256 &&
            decoded.recordingBinding == encoded.recordingBinding && decoded.sourceFileBinding == encoded.sourceFileBinding &&
            decoded.committedOffset == encoded.committedOffset && decoded.fragmentRegionBytes == encoded.fragmentRegionBytes &&
            decoded.fragmentRegionSha256 == encoded.fragmentRegionSha256
    }
    func closeIncomplete() throws -> PrefixClosure {
        lock.lock()
        if let closedResult { lock.unlock(); return closedResult }
        guard phase == .active else { let closing = phase == .closing; lock.unlock(); throw closing ? Failure.busy : Failure.closed }
        guard !busy else { lock.unlock(); throw Failure.busy }
        phase = .closing; admissionClosed = true; lock.unlock()
        do {
            // One real native cancel attempt before any file-token release.
            try muxer.cancel()
            let result = try file.closeIncomplete()
            let closure = PrefixClosure(bytes: result.bytes, deferredFinalHeader: deferredFinalHeader != nil,
                                        completeAudioEOF: frontier?.completeAudioEOF ?? false)
            lock.lock(); closedResult = closure; phase = .closed
            let retained = retainedSelf; retainedSelf = nil; lock.unlock()
            withExtendedLifetime(retained) {} // Any ARC/FFI release is outside lock.
            return closure
        } catch {
            // Never report cleanup after an unknown native or actual FD close.
            lock.lock(); phase = .retained; retainedSelf = self; lock.unlock(); throw error
        }
    }
}


/// Per-real-recorder aggregate media-loss bound. Native input and installed file
/// checkpoints share the declared source presentation axis. This is neither a
/// queue residence timer nor an extra fragment allowance. Locks hold scalars
/// only; no native/FFI/file/payload work runs under this lock.
final class RecordingDurableInputBudget {
    enum Track: Equatable { case audio, video }
    private let owner: UUID
    private let hasVideo: Bool
    private let lock = NSLock()
    private var closed = false
    private var violated = false
    private var announced = false
    private var sourceOrigin: CMTime?
    private var acceptedEnd: CMTime?
    private var audioSupportedEnd: CMTime?
    private var videoSupportedEnd: CMTime?
    private var startupOrigin: CMTime?
    private var startupEnd: CMTime?
    private var startupBound = false
    private var common: CMTime?
    private var sequence: UInt32 = 0
    private var fileEnd: UInt64 = 0
    private var fragmentBytes: UInt64 = 0
    private var lastAudioUnits: UInt64 = 0
    private var lastVideoUnits: UInt64 = 0
    private var lastAudioEnd: UInt64 = 0
    private var lastVideoEnd: UInt64?
    private var audioContext: (start: UInt64, leading: UInt32)?
    private var videoStart: UInt64?
    init(owner: UUID, hasVideo: Bool) { self.owner = owner; self.hasVideo = hasVideo }
    private func refuse() -> Bool { closed = true; violated = true; return false }
    private func numeric(_ value: CMTime) -> Bool { value.isNumeric && value.epoch == 0 }
    private func maximum(_ a: CMTime, _ b: CMTime) -> CMTime { CMTimeCompare(a, b) >= 0 ? a : b }
    private func minimum(_ a: CMTime, _ b: CMTime) -> CMTime { CMTimeCompare(a, b) <= 0 ? a : b }
    private func fits(_ end: CMTime, after frontier: CMTime) -> Bool {
        let delta = CMTimeSubtract(end, frontier)
        return numeric(delta) && CMTimeCompare(delta, CMTime(value: 2, timescale: 1)) <= 0
    }
    /// BEFORE the engine retains its first/every startup CMSampleBuffer. All
    /// original PTS here come from one session synchronizationClock. Starting
    /// a codec or emptying a queue cannot restart this allowance.
    func admitStartup(start: CMTime, duration: CMTime, track: Track) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return false }
        guard !startupBound, sourceOrigin == nil, numeric(start), numeric(duration),
              CMTimeCompare(duration, .zero) > 0, track != .video || hasVideo else { return refuse() }
        guard let span = RecordingNativePCMSpan.conservativeSpan(start: start, duration: duration) else { return refuse() }
        let end = span.end
        let origin = startupOrigin.map { minimum($0, start) } ?? start
        let through = startupEnd.map { maximum($0, end) } ?? end
        let lowerOrigin = CMTimeConvertScale(origin, timescale: 720_000, method: .roundTowardZero)
        guard numeric(lowerOrigin), fits(through, after: lowerOrigin) else { return refuse() }
        startupOrigin = origin; startupEnd = through; return true
    }
    /// The actual CaptureMediaClock epoch is the earliest original retained
    /// PTS. It is checked, not used to rewrite any source/checkpoint timestamp.
    func bindStartup(epoch: CMTime) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return false }
        guard !startupBound, sourceOrigin == nil, let start = startupOrigin, let end = startupEnd,
              numeric(epoch), CMTimeCompare(epoch, start) == 0 else { return refuse() }
        let lowerEpoch = CMTimeConvertScale(epoch, timescale: 720_000, method: .roundTowardZero)
        let span = CMTimeSubtract(end, lowerEpoch)
        guard numeric(span), fits(span, after: .zero) else { return refuse() }
        sourceOrigin = .zero; acceptedEnd = span; startupBound = true; return true
    }
    /// BEFORE native PCM consumption or VT frame reservation. The callback
    /// only fences the recording branch; original publishing proceeds unchanged.
    func admit(start: CMTime, end: CMTime, supportedEnd: CMTime, track: Track) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return false }
        guard numeric(start), numeric(end), numeric(supportedEnd), CMTimeCompare(start, .zero) >= 0,
              CMTimeCompare(supportedEnd, .zero) >= 0, CMTimeCompare(supportedEnd, end) <= 0,
              CMTimeCompare(end, start) > 0, track != .video || hasVideo,
              startupOrigin == nil || startupBound else { return refuse() }
        // A nonzero part begins at its actual first accepted source START,
        // never its latest end or an invented zero. Published PTS stays intact.
        let origin = sourceOrigin.map { minimum($0, start) } ?? start
        let through = acceptedEnd.map { maximum($0, end) } ?? end
        let durable = common.map { maximum(origin, $0) } ?? origin
        guard fits(through, after: durable) else { return refuse() }
        let priorSupport = track == .audio ? audioSupportedEnd : videoSupportedEnd
        guard priorSupport.map({ CMTimeCompare(supportedEnd, $0) >= 0 }) ?? true else { return refuse() }
        sourceOrigin = origin; acceptedEnd = through
        if track == .audio { audioSupportedEnd = supportedEnd } else { videoSupportedEnd = supportedEnd }
        return true
    }
    /// Called ONLY by the exact owner's installed/readback publication cut.
    /// Sequence/file successors come from real SDK checkpoint facts, not AU
    /// counting. Source input may be concurrent; one scalar lock orders both.
    func install(_ value: RecordingCheckpointOwner.Frontier) -> Bool {
        lock.lock(); defer { lock.unlock() }
        // Previously accepted queued units may still form a useful incomplete
        // prefix after loss. They can finish real IO, but grant NO new budget
        // and never clear the sticky failed sustained-run status.
        guard !closed else { return value.owner == owner }
        guard let admittedThrough = acceptedEnd, sourceOrigin != nil else { return refuse() }
        let next = sequence.addingReportingOverflow(1)
        guard value.owner == owner, !next.overflow, value.sequence == next.partialValue,
              value.fileEnd > fileEnd, value.fragmentBytes > fragmentBytes,
              value.audioContainerEnd >= value.audioStart,
              value.audioContainerEnd >= lastAudioEnd,
              value.audioUnits >= lastAudioUnits, value.videoUnits >= lastVideoUnits,
              value.audioUnits > lastAudioUnits || value.videoUnits > lastVideoUnits,
              value.firstLeadingFrames <= 8191,
              value.audioContainerEnd <= UInt64(Int64.max),
              value.videoStart == nil || value.videoEnd != nil,
              value.videoEnd == nil || value.videoStart != nil else { return refuse() }
        if let context = audioContext {
            guard value.audioStart == context.start, value.firstLeadingFrames == context.leading else { return refuse() }
        }
        if let start = videoStart { guard value.videoStart == start else { return refuse() } }
        if let priorEnd = lastVideoEnd { guard value.videoEnd.map({ $0 >= priorEnd }) == true else { return refuse() } }
        if let start = value.videoStart, let end = value.videoEnd {
            guard hasVideo, start <= end, end <= UInt64(Int64.max),
                  value.videoUnits == 0 || start < end else { return refuse() }
        } else if value.videoUnits != 0 { return refuse() }
        let leading = UInt64(value.firstLeadingFrames)
        let audioFrames = value.audioContainerEnd - value.audioStart
        var candidate: CMTime?
        var coverageStart = CMTime(value: Int64(value.audioStart), timescale: 48_000)
        // A head containing only native priming is not durable presentation.
        if value.audioUnits > 0, audioFrames > leading, let audioSupportedEnd {
            var end = minimum(CMTime(value: Int64(value.audioContainerEnd - leading), timescale: 48_000), audioSupportedEnd)
            if hasVideo {
                if value.videoUnits > 0, let videoStart = value.videoStart, let videoEnd = value.videoEnd, let videoSupportedEnd {
                    coverageStart = maximum(coverageStart, CMTime(value: Int64(videoStart), timescale: 90_000))
                    let video = minimum(CMTime(value: Int64(videoEnd), timescale: 90_000), videoSupportedEnd)
                    end = minimum(end, video)
                    if CMTimeCompare(end, coverageStart) > 0 { candidate = end }
                }
            } else { candidate = end }
        }
        if let end = candidate {
            // Native coded start is rounded nearest at 48/90 kHz. One 48k
            // source frame conservatively covers that declared bridge's
            // half-tick residual; this never enlarges the 2-second allowance.
            // File/container coverage cannot certify source that this exact
            // bound native owner has not admitted. Clamp BEFORE withholding
            // rounding guard so even a rounded source upper bound grants less.
            let supported = minimum(end, admittedThrough)
            let safe = CMTimeSubtract(supported, CMTime(value: 1, timescale: 48_000))
            guard numeric(safe), common.map({ CMTimeCompare(safe, $0) >= 0 }) ?? true else { return refuse() }
            if CMTimeCompare(safe, coverageStart) > 0 { common = safe }
        }
        sequence = value.sequence; fileEnd = value.fileEnd; fragmentBytes = value.fragmentBytes
        audioContext = (value.audioStart, value.firstLeadingFrames)
        lastAudioUnits = value.audioUnits; lastVideoUnits = value.videoUnits
        lastAudioEnd = value.audioContainerEnd; lastVideoEnd = value.videoEnd
        if let start = value.videoStart { videoStart = start }
        return true
    }
    func failSourceClock() { lock.lock(); _ = refuse(); lock.unlock() }
    func close() { lock.lock(); closed = true; lock.unlock() }
    func takeFailure() -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard violated, !announced else { return false }; announced = true; return true
    }
    struct Snapshot { let failed: Bool; let sequence: UInt32; let acceptedEnd: CMTime?; let common: CMTime? }
    var snapshot: Snapshot {
        lock.lock(); defer { lock.unlock() }
        return Snapshot(failed: violated, sequence: sequence, acceptedEnd: acceptedEnd, common: common)
    }
}
