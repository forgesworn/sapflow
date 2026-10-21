import Foundation
/// Shared by the production bridge and its cancellation regression. Native
/// cancellation wakes waitEvent before actual runtime destruction; known
/// cancellation must exit immediately, never poll that closing runtime.
final class V4vSignetCancellation {
    private let lock = NSLock()
    private var cancelled = false
    func cancel() { lock.lock(); cancelled = true; lock.unlock() }
    private func check() throws {
        lock.lock(); let stopped = cancelled; lock.unlock()
        if stopped { throw V4vFailure.closed }
    }
    func wait<Value>(read: () throws -> Value?, isClosed: () -> Bool) throws -> Value {
        while true {
            try check()
            let value = try read()
            try check()
            if let value { return value }
            guard !isClosed() else { throw V4vFailure.closed }
        }
    }
}

#if canImport(SignetAuth)
import SignetAuth

/// Actual generated FFI adapter. Its blocking waits run exclusively on the
/// login owner's cold queue. The native worker independently owns its runtime.
final class V4vNativeSignet: V4vLoginSigner {
    private let owner: NativeSignet
    private let cancellation = V4vSignetCancellation()
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
        let event = try cancellation.wait(read: { try owner.waitEvent(timeoutMs: 1_000) }, isClosed: { owner.isClosed() })
        switch event {
        case let .ready(requestId, _, _), let .proof(requestId, _):
            guard requestId == id else { throw V4vFailure.invalidContract }; return event
        case .failed: throw V4vFailure.unauthenticated
        }
    }
    func cancel() { cancellation.cancel(); owner.requestClose() }
    func waitClosed() -> Bool { (try? owner.waitClosed(timeoutMs: 2_000)) == true }
}
#endif
