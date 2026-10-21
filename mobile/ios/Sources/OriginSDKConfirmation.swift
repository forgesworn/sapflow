import Sapflow

/// The production SDK boundary uses the identity captured before transport
/// construction. No latest snapshot can substitute a successor attempt.
final class OriginSDKConfirmation {
    private let session: NativeSession
    init(session: NativeSession) { self.session = session }
    func apply(_ captured: OriginAttemptIdentity, _ nowNs: UInt64) throws {
        try apply(generation: captured.sdkGeneration, attemptEpoch: captured.sdkAttempt, nowNs: nowNs)
    }
    func apply(generation: UInt64, attemptEpoch: UInt64, nowNs: UInt64) throws {
        try session.mediaConfirmedForAttempt(generation: generation, attemptEpoch: attemptEpoch, nowNs: nowNs)
    }
}
