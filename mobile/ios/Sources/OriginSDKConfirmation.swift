import Sapflow

/// The production SDK boundary uses the identity captured before transport
/// construction. No latest snapshot can substitute a successor attempt.
final class OriginSDKConfirmation {
    private let session: NativeSession
    init(session: NativeSession) { self.session = session }
    func apply(_ captured: OriginAttemptIdentity, _ nowNs: UInt64) throws {
        try session.mediaConfirmedForAttempt(generation: captured.sdkGeneration, attemptEpoch: captured.sdkAttempt, nowNs: nowNs)
    }
}
