import XCTest
@testable import SapflowBroadcaster

final class AudioRecordingTimingTests: XCTestCase {
    func testNativeEOFDoesNotAcceptInputRanDryOrEmptyOutput() throws {
        var observed = AudioRecordingTiming.NativeEOFObservation(generation: 1)
        observed.deliveredInputEOS()
        for status: AudioRecordingTiming.NativeEOFObservation.OutputStatus in [.inputRanDry, .haveData, .unknown] {
            try observed.returned(status, packetCount: 0, byteLength: 0)
            XCTAssertFalse(observed.complete)
            XCTAssertEqual(observed.lastOutputStatus, status)
        }
        XCTAssertTrue(observed.inputEOSDelivered)
        XCTAssertEqual(observed.calls, 3)
    }
    func testNativeEOFRequiresInputDeliveryBeforeTerminalReturn() throws {
        var observed = AudioRecordingTiming.NativeEOFObservation(generation: 7)
        try observed.returned(.endOfStream, packetCount: 0, byteLength: 0)
        XCTAssertFalse(observed.complete)
        observed.deliveredInputEOS()
        XCTAssertFalse(observed.complete) // A later callback cannot rewrite the earlier return.
        try observed.returned(.endOfStream, packetCount: 0, byteLength: 0)
        XCTAssertTrue(observed.complete)
        XCTAssertEqual(observed.generation, 7)
    }
    func testNativeEOFRejectsTerminalDataAndError() throws {
        var observed = AudioRecordingTiming.NativeEOFObservation(generation: 1)
        observed.deliveredInputEOS()
        let malformed: [(AudioRecordingTiming.NativeEOFObservation.OutputStatus, UInt32, UInt32)] = [
            (.endOfStream, 1, 12), (.endOfStream, 0, 12), (.error, 0, 0)]
        for (status, packets, bytes) in malformed {
            try observed.returned(status, packetCount: packets, byteLength: bytes)
            XCTAssertFalse(observed.complete)
        }
        var invalid = AudioRecordingTiming.NativeEOFObservation(generation: 0)
        invalid.deliveredInputEOS()
        try invalid.returned(.endOfStream, packetCount: 0, byteLength: 0)
        XCTAssertFalse(invalid.complete)
    }
    func testNativeEOFClosesAtSixteenthReturnButNeverExceedsBound() throws {
        var observed = AudioRecordingTiming.NativeEOFObservation(generation: 2)
        observed.deliveredInputEOS()
        for _ in 0..<15 { try observed.returned(.inputRanDry, packetCount: 0, byteLength: 0) }
        XCTAssertFalse(observed.complete)
        try observed.returned(.endOfStream, packetCount: 0, byteLength: 0)
        XCTAssertTrue(observed.complete)
        XCTAssertEqual(observed.calls, 16)
        XCTAssertThrowsError(try observed.returned(.endOfStream, packetCount: 0, byteLength: 0))
        XCTAssertEqual(observed.calls, 16)
    }
    func testLeadingGroupUsesObservedValueAndHasFiniteCapacity() throws {
        XCTAssertEqual(try AudioRecordingTiming.headUnits(0), 1)
        XCTAssertEqual(try AudioRecordingTiming.headUnits(1023), 1)
        XCTAssertEqual(try AudioRecordingTiming.headUnits(1024), 2)
        XCTAssertEqual(try AudioRecordingTiming.headUnits(8191), 8)
        XCTAssertThrowsError(try AudioRecordingTiming.headUnits(8192))
        XCTAssertThrowsError(try AudioRecordingTiming.headUnits(.max))
    }
    func testCompleteExtentUsesActualInputNotEncodedPadding() throws {
        let cases: [(UInt64, UInt32, UInt32, UInt64)] = [(144000, 2112, 320, 143), (144123, 2112, 197, 143), (1024, 0, 0, 1), (100, 64, 860, 1)]
        for (input, leading, tail, units) in cases {
            let value = AudioRecordingTiming.Extent(inputFrames: input, inputRate: 48000, encodedFrames: units * 1024, leadingFrames: leading, trailingFrames: tail)
            XCTAssertEqual(try AudioRecordingTiming.validFrames(value, observedUnits: units, leadingFrames: leading), Int64(input))
        }
    }
    func testMissingOrRepeatedUnitCannotBecomeACompleteRecording() {
        let value = AudioRecordingTiming.Extent(inputFrames: 2048, inputRate: 48000, encodedFrames: 2048, leadingFrames: 0, trailingFrames: 0)
        for count: UInt64 in [0, 1, 3] { XCTAssertThrowsError(try AudioRecordingTiming.validFrames(value, observedUnits: count, leadingFrames: 0)) }
    }
    func testChangedLeadingOrImpossibleTailIsRejected() {
        for value in [AudioRecordingTiming.Extent(inputFrames: 1024, inputRate: 48000, encodedFrames: 2048, leadingFrames: 0, trailingFrames: 1024),
                      AudioRecordingTiming.Extent(inputFrames: 1024, inputRate: 48000, encodedFrames: 2048, leadingFrames: 64, trailingFrames: 960)] {
            XCTAssertThrowsError(try AudioRecordingTiming.validFrames(value, observedUnits: 2, leadingFrames: 0))
        }
    }
    func testEmptyOrAllPrimingHasNoInventedPresentation() {
        for value in [AudioRecordingTiming.Extent(inputFrames: 0, inputRate: 48000, encodedFrames: 0, leadingFrames: 0, trailingFrames: 0),
                      AudioRecordingTiming.Extent(inputFrames: 0, inputRate: 48000, encodedFrames: 1024, leadingFrames: 1024, trailingFrames: 0)] {
            XCTAssertThrowsError(try AudioRecordingTiming.validFrames(value, observedUnits: value.encodedFrames / 1024, leadingFrames: value.leadingFrames))
        }
    }
    func testInputRateConversionUsesRationalExtent() throws {
        let value = AudioRecordingTiming.Extent(inputFrames: 44100, inputRate: 44100, encodedFrames: 48128, leadingFrames: 0, trailingFrames: 128)
        XCTAssertEqual(try AudioRecordingTiming.validFrames(value, observedUnits: 47, leadingFrames: 0), 48000)
    }
    func testInvalidRatesAndOverflowFailClosed() {
        for rate: UInt32 in [0, 192001] {
            let value = AudioRecordingTiming.Extent(inputFrames: 1024, inputRate: rate, encodedFrames: 1024, leadingFrames: 0, trailingFrames: 0)
            XCTAssertThrowsError(try AudioRecordingTiming.validFrames(value, observedUnits: 1, leadingFrames: 0))
        }
        let huge = AudioRecordingTiming.Extent(inputFrames: .max, inputRate: 48000, encodedFrames: .max, leadingFrames: 0, trailingFrames: 0)
        XCTAssertThrowsError(try AudioRecordingTiming.validFrames(huge, observedUnits: .max, leadingFrames: 0))
    }
}
