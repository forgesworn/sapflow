import Foundation

/// Recording-only scalar facts. Publishing keeps the original AAC buffers and
/// timestamps. No observed device-specific priming value is a constant here.
enum AudioRecordingTiming {
    enum Failure: Error { case unsupported, overflow, inconsistent }
    enum Event {
        case began(leadingFrames: UInt32)
        case ended(Extent)
    }
    struct Extent {
        let inputFrames: UInt64
        let inputRate: UInt32
        let encodedFrames: UInt64
        let leadingFrames: UInt32
        let trailingFrames: UInt32
    }
    /// At most eight canonical AAC-LC units form the leading trim buffer.
    static func headUnits(_ leadingFrames: UInt32) throws -> Int {
        let count = Int(leadingFrames / 1024) + 1
        guard count <= 8 else { throw Failure.unsupported }
        return count
    }
    static func validFrames(_ extent: Extent, observedUnits: UInt64, leadingFrames: UInt32) throws -> Int64 {
        _ = try headUnits(leadingFrames)
        guard extent.inputRate > 0, extent.inputRate <= 192_000,
              extent.leadingFrames == leadingFrames, extent.trailingFrames < 1024 else { throw Failure.inconsistent }
        let encoded = observedUnits.multipliedReportingOverflow(by: 1024)
        let scaled = extent.inputFrames.multipliedReportingOverflow(by: 48_000)
        guard !encoded.overflow, !scaled.overflow else { throw Failure.overflow }
        let rounded = scaled.partialValue.addingReportingOverflow(UInt64(extent.inputRate / 2))
        guard !rounded.overflow else { throw Failure.overflow }
        let valid = rounded.partialValue / UInt64(extent.inputRate)
        let trim = UInt64(leadingFrames) + UInt64(extent.trailingFrames)
        guard encoded.partialValue == extent.encodedFrames, encoded.partialValue > trim,
              encoded.partialValue - trim == valid, valid <= UInt64(Int64.max) else { throw Failure.inconsistent }
        return Int64(valid)
    }
}
