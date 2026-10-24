import Foundation

/// A cold preflight estimate, not a duration limit or a storage guarantee.
enum RecordingStorage {
    enum Failure: Error { case invalidBudget }
    struct Budget: Equatable {
        let horizonSeconds: UInt64
        let payloadBytes: UInt64
        let requiredBytes: UInt64
    }
    enum State { case available, low, unavailable }
    struct Check {
        let state: State
        let budget: Budget
        let availableBytes: UInt64?
    }
    static func budget(profile: CaptureIntent, horizonSeconds: UInt64 = 60,
                       reserveBytes: UInt64 = 32 * 1024 * 1024) throws -> Budget {
        guard horizonSeconds > 0 else { throw Failure.invalidBudget }
        let (bits, overflow) = profile.elementaryBps.multipliedReportingOverflow(by: horizonSeconds)
        guard !overflow else { throw Failure.invalidBudget }
        let payload = bits / 8 + (bits % 8 == 0 ? 0 : 1)
        let headroom = max(16 * 1024 * 1024, payload / 2)
        let (withHeadroom, first) = payload.addingReportingOverflow(headroom)
        let (required, second) = withHeadroom.addingReportingOverflow(reserveBytes)
        guard !first, !second else { throw Failure.invalidBudget }
        return Budget(horizonSeconds: horizonSeconds, payloadBytes: payload, requiredBytes: required)
    }
    static let checkpointMetadataBytes: UInt64 = 256 * 1024
    /// Additional bounded slot/temp/receipt headroom. Existing budget stays
    /// unchanged for the legacy AVAssetWriter path.
    static func checkpointBudget(profile: CaptureIntent, horizonSeconds: UInt64 = 60,
                                 reserveBytes: UInt64 = 32 * 1024 * 1024) throws -> Budget {
        let base = try budget(profile: profile, horizonSeconds: horizonSeconds, reserveBytes: reserveBytes)
        let (required, overflow) = base.requiredBytes.addingReportingOverflow(checkpointMetadataBytes)
        guard !overflow else { throw Failure.invalidBudget }
        return Budget(horizonSeconds: base.horizonSeconds, payloadBytes: base.payloadBytes, requiredBytes: required)
    }
    static func check(budget: Budget, available: () throws -> UInt64?) -> Check {
        let bytes = try? available()
        let state: State = bytes.map { $0 >= budget.requiredBytes ? .available : .low } ?? .unavailable
        return Check(state: state, budget: budget, availableBytes: bytes)
    }
}
