import Foundation

protocol V4vJournalStore: AnyObject {
    /// nil means a genuinely new store, not an unreadable/corrupt existing file.
    func load() throws -> V4vJSON?
    func save(_ document: V4vJSON) throws
}

struct V4vClock { let wallMs: UInt64; let monotonicNs: UInt64 }

struct V4vControlRequest: CustomStringConvertible, CustomDebugStringConvertible {
    let operation: V4vOperation
    let bearer: String
    var description: String { "V4V authenticated request (redacted)" }
    var debugDescription: String { description }
}

struct V4vObservationStatusPermit: CustomStringConvertible, CustomDebugStringConvertible {
    let call: V4vHTTPCall
    let identity: V4vIdentity
    let expiresAtNs: UInt64
    var description: String { "Ephemeral V4V status authority (redacted)" }
    var debugDescription: String { description }
}

struct V4vPublishingGrantPermit: CustomStringConvertible, CustomDebugStringConvertible {
    let grant: V4vGrant
    let expiresAtNs: UInt64
    var description: String { "V4V publishing authority (redacted)" }
    var debugDescription: String { description }
}

/// Runtime-free, single serial owner. It stages exact durable requests; the
/// caller runs HTTP asynchronously and returns the captured operation. No timer,
/// automatic capture start, signer, socket or camera is owned here.
final class V4vSessionEngine {
    private let profile: V4vConnectorProfile
    private let store: V4vJournalStore
    private let fencePublisher: () -> Void
    private let initial: V4vClock
    private let anchoredWall: UInt64
    private var lastNs: UInt64
    private var lastMs: UInt64
    private var document: [String: V4vJSON]
    private var unusable = false
    private var closed = false
    private var restoredRequiresRefresh: Bool
    private var expiredScope: V4vIdentity?
    private(set) var terminalIntent: Bool
    private(set) var pending: V4vOperation?
    private(set) var session: V4vSessionReceipt?
    private var grant: V4vGrant?

    /// Immutable composition check, safe before allocating another client.
    /// A bearer owned by this engine must never be routed by a different
    /// control or playback profile, even if that profile is otherwise valid.
    func requireProfile(_ candidate: V4vConnectorProfile) throws {
        guard candidate == profile else { throw V4vFailure.invalidProfile }
    }

    init(profile: V4vConnectorProfile, store: V4vJournalStore, clock: V4vClock, fencePublisher: @escaping () -> Void) throws {
        guard clock.wallMs > 0, clock.wallMs <= V4vJSON.maxEpoch else { throw V4vFailure.bounds }
        self.profile = profile; self.store = store; self.fencePublisher = fencePublisher; initial = clock; lastNs = clock.monotonicNs
        let d = try store.load()?.object(required: ["version", "apiOrigin", "clockFloorMs", "terminalIntent"], optional: ["pubkey", "bearer", "bearerExpiresMs", "session", "grant", "pending", "expiredScope", "grantPolicy", "grantExpiryMs"]) ?? ["version": .integer(1), "apiOrigin": .string(profile.apiOrigin), "clockFloorMs": .integer(0), "terminalIntent": .bool(false)]
        guard try d.uint("version", min: 1, max: 1) == 1, try d.text("apiOrigin") == profile.apiOrigin else { throw V4vFailure.journal }
        let floor = try d.uint("clockFloorMs", max: V4vJSON.maxEpoch)
        let effectiveWall = max(clock.wallMs, floor)
        anchoredWall = effectiveWall; lastMs = effectiveWall; document = d
        terminalIntent = try d.flag("terminalIntent")
        session = try d["session"].map(V4vSessionReceipt.init)
        pending = try d["pending"].map(V4vOperation.init(journal:))
        grant = try d["grant"].map { try V4vGrant($0, nowMs: effectiveWall) }
        restoredRequiresRefresh = session != nil
        if d["bearer"] != nil || d["pubkey"] != nil || d["bearerExpiresMs"] != nil {
            try Self.validateAuth(pubkey: d.text("pubkey"), token: d.text("bearer", max: 8192))
            _ = try d.uint("bearerExpiresMs", min: 1, max: V4vJSON.maxEpoch)
        }
        if let raw = d["expiredScope"] { expiredScope = try V4vIdentity(raw.object(required: ["contractVersion", "eventId", "eventGeneration", "sessionId", "revision"])) }
        if d["grantPolicy"] != nil || d["grantExpiryMs"] != nil {
            guard try d.text("grantPolicy", max: 64).range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil, session != nil else { throw V4vFailure.journal }
            _ = try d.uint("grantExpiryMs", min: 1, max: V4vJSON.maxEpoch)
        }
        if let grant { guard session?.identity == grant.identity, !terminalIntent, expiredScope == nil, try d.text("grantPolicy") == grant.policyDigest, try d.uint("grantExpiryMs") == grant.expiresMs else { throw V4vFailure.journal } }
        if let pending { try Self.validatePending(pending, session: session, terminal: terminalIntent) }
        if terminalIntent && grant != nil { throw V4vFailure.journal }
        if let expiredScope { guard let session, expiredScope.sameSession(session.identity) else { throw V4vFailure.journal } }
        if let session, session.terminal, !terminalIntent {
            // The only legitimate nonterminal row retaining an old terminal
            // receipt is an explicitly staged replacement Prepare.
            guard pending?.kind == .prepare else { throw V4vFailure.journal }
        }
    }

