import Foundation
import Security

enum OriginObservationFailure: String, Error, CustomStringConvertible {
    case scope, unavailable, bounds, cancelled, deadline, busy, unsupported
    case network, decode, progress, cleanup
    var description: String { "Origin observation unconfirmed (\(rawValue))" }
}

/// Immutable local ownership, captured before constructing a transport. None
/// of these fields may be reconstructed from a journal or a server reply.
struct OriginAttemptIdentity: Equatable {
    let destination: UInt64, codecEpoch: UInt64, token: UInt64
    let sdkGeneration: UInt64, sdkAttempt: UInt64
    init(destination: UInt64, codecEpoch: UInt64, token: UInt64, sdkGeneration: UInt64, sdkAttempt: UInt64) throws {
        guard destination > 0, codecEpoch > 0, token > 0, sdkGeneration > 0, sdkAttempt > 0 else { throw OriginObservationFailure.scope }
        self.destination = destination; self.codecEpoch = codecEpoch; self.token = token
        self.sdkGeneration = sdkGeneration; self.sdkAttempt = sdkAttempt
    }
}

struct V4vObservedAttempt: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let local: OriginAttemptIdentity
    let identity: V4vIdentity
    let policyDigest: String
    let nonce: String
    init(local: OriginAttemptIdentity, grant: V4vGrant, nonce: String) throws {
        _ = try V4vIdentity.id(nonce)
        self.local = local; identity = grant.identity; policyDigest = grant.policyDigest; self.nonce = nonce
    }
    static func randomNonce() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { throw OriginObservationFailure.unavailable }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }
    func wireStreamID(grant: V4vGrant) throws -> String {
        guard identity.sameSession(grant.identity), grant.identity.revision >= identity.revision, policyDigest == grant.policyDigest else { throw OriginObservationFailure.scope }
        let prefix = "publish:live/\(identity.eventId):\(identity.sessionId):"
        guard grant.streamId.hasPrefix(prefix) else { throw OriginObservationFailure.scope }
        let password = String(grant.streamId.dropFirst(prefix.count))
        guard password.range(of: #"^[A-Za-z0-9_-]{43}\z"#, options: .regularExpression) != nil else { throw OriginObservationFailure.scope }
        return "#!::m=publish,r=live/\(identity.eventId),u=\(identity.sessionId),s=\(password),forgesworn_attempt=\(nonce)"
    }
    var description: String { "Captured V4V observation attempt (redacted)" }
    var debugDescription: String { description }
}

/// Mutable authority is supplied by the serial destination owner, never by a
/// decoder callback. Revisions may advance only while immutable grant policy
/// and local transport ownership remain unchanged.
struct OriginCurrentAuthority {
    let attempt: V4vObservedAttempt
    let grant: V4vGrant
    let expiresAtNs: UInt64
    func permits(_ captured: V4vObservedAttempt, nowNs: UInt64) -> Bool {
        attempt == captured && captured.identity.sameSession(grant.identity) && grant.identity.revision >= captured.identity.revision &&
            grant.policyDigest == captured.policyDigest && nowNs < expiresAtNs
    }
}

struct OriginMediaSelection: Equatable, CustomStringConvertible {
    let epoch: UInt64, initialization: UInt64, part: UInt64
    let initPath: String, partPath: String
    private init(epoch: UInt64, initialization: UInt64, part: UInt64, initPath: String, partPath: String) {
        self.epoch = epoch; self.initialization = initialization; self.part = part; self.initPath = initPath; self.partPath = partPath
    }
    static func match(_ receipt: V4vSessionReceipt, attempt: V4vObservedAttempt, nowMs: UInt64) throws -> Self {
        guard receipt.identity.sameSession(attempt.identity), receipt.identity.revision >= attempt.identity.revision,
              receipt.phase == "active", receipt.expiresMs > nowMs else { throw OriginObservationFailure.scope }
        guard let origin = receipt.origin else { throw OriginObservationFailure.unavailable }
        guard origin.identity == receipt.identity, origin.admission == "open", origin.expiresMs == receipt.expiresMs else { throw OriginObservationFailure.scope }
        guard origin.state == "live", origin.proto == "active", origin.attempt == attempt.nonce, let media = origin.freshMedia,
              media.connectionEpoch == origin.epoch, media.hasAudio else { throw OriginObservationFailure.unavailable }
        return Self(epoch: origin.epoch, initialization: media.initializationId, part: media.partId, initPath: media.initPath, partPath: media.partPath)
    }
    func stillCurrent(_ newer: Self) -> Bool {
        epoch == newer.epoch && initialization == newer.initialization && initPath == newer.initPath && newer.part >= part
    }
    var description: String { "Correlated origin resources (not decoded)" }
}

