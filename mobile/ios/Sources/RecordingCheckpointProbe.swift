import Foundation
import CryptoKit
import Sapflow

/// Cold V1 SDK selection. Closed observations grant no recovery capability;
/// explicit recovery retains the source owner through the actual new-file job.
/// Neither operation grants native EOF, decoded presentation or app admission.
enum RecordingCheckpointProbe {
    enum Failure: Error {
        case noCheckpoint
        case decoderRefusal(slot: Int, actual: RecordingCheckpointError)
        case parityOrSequence
        case physicalPrefix
    }
    struct Observation {
        let identity: RecordingCatalogue.Identity
        let sequence: UInt32
        let committedOffset: UInt64
        let fragmentRegionBytes: UInt64
        let selectedSlotSHA256: Data
        let savedRecoveryHeader: Data
        let observedCurrentHeaderSHA256: Data
        fileprivate init(identity: RecordingCatalogue.Identity, sequence: UInt32, committedOffset: UInt64,
                         fragmentRegionBytes: UInt64, selectedSlotSHA256: Data, savedRecoveryHeader: Data,
                         observedCurrentHeaderSHA256: Data) {
            self.identity = identity; self.sequence = sequence; self.committedOffset = committedOffset
            self.fragmentRegionBytes = fragmentRegionBytes; self.selectedSlotSHA256 = selectedSlotSHA256
            self.savedRecoveryHeader = savedRecoveryHeader; self.observedCurrentHeaderSHA256 = observedCurrentHeaderSHA256
        }
    }
    private struct Row {
        let slot: Int; let bytes: Data; let decoded: DecodedRecordingCheckpoint
        let info: DecodedRecordingCheckpointInfo; let header: Data
    }
    private struct HeldSelection {
        let reader: RecordingCatalogue.CheckpointRead
        let use: RecordingCatalogue.CheckpointRead.Use
        let snapshot: RecordingCatalogue.CheckpointRead.Snapshot
        let latest: Row
        let currentHeaderSHA256: Data
    }
    /// SDK decoding and BOTH physical prefixes remain inside this same live
    /// source Use. A closed Observation cannot be converted into this owner.
    private static func select(catalogue: RecordingCatalogue, selected: RecordingCatalogue.Entry,
                               maximumFragmentBytes: UInt64, cancelled: () -> Bool = { false }) throws -> HeldSelection {
        let reader = try catalogue.beginCheckpointRead(selected: selected)
        let use: RecordingCatalogue.CheckpointRead.Use
        do { use = try reader.start() }
        catch { let original = error; try reader.close(); throw original }
        do {
            if cancelled() { reader.cancel(); throw RecordingCatalogue.Failure.cancelled }
            let snapshot = try reader.snapshot(use)
            var rows: [Row] = []
            for slot in 0..<2 {
                if cancelled() { reader.cancel(); throw RecordingCatalogue.Failure.cancelled }
                guard let bytes = snapshot.slots[slot] else { continue }
                let decoded: DecodedRecordingCheckpoint
                do {
                    decoded = try decodeRecordingCheckpoint(bytes: bytes,
                        expectedRecordingBinding: snapshot.bindings.recording,
                        expectedSourceFileBinding: snapshot.bindings.sourceFile)
                } catch let failure as RecordingCheckpointError {
                    // Preserve the exact SDK refusal. V1 checks geometry before
                    // magic/version; a valid V2 input need not yield Unsupported.
                    // Any present refusal blocks selection; no sniff/retry/fallback.
                    throw Failure.decoderRefusal(slot: slot, actual: failure)
                }
                let info = decoded.info(), header = decoded.header()
                let (end, overflow) = UInt64(info.initialHeaderLen).addingReportingOverflow(info.fragmentRegionBytes)
                guard !overflow, end == info.committedOffset, end <= snapshot.mediaBytes,
                      end <= UInt64(Int64.max), (1...65_536).contains(info.initialHeaderLen),
                      header.count == Int(info.initialHeaderLen),
                      decoded.fragmentRegionSha256() == info.fragmentRegionSha256 else { throw Failure.physicalPrefix }
                guard Int(info.facts.sequence & 1) == slot else { throw Failure.parityOrSequence }
                rows.append(Row(slot: slot, bytes: bytes, decoded: decoded, info: info, header: header))
            }
            guard !rows.isEmpty else { throw Failure.noCheckpoint }
            rows.sort { $0.info.facts.sequence < $1.info.facts.sequence }
            if rows.count == 1 {
                guard rows[0].slot == 1, rows[0].info.facts.sequence == 1 else { throw Failure.parityOrSequence }
            } else {
                // SDK validates context, exact adjacency/progress and no prior EOF.
                try validateRecordingCheckpointSuccessor(prior: rows[0].decoded, next: rows[1].decoded)
            }
            let first = rows[0], latest = rows[rows.count - 1]
            let hashes = try reader.hashPrefixes(use, snapshot: snapshot, initialLength: first.info.initialHeaderLen,
                committedOffsets: rows.map { $0.info.committedOffset }, maximumFragmentBytes: maximumFragmentBytes)
            guard hashes.fragmentHashes == rows.map({ $0.info.fragmentRegionSha256 }) else { throw Failure.physicalPrefix }
            if cancelled() { reader.cancel(); throw RecordingCatalogue.Failure.cancelled }
            return HeldSelection(reader: reader, use: use, snapshot: snapshot, latest: latest,
                                 currentHeaderSHA256: hashes.currentHeaderSHA256)
        } catch {
            let original = error
            // Unknown close supersedes semantic refusal and retains exact owners.
            try reader.abort(use)
            throw original
        }
    }

