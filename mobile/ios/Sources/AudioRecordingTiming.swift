import Foundation

/// Recording-only scalar facts. Publishing keeps the original AAC buffers and
/// timestamps. No observed device-specific priming value is a constant here.
enum AudioRecordingTiming {
    enum Failure: Error { case unsupported, overflow, inconsistent, stale, closed }
    /// Cold observations from the real converter drain only. An empty buffer
    /// or temporary lack of input cannot stand in for its terminal status.
    struct NativeEOFObservation: Equatable {
        enum OutputStatus: String { case haveData, inputRanDry, endOfStream, error, unknown }
        let generation: UInt64
        private(set) var inputEOSDelivered = false
        private(set) var calls = 0
        private(set) var lastOutputStatus: OutputStatus?
        private(set) var lastPacketCount: UInt32 = 0
        private(set) var lastByteLength: UInt32 = 0
        private var terminalReturnHadInputEOS = false
        init(generation: UInt64) { self.generation = generation }
        mutating func deliveredInputEOS() { inputEOSDelivered = true }
        mutating func returned(_ status: OutputStatus, packetCount: UInt32, byteLength: UInt32) throws {
            guard calls < 16 else { throw Failure.unsupported }
            calls += 1; lastOutputStatus = status
            lastPacketCount = packetCount; lastByteLength = byteLength
            terminalReturnHadInputEOS = inputEOSDelivered
        }
        var complete: Bool {
            generation > 0 && terminalReturnHadInputEOS && lastOutputStatus == .endOfStream &&
                lastPacketCount == 0 && lastByteLength == 0
        }
    }
    enum Event {
        case began(leadingFrames: UInt32)
        case ended(Extent)
        case generationEnded(GenerationExtent)
        case endedGenerations(FileExtent)
        case failed
    }
    struct Extent {
        let inputFrames: UInt64
        let inputRate: UInt32
        let encodedFrames: UInt64
        let leadingFrames: UInt32
        let trailingFrames: UInt32
        let sourceMapping: SourceFrameMapping
        init(inputFrames: UInt64, inputRate: UInt32, encodedFrames: UInt64, leadingFrames: UInt32,
             trailingFrames: UInt32, sourceMapping: SourceFrameMapping = .nearest) {
            self.inputFrames = inputFrames; self.inputRate = inputRate; self.encodedFrames = encodedFrames
            self.leadingFrames = leadingFrames; self.trailingFrames = trailingFrames; self.sourceMapping = sourceMapping
        }
    }
    /// Existing callers retain nearest-frame mapping. A native resampling
    /// generation explicitly records its completed (floor) output sample clock.
    enum SourceFrameMapping: String { case nearest, floor }
    struct SourceFrameExtent: Equatable {
        let frames: UInt64
        /// Signed output-frame error against the exact rational source extent.
        /// This is a declared fraction, never extra accepted source frames.
        let quantizationNumerator: Int64
        let quantizationDenominator: UInt32
    }
    enum InputSource: Equatable { case raw, zero }
    /// AudioBufferList describes planes, not channels. In particular stereo
    /// interleaved PCM has one buffer. CoreMedia receives the exact ABI size.
    struct PCMBufferLayout {
        let bufferCount: Int
        let channelsPerBuffer: UInt32
        init(channels: UInt32, interleaved: Bool) throws {
            guard (1...8).contains(channels) else { throw Failure.unsupported }
            bufferCount = interleaved ? 1 : Int(channels)
            channelsPerBuffer = interleaved ? channels : 1
        }
        func byteCount(headerBytes: Int, bufferStride: Int) throws -> Int {
            guard headerBytes > 0, bufferStride > 0 else { throw Failure.inconsistent }
            let extra = (bufferCount - 1).multipliedReportingOverflow(by: bufferStride)
            let total = headerBytes.addingReportingOverflow(extra.partialValue)
            guard !extra.overflow, !total.overflow else { throw Failure.overflow }
            return total.partialValue
        }
    }
    fileprivate final class InputIdentity {}
    struct InputToken: Equatable {
        fileprivate let owner: InputIdentity
        let generation: UInt64
        let revision: UInt64
        let source: InputSource
        static func == (a: Self, b: Self) -> Bool {
            a.owner === b.owner && a.generation == b.generation && a.revision == b.revision && a.source == b.source
        }
    }
    /// Serial native-owner policy. Tokens retain their unique owner identity;
    /// neither a new encoder nor a later raw/zero revision can reuse old work.
    final class InputAdmission {
        private let identity = InputIdentity()
        private var revision: UInt64 = 1
        private var generation: UInt64 = 1
        private var active: InputSource? = .raw
        private var closing: InputToken?
        private(set) var failed = false
        private(set) var finished = false
        var token: InputToken? {
            guard !failed, !finished, let active else { return nil }
            return InputToken(owner: identity, generation: generation, revision: revision, source: active)
        }
        func permits(_ token: InputToken, source: InputSource) -> Bool { self.token == token && token.source == source }
        func fence(_ token: InputToken) throws {
            guard self.token == token else { throw Failure.stale }
            active = nil; closing = token
        }
        func permitsOutput(generation: UInt64) -> Bool {
            !failed && !finished && (token?.generation == generation || closing?.generation == generation)
        }
        func installZero(initial: Bool) throws -> InputToken {
            guard !failed, !finished, active == nil, closing != nil else { throw Failure.closed }
            guard revision < UInt64.max, initial || generation < UInt64.max else { fail(); throw Failure.overflow }
            if !initial { generation += 1 }
            revision += 1; closing = nil; active = .zero
            return token!
        }
        func resume(_ token: InputToken) throws -> InputToken {
            guard permits(token, source: .zero) else { throw Failure.stale }
            guard revision < UInt64.max else { fail(); throw Failure.overflow }
            revision += 1; active = .raw; return self.token!
        }
        func fail() { failed = true; active = nil; closing = nil }
        func finish() { finished = true; active = nil; closing = nil }
    }
    struct GenerationExtent {
        let generation: UInt64
        let inputFrames: UInt64
        let rawFrames: UInt64
        let zeroFrames: UInt64
        let inputRate: UInt32
        let encodedFrames: UInt64
        /// Actual native prime facts. A generation is supported only when these
        /// close its measured 48 kHz output extent, never by guessing a scale.
        let leadingFrames: UInt32
        let trailingFrames: UInt32
        let sourceMapping: SourceFrameMapping
        init(generation: UInt64, inputFrames: UInt64, rawFrames: UInt64, zeroFrames: UInt64,
             inputRate: UInt32, encodedFrames: UInt64, leadingFrames: UInt32, trailingFrames: UInt32,
             sourceMapping: SourceFrameMapping = .nearest) {
            self.generation = generation; self.inputFrames = inputFrames; self.rawFrames = rawFrames
            self.zeroFrames = zeroFrames; self.inputRate = inputRate; self.encodedFrames = encodedFrames
            self.leadingFrames = leadingFrames; self.trailingFrames = trailingFrames; self.sourceMapping = sourceMapping
        }
        var sourceExtent: SourceFrameExtent? {
            try? AudioRecordingTiming.sourceFrameExtent(inputFrames, rate: inputRate, mapping: sourceMapping)
        }
    }
    struct FileExtent: Equatable {
        let generationCount: UInt64
        let sourceFramesAt48k: UInt64
        let intentionalStandbyFrames: UInt64
        let encodedFrames: UInt64
        let leadingFrames: UInt32
        let trailingFrames: UInt32
    }
    struct LedgerSnapshot {
        let completed: FileExtent?
        let lastGeneration: GenerationExtent?
        /// Diagnostic only, never accepted as a completed/file extent.
        let lastAttemptedGeneration: GenerationExtent?
        let activeGeneration: UInt64?
        let activeInputFrames: UInt64
        let activeRawFrames: UInt64
        let activeZeroFrames: UInt64
        let activeInputRate: UInt32
        let activeEncodedFrames: UInt64
        let nativeLeadingFrames: UInt32?
        let failed: Bool
        let finished: Bool
    }
    /// Constant-space receipts. Completed generations are folded into one
    /// checked aggregate, retaining only the last exact native receipt.
    struct Ledger {
        private(set) var extent: FileExtent?
        private(set) var last: GenerationExtent?
        mutating func append(_ value: GenerationExtent) throws {
            let priorCount = extent?.generationCount ?? 0
            guard priorCount < UInt64.max, value.generation == priorCount + 1 else { throw Failure.inconsistent }
            let inputs = try add(value.rawFrames, value.zeroFrames)
            guard inputs == value.inputFrames, value.encodedFrames > 0, value.encodedFrames % 1024 == 0 else { throw Failure.inconsistent }
            let valid = try sourceFrameExtent(value.inputFrames, rate: value.inputRate, mapping: value.sourceMapping).frames
            _ = try headUnits(value.leadingFrames)
            guard value.trailingFrames < 1024,
                  try add(valid, UInt64(value.leadingFrames) + UInt64(value.trailingFrames)) == value.encodedFrames else {
                throw value.inputRate == 48_000 ? Failure.inconsistent : Failure.unsupported
            }
            let interior = extent.map { UInt64($0.trailingFrames) + UInt64(value.leadingFrames) } ?? 0
            let next = FileExtent(generationCount: priorCount + 1,
                sourceFramesAt48k: try add(extent?.sourceFramesAt48k ?? 0, valid),
                intentionalStandbyFrames: try add(extent?.intentionalStandbyFrames ?? 0, interior),
                encodedFrames: try add(extent?.encodedFrames ?? 0, value.encodedFrames),
                leadingFrames: extent?.leadingFrames ?? value.leadingFrames, trailingFrames: value.trailingFrames)
            _ = try validFrames(next, observedUnits: next.encodedFrames / 1024, leadingFrames: next.leadingFrames)
            extent = next; last = value
        }
    }
    struct ZeroPlan {
        let additionalSourceFrames: UInt64
        let requestedValidFrames: UInt64
        let attainableValidFrames: UInt64
        let quantizationOutputFrames: Int64
        let sourceExtent: SourceFrameExtent
        let sourceMapping: SourceFrameMapping
    }
    static func zeroPlan(target: UInt64, firstLeading: UInt32, priorCoded: UInt64,
                         currentLeading: UInt32, currentInput: UInt64, inputRate: UInt32,
                         sourceMapping: SourceFrameMapping = .nearest) throws -> ZeroPlan {
        guard target <= UInt64(Int64.max), inputRate > 0, inputRate <= 192_000 else { throw Failure.unsupported }
        let total = try add(target, UInt64(firstLeading))
        let overhead = try add(priorCoded, UInt64(currentLeading))
        guard total >= overhead else { throw Failure.unsupported }
        let outputFrames = total - overhead
        let scaled = outputFrames.multipliedReportingOverflow(by: UInt64(inputRate))
        guard !scaled.overflow else { throw Failure.overflow }
        // Floor SRC cannot round a future source sample into the requested
        // output slot. Choose the first source count reaching the target and
        // report any unattainable output-frame fraction explicitly.
        let desiredInput = try add(scaled.partialValue, sourceMapping == .floor ? 47_999 : 24_000) / 48_000
        guard desiredInput >= currentInput else { throw Failure.unsupported }
        let sourceExtent = try sourceFrameExtent(desiredInput, rate: inputRate, mapping: sourceMapping)
        let represented = sourceExtent.frames
        let codedWithoutTail = try add(overhead, represented)
        guard codedWithoutTail >= UInt64(firstLeading) else { throw Failure.inconsistent }
        let attainable = codedWithoutTail - UInt64(firstLeading)
        guard attainable <= UInt64(Int64.max) else { throw Failure.overflow }
        return ZeroPlan(additionalSourceFrames: desiredInput - currentInput, requestedValidFrames: target,
            attainableValidFrames: attainable, quantizationOutputFrames: Int64(attainable) - Int64(target),
            sourceExtent: sourceExtent, sourceMapping: sourceMapping)
    }
    private static func add(_ a: UInt64, _ b: UInt64) throws -> UInt64 {
        let next = a.addingReportingOverflow(b); guard !next.overflow else { throw Failure.overflow }; return next.partialValue
    }
    static func framesAt48k(_ input: UInt64, rate: UInt32) throws -> UInt64 {
        try sourceFrameExtent(input, rate: rate, mapping: .nearest).frames
    }
    static func sourceFrameExtent(_ input: UInt64, rate: UInt32, mapping: SourceFrameMapping) throws -> SourceFrameExtent {
        guard rate > 0, rate <= 192_000 else { throw Failure.unsupported }
        let scaled = input.multipliedReportingOverflow(by: 48_000)
        guard !scaled.overflow else { throw Failure.overflow }
        let divisor = UInt64(rate)
        let whole = scaled.partialValue / divisor, remainder = scaled.partialValue % divisor
        let roundUp = mapping == .nearest && remainder >= (divisor + 1) / 2
        let represented = roundUp ? try add(whole, 1) : whole
        let error = roundUp ? Int64(divisor - remainder) : -Int64(remainder)
        return SourceFrameExtent(frames: represented, quantizationNumerator: error, quantizationDenominator: rate)
    }
    static func validFrames(_ extent: FileExtent, observedUnits: UInt64, leadingFrames: UInt32) throws -> Int64 {
        guard extent.generationCount > 0, extent.leadingFrames == leadingFrames, extent.trailingFrames < 1024 else { throw Failure.inconsistent }
        _ = try headUnits(leadingFrames)
        let coded = observedUnits.multipliedReportingOverflow(by: 1024)
        guard !coded.overflow else { throw Failure.overflow }
        let trims = UInt64(leadingFrames) + UInt64(extent.trailingFrames)
        let valid = try add(extent.sourceFramesAt48k, extent.intentionalStandbyFrames)
        guard coded.partialValue == extent.encodedFrames, extent.encodedFrames > trims,
              extent.encodedFrames - trims == valid, valid <= UInt64(Int64.max) else { throw Failure.inconsistent }
        return Int64(valid)
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
        guard !encoded.overflow else { throw Failure.overflow }
        let valid = try sourceFrameExtent(extent.inputFrames, rate: extent.inputRate, mapping: extent.sourceMapping).frames
        let trim = UInt64(leadingFrames) + UInt64(extent.trailingFrames)
        guard encoded.partialValue == extent.encodedFrames, encoded.partialValue > trim,
              encoded.partialValue - trim == valid, valid <= UInt64(Int64.max) else { throw Failure.inconsistent }
        return Int64(valid)
    }
}
