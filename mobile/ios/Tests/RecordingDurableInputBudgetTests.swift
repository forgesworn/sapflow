import XCTest
import Foundation
import CoreMedia
import Darwin
import Sapflow
@testable import SapflowBroadcaster

/// Explicit scalar test controls below assert that their declared endpoint is
/// ALREADY supported by a native source. Real production must supply its
/// separate prior-consumed/prior-accepted endpoint and has no such overload.
private extension RecordingDurableInputBudget {
    @discardableResult func admit(start: CMTime, end: CMTime, track: Track) -> Bool {
        admit(start: start, end: end, supportedEnd: end, track: track)
    }
}

/// Scalar controls are explicitly synthetic. The final tests use real SDK
/// objects + actual append/install/readback; none claim real codec/device proof.
final class RecordingDurableInputBudgetTests: XCTestCase {
    private func t(_ value: Int64, _ scale: Int32 = 48_000) -> CMTime { CMTime(value: value, timescale: scale) }
    private func facts(_ owner: UUID, sequence: UInt32 = 1, units: UInt64 = 48,
                       start: UInt64 = 0, leading: UInt32 = 0, tail: UInt64 = 0,
                       videoStart: UInt64? = nil, videoEnd: UInt64? = nil) -> RecordingCheckpointOwner.Frontier {
        .init(owner: owner, sequence: sequence, fileEnd: UInt64(sequence) * 4096,
            fragmentBytes: UInt64(sequence) * 4000, audioUnits: units, videoUnits: videoEnd == nil ? 0 : 15,
            audioStart: start, firstLeadingFrames: leading, audioContainerEnd: start + units * 1024 - tail,
            videoStart: videoStart, videoEnd: videoEnd, completeAudioEOF: tail > 0)
    }
    func testAdmissionFailsBeforeAcceptingBeyondTwoSecondsAndDoesNotAddFragmentAllowance() {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(gate.admit(start: .zero, end: t(96_000), track: .audio))
        XCTAssertFalse(gate.admit(start: t(96_000), end: t(96_001), track: .audio))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.acceptedEnd!, t(96_000)), 0)
        XCTAssertTrue(gate.takeFailure()); XCTAssertFalse(gate.takeFailure())
        XCTAssertTrue(gate.install(facts(id))) // Safe incomplete-prefix IO, NO grant/recovery.
        XCTAssertEqual(gate.snapshot.sequence, 0)
        XCTAssertFalse(gate.admit(start: t(96_000), end: t(96_001), track: .audio))
    }
    func testNewCheckpointMovesOneAggregateAllowanceFromCommonDurablePresentation() {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(gate.admit(start: .zero, end: t(96_000), track: .audio))
        XCTAssertTrue(gate.install(facts(id, units: 24)))
        let end = Int64(24 * 1024 - 1) // Conservative actual clock rounding guard.
        XCTAssertEqual(CMTimeCompare(gate.snapshot.common!, t(end)), 0)
        XCTAssertTrue(gate.admit(start: t(480), end: t(end + 96_000), track: .audio))
        XCTAssertFalse(gate.admit(start: t(end + 96_000), end: t(end + 96_001), track: .audio))
    }
    func testAVCommonFrontierUsesMinimumAndNeverMovieMaximum() {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: true)
        XCTAssertTrue(gate.admit(start: .zero, end: t(96_000), track: .audio))
        XCTAssertTrue(gate.admit(start: .zero, end: t(96_000), track: .video))
        XCTAssertTrue(gate.install(facts(id, videoStart: 0, videoEnd: 45_000)))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.common!, t(23_999)), 0)
        XCTAssertFalse(gate.admit(start: t(480), end: t(120_000), track: .video))
    }
    func testActualPrimingRemovedOnceAndEOFUsesContainerEndRatherThanCodedEnd() {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(gate.admit(start: t(4_800), end: t(100_800), track: .audio))
        XCTAssertTrue(gate.install(facts(id, start: 4_800, leading: 2_112, tail: 17)))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.common!, t(4_800 + 49_152 - 17 - 2_112 - 1)), 0)
    }
    func testMissingAVTrackAndPrimingOnlyHeadGrantNoCommonCoverage() {
        let id = UUID(), av = RecordingDurableInputBudget(owner: id, hasVideo: true)
        XCTAssertTrue(av.admit(start: .zero, end: t(480), track: .audio))
        XCTAssertTrue(av.install(facts(id))); XCTAssertNil(av.snapshot.common)
        let disjoint = RecordingDurableInputBudget(owner: id, hasVideo: true)
        XCTAssertTrue(disjoint.admit(start: .zero, end: t(4_800), track: .audio))
        XCTAssertTrue(disjoint.admit(start: t(48_000), end: t(52_800), track: .video))
        XCTAssertTrue(disjoint.install(facts(id, units: 5, videoStart: 90_000, videoEnd: 99_000)))
        XCTAssertNil(disjoint.snapshot.common) // Ends alone do not prove overlapping A/V coverage.
        let head = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(head.admit(start: .zero, end: t(480), track: .audio))
        XCTAssertTrue(head.install(facts(id, units: 2, leading: 2_112)))
        XCTAssertNil(head.snapshot.common)
    }
    func testForeignSkippedReplayedRegressedAndOverflowGrantsFailSticky() {
        for attack in 0..<5 {
            let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
            XCTAssertTrue(gate.admit(start: .zero, end: t(480), track: .audio))
            XCTAssertTrue(gate.install(facts(id)))
            let next: RecordingCheckpointOwner.Frontier
            switch attack {
            case 0: next = facts(UUID(), sequence: 2, units: 96)
            case 1: next = facts(id, sequence: 3, units: 96)
            case 2: next = facts(id, units: 48)
            case 3: next = facts(id, sequence: 2, units: 24)
            default:
                next = .init(owner: id, sequence: 2, fileEnd: 8192, fragmentBytes: 8000,
                    audioUnits: 96, videoUnits: 0, audioStart: 0, firstLeadingFrames: 0,
                    audioContainerEnd: UInt64.max, videoStart: nil, videoEnd: nil, completeAudioEOF: false)
            }
            XCTAssertFalse(gate.install(next)); XCTAssertTrue(gate.snapshot.failed)
            XCTAssertFalse(gate.admit(start: t(480), end: t(960), track: .audio))
        }
    }
    func testFirstRetainedStartupBufferStartsAllowanceAndCodecCannotRestartIt() {
        let gate = RecordingDurableInputBudget(owner: UUID(), hasVideo: true)
        XCTAssertTrue(gate.admitStartup(start: t(480_000), duration: t(480), track: .audio))
        XCTAssertTrue(gate.admitStartup(start: t(575_000), duration: t(1_000), track: .video))
        XCTAssertTrue(gate.bindStartup(epoch: t(480_000)))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.acceptedEnd!, t(96_000)), 0)
        XCTAssertFalse(gate.admit(start: t(96_000), end: t(96_001), track: .audio))
        let refused = RecordingDurableInputBudget(owner: UUID(), hasVideo: true)
        XCTAssertTrue(refused.admitStartup(start: t(480_000), duration: t(480), track: .audio))
        XCTAssertFalse(refused.admitStartup(start: t(576_000), duration: t(1), track: .video))
    }
    func testNonzeroPartOriginIsActualFirstSourceStartAndTimestampRemainsNonzero() {
        let gate = RecordingDurableInputBudget(owner: UUID(), hasVideo: false)
        XCTAssertTrue(gate.admit(start: t(480_000), end: t(576_000), track: .audio))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.acceptedEnd!, t(576_000)), 0)
        XCTAssertFalse(gate.admit(start: t(576_000), end: t(576_001), track: .audio))
    }
    func testReorderedFirstAVInputMovesUndurableOriginEarlierWithoutRebasingPTS() {
        let gate = RecordingDurableInputBudget(owner: UUID(), hasVideo: true)
        XCTAssertTrue(gate.admit(start: t(480_000), end: t(481_920), track: .video))
        // Video first at10s cannot lend a new10s baseline to audio that began9s.
        XCTAssertFalse(gate.admit(start: t(432_000), end: t(552_000), track: .audio))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.acceptedEnd!, t(481_920)), 0)
        XCTAssertTrue(gate.snapshot.failed)
    }
    func testPreInputCheckpointCannotCertifySourceAndGrantCannotExceedAcceptedSpan() {
        let id = UUID(), absent = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertFalse(absent.install(facts(id)))
        XCTAssertNil(absent.snapshot.common)
        let bound = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(bound.admit(start: .zero, end: t(480), track: .audio))
        XCTAssertTrue(bound.install(facts(id)))
        XCTAssertEqual(CMTimeCompare(bound.snapshot.common!, t(479)), 0)
        XCTAssertFalse(bound.admit(start: t(480), end: t(96_480), track: .audio))
    }
    func testRepeatedNegativeRawJitterCannotHideConsumedPCMFromBudget() throws {
        let gate = RecordingDurableInputBudget(owner: UUID(), hasVideo: false)
        for block in 0..<100 {
            let raw = t(Int64(block) * (960 - 192)) // -4ms each block, accepted by old native continuity policy.
            let span = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: .zero,
                codedGenerationStart: 0, generationLeading: 2_112, fileLeading: 2_112,
                consumedSourceFrames: UInt64(block * 960), pendingSourceFrames: 960,
                sourceRate: 48_000, rawTime: raw))
            XCTAssertTrue(gate.admit(start: span.start, end: span.end, supportedEnd: span.supportedEnd, track: .audio))
        }
        let next = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: .zero,
            codedGenerationStart: 0, generationLeading: 2_112, fileLeading: 2_112,
            consumedSourceFrames: 96_000, pendingSourceFrames: 960, sourceRate: 48_000, rawTime: t(76_800)))
        XCTAssertFalse(gate.admit(start: next.start, end: next.end, supportedEnd: next.supportedEnd, track: .audio))
    }
    func testRepeatedPositiveRawJitterIsChargedWithoutChangingNativeSamples() throws {
        let gate = RecordingDurableInputBudget(owner: UUID(), hasVideo: false)
        var refused = false
        for block in 0..<100 {
            let span = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: .zero,
                codedGenerationStart: 0, generationLeading: 2_112, fileLeading: 2_112,
                consumedSourceFrames: UInt64(block * 960), pendingSourceFrames: 960,
                sourceRate: 48_000, rawTime: t(Int64(block) * (960 + 192))))
            if !gate.admit(start: span.start, end: span.end, supportedEnd: span.supportedEnd, track: .audio) { refused = true; break }
        }
        XCTAssertTrue(refused)
    }
    func testUnequalTrackHorizonsCannotLendVideoCoverageToAudioPadding() {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: true)
        XCTAssertTrue(gate.admit(start: .zero, end: t(4_800), supportedEnd: t(4_800), track: .audio))
        XCTAssertTrue(gate.admit(start: .zero, end: t(96_000), supportedEnd: t(96_000), track: .video))
        XCTAssertTrue(gate.install(facts(id, units: 5, videoStart: 0, videoEnd: 180_000)))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.common!, t(4_799)), 0)
    }
    func testPositiveRawJitterCannotCertifyUnconsumedCodedAudioPadding() throws {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
        XCTAssertTrue(gate.admit(start: .zero, end: t(480), supportedEnd: .zero, track: .audio))
        let span = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: .zero,
            codedGenerationStart: 0, generationLeading: 2_112, fileLeading: 2_112,
            consumedSourceFrames: 4_800, pendingSourceFrames: 480, sourceRate: 48_000, rawTime: t(9_600)))
        XCTAssertEqual(CMTimeCompare(span.supportedEnd, t(4_800)), 0)
        XCTAssertTrue(gate.admit(start: span.start, end: span.end, supportedEnd: span.supportedEnd, track: .audio))
        XCTAssertTrue(gate.install(facts(id, units: 10)))
        XCTAssertEqual(CMTimeCompare(gate.snapshot.common!, t(4_799)), 0)
    }
    func testPendingOnlyPCMChargesTwoSecondsButCannotGrantAnyDurableSourceSupport() throws {
        let id = UUID(), gate = RecordingDurableInputBudget(owner: id, hasVideo: false)
        let span = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: .zero,
            codedGenerationStart: 0, generationLeading: 2_112, fileLeading: 2_112,
            consumedSourceFrames: 0, pendingSourceFrames: 96_000, sourceRate: 48_000, rawTime: .zero))
        XCTAssertTrue(gate.admit(start: span.start, end: span.end, supportedEnd: span.supportedEnd, track: .audio))
        XCTAssertTrue(gate.install(facts(id)))
        XCTAssertNil(gate.snapshot.common)
        XCTAssertFalse(gate.admit(start: t(96_000), end: t(96_001), supportedEnd: .zero, track: .audio))
    }
    func testRationalNativeSourceRatesAndActualInteriorPrimeAreCharged() throws {
        for rate in [UInt32(44_100), 48_000, 96_000] {
            let span = try XCTUnwrap(RecordingNativePCMSpan.make(firstTime: t(17),
                codedGenerationStart: 51_200, generationLeading: 2_112, fileLeading: 2_112,
                consumedSourceFrames: UInt64(rate), pendingSourceFrames: rate / 100,
                sourceRate: rate, rawTime: t(17 + 51_200 + 48_000)))
            XCTAssertEqual(CMTimeCompare(span.end, t(17 + 51_200 + 48_000 + 480)), 0)
        }
        XCTAssertNil(RecordingNativePCMSpan.make(firstTime: .zero, codedGenerationStart: 0,
            generationLeading: 0, fileLeading: 2_112, consumedSourceFrames: 0,
            pendingSourceFrames: 480, sourceRate: 48_000, rawTime: .zero))
        XCTAssertNil(RecordingNativePCMSpan.make(firstTime: .zero, codedGenerationStart: 0,
            generationLeading: 2_112, fileLeading: 2_112, consumedSourceFrames: 0,
            pendingSourceFrames: 480, sourceRate: 192_001, rawTime: .zero))
        XCTAssertNil(RecordingNativePCMSpan.make(firstTime: .zero, codedGenerationStart: UInt64.max,
            generationLeading: 2_112, fileLeading: 2_112, consumedSourceFrames: 0,
            pendingSourceFrames: 480, sourceRate: 48_000, rawTime: .zero))
    }
    private func realPublication(cancelDuringReadback: Bool) throws {
        let scratch = FileManager.default.temporaryDirectory.appendingPathComponent("sapflow-durable-budget-" + UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: scratch) }
        var owner: RecordingCheckpointOwner?, gate: RecordingDurableInputBudget?, observed = false
        let metadata = RecordingCatalogue.CheckpointIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            rename: { Darwin.renameat($0, $1, $0, $2) }, synchronizeDirectory: Darwin.fsync,
            read: { fd, buffer, count in
                observed = true
                XCTAssertEqual(gate?.snapshot.sequence, 0) // Real installed slot readback not closed yet.
                if cancelDuringReadback { owner?.cancel() }
                return Darwin.read(fd, buffer, count)
            })
        let capacity = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { _ in 1024 * 1024 * 1024 })
        let catalogue = try RecordingCatalogue(root: scratch, appendIO: capacity, checkpointIO: metadata)
        defer { try? catalogue.close() }
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let actual = try RecordingCheckpointOwner.open(catalogue: catalogue, identity: identity,
            mediaEpochNs: 0, maxFragmentBytes: 4096, maxFragmentMs: 100)
        owner = actual
        let budget = RecordingDurableInputBudget(owner: actual.durableBudgetOwner, hasVideo: false); gate = budget
        XCTAssertTrue(actual.observeInstalledFrontier { budget.install($0) })
        XCTAssertTrue(budget.admit(start: .zero, end: t(480), track: .audio))
        try actual.beginAudio(firstLeadingFrames: 0) // Explicit synthetic AAC container control.
        let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
        var failure: Error?
        do {
            for index in 0..<9 {
                let sample = try codec.sample(bytes: Data([1, 2, UInt8(index)]), pts: Int64(index * 1024),
                    dts: Int64(index * 1024), duration: 1024, timescale: 48_000, keyframe: false)
                try actual.push(sample: sample)
                if observed { break }
            }
        } catch { failure = error }
        XCTAssertTrue(observed)
        if cancelDuringReadback {
            XCTAssertNotNil(failure); XCTAssertEqual(budget.snapshot.sequence, 0); XCTAssertNil(budget.snapshot.common)
        } else {
            XCTAssertNil(failure); XCTAssertEqual(budget.snapshot.sequence, 1)
            XCTAssertEqual(actual.frontier?.sequence, budget.snapshot.sequence)
            XCTAssertNotNil(budget.snapshot.common)
        }
        _ = try actual.closeIncomplete(); XCTAssertTrue(actual.cleanupComplete)
    }
    func testActualSDKReadbackMustFinishBeforeBudgetGrant() throws { try realPublication(cancelDuringReadback: false) }
    func testCancellationDuringActualReadbackCannotGrantBudget() throws { try realPublication(cancelDuringReadback: true) }
}