struct OriginApprovedResources: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let initialization: URL, part: URL
    init(profile: V4vConnectorProfile, media: OriginMediaSelection) throws {
        func mapped(_ path: String) throws -> URL {
            // playbackPrefix replaces the canonical live-directory prefix.
            // It is not a URL base and cannot be replaced by a server URL.
            guard path.hasPrefix("/live/"), let url = URL(string: profile.playbackOrigin + profile.playbackPrefix + String(path.dropFirst(6))),
                  let u = URLComponents(url: url, resolvingAgainstBaseURL: false), u.scheme == "https",
                  u.user == nil, u.password == nil, u.query == nil, u.fragment == nil,
                  u.percentEncodedPath == profile.playbackPrefix + String(path.dropFirst(6)) else { throw OriginObservationFailure.scope }
            return url
        }
        initialization = try mapped(media.initPath); part = try mapped(media.partPath)
    }
    func request(_ url: URL, timeout: TimeInterval) throws -> URLRequest {
        guard url == initialization || url == part, timeout > 0, timeout <= 8 else { throw OriginObservationFailure.scope }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: timeout)
        request.httpMethod = "GET"; request.httpShouldHandleCookies = false
        request.setValue("video/mp4, application/mp4", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        return request // No bearer, cookies, arbitrary headers or query parameters.
    }
    var description: String { "Approved origin resources (redacted)" }
    var debugDescription: String { description }
}

struct OriginMediaBody {
    static let initLimit = 256 * 1024
    static let totalLimit = 8 * 1024 * 1024
    let limit: Int
    private(set) var bytes = Data()
    init(limit: Int) throws {
        guard limit > 0, limit <= Self.totalLimit else { throw OriginObservationFailure.bounds }; self.limit = limit
    }
    static func response(status: Int, length: Int64, mime: String?, limit: Int) throws {
        guard status == 200, ["video/mp4", "application/mp4"].contains(mime?.lowercased() ?? "") else { throw OriginObservationFailure.network }
        guard length >= -1, length <= limit else { throw OriginObservationFailure.bounds }
    }
    mutating func append(_ data: Data) throws {
        guard data.count <= limit - bytes.count else { throw OriginObservationFailure.bounds }; bytes.append(data)
    }
}

/// The same cold lock must order Stop/End/expiry/transport-failure/codec-close
/// against final SDK confirmation. No socket, storage, decode, close/join or
/// notification callback belongs inside it.
final class OriginConfirmationGate: @unchecked Sendable {
    private let lock = NSLock()
    private var authority: OriginCurrentAuthority?
    private var closed = false
    func replace(_ next: OriginCurrentAuthority) throws {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { throw OriginObservationFailure.cancelled }; authority = next
    }
    func invalidate() { lock.lock(); authority = nil; lock.unlock() }
    func invalidate(attempt: V4vObservedAttempt) {
        lock.lock(); defer { lock.unlock() }
        if authority?.attempt == attempt { authority = nil }
    }
    func close() { lock.lock(); closed = true; authority = nil; lock.unlock() }
    func permits(_ attempt: V4vObservedAttempt, nowNs: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }; return !closed && authority?.permits(attempt, nowNs: nowNs) == true
    }
    func confirm(_ attempt: V4vObservedAttempt, beforeNs: UInt64, clock: () -> UInt64,
                 apply: (OriginAttemptIdentity, UInt64) throws -> Void) throws {
        lock.lock(); defer { lock.unlock() }
        // The lock may have waited across expiry. Sample after acquisition,
        // including the original observation and bearer-permit deadline.
        let nowNs = clock()
        guard !closed, nowNs < beforeNs, authority?.permits(attempt, nowNs: nowNs) == true else { throw OriginObservationFailure.scope }
        // The caller invokes only mediaConfirmedForAttempt with these captured
        // SDK fields. It must not fetch a newer SDK snapshot to replace them.
        try apply(attempt.local, nowNs)
    }
}
