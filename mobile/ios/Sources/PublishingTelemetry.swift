import Foundation

/// Bytes consumed by the local Network.framework stack, not NIC, peer or
/// decoder delivery. `complete` is false after overflow or an unresolved final
/// write; unavailable samples must never be displayed as a fresh zero rate.
struct SocketOutboundSample: Equatable {
    let completedBytes: UInt64
    let measuredAtNs: UInt64
    let available: Bool
    let complete: Bool
    var continuity: UInt64 = 0
}

/// Network-queue value state only. A ticket adds no owner, dispatch, payload
/// copy, allocation, clock read or FFI call to an existing send completion.
struct SocketCompletionCounter {
    struct Write: Equatable { fileprivate let serial: UInt64; fileprivate let bytes: UInt64 }
    enum Completion: Equatable { case matched, untracked, stale }
    private var serial: UInt64 = 0
    private var pending: Write?
    private var invalid = false
    private var bytes: UInt64 = 0
    private var sealed: SocketOutboundSample?
    init(completedBytes: UInt64 = 0) { bytes = completedBytes }
    mutating func begin(bytes count: Int) -> Write? {
        guard sealed == nil, count > 0, pending == nil, serial < UInt64.max else { invalid = true; return nil }
        serial += 1
        let write = Write(serial: serial, bytes: UInt64(count)); pending = write
        return write
    }
    @discardableResult mutating func completed(_ write: Write?, success: Bool) -> Completion {
        guard sealed == nil else { return .stale }
        // Exhausted/unavailable metrics cannot block real transport progress.
        guard let write else { return .untracked }
        guard pending == write else { return .stale }
        pending = nil
        // A failed write can have partially processed data. No partial byte
        // count is available, so the attempt's accounting remains incomplete.
        guard success else { invalid = true; return .matched }
        let sum = bytes.addingReportingOverflow(write.bytes)
        if sum.overflow { invalid = true } else { bytes = sum.partialValue }
        return .matched
    }
    func sample(nowNs: UInt64, available: Bool, final: Bool = false) -> SocketOutboundSample {
        if let sealed { return sealed }
        return SocketOutboundSample(completedBytes: bytes, measuredAtNs: nowNs, available: available,
                             complete: !invalid && (!final || pending == nil))
    }
    mutating func seal(nowNs: UInt64) -> SocketOutboundSample {
        if let sealed { return sealed }
        let final = sample(nowNs: nowNs, available: false, final: true)
        sealed = final; return final
    }
}

/// One logical destination's bounded accounting. Only the current and closing
/// contexts are retained. Final reports fold before actual-close completion;
/// repeated/stale reads and duplicate finals cannot add another copy of bytes.
struct PublishingTelemetryAggregate<Context: Equatable> {
    private struct Entry { let context: Context; var last: SocketOutboundSample? }
    private var current: Entry?
    private var closing: Entry?
    private var retired: UInt64 = 0
    private var continuity: UInt64 = 0
    private var invalid = false
    mutating func opened(_ context: Context) {
        guard current?.context != context, closing?.context != context else { return }
        if current != nil { invalid = true }
        current = Entry(context: context)
    }
    mutating func closing(_ context: Context) {
        guard current?.context == context else { return }
        if closing != nil { invalid = true }
        closing = current; current = nil
    }
    mutating func finished(_ context: Context, sample: SocketOutboundSample?) {
        let old: Entry
        if closing?.context == context { old = closing!; closing = nil }
        else if current?.context == context { old = current!; current = nil }
        else { return }
        let final = sample ?? old.last
        if sample?.complete != true { breakContinuity() }
        if let final {
            if let last = old.last, final.completedBytes < last.completedBytes || final.measuredAtNs < last.measuredAtNs { invalid = true }
            let sum = retired.addingReportingOverflow(final.completedBytes)
            if sum.overflow { invalid = true } else { retired = sum.partialValue }
        }
    }
    mutating func read(_ context: Context, sample: SocketOutboundSample?) -> SocketOutboundSample? {
        guard var value = current, value.context == context, let sample else { return nil }
        if let last = value.last, sample.completedBytes < last.completedBytes || sample.measuredAtNs < last.measuredAtNs { invalid = true }
        value.last = sample; current = value
        let withCurrent = retired.addingReportingOverflow(sample.completedBytes)
        let total = withCurrent.partialValue.addingReportingOverflow(closing?.last?.completedBytes ?? 0)
        if withCurrent.overflow || total.overflow { invalid = true }
        return SocketOutboundSample(completedBytes: total.partialValue, measuredAtNs: sample.measuredAtNs,
            available: sample.available && closing == nil, complete: sample.complete && !invalid, continuity: continuity)
    }
    private mutating func breakContinuity() {
        let next = continuity.addingReportingOverflow(1)
        if next.overflow { invalid = true } else { continuity = next.partialValue }
    }
}

struct SocketOutboundRate {
    static let freshnessNs: UInt64 = 3_000_000_000
    private var previous: SocketOutboundSample?
    private(set) var kilobitsPerSecond: Double?
    private var lastReadNs: UInt64?
    mutating func update(_ sample: SocketOutboundSample?, nowNs: UInt64) {
        guard let sample, sample.available, sample.complete, nowNs >= sample.measuredAtNs,
              nowNs - sample.measuredAtNs <= Self.freshnessNs else { clear(); return }
        lastReadNs = sample.measuredAtNs
        defer { previous = sample }
        guard let old = previous, old.continuity == sample.continuity,
              sample.measuredAtNs > old.measuredAtNs, sample.completedBytes >= old.completedBytes else {
            kilobitsPerSecond = nil; return
        }
        let elapsed = sample.measuredAtNs - old.measuredAtNs
        guard elapsed <= Self.freshnessNs else { kilobitsPerSecond = nil; return }
        let value = Double(sample.completedBytes - old.completedBytes) * 8_000_000 / Double(elapsed)
        kilobitsPerSecond = value.isFinite ? value : nil
    }
    mutating func expire(nowNs: UInt64) {
        guard let lastReadNs, nowNs >= lastReadNs, nowNs - lastReadNs <= Self.freshnessNs else { clear(); return }
    }
    private mutating func clear() { previous = nil; lastReadNs = nil; kilobitsPerSecond = nil }
}

struct CaptureElapsedSnapshot: Equatable {
    let captureNs: UInt64?
    let broadcastNs: UInt64?
    let captureRunning: Bool
    let broadcastRunning: Bool
}

/// A blocked socket queue retains one request, not another per timer firing.
/// Retired answers cannot release a successor's outstanding read.
struct TelemetryReadAdmission<Context: Equatable> {
    struct Ticket: Equatable { fileprivate let serial: UInt64; let context: Context }
    private var serial: UInt64 = 0
    private var pending: Ticket?
    mutating func begin(_ context: Context) -> Ticket? {
        guard pending == nil, serial < UInt64.max else { return nil }
        serial += 1
        let ticket = Ticket(serial: serial, context: context); pending = ticket; return ticket
    }
    mutating func complete(_ ticket: Ticket) -> Bool {
        guard pending == ticket else { return false }; pending = nil; return true
    }
    mutating func invalidate() { pending = nil }
}
