import Foundation
import Network
import CryptoKit

/// Redacted errors only. Server bodies, credentials and URLs are never errors.
enum V4vFailure: Error, CustomStringConvertible {
    case invalidContract, invalidProfile, bounds, unauthenticated, conflict, terminal, unavailable, journal, busy, closed
    var description: String { "V4V control failed (\(self.code))" }
    private var code: String {
        switch self {
        case .invalidContract: return "contract"
        case .invalidProfile: return "profile"
        case .bounds: return "bounds"
        case .unauthenticated: return "authentication"
        case .conflict: return "conflict"
        case .terminal: return "terminal"
        case .unavailable: return "unconfirmed"
        case .journal: return "journal"
        case .busy: return "busy"
        case .closed: return "closed"
        }
    }
}

/// Preserve lexical number types and decoded duplicate keys BEFORE Foundation
/// normalises an object. Foundation's maintained String decoder validates escape
/// and surrogate syntax; no networking or Foundation object-number coercion.
indirect enum V4vJSON: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    case object([String: V4vJSON]), array([V4vJSON]), string(String), number(String), bool(Bool), null
    static let maxBody = 65_536
    static let maxSafe: UInt64 = 9_007_199_254_740_991
    static let maxEpoch: UInt64 = 253_402_300_799_999
    var description: String { "V4V JSON (redacted)" }
    var debugDescription: String { description }
    static func integer(_ value: UInt64) -> V4vJSON { .number(String(value)) }
    static func parse(_ data: Data, limit: Int = maxBody) throws -> V4vJSON {
        guard data.count <= limit, limit <= 262_144, String(data: data, encoding: .utf8) != nil else { throw V4vFailure.bounds }
        do {
            var parser = Parser(bytes: Array(data))
            let value = try parser.value(depth: 0)
            parser.space()
            guard parser.index == parser.bytes.count else { throw V4vFailure.invalidContract }
            return value
        } catch let error as V4vFailure { throw error }
        catch { throw V4vFailure.invalidContract }
    }
    func encoded(limit: Int = maxBody) throws -> Data {
        func render(_ value: V4vJSON, _ depth: Int) throws -> String {
            guard depth <= 16 else { throw V4vFailure.bounds }
            switch value {
            case let .object(fields):
                return "{" + (try fields.keys.sorted().map { key in
                    let name = String(decoding: try JSONEncoder().encode(key), as: UTF8.self)
                    return name + ":" + (try render(fields[key]!, depth + 1))
                }).joined(separator: ",") + "}"
            case let .array(values): return "[" + (try values.map { try render($0, depth + 1) }).joined(separator: ",") + "]"
            case let .string(value): return String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
            case let .number(value):
                guard value.range(of: #"^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
                return value
            case let .bool(value): return value ? "true" : "false"
            case .null: return "null"
            }
        }
        let result = Data(try render(self, 0).utf8)
        guard result.count <= limit else { throw V4vFailure.bounds }
        return result
    }
    func object(required: Set<String>, optional: Set<String> = []) throws -> [String: V4vJSON] {
        guard case let .object(fields) = self, required.isSubset(of: Set(fields.keys)), Set(fields.keys).isSubset(of: required.union(optional)) else { throw V4vFailure.invalidContract }
        return fields
    }
    func text(max: Int = 4096) throws -> String {
        guard case let .string(value) = self, value.utf8.count <= max, !value.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }) else { throw V4vFailure.invalidContract }
        return value
    }
    func uint(min: UInt64 = 0, max: UInt64 = maxSafe) throws -> UInt64 {
        guard case let .number(value) = self, value.range(of: #"^(0|[1-9][0-9]*)\z"#, options: .regularExpression) != nil,
              let number = UInt64(value), number >= min, number <= max else { throw V4vFailure.invalidContract }
        return number
    }
    func flag() throws -> Bool { guard case let .bool(value) = self else { throw V4vFailure.invalidContract }; return value }
    var isNull: Bool { self == .null }
    private struct Parser {
        let bytes: [UInt8]
        var index = 0
        mutating func space() { while index < bytes.count && [9, 10, 13, 32].contains(bytes[index]) { index += 1 } }
        mutating func consume(_ byte: UInt8) -> Bool { space(); guard index < bytes.count, bytes[index] == byte else { return false }; index += 1; return true }
        mutating func string() throws -> String {
            space(); let start = index
            guard index < bytes.count, bytes[index] == 34 else { throw V4vFailure.invalidContract }
            index += 1
            while index < bytes.count {
                let byte = bytes[index]; index += 1
                if byte == 34 { return try JSONDecoder().decode(String.self, from: Data(bytes[start..<index])) }
                if byte == 92 { guard index < bytes.count else { throw V4vFailure.invalidContract }; index += 1 }
                else if byte < 32 { throw V4vFailure.invalidContract }
            }
            throw V4vFailure.invalidContract
        }
        mutating func value(depth: Int) throws -> V4vJSON {
            space(); guard index < bytes.count else { throw V4vFailure.invalidContract }
            if bytes[index] == 34 { return .string(try string()) }
            if consume(123) {
                guard depth < 16 else { throw V4vFailure.bounds }
                var fields: [String: V4vJSON] = [:]
                if consume(125) { return .object(fields) }
                repeat {
                    let key = try string()
                    guard fields[key] == nil, consume(58) else { throw V4vFailure.invalidContract }
                    fields[key] = try value(depth: depth + 1)
                    if consume(125) { return .object(fields) }
                    guard consume(44) else { throw V4vFailure.invalidContract }
                } while true
            }
            if consume(91) {
                guard depth < 16 else { throw V4vFailure.bounds }
                var values: [V4vJSON] = []
                if consume(93) { return .array(values) }
                repeat {
                    values.append(try value(depth: depth + 1))
                    if consume(93) { return .array(values) }
                    guard consume(44) else { throw V4vFailure.invalidContract }
                } while true
            }
            for (word, result) in [("true", V4vJSON.bool(true)), ("false", .bool(false)), ("null", .null)] {
                let token = Array(word.utf8)
                if bytes[index...].starts(with: token) { index += token.count; return result }
            }
            let start = index
            while index < bytes.count && ([45, 43, 46, 69, 101].contains(bytes[index]) || (48...57).contains(bytes[index])) { index += 1 }
            guard index > start else { throw V4vFailure.invalidContract }
            let number = String(decoding: bytes[start..<index], as: UTF8.self)
            guard number.range(of: #"^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
            return .number(number)
        }
    }
}

/// Availability metadata, never evidence that a decoder produced frames/PCM.
struct V4vFreshMedia: Equatable {
    let connectionEpoch: UInt64
    let partId: UInt64
    let initializationId: UInt64
    let initPath: String
    let partPath: String
    let hasVideo: Bool
    let hasAudio: Bool
    init(_ json: V4vJSON, event: String, epoch: UInt64) throws {
        let f = try json.object(required: ["connectionEpoch", "partId", "initializationId", "initPath", "partPath", "hasVideo", "hasAudio"])
        connectionEpoch = try f.uint("connectionEpoch", min: 1)
        partId = try f.uint("partId"); initializationId = try f.uint("initializationId")
        initPath = try f.text("initPath", max: 256); partPath = try f.text("partPath", max: 256)
        hasVideo = try f.flag("hasVideo"); hasAudio = try f.flag("hasAudio")
        guard connectionEpoch == epoch, hasVideo || hasAudio else { throw V4vFailure.invalidContract }
        let base = "/live/" + event + "/"
        guard initPath.hasPrefix(base), partPath.hasPrefix(base) else { throw V4vFailure.invalidContract }
        let initName = String(initPath.dropFirst(base.count)), partName = String(partPath.dropFirst(base.count))
        let prefix = String(initName.prefix(while: { $0 != "_" }))
        guard prefix.range(of: #"^[0-9a-f]{1,12}\z"#, options: .regularExpression) != nil,
              initName == prefix + (initializationId == 0 ? "_init.mp4" : "_init\(initializationId).mp4"),
              partName == prefix + "_part\(partId).mp4" else { throw V4vFailure.invalidContract }
    }
}

struct V4vOriginReceipt: Equatable {
    let identity: V4vIdentity
    let state: String
    let admission: String
    let proto: String
    let recording: String
    let callbacks: String
    let expiresMs: UInt64
    let epoch: UInt64
    let attempt: String?
    let freshMedia: V4vFreshMedia?
    init(_ json: V4vJSON) throws {
        let f = try json.object(required: ["contractVersion", "eventId", "eventGeneration", "sessionId", "revision", "expiresAtUnixMs", "connectionEpoch", "state", "admission", "protocol", "recording", "callbacks"], optional: ["eventSequence", "connectionAttemptId", "freshMedia"])
        identity = try V4vIdentity(f)
        state = try f.text("state"); admission = try f.text("admission"); proto = try f.text("protocol")
        recording = try f.text("recording"); callbacks = try f.text("callbacks"); epoch = try f.uint("connectionEpoch")
        guard ["inactive", "prepared", "connected", "live", "held", "expired", "revoked"].contains(state), ["open", "denied"].contains(admission),
              ["none", "active", "stopping", "stopped"].contains(proto), ["never-started", "active", "finalising", "finalised", "failed", "incomplete"].contains(recording),
              ["disabled", "none", "pending", "delivered", "failed"].contains(callbacks) else { throw V4vFailure.invalidContract }
        expiresMs = try f.uint("expiresAtUnixMs", max: V4vJSON.maxEpoch)
        if expiresMs == 0 {
            guard state == "revoked", admission == "denied", epoch == 0, ["none", "stopped"].contains(proto), recording == "never-started", ["disabled", "none"].contains(callbacks) else { throw V4vFailure.invalidContract }
        }
        if ["expired", "revoked"].contains(state), admission != "denied" { throw V4vFailure.invalidContract }
        if let sequence = f["eventSequence"] { _ = try sequence.uint() }
        if let value = f["connectionAttemptId"], !value.isNull {
            attempt = try V4vIdentity.id(value.text(max: 32))
            guard epoch > 0, ["connected", "live"].contains(state), admission == "open", proto == "active" else { throw V4vFailure.invalidContract }
        } else { attempt = nil }
        if let value = f["freshMedia"], !value.isNull {
            guard state == "live", admission == "open", proto == "active" else { throw V4vFailure.invalidContract }
            freshMedia = try V4vFreshMedia(value, event: identity.eventId, epoch: epoch)
        } else { freshMedia = nil }
    }
    var endComplete: Bool { admission == "denied" && ["none", "stopped"].contains(proto) && ["never-started", "finalised"].contains(recording) && ["disabled", "none", "delivered"].contains(callbacks) }
    var recoveryOffered: Bool { admission == "denied" && ["none", "stopped"].contains(proto) && (["failed", "incomplete"].contains(recording) || callbacks == "failed") }
}

struct V4vSessionReceipt: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let raw: V4vJSON
    let identity: V4vIdentity
    let phase: String
    let expiresMs: UInt64
    let refreshMs: UInt64
    let graceMs: UInt64
    let origin: V4vOriginReceipt?
    let endComplete: Bool
    let recoveryState: String?
    let acknowledgementKey: String?
    init(_ json: V4vJSON) throws {
        let f = try json.object(required: ["contractVersion", "eventId", "eventGeneration", "sessionId", "revision", "expiresAtUnixMs", "refreshAfterUnixMs", "reconnectGraceMs", "phase", "origin", "endComplete"], optional: ["recovery"])
        raw = json; identity = try V4vIdentity(f); phase = try f.text("phase", max: 24)
        expiresMs = try f.uint("expiresAtUnixMs", min: 1, max: V4vJSON.maxEpoch)
        refreshMs = try f.uint("refreshAfterUnixMs", min: 1, max: V4vJSON.maxEpoch)
        graceMs = try f.uint("reconnectGraceMs", max: 3_600_000)
        guard refreshMs < expiresMs, ["preparing", "active", "refreshing", "unconfirmed", "ending", "ended", "expired", "revoked"].contains(phase) else { throw V4vFailure.invalidContract }
        origin = try f.need("origin").isNull ? nil : V4vOriginReceipt(f.need("origin"))
        if let origin {
            guard origin.identity.sameSession(identity), origin.identity.revision <= identity.revision else { throw V4vFailure.invalidContract }
            if phase == "active" { guard origin.identity == identity else { throw V4vFailure.invalidContract } }
            // The only zero-expiry exception is a validated revoke-before-
            // provision tombstone; it cannot supply a usable active grant.
            if origin.identity == identity, origin.expiresMs != 0 {
                guard origin.expiresMs == expiresMs else { throw V4vFailure.invalidContract }
            }
        }
        endComplete = try f.flag("endComplete")
        if let recovery = f["recovery"] {
            let r = try recovery.object(required: ["state", "acknowledgementKey", "eventGeneration", "revision", "explicitPrepareRequired", "origin"], optional: ["successor"])
            let state = try r.text("state")
            guard ["acknowledging", "acknowledged", "consumed"].contains(state), try r.uint("eventGeneration", min: 1) == identity.generation,
                  try r.uint("revision", min: 1) == identity.revision, try r.flag("explicitPrepareRequired"), !endComplete else { throw V4vFailure.invalidContract }
            recoveryState = state; acknowledgementKey = try V4vIdentity.id(r.text("acknowledgementKey", max: 32))
            let old = try V4vOriginReceipt(r.need("origin"))
            guard old.identity == identity, old.recoveryOffered else { throw V4vFailure.invalidContract }
            if let successor = r["successor"] {
                let s = try successor.object(required: ["sessionId", "eventGeneration", "prepareKey"])
                _ = try V4vIdentity.id(s.text("sessionId", max: 32)); _ = try V4vIdentity.id(s.text("prepareKey", max: 32))
                guard state == "consumed", try s.uint("eventGeneration", min: 1) > identity.generation else { throw V4vFailure.invalidContract }
            }
        } else { recoveryState = nil; acknowledgementKey = nil }
        if endComplete { guard recoveryState == nil, origin?.identity == identity, origin?.endComplete == true, ["ended", "expired", "revoked"].contains(phase) else { throw V4vFailure.invalidContract } }
    }
    var terminal: Bool { ["ending", "ended", "expired", "revoked"].contains(phase) }
    var permitsGrant: Bool {
        phase == "active" && origin?.identity == identity && origin?.expiresMs == expiresMs && origin?.admission == "open" &&
        ["prepared", "connected", "live", "held"].contains(origin?.state ?? "")
    }
    var description: String { "V4V session receipt (redacted)" }
    var debugDescription: String { description }
}

struct V4vCurrentSession {
    let generation: UInt64
    let receipt: V4vSessionReceipt?
    init(_ answer: V4vJSON, event: String) throws {
        _ = try V4vIdentity.event(event)
        let response = try answer.object(required: ["ok", "session"])
        guard try response.flag("ok") else { throw V4vFailure.invalidContract }
        let value = try response.need("session")
        if case let .object(f) = value, f["sessionId"] == .null {
            let empty = try value.object(required: ["contractVersion", "eventId", "eventGeneration", "sessionId"])
            guard try empty.uint("contractVersion", min: 1, max: 1) == 1, try empty.text("eventId") == event,
                  try empty.uint("eventGeneration", max: 0) == 0 else { throw V4vFailure.invalidContract }
            generation = 0; receipt = nil
        } else {
            let parsed = try V4vSessionReceipt(value)
            guard parsed.identity.eventId == event else { throw V4vFailure.invalidContract }
            generation = parsed.identity.generation; receipt = parsed
        }
    }
}

struct V4vGrant: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let raw: V4vJSON
    let identity: V4vIdentity
    let endpoint: String
    let streamId: String
    let passphrase: String
    let expiresMs: UInt64
    let refreshMs: UInt64
    let graceMs: UInt64
    let maxPublishBps: UInt64
    let maxQueueBytes: UInt64
    init(_ json: V4vJSON, nowMs: UInt64) throws {
        let f = try json.object(required: ["contractVersion", "eventId", "eventGeneration", "sessionId", "revision", "destination", "transport", "endpoint", "streamId", "encryption", "expiresAtUnixMs", "refreshAfterUnixMs", "reconnectGraceMs", "limits"], optional: ["stageUrl", "playbackUrl"])
        raw = json; identity = try V4vIdentity(f)
        let destination = try f.need("destination").object(required: ["kind", "connector", "unlimited", "watermark", "subscriptionRequired"])
        guard try destination.text("kind") == "v4v", try destination.text("connector") == "sapflow", try destination.flag("unlimited"), try !destination.flag("watermark"), try !destination.flag("subscriptionRequired"), try f.text("transport") == "srt" else { throw V4vFailure.invalidContract }
        endpoint = try f.text("endpoint", max: 256); streamId = try f.text("streamId", max: 512)
        let encryption = try f.need("encryption").object(required: ["cipher", "passphrase"])
        passphrase = try encryption.text("passphrase", max: 79)
        guard try encryption.text("cipher") == "aes256-ctr", passphrase.range(of: #"^[A-Za-z0-9_-]{43}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
        let prefix = "publish:live/\(identity.eventId):\(identity.sessionId):"
        guard streamId.hasPrefix(prefix), String(streamId.dropFirst(prefix.count)).range(of: #"^[A-Za-z0-9_-]{43}\z"#, options: .regularExpression) != nil,
              String(streamId.dropFirst(prefix.count)) != passphrase else { throw V4vFailure.invalidContract }
        do { _ = try ManualSrtDestination(endpoint: endpoint, streamID: streamId, passphrase: passphrase, allowPlaintext: false) } catch { throw V4vFailure.invalidContract }
        expiresMs = try f.uint("expiresAtUnixMs", min: 1, max: V4vJSON.maxEpoch)
        refreshMs = try f.uint("refreshAfterUnixMs", min: 1, max: V4vJSON.maxEpoch)
        graceMs = try f.uint("reconnectGraceMs", max: 3_600_000)
        guard refreshMs < expiresMs, expiresMs <= nowMs || expiresMs - nowMs <= 900_000 else { throw V4vFailure.invalidContract }
        let limits = try f.need("limits").object(required: ["maxPublishBps", "maxQueueBytes"])
        maxPublishBps = try limits.uint("maxPublishBps", min: 1); maxQueueBytes = try limits.uint("maxQueueBytes", min: 1500, max: 4_194_304)
        for key in ["stageUrl", "playbackUrl"] { if let url = f[key] { _ = try url.text(max: 2048) } } // Display metadata only, never fetched or used for authority.
    }
    func immutableMatches(_ other: Self) -> Bool {
        identity.sameSession(other.identity) && endpoint == other.endpoint && streamId == other.streamId && passphrase == other.passphrase && graceMs == other.graceMs && maxPublishBps == other.maxPublishBps && maxQueueBytes == other.maxQueueBytes
    }
    var policyDigest: String {
        // Fixed validated fields; never persist raw credentials in the policy
        // pin after End/expiry has removed the active grant.
        let fields = [endpoint, streamId, passphrase, String(graceMs), String(maxPublishBps), String(maxQueueBytes)]
        let framed = fields.map { "\($0.utf8.count):" + $0 }.joined()
        return SHA256.hash(data: Data(framed.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    var description: String { "V4V grant (redacted)" }
    var debugDescription: String { description }
}

enum V4vOperationKind: String { case prepare, refresh, end, acknowledge }
struct V4vOperation: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let kind: V4vOperationKind
    let event: String
    let session: String?
    let body: V4vJSON
    let bytes: Data
    init(kind: V4vOperationKind, event: String, session: String?, body: V4vJSON) throws {
        self.kind = kind; self.event = try V4vIdentity.event(event); self.session = try session.map(V4vIdentity.id); self.body = body
        bytes = try body.encoded(limit: 4096)
        let f: [String: V4vJSON]
        switch kind {
        case .prepare:
            guard session == nil else { throw V4vFailure.invalidContract }
            f = try body.object(required: ["expectedEventGeneration", "idempotencyKey"], optional: ["replaces"])
            _ = try f.uint("expectedEventGeneration")
            if let old = f["replaces"] {
                let r = try old.object(required: ["sessionId", "revision", "acknowledgementKey"])
                _ = try V4vIdentity.id(r.text("sessionId")); _ = try r.uint("revision", min: 1); _ = try V4vIdentity.id(r.text("acknowledgementKey"))
            }
        case .refresh, .end:
            guard session != nil else { throw V4vFailure.invalidContract }
            f = try body.object(required: ["revision", "idempotencyKey"]); _ = try f.uint("revision", min: 1)
        case .acknowledge:
            guard session != nil else { throw V4vFailure.invalidContract }
            f = try body.object(required: ["eventGeneration", "revision", "idempotencyKey", "acknowledgement"])
            _ = try f.uint("eventGeneration", min: 1); _ = try f.uint("revision", min: 1)
            guard try f.text("acknowledgement") == "abandon-automatic-recording" else { throw V4vFailure.invalidContract }
        }
        _ = try V4vIdentity.id(f.text("idempotencyKey"))
    }
    var path: String {
        let base = "/api/live-events/\(event)/native-session"
        switch kind { case .prepare: return base; case .refresh: return base + "/\(session!)/refresh"; case .end: return base + "/\(session!)/end"; case .acknowledge: return base + "/\(session!)/recovery-ack" }
    }
    var journal: V4vJSON { .object(["kind": .string(kind.rawValue), "event": .string(event), "session": session.map(V4vJSON.string) ?? .null, "body": body]) }
    init(journal: V4vJSON) throws {
        let f = try journal.object(required: ["kind", "event", "session", "body"])
        guard let kind = V4vOperationKind(rawValue: try f.text("kind")) else { throw V4vFailure.journal }
        try self.init(kind: kind, event: f.text("event"), session: f.need("session").isNull ? nil : f.text("session"), body: f.need("body"))
    }
    var description: String { "V4V saved operation (redacted)" }
    var debugDescription: String { description }
}

extension Dictionary where Key == String, Value == V4vJSON {
    func need(_ key: String) throws -> V4vJSON { guard let value = self[key] else { throw V4vFailure.invalidContract }; return value }
    func text(_ key: String, max: Int = 4096) throws -> String { try need(key).text(max: max) }
    func uint(_ key: String, min: UInt64 = 0, max: UInt64 = V4vJSON.maxSafe) throws -> UInt64 { try need(key).uint(min: min, max: max) }
    func flag(_ key: String) throws -> Bool { try need(key).flag() }
}

struct V4vConnectorProfile: Equatable, CustomStringConvertible {
    let apiOrigin: String
    let playbackOrigin: String
    let playbackPrefix: String
    /// Caller is reviewed build configuration, never user/imported destination
    /// data. There is deliberately no enabled default or guessed public host.
    init(apiOrigin: String, playbackOrigin: String, playbackPrefix: String) throws {
        func origin(_ value: String) throws -> String {
            guard value.utf8.count <= 512, !value.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) || $0.value < 32 }), let u = URLComponents(string: value), u.scheme == "https", let host = u.host, !host.isEmpty,
                  u.user == nil, u.password == nil, u.query == nil, u.fragment == nil, u.path.isEmpty || u.path == "/",
                  u.port == nil || (1...65535).contains(u.port!) else { throw V4vFailure.invalidProfile }
            return String(value.dropLast(value.hasSuffix("/") ? 1 : 0))
        }
        self.apiOrigin = try origin(apiOrigin); self.playbackOrigin = try origin(playbackOrigin)
        guard playbackPrefix.hasPrefix("/"), playbackPrefix.hasSuffix("/"), playbackPrefix.utf8.count <= 256,
              !playbackPrefix.contains(".."), !playbackPrefix.contains("//"), playbackPrefix.range(of: #"^/[A-Za-z0-9/_-]*\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidProfile }
        self.playbackPrefix = playbackPrefix
    }
    var description: String { "Reviewed V4V connector (redacted)" }
}

struct V4vIdentity: Equatable, CustomStringConvertible {
    let eventId: String
    let generation: UInt64
    let sessionId: String
    let revision: UInt64
    static func event(_ text: String) throws -> String {
        guard text.range(of: #"^event-[a-z0-9-]{1,122}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }; return text
    }
    static func id(_ text: String) throws -> String {
        guard text.range(of: #"^[0-9a-f]{32}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }; return text
    }
    init(eventId: String, generation: UInt64, sessionId: String, revision: UInt64) throws {
        self.eventId = try Self.event(eventId); self.sessionId = try Self.id(sessionId)
        guard (1...V4vJSON.maxSafe).contains(generation), (1...V4vJSON.maxSafe).contains(revision) else { throw V4vFailure.invalidContract }
        self.generation = generation; self.revision = revision
    }
    init(_ fields: [String: V4vJSON]) throws {
        guard try fields.uint("contractVersion", min: 1, max: 1) == 1 else { throw V4vFailure.invalidContract }
        try self.init(eventId: fields.text("eventId"), generation: fields.uint("eventGeneration", min: 1), sessionId: fields.text("sessionId"), revision: fields.uint("revision", min: 1))
    }
    func sameSession(_ other: Self) -> Bool { eventId == other.eventId && generation == other.generation && sessionId == other.sessionId }
    var description: String { "V4V session identity" }
}