    private static func validateAuth(pubkey: String, token: String) throws {
        guard pubkey.range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil,
              (1...8192).contains(token.utf8.count), token.unicodeScalars.allSatisfy({ $0.value >= 33 && $0.value <= 126 }) else { throw V4vFailure.unauthenticated }
    }
    private static func validatePending(_ op: V4vOperation, session: V4vSessionReceipt?, terminal: Bool) throws {
        if op.kind == .prepare { return }
        guard let session, op.event == session.identity.eventId, op.session == session.identity.sessionId,
              try op.body.object(required: op.kind == .acknowledge ? ["eventGeneration", "revision", "idempotencyKey", "acknowledgement"] : ["revision", "idempotencyKey"]).uint("revision", min: 1) == session.identity.revision else { throw V4vFailure.journal }
        if op.kind == .end || op.kind == .acknowledge { guard terminal else { throw V4vFailure.journal } }
        if op.kind == .acknowledge { guard try op.body.object(required: ["eventGeneration", "revision", "idempotencyKey", "acknowledgement"]).uint("eventGeneration", min: 1) == session.identity.generation else { throw V4vFailure.journal } }
    }
    private func usable() throws { guard !unusable, !closed else { throw V4vFailure.closed } }
    /// Wall rollback cannot extend a lease. A rollback of the supplied monotonic
    /// clock is a caller error and fences this owner rather than reopening it.
    @discardableResult private func now(_ clock: V4vClock) throws -> UInt64 {
        try usable()
        guard clock.monotonicNs >= lastNs, clock.wallMs > 0, clock.wallMs <= V4vJSON.maxEpoch else { unusable = true; fencePublisher(); throw V4vFailure.bounds }
        let elapsed = (clock.monotonicNs - initial.monotonicNs) / 1_000_000
        let sum = anchoredWall.addingReportingOverflow(elapsed)
        guard !sum.overflow, sum.partialValue <= V4vJSON.maxEpoch else { unusable = true; fencePublisher(); throw V4vFailure.bounds }
        lastNs = clock.monotonicNs; lastMs = max(lastMs, clock.wallMs, sum.partialValue)
        return lastMs
    }
    private func save(_ d: [String: V4vJSON]) throws {
        try usable()
        var next = d; next["clockFloorMs"] = .integer(lastMs)
        do { try store.save(.object(next)); document = next }
        catch { unusable = true; fencePublisher(); throw V4vFailure.journal }
    }
    private func auth(_ clock: V4vClock) throws -> String {
        let time = try now(clock)
        guard let raw = document["bearer"], time < (try document.uint("bearerExpiresMs", min: 1, max: V4vJSON.maxEpoch)) else { throw V4vFailure.unauthenticated }
        return try raw.text(max: 8192)
    }
    /// Called only by a future verified creator-auth adapter after actual Nostr
    /// login. No imported bearer UI or key custody is implemented in this packet.
    func installAuthenticatedSession(pubkey: String, token: String, maxAgeSeconds: UInt64, clock: V4vClock) throws {
        try Self.validateAuth(pubkey: pubkey, token: token)
        let time = try now(clock)
        guard (1...86_400).contains(maxAgeSeconds), time <= V4vJSON.maxEpoch - maxAgeSeconds * 1000 else { throw V4vFailure.bounds }
        if let old = document["pubkey"], try old.text() != pubkey, session != nil || pending != nil { throw V4vFailure.conflict }
        var d = document; d["pubkey"] = .string(pubkey); d["bearer"] = .string(token); d["bearerExpiresMs"] = .integer(time + maxAgeSeconds * 1000)
        try save(d)
    }
    func stagePrepare(event: String, expectedGeneration: UInt64, key: String, replacingAcknowledged: Bool = false, clock: V4vClock) throws {
        _ = try auth(clock)
        guard pending == nil else { throw V4vFailure.busy }
        var body: [String: V4vJSON] = ["expectedEventGeneration": .integer(expectedGeneration), "idempotencyKey": .string(key)]
        if let old = session {
            guard old.endComplete || (replacingAcknowledged && old.recoveryState == "acknowledged") else { throw V4vFailure.terminal }
            if replacingAcknowledged {
                guard event == old.identity.eventId, expectedGeneration == old.identity.generation, let ack = old.acknowledgementKey else { throw V4vFailure.conflict }
                body["replaces"] = .object(["sessionId": .string(old.identity.sessionId), "revision": .integer(old.identity.revision), "acknowledgementKey": .string(ack)])
            }
        } else if replacingAcknowledged { throw V4vFailure.conflict }
        let op = try V4vOperation(kind: .prepare, event: event, session: nil, body: .object(body))
        var d = document; d["pending"] = op.journal; d["terminalIntent"] = .bool(false); d.removeValue(forKey: "grant")
        try save(d); pending = op; terminalIntent = false; grant = nil
    }
    func stageRefresh(key: String, clock: V4vClock) throws {
        _ = try auth(clock)
        guard pending == nil, !terminalIntent, let session, !session.terminal, session.phase == "active", session.expiresMs > lastMs, expiredScope == nil else { throw V4vFailure.terminal }
        try stage(.refresh, session: session, key: key)
    }
    /// Local publishing is fenced immediately, even if persistence/auth/network
    /// fails. An uncertain earlier request is retained and resolved EXACTLY
    /// before continueEnd can stage End at its acknowledged revision.
    func requestEnd(clock: V4vClock) throws {
        fencePublisher(); terminalIntent = true; grant = nil
        try now(clock)
        var d = document; d["terminalIntent"] = .bool(true); d.removeValue(forKey: "grant")
        try save(d)
    }
    func continueEnd(key: String, clock: V4vClock) throws {
        _ = try auth(clock)
        guard terminalIntent, pending == nil, let session else { throw V4vFailure.busy }
        if session.endComplete || ["ending", "ended", "revoked"].contains(session.phase) { return }
        try stage(.end, session: session, key: key)
    }
    func stageAcknowledgement(key: String, clock: V4vClock) throws {
        _ = try auth(clock)
        guard terminalIntent, pending == nil, let session, ["ended", "expired", "revoked"].contains(session.phase), !session.endComplete,
              session.recoveryState == nil, session.origin?.identity == session.identity, session.origin?.recoveryOffered == true else { throw V4vFailure.terminal }
        try stage(.acknowledge, session: session, key: key)
    }
    private func stage(_ kind: V4vOperationKind, session: V4vSessionReceipt, key: String) throws {
        var body: [String: V4vJSON] = ["revision": .integer(session.identity.revision), "idempotencyKey": .string(key)]
        if kind == .acknowledge { body["eventGeneration"] = .integer(session.identity.generation); body["acknowledgement"] = .string("abandon-automatic-recording") }
        let op = try V4vOperation(kind: kind, event: session.identity.eventId, session: session.identity.sessionId, body: .object(body))
        var d = document; d["pending"] = op.journal
        try save(d); pending = op
    }
    func pendingRequest(clock: V4vClock) throws -> V4vControlRequest? {
        let bearer = try auth(clock)
        return pending.map { V4vControlRequest(operation: $0, bearer: bearer) }
    }
    /// Cold creator reads only: these do not adopt a receipt or grant, clear an
    /// uncertain operation, or change terminal End intent. Existing tick still
    /// fences an expired grant. Collection metadata is not fresh origin proof.
    func creatorEventsRequest(clock: V4vClock) throws -> V4vHTTPCall {
        try tick(clock: clock)
        return V4vHTTPCall(creatorEvents: (), bearer: try auth(clock))
    }
    func currentRequest(event: String, clock: V4vClock) throws -> V4vHTTPCall {
        try tick(clock: clock)
        return try V4vHTTPCall(current: event, bearer: auth(clock))
    }
    /// No caller-provided SID: status can target only the engine's existing
    /// durable session, including pending or terminal operations needing review.
    func sessionStatusRequest(clock: V4vClock) throws -> V4vHTTPCall {
        try tick(clock: clock)
        let bearer = try auth(clock)
        guard let session else { throw V4vFailure.terminal }
        return V4vHTTPCall(status: session.identity, bearer: bearer)
    }
    /// Fresh exact-session GET for the observer's separate control client.
    /// Neither the journal nor a collection lookup is an observation receipt.
    func observationStatusRequest(clock: V4vClock) throws -> V4vObservationStatusPermit {
        try tick(clock: clock)
        let bearer = try auth(clock)
        guard pending == nil, !terminalIntent, !restoredRequiresRefresh, expiredScope == nil,
              let session, session.phase == "active", session.permitsGrant, let grant, grant.identity == session.identity else { throw V4vFailure.terminal }
        let expiryMs = min(grant.expiresMs, try document.uint("bearerExpiresMs", min: 1, max: V4vJSON.maxEpoch))
        guard expiryMs > lastMs, expiryMs - lastMs <= (UInt64.max - clock.monotonicNs) / 1_000_000 else { throw V4vFailure.terminal }
        return V4vObservationStatusPermit(call: V4vHTTPCall(status: session.identity, bearer: bearer), identity: session.identity,
                                         expiresAtNs: clock.monotonicNs + (expiryMs - lastMs) * 1_000_000)
    }
    /// No failure path clears pending. Re-send this same operation after timeout,
    /// including process restart; status is not an idempotency acknowledgement.
    func applyResponse(_ answer: V4vJSON, for operation: V4vOperation, clock: V4vClock) throws {
        try tick(clock: clock)
        guard pending == operation else { throw V4vFailure.conflict }
        let f = try answer.object(required: ["ok", "session"], optional: ["grant"])
        guard try f.flag("ok") else { throw V4vFailure.invalidContract }
        let receipt = try V4vSessionReceipt(f.need("session"))
        guard receipt.identity.eventId == operation.event, operation.session == nil || receipt.identity.sessionId == operation.session else { throw V4vFailure.conflict }
        if operation.kind == .prepare {
            let fields = try operation.body.object(required: ["expectedEventGeneration", "idempotencyKey"], optional: ["replaces"])
            guard receipt.identity.generation == (try fields.uint("expectedEventGeneration")) + 1 else { throw V4vFailure.conflict }
        } else if let old = session {
            guard receipt.identity.sameSession(old.identity), receipt.identity.revision >= old.identity.revision else { throw V4vFailure.conflict }
        }
        let supplied = try f["grant"].flatMap { $0.isNull ? nil : try V4vGrant($0, nowMs: lastMs) }
        if let supplied {
            guard [.prepare, .refresh].contains(operation.kind), receipt.permitsGrant, supplied.identity == receipt.identity,
                  supplied.expiresMs == receipt.expiresMs, supplied.refreshMs == receipt.refreshMs, supplied.graceMs == receipt.graceMs else { throw V4vFailure.invalidContract }
            if operation.kind == .refresh, document["grantPolicy"] != nil {
                guard supplied.policyDigest == (try document.text("grantPolicy")), supplied.expiresMs >= (try document.uint("grantExpiryMs")) else { throw V4vFailure.conflict }
            }
        }
        if operation.kind == .acknowledge { guard receipt.recoveryState != nil, !receipt.endComplete else { throw V4vFailure.invalidContract } }
        if operation.kind == .end { guard receipt.terminal else { throw V4vFailure.invalidContract } }
        let newScope = operation.kind == .prepare && (session == nil || !receipt.identity.sameSession(session!.identity))
        let expired = !newScope && expiredScope?.sameSession(receipt.identity) == true
        let permit = !terminalIntent && !receipt.terminal && !expired && (supplied?.expiresMs ?? 0) > lastMs
        var d = document; d["session"] = receipt.raw; d.removeValue(forKey: "pending")
        d["grant"] = permit ? supplied?.raw : nil
        if receipt.terminal { d["terminalIntent"] = .bool(true) }
        if newScope { d.removeValue(forKey: "expiredScope"); d.removeValue(forKey: "grantPolicy"); d.removeValue(forKey: "grantExpiryMs") }
        if let supplied { d["grantPolicy"] = .string(supplied.policyDigest); d["grantExpiryMs"] = .integer(supplied.expiresMs) }
        try save(d)
        session = receipt; pending = nil; grant = permit ? supplied : nil
        terminalIntent = terminalIntent || receipt.terminal
        if newScope { expiredScope = nil }
        if permit { restoredRequiresRefresh = false }
    }
    func applyStatus(_ answer: V4vJSON, clock: V4vClock) throws {
        try tick(clock: clock)
        let receipt: V4vSessionReceipt
        do {
            let f = try answer.object(required: ["ok", "session"])
            guard try f.flag("ok"), let old = session else { throw V4vFailure.invalidContract }
            receipt = try V4vSessionReceipt(f.need("session"))
            guard receipt.identity.sameSession(old.identity), receipt.identity.revision >= old.identity.revision else { throw V4vFailure.conflict }
        } catch {
            // A contradictory authenticated status cannot leave an old grant
            // available for a fresh capture admission. Keep exact pending ops.
            fencePublisher(); grant = nil; restoredRequiresRefresh = true
            var d = document; d.removeValue(forKey: "grant"); try save(d)
            throw error
        }
        var d = document
        if pending == nil { d["session"] = receipt.raw }
        if receipt.terminal { fencePublisher(); terminalIntent = true; grant = nil; d["terminalIntent"] = .bool(true); d.removeValue(forKey: "grant") }
        else if !receipt.permitsGrant { fencePublisher(); grant = nil; d.removeValue(forKey: "grant") }
        try save(d)
        if pending == nil { session = receipt }
    }
    func tick(clock: V4vClock) throws {
        let time = try now(clock)
        if let session, time >= session.expiresMs, expiredScope?.sameSession(session.identity) != true {
            fencePublisher(); expiredScope = session.identity; grant = nil
            var d = document; d.removeValue(forKey: "grant"); d["expiredScope"] = Self.identityJSON(session.identity)
            try save(d)
        }
    }
    /// Cold admission query only. Capture must also retain a separate monotonic
    /// expiry alarm outside the HTTP owner; a blocked request cannot extend it.
    func captureGrant(clock: V4vClock) throws -> V4vGrant {
        try tick(clock: clock); _ = try auth(clock)
        guard !restoredRequiresRefresh, !terminalIntent, pending == nil, expiredScope == nil, let grant, session?.identity == grant.identity,
              session?.permitsGrant == true, grant.expiresMs > lastMs, grant.maxPublishBps >= 4_000_000 else { throw V4vFailure.terminal }
        return grant
    }
    /// Use this effective, rollback-resistant deadline for the independent
    /// publishing expiry alarm. Raw wall time must never extend a grant.
    func publishingGrant(clock: V4vClock) throws -> V4vPublishingGrantPermit {
        let grant = try captureGrant(clock: clock)
        guard grant.expiresMs > lastMs, grant.expiresMs - lastMs <= (UInt64.max - clock.monotonicNs) / 1_000_000 else { throw V4vFailure.bounds }
        return V4vPublishingGrantPermit(grant: grant, expiresAtNs: clock.monotonicNs + (grant.expiresMs - lastMs) * 1_000_000)
    }
    func close() { if !closed { closed = true; fencePublisher() } }
    private static func identityJSON(_ id: V4vIdentity) -> V4vJSON {
        .object(["contractVersion": .integer(1), "eventId": .string(id.eventId), "eventGeneration": .integer(id.generation), "sessionId": .string(id.sessionId), "revision": .integer(id.revision)])
    }
}
