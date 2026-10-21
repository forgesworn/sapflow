import Foundation
#if canImport(SignetAuth)
import SignetAuth

/// Actual generated FFI adapter. Its blocking waits run exclusively on the
/// login owner's cold queue. The native worker independently owns its runtime.
final class V4vNativeSignet: V4vLoginSigner {
    private let owner: NativeSignet
    init(bunkerURI: String, restoredCredential: Data?, remainingMs: UInt32) throws {
        if let restoredCredential {
            owner = try NativeSignet.restore(credential: SignetCredential.restore(bytes: restoredCredential), deadlineMs: remainingMs)
        } else { owner = try NativeSignet.pairBunker(uri: bunkerURI, deadlineMs: remainingMs) }
    }
    func connect() throws -> (persona: String, credential: Data) {
        try owner.connect(requestId: 1)
        switch try wait(id: 1) {
        case let .ready(_, persona, credential): return (persona, credential.encoded())
        default: throw V4vFailure.invalidContract
        }
    }
    func sign(_ template: V4vLoginTemplate) throws -> Data {
        try owner.signLogin(requestId: 2, challenge: template.challenge.challenge, origin: template.origin, appName: template.challenge.appName, createdAtSeconds: template.createdAt)
        switch try wait(id: 2) {
        case let .proof(_, bytes): return bytes
        default: throw V4vFailure.invalidContract
        }
    }
    private func wait(id: UInt64) throws -> SignetEvent {
        while true {
            if let event = try owner.waitEvent(timeoutMs: 1_000) {
                switch event {
                case let .ready(requestId, _, _), let .proof(requestId, _):
                    guard requestId == id else { throw V4vFailure.invalidContract }; return event
                case .failed: throw V4vFailure.unauthenticated
                }
            }
            guard !owner.isClosed() else { throw V4vFailure.closed }
        }
    }
    func cancel() { owner.requestClose() }
    func waitClosed() -> Bool { (try? owner.waitClosed(timeoutMs: 2_000)) == true }
}
#endif
