import Foundation

/// Authentication is a separate admission boundary. A valid signer proof does
/// not grant event-management authority; only the site's login response does.
struct V4vLoginChallenge: CustomStringConvertible {
    let challenge: String
    let appName: String
    init(_ json: V4vJSON) throws {
        let f = try json.object(required: ["ok", "enabled", "registration", "appName", "challenge"], optional: ["relayUrl"])
        guard try f.flag("ok"), try f.flag("enabled"), ["open", "closed"].contains(try f.text("registration", max: 6)) else { throw V4vFailure.unauthenticated }
        challenge = try f.text("challenge", max: 64)
        guard challenge.range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
        appName = try f.text("appName", max: 64)
        guard !appName.isEmpty else { throw V4vFailure.invalidContract }
        // A site's suggested relay never selects or changes the signer route.
        if let relay = f["relayUrl"], !relay.isNull { _ = try relay.text(max: 1024) }
    }
    var description: String { "V4V login challenge (redacted)" }
}

struct V4vLoginTemplate {
    let persona: String
    let challenge: V4vLoginChallenge
    let origin: String
    let createdAt: UInt64
    init(persona: String, challenge: V4vLoginChallenge, profile: V4vConnectorProfile, createdAt: UInt64) throws {
        guard persona.range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil, createdAt > 0, createdAt <= V4vJSON.maxSafe else { throw V4vFailure.invalidContract }
        self.persona = persona; self.challenge = challenge; origin = profile.apiOrigin; self.createdAt = createdAt
    }
    /// Rust verifies the canonical event ID and Schnorr signature first. This
    /// second boundary checks the exact fixed template before site submission.
    func checkedProof(_ bytes: Data) throws -> V4vLoginProof {
        let json = try V4vJSON.parse(bytes, limit: 16_384)
        let f = try json.object(required: ["id", "pubkey", "created_at", "kind", "tags", "content", "sig"])
        guard try f.text("pubkey", max: 64) == persona, try f.uint("created_at", min: 1) == createdAt,
              try f.uint("kind") == 21236, try f.text("content", max: 0).isEmpty,
              f["tags"] == .array([.array([.string("challenge"), .string(challenge.challenge)]), .array([.string("origin"), .string(origin)]), .array([.string("app"), .string(challenge.appName)])]) else { throw V4vFailure.invalidContract }
        for (key, count) in [("id", 64), ("sig", 128)] {
            let value = try f.text(key, max: count)
            guard value.range(of: "^[0-9a-f]{\(count)}\\z", options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
        }
        return V4vLoginProof(event: json)
    }
}

struct V4vLoginProof: CustomStringConvertible {
    fileprivate let event: V4vJSON
    fileprivate init(event: V4vJSON) { self.event = event }
    var description: String { "V4V signed login proof (redacted)" }
    var body: Data { get throws { try V4vJSON.object(["authEvent": event]).encoded(limit: 16_400) } }
}

struct V4vLoginResult: CustomStringConvertible {
    let persona: String
    let token: String
    let maxAgeSeconds: UInt64
    init(_ json: V4vJSON, persona: String) throws {
        guard persona.range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
        let f = try json.object(required: ["ok", "pubkey", "registered", "operator", "sessionToken", "sessionTokenMaxAgeSec"])
        guard try f.flag("ok"), try f.text("pubkey", max: 64) == persona else { throw V4vFailure.unauthenticated }
        _ = try f.flag("registered"); _ = try f.flag("operator")
        token = try f.text("sessionToken", max: 8192)
        guard !token.isEmpty, token.utf8.allSatisfy({ $0 >= 33 && $0 <= 126 }) else { throw V4vFailure.invalidContract }
        maxAgeSeconds = try f.uint("sessionTokenMaxAgeSec", min: 1, max: 86_400)
        self.persona = persona
    }
    private init(persona: String, token: String, maxAgeSeconds: UInt64) {
        self.persona = persona; self.token = token; self.maxAgeSeconds = maxAgeSeconds
    }
    /// The site's token is minted after the POST starts. Its backend rounds
    /// mint time down to integer seconds. Reserve that second and subtract all
    /// request/cleanup/delivery elapsed time, rounding conservatively upwards.
    func remaining(postStartedNs: UInt64, nowNs: UInt64) throws -> Self {
        guard nowNs >= postStartedNs else { throw V4vFailure.unauthenticated }
        let elapsed = nowNs - postStartedNs
        let spent = elapsed / 1_000_000_000 + (elapsed % 1_000_000_000 == 0 ? 0 : 1) + 1
        guard maxAgeSeconds > spent else { throw V4vFailure.unauthenticated }
        return Self(persona: persona, token: token, maxAgeSeconds: maxAgeSeconds - spent)
    }
    var description: String { "V4V authenticated session (redacted)" }
}
