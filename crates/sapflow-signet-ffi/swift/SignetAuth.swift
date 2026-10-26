import Foundation

#if canImport(SignetAuthFFI)
import SignetAuthFFI
#endif

public enum SignetError: Swift.Error, Equatable, Hashable, Foundation.LocalizedError {
    case InvalidInput
    case InvalidCredential
    case InvalidProof
    case Busy
    case Deadline
    case Cancelled
    case Refused
    case Unavailable
    case Closed

    public var errorDescription: String? {
        String(reflecting: self)
    }
}

#if compiler(>=6)
extension SignetError: Sendable {}
#endif

private let signetAbiIsCompatible = sapflow_signet_abi_version() == SAPFLOW_SIGNET_ABI_VERSION

private func signetError(_ code: Int32) -> SignetError {
    switch code {
    case 1: return .InvalidInput
    case 2: return .InvalidCredential
    case 3: return .InvalidProof
    case 4: return .Busy
    case 5: return .Deadline
    case 6: return .Cancelled
    case 7: return .Refused
    case 8: return .Unavailable
    case 9: return .Closed
    default: return .Unavailable
    }
}

private func check(_ status: Int32) throws {
    precondition(signetAbiIsCompatible, "Signet native/library ABI mismatch")
    if status != 0 { throw signetError(status) }
}

private func take(_ buffer: SapflowSignetBuffer) -> Data {
    guard let ptr = buffer.ptr, buffer.len > 0 else {
        sapflow_signet_buffer_free(buffer)
        return Data()
    }
    return Data(bytesNoCopy: ptr, count: buffer.len, deallocator: .custom { ptr, len in
        sapflow_signet_buffer_free(SapflowSignetBuffer(ptr: ptr.assumingMemoryBound(to: UInt8.self), len: len))
    })
}

private func takeSecret(_ buffer: SapflowSignetBuffer) -> Data {
    guard let ptr = buffer.ptr, buffer.len > 0 else {
        sapflow_signet_secret_free(buffer)
        return Data()
    }
    return Data(bytesNoCopy: ptr, count: buffer.len, deallocator: .custom { ptr, len in
        sapflow_signet_secret_free(SapflowSignetBuffer(ptr: ptr.assumingMemoryBound(to: UInt8.self), len: len))
    })
}

private func withData<R>(_ data: Data, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try data.withUnsafeBytes { raw in
        try body(raw.baseAddress?.assumingMemoryBound(to: UInt8.self), raw.count)
    }
}

private func handle(_ body: (UnsafeMutablePointer<UnsafeMutableRawPointer?>) -> Int32) throws -> UnsafeMutableRawPointer {
    var out: UnsafeMutableRawPointer?
    try check(body(&out))
    guard let out else { throw SignetError.Unavailable }
    return out
}

private func boolResult(_ body: (UnsafeMutablePointer<Bool>) -> Int32) throws -> Bool {
    var out = false
    try check(body(&out))
    return out
}

public protocol SignetCredentialProtocol: AnyObject, Sendable {
    func encoded() -> Data
}

open class SignetCredential: SignetCredentialProtocol, @unchecked Sendable {
    fileprivate let raw: UnsafeMutableRawPointer

    fileprivate init(raw: UnsafeMutableRawPointer) {
        self.raw = raw
    }

    public static func restore(bytes: Data) throws -> SignetCredential {
        let raw = try withData(bytes) { input, len in
            try handle { out in sapflow_signet_credential_restore(input, len, out) }
        }
        return SignetCredential(raw: raw)
    }

    deinit { sapflow_signet_credential_free(raw) }

    open func encoded() -> Data {
        var out = SapflowSignetBuffer()
        try! check(sapflow_signet_credential_encoded(raw, &out))
        return takeSecret(out)
    }
}

public enum SignetEvent {
    case ready(requestId: UInt64, persona: String, credential: SignetCredential)
    case proof(requestId: UInt64, json: Data)
    case failed(requestId: UInt64, error: SignetError)
}

