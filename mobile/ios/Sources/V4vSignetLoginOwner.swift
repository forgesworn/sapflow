import Foundation

/// Injected only at this cold boundary. The real adapter is generated SignetAuth;
/// tests use generated proofs and never pair with a real account/signer.
protocol V4vLoginSigner: AnyObject {
    func connect() throws -> (persona: String, credential: Data)
    func sign(_ template: V4vLoginTemplate) throws -> Data
    func cancel()
    func waitClosed() -> Bool
}
protocol V4vLoginHTTP: AnyObject {
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void)
    func close()
}
extension V4vControlClient: V4vLoginHTTP {}

/// Use a DISTINCT encrypted journal directory and Keychain service from the
/// publisher's durable End/pending-operation journal. Forgetting a signer client
/// is not publish End, bearer revocation or remote signer grant revocation.
final class V4vSignetCredentialStore {
    private let journal: V4vJournalStore
    init(journal: V4vJournalStore) { self.journal = journal }
    func load() throws -> Data? {
        guard let document = try journal.load() else { return nil }
        let f = try document.object(required: ["version", "credential"])
        guard try f.uint("version") == 1 else { throw V4vFailure.journal }
        if f["credential"] == .null { return nil }
        let value = try f.text("credential", max: 21_848)
        guard let bytes = Data(base64Encoded: value), !bytes.isEmpty, bytes.count <= 16_384,
              bytes.base64EncodedString() == value else { throw V4vFailure.journal }
        return bytes
    }
    func save(_ bytes: Data) throws {
        guard !bytes.isEmpty, bytes.count <= 16_384 else { throw V4vFailure.journal }
        try journal.save(.object(["version": .integer(1), "credential": .string(bytes.base64EncodedString())]))
    }
    func forget() throws { try journal.save(.object(["version": .integer(1), "credential": .null])) }
}

