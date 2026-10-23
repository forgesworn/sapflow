import Foundation
import CryptoKit
import Sapflow

/// Prefix-only SDK/file compound owner. No encoder, queue, capture or normal Stop
/// adoption. Every SDK/hasher/file operation shares the same non-reentrant guard.
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
    private var nativeFinished = false
    private let lock = NSLock()
    private enum Phase { case active, closing, retained, closed }
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
    /// No final-header installer exists in this packet, even at checked AAC EOF.
    var normalStopAvailable: Bool { false }
    /// Scalar fencing only, safe during a held real filesystem/native operation.
    func cancel() {
        lock.lock(); admissionClosed = true; lock.unlock()
        file.cancel()
    }
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
            guard !nativeFinished else { throw Failure.closed }
            try muxer.finishNative(outcome: outcome)
            try drain(); nativeFinished = true
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
            guard let chunk = try muxer.takeNativeOutput() else { return }
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
                // Intentionally uninstalled prefix-only metadata. Do not append,
                // hash, rewrite offset zero, advance sequence or mark finalised.
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