    static func inspect(catalogue: RecordingCatalogue, selected: RecordingCatalogue.Entry,
                        maximumFragmentBytes: UInt64) throws -> Observation {
        let held = try select(catalogue: catalogue, selected: selected, maximumFragmentBytes: maximumFragmentBytes)
        let latest = held.latest
        let result = Observation(identity: held.snapshot.identity, sequence: latest.info.facts.sequence,
            committedOffset: latest.info.committedOffset, fragmentRegionBytes: latest.info.fragmentRegionBytes,
            selectedSlotSHA256: Data(SHA256.hash(data: latest.bytes)), savedRecoveryHeader: latest.header,
            observedCurrentHeaderSHA256: held.currentHeaderSHA256)
        // Saved recovery-duration header is NOT the original INITIAL hash.
        // Final checked closure has its own outcome. Do not mask an uncertain
        // close by trying to abort the now-retained exact child a second time.
        try held.reader.finish(held.use, snapshot: held.snapshot)
        return result
    }

    /// Sole production recovery front door. Native source EOF, full recording
    /// completion and decoded playback are NOT inferred from a recovered prefix.
    static func recover(catalogue: RecordingCatalogue, selected: RecordingCatalogue.Entry,
                        maximumFragmentBytes: UInt64, cancelled: () -> Bool = { false },
                        willPublish: () throws -> Void = {}) throws -> RecordingCatalogue.RecoveredPrefix {
        let held = try select(catalogue: catalogue, selected: selected,
                              maximumFragmentBytes: maximumFragmentBytes, cancelled: cancelled)
        let latest = held.latest
        let recipe = RecordingCatalogue.RecoveryRecipe(sequence: latest.info.facts.sequence,
            checkpointSHA256: Data(SHA256.hash(data: latest.bytes)), header: latest.header,
            fragmentBytes: latest.info.fragmentRegionBytes, fragmentSHA256: latest.info.fragmentRegionSha256)
        // recover owns every subsequent close/publication result; never mask an
        // uncertain close by aborting this exact source child a second time.
        return try held.reader.recover(held.use, snapshot: held.snapshot, recipe: recipe,
            maximumFragmentBytes: maximumFragmentBytes, cancelled: cancelled, willPublish: willPublish)
    }
}