/// One explicit, single-use creator login. No UI, imported bearer, automatic
/// capture or background re-pairing. A caller supplies its actual engine-owner
/// queue for adoption; commit must install into that same serial engine owner.
/// The original total deadline includes queueing, remote consent and site HTTP.
/// Success is reported only after native runtime AND URLSession truly close.
final class V4vSignetLoginOwner: @unchecked Sendable {
    private static let ownership = NSLock()
    private static var occupied = false
    typealias SignerFactory = (_ restoredCredential: Data?, _ remainingMs: UInt32) throws -> V4vLoginSigner
    typealias HTTPFactory = (_ onClosed: @escaping () -> Void) -> V4vLoginHTTP
    private let work = DispatchQueue(label: "sapflow.v4v.signet.login")
    private let delivery: DispatchQueue
    private let lock = NSRecursiveLock()
    private let profile: V4vConnectorProfile
    private let credentials: V4vSignetCredentialStore
    private let makeSigner: SignerFactory
    private let makeHTTP: HTTPFactory
    private let clock: () -> UInt64
    private let unixSeconds: () -> UInt64
    private let commit: (V4vLoginResult) throws -> Void
    private let onCleanupComplete: () -> Void
    // Keep one bounded owner alive across a failed close receipt. A later real
    // HTTP invalidation must still reach it even if its caller drops the UI.
    private var retainedSelf: V4vSignetLoginOwner?
    private var admitted = false, cancelled = false, finished = false, nativeClosed = true, httpClosed = true
    private var signer: V4vLoginSigner?, http: V4vLoginHTTP?
    private var deadline: UInt64 = 0, lastNs: UInt64 = 0
    private var loginPostNs: UInt64?
    private var timer: DispatchSourceTimer?
    private var result: Result<V4vLoginResult, V4vFailure>?
    private var completion: ((Result<Void, V4vFailure>) -> Void)?
    init(profile: V4vConnectorProfile, credentials: V4vSignetCredentialStore,
         delivery: DispatchQueue, makeSigner: @escaping SignerFactory, makeHTTP: @escaping HTTPFactory,
         clock: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds },
         unixSeconds: @escaping () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970)) },
         onCleanupComplete: @escaping () -> Void = {},
         commit: @escaping (V4vLoginResult) throws -> Void) {
        self.profile = profile; self.credentials = credentials; self.delivery = delivery
        self.makeSigner = makeSigner; self.makeHTTP = makeHTTP; self.clock = clock; self.unixSeconds = unixSeconds; self.commit = commit
        self.onCleanupComplete = onCleanupComplete
    }
    /// Restore never silently generates a fresh client identity on a bad journal.
    @discardableResult func start(restoring: Bool, timeoutMs: UInt32 = 120_000, completion: @escaping (Result<Void, V4vFailure>) -> Void) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !admitted, !cancelled, (1...120_000).contains(timeoutMs) else { return false }
        let end = clock().addingReportingOverflow(UInt64(timeoutMs) * 1_000_000)
        guard !end.overflow else { return false }
        Self.ownership.lock()
        guard !Self.occupied else { Self.ownership.unlock(); return false }
        Self.occupied = true; Self.ownership.unlock()
        admitted = true; retainedSelf = self; deadline = end.partialValue; lastNs = clock(); self.completion = completion
        let alarm = DispatchSource.makeTimerSource(queue: delivery); timer = alarm
        alarm.schedule(deadline: .now() + .milliseconds(Int(timeoutMs)))
        alarm.setEventHandler { [weak self] in self?.cancel() }; alarm.resume()
        work.async { [self] in begin(restoring: restoring) }
        return true
    }
    /// This fence is synchronous and independent of a blocked consent waiter.
    /// commit and cancel share a recursive lock: once cancel returns no later
    /// response can begin adoption, including a queued delivery callback.
    func cancel() {
        lock.lock()
        guard !cancelled else { lock.unlock(); return }
        cancelled = true
        guard admitted, !finished else { lock.unlock(); return }
        let signer = self.signer, http = self.http; lock.unlock()
        signer?.cancel(); http?.close()
        work.async { [self] in finish(.failure(.closed)) }
    }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return finished && nativeClosed && httpClosed }
    private func check() throws -> UInt32 {
        lock.lock(); defer { lock.unlock() }
        guard !cancelled, !finished else { throw V4vFailure.closed }
        let now = clock()
        guard now >= lastNs, now < deadline else { throw V4vFailure.unavailable }
        lastNs = now
        return UInt32(min(120_000, max(1, (deadline - now) / 1_000_000)))
    }
    private func begin(restoring: Bool) {
        do {
            let saved = restoring ? try credentials.load() : nil
            if restoring && saved == nil { throw V4vFailure.journal }
            let next = try makeSigner(saved, check())
            lock.lock(); signer = next; nativeClosed = false; let stopped = cancelled; lock.unlock()
            if stopped { next.cancel(); throw V4vFailure.closed }
            let ready = try next.connect(); _ = try check()
            guard ready.persona.range(of: #"^[0-9a-f]{64}\z"#, options: .regularExpression) != nil else { throw V4vFailure.invalidContract }
            try credentials.save(ready.credential); _ = try check()
            let client = makeHTTP { [weak self] in
                guard let self else { return }
                self.work.async { self.lock.lock(); self.httpClosed = true; self.lock.unlock(); self.deliverIfClosed() }
            }
            lock.lock(); http = client; httpClosed = false; let closed = cancelled; lock.unlock()
            if closed { client.close(); throw V4vFailure.closed }
            client.execute(V4vHTTPCall(loginChallenge: ())) { [weak self] response in
                guard let self else { return }; self.work.async { self.challenge(response, persona: ready.persona) }
            }
        } catch { finish(.failure((error as? V4vFailure) ?? .unavailable)) }
    }
    private func challenge(_ response: Result<V4vJSON, V4vFailure>, persona: String) {
        do {
            _ = try check()
            let challenge = try V4vLoginChallenge(response.get())
            let template = try V4vLoginTemplate(persona: persona, challenge: challenge, profile: profile, createdAt: unixSeconds())
            guard let signer else { throw V4vFailure.closed }
            let proof = try template.checkedProof(signer.sign(template)); _ = try check()
            loginPostNs = lastNs
            http?.execute(try V4vHTTPCall(login: proof)) { [weak self] response in
                guard let self else { return }; self.work.async {
                    do { _ = try self.check(); self.finish(.success(try V4vLoginResult(response.get(), persona: persona))) }
                    catch { self.finish(.failure((error as? V4vFailure) ?? .unavailable)) }
                }
            }
        } catch { finish(.failure((error as? V4vFailure) ?? .unavailable)) }
    }
    private func finish(_ result: Result<V4vLoginResult, V4vFailure>) {
        lock.lock()
        guard self.result == nil else { lock.unlock(); return }
        self.result = result; let signer = self.signer, http = self.http
        lock.unlock()
        signer?.cancel(); http?.close()
        let released = signer?.waitClosed() ?? true
        lock.lock(); nativeClosed = released; lock.unlock()
        // A timed-out runtime retains its own global native permit. Keep this
        // owner and HTTP alive; no false cleanup callback or automatic retry.
        if !released { reportCleanupFailure() } else { deliverIfClosed() }
        work.asyncAfter(deadline: .now() + 2) { [self] in
            lock.lock(); let done = finished; lock.unlock()
            if !done { reportCleanupFailure() }
        }
    }
    private func reportCleanupFailure() {
        delivery.async { [self] in
            lock.lock(); let callback = completion; completion = nil; lock.unlock()
            callback?(.failure(.unavailable))
        }
    }
    private func deliverIfClosed() {
        lock.lock()
        guard !finished, nativeClosed, httpClosed, let result else { lock.unlock(); return }
        finished = true
        timer?.setEventHandler {}; timer?.cancel(); timer = nil
        lock.unlock()
        delivery.async { [self] in
            // Emitted once after the actual work-queue close transition, even
            // if a prior failure consumed the result callback. The later
            // registered unlock runs first, so this is outside the owner lock.
            defer { onCleanupComplete() }
            lock.lock(); defer { lock.unlock() }
            guard let callback = completion else {
                // A bounded cleanup-failure result may already have consumed
                // the callback. Proven later closure still releases capacity,
                // but must never adopt that late authentication result.
                Self.ownership.lock(); Self.occupied = false; Self.ownership.unlock(); retainedSelf = nil
                return
            }; completion = nil
            do {
                let now = clock()
                guard !cancelled, now >= lastNs, now < deadline else { throw V4vFailure.closed }
                let accepted = try result.get()
                guard let postStarted = loginPostNs else { throw V4vFailure.unavailable }
                let login = try accepted.remaining(postStartedNs: postStarted, nowNs: now); try commit(login)
                Self.ownership.lock(); Self.occupied = false; Self.ownership.unlock(); retainedSelf = nil
                callback(.success(()))
            } catch {
                Self.ownership.lock(); Self.occupied = false; Self.ownership.unlock(); retainedSelf = nil
                callback(.failure((error as? V4vFailure) ?? .unavailable))
            }
        }
    }
}