#if compiler(>=6)
extension SignetEvent: Sendable {}
#endif

public protocol NativeSignetProtocol: AnyObject, Sendable {
    func connect(requestId: UInt64) throws
    func isClosed() -> Bool
    func requestClose()
    func signLogin(requestId: UInt64, challenge: String, origin: String, appName: String, createdAtSeconds: UInt64) throws
    func waitClosed(timeoutMs: UInt32) throws -> Bool
    func waitEvent(timeoutMs: UInt32) throws -> SignetEvent?
}

open class NativeSignet: NativeSignetProtocol, @unchecked Sendable {
    private let raw: UnsafeMutableRawPointer

    private init(raw: UnsafeMutableRawPointer) {
        self.raw = raw
    }

    public static func pairBunker(uri: String, deadlineMs: UInt32) throws -> NativeSignet {
        let raw = try withData(Data(uri.utf8)) { uri, len in
            try handle { out in sapflow_signet_native_pair_bunker(uri, len, deadlineMs, out) }
        }
        return NativeSignet(raw: raw)
    }

    public static func restore(credential: SignetCredential, deadlineMs: UInt32) throws -> NativeSignet {
        NativeSignet(raw: try handle { out in
            sapflow_signet_native_restore(credential.raw, deadlineMs, out)
        })
    }

    deinit { sapflow_signet_native_free(raw) }

    open func connect(requestId: UInt64) throws {
        try check(sapflow_signet_native_connect(raw, requestId))
    }

    open func isClosed() -> Bool {
        try! boolResult { out in sapflow_signet_native_is_closed(raw, out) }
    }

    open func requestClose() {
        try! check(sapflow_signet_native_request_close(raw))
    }

    open func signLogin(
        requestId: UInt64,
        challenge: String,
        origin: String,
        appName: String,
        createdAtSeconds: UInt64
    ) throws {
        try withData(Data(challenge.utf8)) { challenge, challengeLen in
            try withData(Data(origin.utf8)) { origin, originLen in
                try withData(Data(appName.utf8)) { appName, appNameLen in
                    try check(sapflow_signet_native_sign_login(
                        raw, requestId,
                        challenge, challengeLen,
                        origin, originLen,
                        appName, appNameLen,
                        createdAtSeconds
                    ))
                }
            }
        }
    }

    open func waitClosed(timeoutMs: UInt32) throws -> Bool {
        try boolResult { out in sapflow_signet_native_wait_closed(raw, timeoutMs, out) }
    }

    open func waitEvent(timeoutMs: UInt32) throws -> SignetEvent? {
        var out = SapflowSignetEvent()
        try check(sapflow_signet_native_wait_event(raw, timeoutMs, &out))
        switch out.tag {
        case UInt32(SAPFLOW_SIGNET_EVENT_NONE):
            sapflow_signet_event_free(out)
            return nil
        case UInt32(SAPFLOW_SIGNET_EVENT_READY):
            let persona = String(decoding: take(out.bytes), as: UTF8.self)
            out.bytes = SapflowSignetBuffer()
            guard let credential = out.credential else {
                sapflow_signet_event_free(out)
                throw SignetError.Unavailable
            }
            out.credential = nil
            sapflow_signet_event_free(out)
            return .ready(
                requestId: out.request_id,
                persona: persona,
                credential: SignetCredential(raw: credential)
            )
        case UInt32(SAPFLOW_SIGNET_EVENT_PROOF):
            let json = take(out.bytes)
            out.bytes = SapflowSignetBuffer()
            sapflow_signet_event_free(out)
            return .proof(requestId: out.request_id, json: json)
        case UInt32(SAPFLOW_SIGNET_EVENT_FAILED):
            let error = signetError(out.error)
            sapflow_signet_event_free(out)
            return .failed(requestId: out.request_id, error: error)
        default:
            sapflow_signet_event_free(out)
            throw SignetError.Unavailable
        }
    }
}
