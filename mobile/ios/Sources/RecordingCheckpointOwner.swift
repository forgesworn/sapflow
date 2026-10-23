import Foundation
import CryptoKit
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
        let audioContainerEnd: UInt64
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
                    audioContainerEnd: info.facts.audioContainerEnd, videoEnd: info.facts.videoEnd,
                    completeAudioEOF: info.facts.completeAudioEof)
                lock.lock()
                guard phase == .active, !admissionClosed, busy else { lock.unlock(); throw Failure.closed }
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
