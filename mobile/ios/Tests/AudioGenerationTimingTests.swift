import XCTest
@testable import SapflowBroadcaster

final class AudioGenerationTimingTests: XCTestCase {
    typealias Timing = AudioRecordingTiming
    // Illustrative generated receipts. These values are never codec constants.
    private func extent(_ id: UInt64, input: UInt64, raw: UInt64? = nil, rate: UInt32 = 48_000,
                        coded: UInt64, leading: UInt32 = 2112, trailing: UInt32,
                        mapping: Timing.SourceFrameMapping = .nearest) -> Timing.GenerationExtent {
        .init(generation: id, inputFrames: input, rawFrames: raw ?? input,
            zeroFrames: input - (raw ?? input), inputRate: rate, encodedFrames: coded,
            leadingFrames: leading, trailingFrames: trailing, sourceMapping: mapping)
    }
    func testSingleGenerationPreservesLegacyExtent() throws {
        let legacy = Timing.Extent(inputFrames: 144000, inputRate: 48000, encodedFrames: 146432, leadingFrames: 2112, trailingFrames: 320)
        XCTAssertEqual(try Timing.validFrames(legacy, observedUnits: 143, leadingFrames: 2112), 144000)
        var ledger = Timing.Ledger()
        try ledger.append(extent(1, input: 144000, coded: 146432, trailing: 320))
        XCTAssertEqual(try Timing.validFrames(XCTUnwrap(ledger.extent), observedUnits: 143, leadingFrames: 2112), 144000)
        XCTAssertEqual(ledger.extent?.intentionalStandbyFrames, 0)
    }
    func testTwoGenerationsCountInteriorStandbyWithoutFakingSourceSum() throws {
        var ledger = Timing.Ledger()
        try ledger.append(extent(1, input: 48257, coded: 51200, trailing: 831))
        try ledger.append(extent(2, input: 93057, raw: 0, coded: 95232, trailing: 63))
        let file = try XCTUnwrap(ledger.extent)
        XCTAssertEqual(file.sourceFramesAt48k, 141314)
        XCTAssertEqual(file.intentionalStandbyFrames, 2943)
        XCTAssertEqual(file.generationCount, 2)
        XCTAssertEqual(try Timing.validFrames(file, observedUnits: 143, leadingFrames: 2112), 144257)
    }
    func testThreeGenerationsFoldConstantSpaceReceipts() throws {
        var ledger = Timing.Ledger()
        try ledger.append(extent(1, input: 48257, coded: 51200, trailing: 831))
        try ledger.append(extent(2, input: 45074, raw: 5000, coded: 48128, trailing: 942))
        try ledger.append(extent(3, input: 44929, raw: 0, coded: 47104, trailing: 63))
        let file = try XCTUnwrap(ledger.extent)
        XCTAssertEqual(file.generationCount, 3); XCTAssertEqual(file.intentionalStandbyFrames, 5997)
        XCTAssertEqual(ledger.last?.generation, 3)
        XCTAssertEqual(try Timing.validFrames(file, observedUnits: 143, leadingFrames: 2112), 144257)
    }
    func testInitialZeroGenerationHasOnlyFileLeadingAndTail() throws {
        var ledger = Timing.Ledger()
        try ledger.append(extent(1, input: 96000, raw: 0, coded: 98304, trailing: 192))
        let file = try XCTUnwrap(ledger.extent)
        XCTAssertEqual(file.intentionalStandbyFrames, 0)
        XCTAssertEqual(try Timing.validFrames(file, observedUnits: 96, leadingFrames: 2112), 96000)
    }
    func testExactZeroPlanSubtractsMeasuredInteriorSlots() throws {
        let plan = try Timing.zeroPlan(target: 144257, firstLeading: 2112, priorCoded: 51200,
            currentLeading: 2112, currentInput: 0, inputRate: 48000)
        XCTAssertEqual(plan.additionalSourceFrames, 93057)
        XCTAssertEqual(plan.attainableValidFrames, 144257); XCTAssertEqual(plan.quantizationOutputFrames, 0)
    }
    func testPlanAccountsAlreadyFedZeroAndResumedRaw() throws {
        let plan = try Timing.zeroPlan(target: 144257, firstLeading: 2112, priorCoded: 51200,
            currentLeading: 2112, currentInput: 12345, inputRate: 48000)
        XCTAssertEqual(plan.additionalSourceFrames, 80712)
    }
    func testPlanRefusesTooShortOrAlreadyExceededTarget() {
        XCTAssertThrowsError(try Timing.zeroPlan(target: 48768, firstLeading: 2112, priorCoded: 51200,
            currentLeading: 2112, currentInput: 0, inputRate: 48000))
        XCTAssertThrowsError(try Timing.zeroPlan(target: 144257, firstLeading: 2112, priorCoded: 51200,
            currentLeading: 2112, currentInput: 93058, inputRate: 48000))
    }
    func testRational44100PlanReportsQuantizationExplicitly() throws {
        let plan = try Timing.zeroPlan(target: 6, firstLeading: 0, priorCoded: 0,
            currentLeading: 0, currentInput: 0, inputRate: 44100)
        XCTAssertEqual(plan.additionalSourceFrames, 6)
        XCTAssertEqual(plan.requestedValidFrames, 6); XCTAssertEqual(plan.attainableValidFrames, 7)
        XCTAssertEqual(plan.quantizationOutputFrames, 1)
        XCTAssertEqual(try Timing.framesAt48k(44100, rate: 44100), 48000)
    }
    func testMeasured44100ReceiptMustCloseBeforeBeingAccepted() throws {
        var ledger = Timing.Ledger()
        try ledger.append(extent(1, input: 44100, rate: 44100, coded: 50176, trailing: 64))
        XCTAssertEqual(try Timing.validFrames(XCTUnwrap(ledger.extent), observedUnits: 49, leadingFrames: 2112), 48000)
        var refused = Timing.Ledger()
        XCTAssertThrowsError(try refused.append(extent(1, input: 44100, rate: 44100, coded: 50176, trailing: 63)))
        XCTAssertNil(refused.extent)
    }
    func testResamplingFloorIsExplicitAndPreservesExistingNearestCallers() throws {
        XCTAssertEqual(try Timing.framesAt48k(44117, rate: 44100), 48019)
        let converted = try Timing.sourceFrameExtent(44117, rate: 44100, mapping: .floor)
        XCTAssertEqual(converted.frames, 48018)
        XCTAssertEqual(converted.quantizationNumerator, -22200)
        XCTAssertEqual(converted.quantizationDenominator, 44100)
        let nearest = try Timing.sourceFrameExtent(44117, rate: 44100, mapping: .nearest)
        XCTAssertEqual(nearest.frames, 48019); XCTAssertEqual(nearest.quantizationNumerator, 21900)
    }
    func testMeasuredResamplingReceiptClosesFloorAndRefusesWrongMappingOrNativePrime() throws {
        let measured = extent(1, input: 44117, rate: 44100, coded: 50176, trailing: 46, mapping: .floor)
        var ledger = Timing.Ledger(); try ledger.append(measured)
        XCTAssertEqual(try XCTUnwrap(ledger.extent).sourceFramesAt48k, 48018)
        XCTAssertEqual(measured.sourceExtent?.quantizationNumerator, -22200)
        var wrongMapping = Timing.Ledger()
        XCTAssertThrowsError(try wrongMapping.append(extent(1, input: 44117, rate: 44100, coded: 50176, trailing: 46)))
        XCTAssertNil(wrongMapping.extent)
        var wrongPrime = Timing.Ledger()
        XCTAssertThrowsError(try wrongPrime.append(extent(1, input: 44117, rate: 44100, coded: 50176, trailing: 45, mapping: .floor)))
        XCTAssertNil(wrongPrime.extent)
        // The old composed .none facts must still fail, not absorb five frames.
        XCTAssertThrowsError(try wrongPrime.append(extent(1, input: 14837, rate: 44100, coded: 18432,
            leading: 2144, trailing: 134, mapping: .floor)))
        XCTAssertNil(wrongPrime.extent)
    }
    func testLegacySingleGenerationReceiptCanExplicitlyValidateResamplingFloor() throws {
        let actual = Timing.Extent(inputFrames: 44117, inputRate: 44100, encodedFrames: 50176,
            leadingFrames: 2112, trailingFrames: 46, sourceMapping: .floor)
        XCTAssertEqual(try Timing.validFrames(actual, observedUnits: 49, leadingFrames: 2112), 48018)
        let wrong = Timing.Extent(inputFrames: 44117, inputRate: 44100, encodedFrames: 50176,
            leadingFrames: 2112, trailingFrames: 46)
        XCTAssertThrowsError(try Timing.validFrames(wrong, observedUnits: 49, leadingFrames: 2112))
    }
    func testExternallyPinnedOneSecondResumeUsesExplicitSourceQuantisation() throws {
        // Captured native old-prefix coded extent and equal measured heads.
        // The requested B time is independently fixed at one second.
        let plan = try Timing.zeroPlan(target: 48000, firstLeading: 2112, priorCoded: 18432,
            currentLeading: 2112, currentInput: 0, inputRate: 44100, sourceMapping: .floor)
        XCTAssertEqual(plan.additionalSourceFrames, 27166)
        XCTAssertEqual(plan.attainableValidFrames, 48000)
        XCTAssertEqual(plan.sourceExtent.frames, 29568)
        XCTAssertEqual(plan.sourceExtent.quantizationNumerator, -19200)
        XCTAssertEqual(plan.quantizationOutputFrames, 0)
        XCTAssertEqual(plan.sourceMapping, .floor)
        // Not every requested slot is representable by an integer 44.1k input.
        let skipped = try Timing.zeroPlan(target: 12, firstLeading: 0, priorCoded: 0,
            currentLeading: 0, currentInput: 0, inputRate: 44100, sourceMapping: .floor)
        XCTAssertEqual(skipped.additionalSourceFrames, 12)
        XCTAssertEqual(skipped.attainableValidFrames, 13)
        XCTAssertEqual(skipped.quantizationOutputFrames, 1)
    }
    func testResampledGenerationReplayAndBadEOFCannotChangeAcceptedPrefix() throws {
        var ledger = Timing.Ledger()
        let first = extent(1, input: 14837, rate: 44100, coded: 18432, trailing: 171, mapping: .floor)
        try ledger.append(first); let prefix = ledger.extent
        XCTAssertThrowsError(try ledger.append(first)); XCTAssertEqual(ledger.extent, prefix)
        XCTAssertThrowsError(try ledger.append(extent(2, input: 44117, rate: 44100, coded: 50176, trailing: 47, mapping: .floor)))
        XCTAssertEqual(ledger.extent, prefix)
        try ledger.append(extent(2, input: 44117, rate: 44100, coded: 50176, trailing: 46, mapping: .floor))
        XCTAssertEqual(ledger.extent?.generationCount, 2)
    }
    func testReplayedSkippedAndContradictoryReceiptsCannotMutateAggregate() throws {
        var ledger = Timing.Ledger()
        let first = extent(1, input: 48257, coded: 51200, trailing: 831)
        try ledger.append(first); let original = ledger.extent
        XCTAssertThrowsError(try ledger.append(first))
        XCTAssertThrowsError(try ledger.append(extent(3, input: 93057, raw: 0, coded: 95232, trailing: 63)))
        XCTAssertThrowsError(try ledger.append(extent(2, input: 93057, raw: 0, coded: 95232, trailing: 62)))
        XCTAssertEqual(ledger.extent, original)
    }
    func testMissingObservedUnitsAndRelabelledFileTrimRefuse() throws {
        var ledger = Timing.Ledger(); try ledger.append(extent(1, input: 48257, coded: 51200, trailing: 831))
        let file = try XCTUnwrap(ledger.extent)
        XCTAssertThrowsError(try Timing.validFrames(file, observedUnits: 49, leadingFrames: 2112))
        XCTAssertThrowsError(try Timing.validFrames(file, observedUnits: 50, leadingFrames: 0))
    }
    func testOverflowAndUnsupportedRatesRefuse() {
        XCTAssertThrowsError(try Timing.framesAt48k(UInt64.max, rate: 48000))
        XCTAssertThrowsError(try Timing.framesAt48k(1, rate: 0))
        XCTAssertThrowsError(try Timing.zeroPlan(target: UInt64.max, firstLeading: 2112, priorCoded: 0,
            currentLeading: 0, currentInput: 0, inputRate: 48000))
    }
    func testFenceClosesRawBeforeAnyReplacementAndAcceptsOnlyOldOutputDrain() throws {
        let policy = Timing.InputAdmission(); let raw = try XCTUnwrap(policy.token)
        try policy.fence(raw)
        XCTAssertNil(policy.token); XCTAssertFalse(policy.permits(raw, source: .raw))
        XCTAssertTrue(policy.permitsOutput(generation: raw.generation))
        let zero = try policy.installZero(initial: false)
        XCTAssertEqual(zero.generation, 2); XCTAssertTrue(policy.permits(zero, source: .zero))
        XCTAssertFalse(policy.permitsOutput(generation: raw.generation))
        XCTAssertFalse(policy.permits(raw, source: .raw))
    }
    func testInitialStandbyHasNoRawAdmissionEvenWithoutOldConverter() throws {
        let policy = Timing.InputAdmission(); let raw = try XCTUnwrap(policy.token)
        try policy.fence(raw); let zero = try policy.installZero(initial: true)
        XCTAssertEqual(zero.generation, 1); XCTAssertEqual(zero.source, .zero)
        XCTAssertFalse(policy.permits(raw, source: .raw))
    }
    func testExplicitResumeKeepsCodecGenerationAndRetiresZeroRevision() throws {
        let policy = Timing.InputAdmission(); try policy.fence(XCTUnwrap(policy.token))
        let zero = try policy.installZero(initial: false)
        let raw = try policy.resume(zero)
        XCTAssertEqual(raw.generation, zero.generation); XCTAssertGreaterThan(raw.revision, zero.revision)
        XCTAssertTrue(policy.permits(raw, source: .raw)); XCTAssertFalse(policy.permits(zero, source: .zero))
        XCTAssertThrowsError(try policy.fence(zero)); XCTAssertTrue(policy.permits(raw, source: .raw))
    }
    func testForeignOwnerCannotFenceOrResumeSuccessor() throws {
        let first = Timing.InputAdmission(); let other = Timing.InputAdmission()
        let token = try XCTUnwrap(first.token)
        XCTAssertThrowsError(try other.fence(token)); XCTAssertNotNil(other.token)
        XCTAssertThrowsError(try other.resume(token)); XCTAssertNotNil(other.token)
    }
    func testFailedPreflightNeverReopensRawOrInstallsFallback() throws {
        let policy = Timing.InputAdmission(); let raw = try XCTUnwrap(policy.token)
        try policy.fence(raw); policy.fail()
        XCTAssertNil(policy.token); XCTAssertFalse(policy.permits(raw, source: .raw))
        XCTAssertFalse(policy.permitsOutput(generation: 1))
        XCTAssertThrowsError(try policy.installZero(initial: false))
        XCTAssertTrue(policy.failed)
    }
    func testFinalEOFOwnershipIsOneWayAndIdempotent() throws {
        let policy = Timing.InputAdmission(); let raw = try XCTUnwrap(policy.token)
        policy.finish(); policy.finish()
        XCTAssertNil(policy.token); XCTAssertTrue(policy.finished)
        XCTAssertFalse(policy.permitsOutput(generation: raw.generation))
        XCTAssertThrowsError(try policy.fence(raw)); XCTAssertThrowsError(try policy.resume(raw))
    }
    func testInterleavedPCMUsesOneBufferRegardlessOfChannelCount() throws {
        for channels: UInt32 in [1, 2, 8] {
            let layout = try Timing.PCMBufferLayout(channels: channels, interleaved: true)
            XCTAssertEqual(layout.bufferCount, 1); XCTAssertEqual(layout.channelsPerBuffer, channels)
            XCTAssertEqual(try layout.byteCount(headerBytes: 24, bufferStride: 16), 24)
        }
    }
    func testPlanarPCMUsesOneBufferPerChannelWithExactABIExtent() throws {
        let stereo = try Timing.PCMBufferLayout(channels: 2, interleaved: false)
        XCTAssertEqual(stereo.bufferCount, 2); XCTAssertEqual(stereo.channelsPerBuffer, 1)
        XCTAssertEqual(try stereo.byteCount(headerBytes: 24, bufferStride: 16), 40)
        let maximum = try Timing.PCMBufferLayout(channels: 8, interleaved: false)
        XCTAssertEqual(maximum.bufferCount, 8)
        XCTAssertEqual(try maximum.byteCount(headerBytes: 24, bufferStride: 16), 136)
    }
    func testMonoPlanarAndInterleavedHaveTheSameOneBufferExtent() throws {
        for interleaved in [false, true] {
            let layout = try Timing.PCMBufferLayout(channels: 1, interleaved: interleaved)
            XCTAssertEqual(layout.bufferCount, 1); XCTAssertEqual(layout.channelsPerBuffer, 1)
            XCTAssertEqual(try layout.byteCount(headerBytes: 24, bufferStride: 16), 24)
        }
    }
    func testPCMLayoutRejectsUnsupportedCountsInvalidABIAndOverflow() throws {
        XCTAssertThrowsError(try Timing.PCMBufferLayout(channels: 0, interleaved: true))
        XCTAssertThrowsError(try Timing.PCMBufferLayout(channels: 9, interleaved: false))
        let stereo = try Timing.PCMBufferLayout(channels: 2, interleaved: false)
        XCTAssertThrowsError(try stereo.byteCount(headerBytes: 0, bufferStride: 16))
        XCTAssertThrowsError(try stereo.byteCount(headerBytes: 24, bufferStride: 0))
        XCTAssertThrowsError(try stereo.byteCount(headerBytes: Int.max, bufferStride: 16))
    }
}
