import XCTest
import Sapflow
@testable import SapflowBroadcaster

private final class CreatorJournal: V4vJournalStore, V4vJournalShowLeasing {
    var document: V4vJSON?
    var failSave = false
    var leased: UUID?, leaseAcquires = 0, leaseReleases = 0
    func acquireShowLease(owner: UUID) throws { guard leased == nil else { throw V4vFailure.busy }; leased = owner; leaseAcquires += 1 }
    func releaseShowLease(owner: UUID) { guard leased == owner else { return }; leased = nil; leaseReleases += 1 }
    func load() throws -> V4vJSON? { document }
    func save(_ document: V4vJSON) throws { if failSave { throw V4vFailure.journal }; self.document = document }
}
private final class CreatorHTTP: V4vCreatorHTTP {
    var calls: [V4vHTTPCall] = []
    var reply: ((Result<V4vJSON, V4vFailure>) -> Void)?
    var cleanupComplete = false
    var closing = false
    let onClosed: () -> Void
    init(_ onClosed: @escaping () -> Void) { self.onClosed = onClosed }
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) {
        XCTAssertNil(reply); calls.append(call); reply = completion
    }
    func answer(_ result: Result<V4vJSON, V4vFailure>) { let callback = reply; reply = nil; callback?(result) }
    func close() { closing = true }
    func closed() { cleanupComplete = true; onClosed() }
}
private final class CreatorCapture: V4vCreatorCapturePort {
    let admission = CaptureAdmission()
    var canChooseCaptureProfile: Bool { admission.canChooseCaptureProfile }
    var prepared = 0, started = 0, globalStops = 0, cancels = 0, reservations = 0
    var pendingAttach: ((Bool) -> Void)?
    var holdAttach = false
    var beforeAttachCompletion: (() -> Void)?
    var ticket: CaptureAdmission.Preparation?
    private var onCleanup: (() -> Void)?
    func whenCleanupComplete(_ ticket: CaptureAdmission.Preparation, completion: @escaping () -> Void) {
        if admission.ownsPreparation(ticket) { XCTAssertNil(onCleanup); onCleanup = completion } else { completion() }
    }
    func completedActualCleanup() {
        let stop = admission.stop(); guard admission.completeStop(stop, cleanupComplete: true) else { return }
        let callback = onCleanup; onCleanup = nil; callback?()
    }
    func reservePreparation(profile: CaptureIntent) -> CaptureAdmission.Preparation? { reservations += 1; ticket = admission.reservePreparation(profile: profile); return ticket }
    func abandonPreparation(_ ticket: CaptureAdmission.Preparation) {
        admission.candidateClosed(ticket, cleanupComplete: true); admission.failed(ticket, cleanupComplete: true)
        if !admission.ownsPreparation(ticket) { let callback = onCleanup; onCleanup = nil; callback?() }
    }
    func setDestination(_ sink: EncodedPublishingSink?, preparation: CaptureAdmission.Preparation, completion: @escaping (Bool) -> Void) {
        let result = admission.attached(preparation)
        if !result {
            sink?.endDestination()
            if let sink {
                sink.stop { [admission] in _ = admission.candidateClosed(preparation, cleanupComplete: sink.cleanupComplete) }
            } else { _ = admission.candidateClosed(preparation, cleanupComplete: true) }
            completion(false); return
        }
        beforeAttachCompletion?()
        if holdAttach { pendingAttach = { completion($0 && result) } } else { completion(result) }
    }
    func prepare(_ ticket: CaptureAdmission.Preparation) {
        if admission.beginPermissions(ticket) { _ = admission.prepared(ticket); prepared += 1 }
    }
    func start(preparation: CaptureAdmission.Preparation) {
        if let start = admission.reserveStart(preparation), admission.started(start) { started += 1 }
    }
    func cancelPendingPreparation(_ ticket: CaptureAdmission.Preparation) {
        cancels += 1
        // Production-shared ticket fence, including committed-run preservation.
        if let stop = admission.stopIfPending(ticket) { _ = admission.completeStop(stop, cleanupComplete: true) }
    }
    func stop() { globalStops += 1; _ = admission.stop() }
    @discardableResult func stop(preparation: CaptureAdmission.Preparation) -> Bool {
        guard admission.stop(preparation: preparation) != nil else { return false }
        globalStops += 1; return true
    }
}
private final class CreatorPublisher: V4vCreatorPublisher {
    var status = "Sending test fixture"
    var cleanupComplete = false
    var ends = 0, commits = 0
    var stopCallbacks: [() -> Void] = []
    var autoClose = true
    var onFirstEnd: (() -> Void)?
    func committed(_ permit: V4vPublishingGrantPermit) throws { commits += 1 }
    func endDestination() { ends += 1; let hook = onFirstEnd; onFirstEnd = nil; hook?() }
    func start() throws {}
    func offer(_ sample: MediaSample) throws {}
    func stop() { stop(completion: {}) }
    func stop(completion: @escaping () -> Void) { stopCallbacks.append(completion); if autoClose { closed() } }
    func closed() { cleanupComplete = true; let callbacks = stopCallbacks; stopCallbacks = []; callbacks.forEach { $0() } }
}
private final class CreatorLogin: V4vCreatorLogin {
    var cleanupComplete = false, cancelled = false
    var acceptStart = true
    var startCalls = 0
    var timeoutMs: UInt32?
    var callback: ((Result<Void, V4vFailure>) -> Void)?
    let commit: (V4vLoginResult) throws -> Void
    let cleanupHint: () -> Void
    init(commit: @escaping (V4vLoginResult) throws -> Void, cleanupHint: @escaping () -> Void) { self.commit = commit; self.cleanupHint = cleanupHint }
    func start(restoring: Bool, timeoutMs: UInt32, completion: @escaping (Result<Void, V4vFailure>) -> Void) -> Bool {
        startCalls += 1; guard acceptStart, !cancelled else { return false }; self.timeoutMs = timeoutMs; callback = completion; return true
    }
    func cancel() { cancelled = true }
    func closed() { cleanupComplete = true; cleanupHint() }
}
private final class CreatorHeldStatus: OriginSessionStatusFetching {
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete = false
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    func fresh(_ attempt: V4vObservedAttempt, completion: @escaping (Result<OriginStatusSnapshot, OriginObservationFailure>) -> Void) {}
    func close() {} // cancellation requested; actual held IO is not closed yet
    func completeCleanup() { cleanupComplete = true; cleanup.complete() }
}
private final class CreatorUnusedProbe: OriginMediaFetching, OriginMediaDecoding {
    var cleanupComplete: Bool { true }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { completion() }
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void) { XCTFail("No media expected") }
    func decode(_ data: Data, deadlineNs: UInt64, completion: @escaping (Result<OriginDecodedMedia, OriginObservationFailure>) -> Void) { XCTFail("No decode expected") }
    func close() {}
}
private final class CreatorActualSigner: V4vLoginSigner {
    var onSign: (() -> Void)?
    func connect() throws -> (persona: String, credential: Data) { (String(repeating: "e", count: 64), Data([1])) }
    func sign(_ template: V4vLoginTemplate) throws -> Data { onSign?(); throw V4vFailure.unavailable }
    func cancel() {}
    func waitClosed() -> Bool { true }
}
private final class CreatorActualSignetHTTP: V4vLoginHTTP {
    let onClosed: () -> Void
    var reply: ((Result<V4vJSON, V4vFailure>) -> Void)?
    let challenge: V4vJSON = .object(["ok": .bool(true), "enabled": .bool(true), "registration": .string("closed"), "appName": .string("Generated test"), "challenge": .string(String(repeating: "b", count: 64))])
    init(_ onClosed: @escaping () -> Void) { self.onClosed = onClosed }
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) { reply = completion; completion(.success(challenge)) }
    func close() {} // held until the test supplies actual invalidation
}
private final class CreatorAlarm: V4vCreatorAlarm {
    private let lock = NSLock()
    private var callback: ((UInt64) -> Void)?
    private var generation: UInt64 = 0
    private var afterFire: (() -> Void)?
    private(set) var deadline: UInt64?
    private(set) var schedules = 0
    private(set) var cancellations = 0
    func schedule(atNs deadline: UInt64?, generation: UInt64, fire: @escaping (UInt64) -> Void) {
        lock.lock(); self.deadline = deadline; self.generation = generation; callback = fire; schedules += 1; lock.unlock()
    }
    func cancel() {
        lock.lock(); deadline = nil; callback = nil; cancellations += 1; lock.unlock()
    }
    func fire(times: Int = 1) {
        lock.lock(); let next = callback, token = generation, after = afterFire; afterFire = nil; lock.unlock()
        for _ in 0..<times { next?(token) }
        after?()
    }
    func afterNextFire(_ action: @escaping () -> Void) { lock.lock(); afterFire = action; lock.unlock() }
}
private final class CreatorFixture {
    let queue = DispatchQueue(label: "generated.creator.control")
    let journal = CreatorJournal(), signerJournal = CreatorJournal(), capture = CreatorCapture(), alarm = CreatorAlarm()
    var client: CreatorHTTP!
    var publishers: [CreatorPublisher] = []
    var publisherProfiles: [CaptureIntent] = []
    var publisherError: V4vFailure?
    var login: CreatorLogin?
    var beforePublisherReturn: ((CreatorPublisher) -> Void)?
    var withPublisherContext: ((V4vObservationSlot) throws -> Void)?
    var refuseLogin = false
    var beforeLoginReturn: ((CreatorLogin) -> Void)?
    var loginFactoryOverride: V4vCreatorOwner.LoginFactory?
    var onSnapshot: ((V4vCreatorOwner.Snapshot) -> Void)?
    var snapshots: [V4vCreatorOwner.Snapshot] = []
    var owner: V4vCreatorOwner!
    var elapsed: UInt64 = 0
    let base = DispatchTime.now().uptimeNanoseconds
    private let clockHookLock = NSLock()
    private var clockHook: (() -> Void)?
    func beforeNextClock(_ action: @escaping () -> Void) { clockHookLock.lock(); clockHook = action; clockHookLock.unlock() }
    func clock() -> V4vClock {
        clockHookLock.lock(); let hook = clockHook; clockHook = nil; clockHookLock.unlock()
        hook?()
        return V4vClock(wallMs: V4vFixture.time + elapsed, monotonicNs: base + elapsed * 1_000_000)
    }
    func seed(pending: Bool = false, active: Bool = false) throws {
        let e = try V4vSessionEngine(profile: V4vFixture.profile(), store: journal, clock: clock(), fencePublisher: {})
        try e.installAuthenticatedSession(pubkey: String(repeating: "d", count: 64), token: "generated-creator-token", maxAgeSeconds: 3600, clock: clock())
        if pending || active {
            try e.stagePrepare(event: V4vFixture.event, expectedGeneration: 0, key: V4vFixture.key, clock: clock())
            if active { try e.applyResponse(V4vFixture.answer(), for: XCTUnwrap(e.pending), clock: clock()) }
        }
    }
    func open() throws {
        owner = V4vCreatorOwner(profile: try V4vFixture.profile(), journal: journal,
            credentials: V4vSignetCredentialStore(journal: signerJournal), capture: capture, owner: queue,
            clock: { [unowned self] in clock() }, delivery: queue, changed: { [unowned self] in snapshots.append($0); onSnapshot?($0) },
            alarm: alarm,
            makeHTTP: { [unowned self] closed in let c = CreatorHTTP(closed); client = c; return c },
            makeLogin: { [unowned self] uri, profile, credentials, queue, clock, commit, cleanup in
                if let override = loginFactoryOverride { return try override(uri, profile, credentials, queue, clock, commit, cleanup) }
                let next = CreatorLogin(commit: commit, cleanupHint: cleanup); next.acceptStart = !refuseLogin; login = next; beforeLoginReturn?(next); return next
            },
            makePublisher: { [unowned self] _, _, _, _, slot, _, captureProfile in
                if let publisherError { throw publisherError }
                let next = CreatorPublisher(); publisherProfiles.append(captureProfile); publishers.append(next)
                try withPublisherContext?(slot); beforePublisherReturn?(next); return next
            })
        flush()
    }
    func flush() { for _ in 0..<8 { queue.sync {} } }
    func answer(_ result: Result<V4vJSON, V4vFailure>) { queue.sync { client.answer(result) }; flush() }
    var last: V4vCreatorOwner.Snapshot { queue.sync { snapshots.last! } }
    var calls: [V4vHTTPCall] { queue.sync { client?.calls ?? [] } }
    func events() throws {
        XCTAssertTrue(owner.reloadEvents()); flush()
        answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event), "title": .string("Generated event")])])])))
        XCTAssertTrue(owner.selectEvent(V4vFixture.event)); flush()
        answer(.success(.object(["ok": .bool(true), "session": .object(["contractVersion": .integer(1), "eventId": .string(V4vFixture.event), "eventGeneration": .integer(0), "sessionId": .null])])))
    }
    func prepared() throws {
        try events(); XCTAssertTrue(owner.prepare()); flush(); answer(.success(V4vFixture.answer()))
        owner.captureChanged(prepared: true, running: false, stopping: false); flush()
    }
    func close() { owner.close(); flush(); queue.sync { client?.closed(); login?.closed() }; flush() }
}

final class V4vCreatorOwnerTests: XCTestCase {
    func testProductionStopActionCannotRetireSuccessorV4vPublisherInSameOwner() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare(captureProfile: .audioOnly48kMono)); f.flush(); f.answer(.success(V4vFixture.answer()))
        f.owner.captureChanged(prepared: true, running: false, stopping: false); f.flush()
        XCTAssertTrue(f.owner.start()); f.flush()
        let a = try XCTUnwrap(f.capture.ticket), action = AudioShowStopControl(preparation: a, admission: f.capture.admission)
        func invoke() -> Bool {
            action.stop(capture: { $0.perform(on: f.capture.admission) != nil },
                        retirePublisher: { f.owner.captureStopped(preparation: $0) })
        }
        XCTAssertTrue(invoke()); f.flush(); f.capture.completedActualCleanup()
        f.owner.captureChanged(prepared: false, running: false, stopping: false); f.flush()
        XCTAssertTrue(f.owner.prepare(captureProfile: .audioOnly48kMono)); f.flush()
        let b = try XCTUnwrap(f.capture.ticket); XCTAssertNotEqual(a, b)
        XCTAssertEqual(f.publishers.count, 2); XCTAssertEqual(f.publishers[1].ends, 0)
        XCTAssertFalse(invoke()); f.flush()
        XCTAssertEqual(f.capture.admission.currentPreparation, b); XCTAssertEqual(f.publishers[1].ends, 0)
    }
    func testBackgroundLeaseRetainsExactOwnerUntilRecordingAndPublisherCleanup() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); try f.events()
        XCTAssertTrue(f.capture.admission.selectBackgroundAudio(true))
        XCTAssertTrue(f.owner.prepare(captureProfile: .audioOnly48kMono)); f.flush(); f.answer(.success(V4vFixture.answer()))
        XCTAssertEqual(f.journal.leaseAcquires, 1); XCTAssertEqual(f.signerJournal.leaseAcquires, 0)
        f.owner.captureChanged(prepared: true, running: false, stopping: false); f.flush()
        XCTAssertTrue(f.owner.start()); f.flush(); XCTAssertEqual(f.capture.started, 1)
        f.owner.captureChanged(prepared: false, running: true, stopping: false); f.flush()
        let ticket = try XCTUnwrap(f.capture.ticket)
        XCTAssertEqual(f.capture.admission.visibilityChanged(foreground: false).continuing, ticket)
        XCTAssertTrue(f.owner.setSceneForeground(false, continuing: ticket))
        XCTAssertFalse(f.owner.signIn(bunkerURI: "bunker://generated")); XCTAssertFalse(f.owner.prepare())
        f.publishers[0].autoClose = false
        weak var retained = f.owner
        f.owner.close(); f.flush(); f.queue.sync { f.client?.closed() }; f.flush()
        f.owner = nil; f.flush()
        XCTAssertNotNil(retained); XCTAssertNotNil(f.journal.leased)
        f.queue.sync { f.publishers[0].closed() }; f.flush()
        XCTAssertNotNil(retained); XCTAssertEqual(f.journal.leaseReleases, 0)
        f.capture.completedActualCleanup(); f.flush()
        XCTAssertNil(f.journal.leased); XCTAssertEqual(f.journal.leaseReleases, 1); XCTAssertNil(retained)
    }
    func testBackgroundLeaseFactoryThrowAndCancelledPrepareReleaseOnlyActualCleanup() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.capture.admission.selectBackgroundAudio(true)); f.publisherError = .unavailable
        XCTAssertTrue(f.owner.prepare(captureProfile: .audioOnly48kMono)); f.flush(); f.answer(.success(V4vFixture.answer()))
        XCTAssertEqual(f.journal.leaseAcquires, 1); XCTAssertEqual(f.journal.leaseReleases, 1)
        f.publisherError = nil
        XCTAssertTrue(f.owner.prepare(captureProfile: .audioOnly48kMono)); f.flush()
        XCTAssertEqual(f.journal.leaseAcquires, 2)
        let ticket = try XCTUnwrap(f.capture.ticket)
        XCTAssertNotNil(f.capture.admission.visibilityChanged(foreground: false).stop)
        XCTAssertFalse(f.owner.setSceneForeground(false, continuing: nil)); f.owner.setActive(false); f.flush()
        XCTAssertEqual(f.journal.leaseReleases, 1)
        XCTAssertNil(f.capture.admission.reserveStart(ticket))
        f.capture.completedActualCleanup(); f.flush()
        XCTAssertEqual(f.journal.leaseReleases, 2)
    }
    func testBootAndReadsNeverSignOrCapture() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.last.ready); XCTAssertNil(f.login); XCTAssertEqual(f.capture.prepared, 0); XCTAssertTrue(f.calls.isEmpty)
        try f.events(); XCTAssertEqual(f.last.events.count, 1); XCTAssertEqual(f.calls.map(\.method), ["GET", "GET"])
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertNil(f.last.publicPersona)
    }
    func testLowCapRejectsBeforeOwnersAndExplicit480ReusesDurableGrant() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.canChangeCaptureSettings)
        XCTAssertTrue(f.owner.prepare()); XCTAssertFalse(f.owner.canChangeCaptureSettings); f.flush()
        let low = V4vFixture.edit(V4vFixture.grant(), ["limits": .object(["maxPublishBps": .integer(1_000_000), "maxQueueBytes": .integer(4_194_304)])])
        f.answer(.success(V4vFixture.answer(grant: low)))
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
        XCTAssertTrue(f.last.error?.contains("Choose a lower quality") == true)
        XCTAssertNil(f.last.pending); XCTAssertTrue(f.owner.canChangeCaptureSettings)
        let requests = f.calls.count
        XCTAssertTrue(f.owner.prepare(captureProfile: .sd480p30)); f.flush()
        XCTAssertEqual(f.calls.count, requests); XCTAssertEqual(f.capture.reservations, 1)
        XCTAssertEqual(f.capture.ticket?.profile, .sd480p30); XCTAssertEqual(f.publisherProfiles, [.sd480p30])
        XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0)
        XCTAssertFalse(f.owner.canChangeCaptureSettings)
        f.owner.captureChanged(prepared: true, running: false, stopping: false); f.flush()
        XCTAssertTrue(f.owner.start()); f.flush(); XCTAssertEqual(f.capture.started, 1)
        XCTAssertEqual(f.capture.admission.currentPreparation?.profile, .sd480p30)
    }
    func testPendingPrepareRetainsExplicitProfileAcrossHTTPAndCallerChanges() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        var selected = CaptureIntent.sd480p30
        XCTAssertTrue(f.owner.prepare(captureProfile: selected)); selected = .hd720p30
        f.flush(); XCTAssertFalse(f.owner.canChangeCaptureSettings)
        f.answer(.success(V4vFixture.answer()))
        XCTAssertEqual(selected, .hd720p30)
        XCTAssertEqual(f.capture.ticket?.profile, .sd480p30); XCTAssertEqual(f.publisherProfiles, [.sd480p30])
        XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0)
    }
    func testPendingAudioOnlyPrepareBindsSameIntentToPublisherAndCaptureAtLowCap() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        var selected = CaptureIntent.audioOnly48kMono
        XCTAssertTrue(f.owner.prepare(captureProfile: selected)); selected = .hd720p30
        f.flush(); XCTAssertFalse(f.owner.canChangeCaptureSettings)
        let grant = V4vFixture.edit(V4vFixture.grant(), ["limits": .object(["maxPublishBps": .integer(1_000_000), "maxQueueBytes": .integer(4_194_304)])])
        f.answer(.success(V4vFixture.answer(grant: grant)))
        XCTAssertEqual(selected, .hd720p30)
        XCTAssertEqual(f.capture.ticket?.profile, .audioOnly48kMono)
        XCTAssertEqual(f.publisherProfiles, [.audioOnly48kMono])
        XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0)
        f.owner.captureChanged(prepared: true, running: false, stopping: false); f.flush()
        XCTAssertTrue(f.owner.start()); f.flush()
        XCTAssertEqual(f.capture.started, 1)
        XCTAssertEqual(f.capture.admission.currentPreparation?.profile, .audioOnly48kMono)
        XCTAssertNil(f.capture.admission.beginRotation(try XCTUnwrap(f.capture.ticket)))
    }
    func testEngineering1080CannotConsumeDefaultGrantOrCreateOwners() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare(captureProfile: .fullHD1080p30)); f.flush()
        f.answer(.success(V4vFixture.answer()))
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
        XCTAssertTrue(f.last.error?.contains("Choose a lower quality") == true)
        XCTAssertTrue(f.owner.canChangeCaptureSettings)
    }
    func testPublisherFactoryFailureReleasesQualityOnlyAfterRealCaptureCleanup() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        f.publisherError = .unavailable
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.answer(.success(V4vFixture.answer()))
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertEqual(f.capture.reservations, 1)
        XCTAssertEqual(f.capture.admission.currentPhase, .idle)
        XCTAssertEqual(f.calls.count, 3); XCTAssertNil(f.last.pending)
        XCTAssertTrue(f.owner.canChangeCaptureSettings)
        f.publisherError = nil
        XCTAssertTrue(f.owner.prepare(captureProfile: .sd480p30)); f.flush()
        XCTAssertEqual(f.capture.ticket?.profile, .sd480p30); XCTAssertFalse(f.owner.canChangeCaptureSettings)
    }
    func testPreparePersistsBeforeHTTPAndNeverStarts() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0); XCTAssertEqual(f.publishers.count, 1)
        XCTAssertEqual(f.calls.last?.method, "POST"); XCTAssertTrue(f.last.canStart)
        let saved = try XCTUnwrap(f.journal.document).object(required: ["version", "apiOrigin", "clockFloorMs", "terminalIntent"], optional: ["pubkey", "bearer", "bearerExpiresMs", "session", "grant", "grantPolicy", "grantExpiryMs"])
        XCTAssertNotNil(saved["grant"]); XCTAssertNil(f.last.error)
    }
    func testLostReplyRetainsExactNonceAndExplicitRetryDoesNotPrepareTwice() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare()); f.flush(); let first = try XCTUnwrap(f.calls.last)
        f.answer(.failure(.conflict)); XCTAssertEqual(f.last.pending, "prepare"); XCTAssertTrue(f.last.canRetry)
        XCTAssertTrue(f.owner.retryPending()); f.flush(); XCTAssertEqual(f.calls.last?.body, first.body)
        f.answer(.success(V4vFixture.answer())); XCTAssertTrue(f.publishers.isEmpty); XCTAssertEqual(f.capture.prepared, 0)
        // Retry resolves uncertainty; a separate explicit Prepare grants camera intent.
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.publishers.count, 1)
    }
    func testEndWhilePrepareUncertainResolvesExactThenEndsNoCapture() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.owner.end(); f.flush()
        f.answer(.success(V4vFixture.answer())); XCTAssertTrue(f.calls.last!.path.hasSuffix("/end"))
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertEqual(f.capture.prepared, 0)
        let origin = V4vFixture.origin(2, state: "revoked", recording: "never-started")
        f.answer(.success(V4vFixture.answer(V4vFixture.session(2, phase: "ended", origin: origin, complete: true), grant: nil)))
        XCTAssertNil(f.last.pending); XCTAssertEqual(f.last.phase, "ended"); XCTAssertTrue(f.last.endComplete); XCTAssertEqual(f.capture.globalStops, 0)
    }
    func testColdEndFencesQueuedStartAtActualAdmission() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.queue.async { entered.signal(); release.wait() }; entered.wait()
        XCTAssertTrue(f.owner.start()); f.owner.end()
        XCTAssertGreaterThan(f.capture.cancels, 0); XCTAssertEqual(f.capture.globalStops, 0)
        release.signal(); f.flush(); XCTAssertEqual(f.capture.started, 0)
    }
    func testInactiveLatePrepareAcknowledgementCannotAttachOrAutoStartOnReturn() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.owner.setActive(false); f.flush()
        f.answer(.success(V4vFixture.answer())); XCTAssertTrue(f.publishers.isEmpty)
        f.owner.setActive(true); f.flush(); XCTAssertTrue(f.publishers.isEmpty); XCTAssertEqual(f.capture.globalStops, 0)
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0)
    }
    func testRestoredGrantNeedsFreshRefreshThenExplicitPrepare() throws {
        let f = CreatorFixture(); try f.seed(active: true); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertTrue(f.publishers.isEmpty)
        XCTAssertTrue(f.owner.refresh()); f.flush()
        f.answer(.success(V4vFixture.answer(V4vFixture.session(2, expiry: V4vFixture.time + 90_000), grant: V4vFixture.grant(2, expiry: V4vFixture.time + 90_000))))
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.publishers.count, 1)
    }
    func testRemoteSessionWithoutJournalIsNotAdopted() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.reloadEvents()); f.flush()
        f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
        XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush(); f.answer(.success(V4vFixture.answer(grant: nil)))
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.calls.count, 2); XCTAssertTrue(f.publishers.isEmpty); XCTAssertNotNil(f.last.error)
    }
    func testExplicitRemoteEndUsesFreshExactStatusAndNeverCreatesCaptureAuthority() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.reloadEvents()); f.flush()
        f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
        XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush()
        f.answer(.success(V4vFixture.answer(grant: nil)))
        XCTAssertTrue(f.last.canEndExisting); XCTAssertFalse(f.last.canEnd); XCTAssertTrue(f.publishers.isEmpty)

        let confirmed = try XCTUnwrap(f.last.endExistingIdentity)
        XCTAssertTrue(f.owner.endExisting(expected: confirmed)); f.flush()
        XCTAssertEqual(f.calls.last?.method, "GET")
        XCTAssertEqual(f.calls.last?.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)")
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
        f.answer(.success(V4vFixture.answer(V4vFixture.session(2), grant: nil)))
        XCTAssertEqual(f.calls.last?.method, "POST"); XCTAssertTrue(f.calls.last?.path.hasSuffix("/\(V4vFixture.sid)/end") == true)
        let body = try V4vJSON.parse(XCTUnwrap(f.calls.last?.body)).object(required: ["revision", "idempotencyKey"])
        XCTAssertEqual(try body.uint("revision"), 2)
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertEqual(f.capture.prepared, 0); XCTAssertTrue(f.publishers.isEmpty)

        let origin = V4vFixture.origin(3, state: "revoked", recording: "never-started")
        f.answer(.success(V4vFixture.answer(V4vFixture.session(3, phase: "ended", origin: origin, complete: true), grant: nil)))
        XCTAssertTrue(f.last.endComplete); XCTAssertEqual(f.last.phase, "ended")
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
    }
    func testRemoteEndContradictionOrJournalFailureCannotAdoptOrSendEnd() throws {
        do {
            let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
            XCTAssertTrue(f.owner.reloadEvents()); f.flush()
            f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
            XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush(); f.answer(.success(V4vFixture.answer(grant: nil)))
            let confirmed = try XCTUnwrap(f.last.endExistingIdentity)
            XCTAssertTrue(f.owner.endExisting(expected: confirmed)); f.flush(); let before = f.calls.count
            let successor = V4vFixture.session(generation: 2, sid: String(repeating: "c", count: 32))
            f.answer(.success(V4vFixture.answer(successor, grant: nil)))
            XCTAssertEqual(f.calls.count, before); XCTAssertFalse(f.last.canEnd); XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
        }
        do {
            let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
            XCTAssertTrue(f.owner.reloadEvents()); f.flush()
            f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
            XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush(); f.answer(.success(V4vFixture.answer(grant: nil)))
            let confirmed = try XCTUnwrap(f.last.endExistingIdentity)
            XCTAssertTrue(f.owner.endExisting(expected: confirmed)); f.flush(); let before = f.calls.count
            f.queue.sync { f.journal.failSave = true }
            f.answer(.success(V4vFixture.answer(V4vFixture.session(2), grant: nil)))
            XCTAssertEqual(f.calls.count, before); XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
        }
    }
    func testStaleRemoteEndConfirmationCannotTargetAnotherIdentity() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.reloadEvents()); f.flush()
        f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
        XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush(); f.answer(.success(V4vFixture.answer(grant: nil)))
        let confirmed = try XCTUnwrap(f.last.endExistingIdentity)
        let successor = try V4vIdentity(eventId: confirmed.eventId, generation: confirmed.generation + 1,
                                        sessionId: String(repeating: "c", count: 32), revision: 1)
        let before = f.calls.count
        XCTAssertTrue(f.owner.endExisting(expected: successor)); f.flush()
        XCTAssertEqual(f.calls.count, before)
        let saved = try XCTUnwrap(f.journal.document).object(
            required: ["version", "apiOrigin", "clockFloorMs", "terminalIntent"],
            optional: ["pubkey", "bearer", "bearerExpiresMs"])
        XCTAssertNil(saved["session"])
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
    }
    func testRemoteEndRetryResponseAfterOriginalWindowCannotAdopt() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.reloadEvents()); f.flush()
        f.answer(.success(.object(["ok": .bool(true), "events": .array([.object(["id": .string(V4vFixture.event)])])])))
        XCTAssertTrue(f.owner.selectEvent(V4vFixture.event)); f.flush(); f.answer(.success(V4vFixture.answer(grant: nil)))
        let confirmed = try XCTUnwrap(f.last.endExistingIdentity)
        XCTAssertTrue(f.owner.endExisting(expected: confirmed)); f.flush()
        let firstReadCount = f.calls.count

        f.queue.sync { f.elapsed = 58_000 }
        f.answer(.failure(.unavailable))
        f.queue.sync { f.elapsed = 59_000 }
        f.queue.sync { f.alarm.fire() }; f.flush()
        XCTAssertEqual(f.calls.count, firstReadCount + 1, "the bounded retry should issue a fresh exact-SID read")
        XCTAssertEqual(f.calls.last?.path, "/api/live-events/\(V4vFixture.event)/native-session/\(V4vFixture.sid)")

        let before = f.journal.document
        f.queue.sync { f.elapsed = 60_001 }
        f.answer(.success(V4vFixture.answer(
            V4vFixture.session(2, expiry: V4vFixture.time + 120_000), grant: nil)))
        XCTAssertEqual(f.journal.document, before)
        XCTAssertEqual(f.calls.count, firstReadCount + 1, "an expired command cannot send End")
        XCTAssertEqual(f.capture.reservations, 0); XCTAssertTrue(f.publishers.isEmpty)
    }
    func testRefreshOnlyCommitsPublisherAfterDurableAcknowledgement() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        XCTAssertTrue(f.owner.refresh()); f.flush(); XCTAssertEqual(f.publishers[0].commits, 0)
        f.queue.sync { f.journal.failSave = true }
        f.answer(.success(V4vFixture.answer(V4vFixture.session(2), grant: V4vFixture.grant(2))))
        XCTAssertEqual(f.publishers[0].commits, 0); XCTAssertGreaterThan(f.publishers[0].ends, 0)
    }
    func testDueAutomaticRefreshSurvivesStartContention() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        XCTAssertNotNil(f.alarm.deadline)
        f.elapsed = 50_000
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.queue.async { entered.signal(); release.wait() }; entered.wait()
        f.queue.async { f.alarm.fire() }
        XCTAssertTrue(f.owner.start(), "Start should own cold admission before the queued alarm executes")
        release.signal(); f.flush()
        XCTAssertEqual(f.capture.started, 1)
        XCTAssertEqual(f.calls.last?.method, "POST")
        XCTAssertTrue(f.calls.last?.path.hasSuffix("/refresh") == true)
        XCTAssertEqual(f.last.pending, "refresh")
    }
    func testForegroundReconcilesWithheldOverdueRefreshExactlyOnce() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        let requests = f.calls.count
        XCTAssertFalse(f.owner.setSceneForeground(false, continuing: nil))
        f.elapsed = 50_000
        XCTAssertTrue(f.owner.setSceneForeground(true, continuing: nil)); f.flush()
        XCTAssertEqual(f.calls.count, requests + 1)
        XCTAssertTrue(f.calls.last?.path.hasSuffix("/refresh") == true)
        XCTAssertEqual(f.last.pending, "refresh")
        f.queue.sync { f.alarm.fire(times: 2) }; f.flush()
        XCTAssertEqual(f.calls.count, requests + 1, "resume and duplicate stale alarm delivery must coalesce")
    }
    func testDuplicateAutomaticRefreshAlarmsStageOneExactOperation() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        f.elapsed = 50_000; let requests = f.calls.count
        f.queue.sync { f.alarm.fire(times: 2) }; f.flush()
        XCTAssertEqual(f.calls.count, requests + 1)
        let first = try XCTUnwrap(f.calls.last?.body)
        f.answer(.failure(.conflict)); XCTAssertEqual(f.last.pending, "refresh")
        XCTAssertTrue(f.owner.retryPending()); f.flush()
        XCTAssertEqual(f.calls.last?.body, first, "a lost automatic reply must retain its exact durable nonce")
    }
    func testEndFencesQueuedAutomaticRefreshAlarm() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        f.elapsed = 50_000
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.queue.async { entered.signal(); release.wait() }; entered.wait()
        f.queue.async { f.alarm.fire() }
        f.owner.end(); release.signal(); f.flush()
        XCTAssertFalse(f.calls.contains(where: { $0.path.hasSuffix("/refresh") }))
        XCTAssertTrue(f.calls.last?.path.hasSuffix("/end") == true)
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
    }
    func testExactStopBetweenAlarmFenceAndAdmissionCannotRefreshStoppedBinding() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        let ticket = try XCTUnwrap(f.capture.ticket)
        f.elapsed = 50_000
        let clockEntered = DispatchSemaphore(value: 0), releaseClock = DispatchSemaphore(value: 0)
        let stopEntered = DispatchSemaphore(value: 0), releaseStop = DispatchSemaphore(value: 0)
        let alarmReturned = DispatchSemaphore(value: 0), stopReturned = DispatchSemaphore(value: 0)
        f.beforeNextClock {
            clockEntered.signal()
            _ = releaseClock.wait(timeout: .now() + 3)
        }
        f.alarm.afterNextFire { alarmReturned.signal() }
        f.publishers[0].onFirstEnd = {
            stopEntered.signal()
            _ = releaseStop.wait(timeout: .now() + 3)
        }
        f.queue.async { f.alarm.fire() }
        XCTAssertEqual(clockEntered.wait(timeout: .now() + 2), .success)
        DispatchQueue.global().async {
            _ = f.capture.stop(preparation: ticket)
            f.owner.captureStopped(preparation: ticket)
            stopReturned.signal()
        }
        XCTAssertEqual(stopEntered.wait(timeout: .now() + 2), .success)
        releaseClock.signal()
        XCTAssertEqual(alarmReturned.wait(timeout: .now() + 2), .success)
        releaseStop.signal()
        XCTAssertEqual(stopReturned.wait(timeout: .now() + 2), .success)
        f.flush()
        XCTAssertFalse(f.calls.contains(where: { $0.path.hasSuffix("/refresh") }))
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
        XCTAssertNil(f.capture.admission.currentPreparation)
    }
    func testExactStopAfterAutomaticAdmissionBeforeQueuedActionCannotRearmStoppedBinding() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        let ticket = try XCTUnwrap(f.capture.ticket)
        f.elapsed = 50_000
        let ownerEntered = DispatchSemaphore(value: 0), releaseOwner = DispatchSemaphore(value: 0)
        let admittedBlocked = DispatchSemaphore(value: 0), releaseAdmitted = DispatchSemaphore(value: 0)
        let stopEntered = DispatchSemaphore(value: 0), releaseStop = DispatchSemaphore(value: 0)
        let stopReturned = DispatchSemaphore(value: 0)
        f.publishers[0].onFirstEnd = {
            stopEntered.signal()
            _ = releaseStop.wait(timeout: .now() + 3)
        }
        f.queue.async {
            ownerEntered.signal()
            _ = releaseOwner.wait(timeout: .now() + 3)
        }
        XCTAssertEqual(ownerEntered.wait(timeout: .now() + 2), .success)
        f.queue.async { f.alarm.fire() }
        // This block was queued before the alarm creates its command body, so
        // reaching it proves the automatic refresh has cold-admitted while its
        // owner-queue action remains behind this barrier.
        f.queue.async {
            admittedBlocked.signal()
            _ = releaseAdmitted.wait(timeout: .now() + 3)
        }
        releaseOwner.signal()
        XCTAssertEqual(admittedBlocked.wait(timeout: .now() + 2), .success)
        DispatchQueue.global().async {
            _ = f.capture.stop(preparation: ticket)
            f.owner.captureStopped(preparation: ticket)
            stopReturned.signal()
        }
        XCTAssertEqual(stopEntered.wait(timeout: .now() + 2), .success)
        releaseAdmitted.signal()
        f.queue.sync {}
        releaseStop.signal()
        XCTAssertEqual(stopReturned.wait(timeout: .now() + 2), .success)
        f.flush()
        XCTAssertFalse(f.calls.contains(where: { $0.path.hasSuffix("/refresh") }))
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
        XCTAssertNil(f.capture.admission.currentPreparation)
    }
    func testExpiredWithheldRefreshFencesPublisherWithoutNetworkOrCapture() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        let requests = f.calls.count, reservations = f.capture.reservations
        XCTAssertFalse(f.owner.setSceneForeground(false, continuing: nil))
        f.elapsed = 60_001
        XCTAssertTrue(f.owner.setSceneForeground(true, continuing: nil)); f.flush()
        XCTAssertEqual(f.calls.count, requests)
        XCTAssertEqual(f.capture.reservations, reservations)
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
        XCTAssertNotNil(f.last.error)
    }
    func testFiniteWindowExpiredResponseCannotCreatePublisher() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.queue.sync { f.elapsed = 60_001 }
        f.answer(.success(V4vFixture.answer(V4vFixture.session(expiry: V4vFixture.time + 120_000), grant: V4vFixture.grant(expiry: V4vFixture.time + 120_000))))
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertEqual(f.capture.started, 0)
    }
    func testCleanupRetainsCapacityAndNeverStopsOtherCapture() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        f.publishers[0].autoClose = false
        f.owner.setActive(false); f.flush(); f.owner.setActive(true); f.flush()
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.publishers.count, 1)
        XCTAssertEqual(f.capture.globalStops, 0)
        f.queue.sync { f.publishers[0].closed() }; f.flush()
    }
    func testInactiveOrClosedOwnerStopCannotStopAnotherMode() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        f.owner.setActive(false); f.owner.stopCapture(); f.flush()
        XCTAssertEqual(f.capture.globalStops, 0)
        f.owner.close(); f.owner.stopCapture(); f.flush(); XCTAssertEqual(f.capture.globalStops, 0)
    }
    func testLateLoginCompletionCannotReleaseAnotherCommandsAdmission() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush()
        let old = try XCTUnwrap(f.login)
        f.owner.end(); f.flush() // no remote scope: terminal intent completes locally
        XCTAssertTrue(f.owner.reloadEvents()); f.flush(); let call = f.calls.last
        f.queue.sync { old.callback?(.failure(.closed)) }; f.flush()
        XCTAssertFalse(f.owner.reloadEvents()); XCTAssertEqual(f.calls.last?.path, call?.path)
        XCTAssertNotNil(f.client.reply, "the current read remains the sole in-flight call")
        f.answer(.success(.object(["ok": .bool(true), "events": .array([])])))
        XCTAssertTrue(f.owner.reloadEvents()); f.flush()
    }
    func testConsumedLoginFailureThenActualLateCloseReleasesRetainedOwner() throws {
        let f = CreatorFixture(); try f.open()
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush()
        let login = try XCTUnwrap(f.login)
        f.owner.close(); f.flush()
        f.queue.sync { login.callback?(.failure(.unavailable)) }; f.flush()
        weak var retained = f.owner; f.owner = nil; f.flush()
        XCTAssertNotNil(retained)
        login.cleanupHint(); f.flush(); XCTAssertNotNil(retained, "a notification alone is not cleanup")
        login.closed(); f.flush(); XCTAssertNil(retained)
    }
    func testActualSignetQueuedCloseAfterConsumedFailureReleasesCreatorOwner() throws {
        let f = CreatorFixture(), signer = CreatorActualSigner()
        let failed = expectation(description: "actual failure consumed"), blocked = expectation(description: "actual login work held")
        let closed = expectation(description: "actual late transition"), release = DispatchSemaphore(value: 0)
        var http: CreatorActualSignetHTTP?, reported = false
        f.onSnapshot = { value in
            if !value.signingIn, value.error != nil, !reported { reported = true; failed.fulfill() }
        }
        f.loginFactoryOverride = { _, profile, credentials, queue, clock, commit, cleanup in
            V4vSignetLoginOwner(profile: profile, credentials: credentials, delivery: queue,
                makeSigner: { _, _ in signer }, makeHTTP: { let next = CreatorActualSignetHTTP($0); http = next; return next },
                clock: { clock().monotonicNs }, unixSeconds: { clock().wallMs / 1000 },
                onCleanupComplete: { cleanup(); closed.fulfill() }, commit: commit)
        }
        try f.open(); XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only"))
        wait(for: [failed], timeout: 4)
        signer.onSign = { blocked.fulfill(); _ = release.wait(timeout: .now() + 3) }
        http?.reply?(.success(try XCTUnwrap(http?.challenge))); wait(for: [blocked], timeout: 2)
        weak var retained = f.owner
        f.owner.close(); f.flush(); f.owner = nil; f.flush()
        XCTAssertNotNil(retained)
        http?.onClosed(); f.flush()
        XCTAssertNotNil(retained, "queued close is not actual login-work completion")
        release.signal(); wait(for: [closed], timeout: 3); f.flush()
        XCTAssertNil(retained)
    }
    func testRefusedLoginHasNoOwnersAndDoesNotPoisonRetryOrClosure() throws {
        let f = CreatorFixture(); try f.open(); f.refuseLogin = true
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush()
        let refused = try XCTUnwrap(f.login)
        XCTAssertFalse(refused.cleanupComplete); XCTAssertTrue(refused.cancelled); XCTAssertTrue(f.last.canSignIn)
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush()
        XCTAssertFalse(f.login === refused); XCTAssertTrue(f.last.canSignIn)
        weak var retained = f.owner; f.owner.close(); f.flush(); f.owner = nil; f.flush()
        XCTAssertNil(retained, "a refused admission cannot hold the journal/owner cleanup barrier")
    }
    func testColdEndDuringLoginFactoryPreventsConsentAdmission() throws {
        let f = CreatorFixture(); try f.open(); defer { f.close() }
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.beforeLoginReturn = { _ in entered.signal(); release.wait() }
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); entered.wait()
        f.owner.end(); release.signal(); f.flush()
        XCTAssertEqual(f.login?.startCalls, 0); XCTAssertTrue(f.login?.cancelled == true)
        XCTAssertFalse(f.last.signingIn)
    }
    func testLoginUsesOriginalColdDeadlineAcrossQueueAndFactory() throws {
        let f = CreatorFixture(); try f.open(); defer { f.close() }
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.queue.async { entered.signal(); release.wait() }; entered.wait()
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only"))
        f.elapsed = 30_000; release.signal(); f.flush()
        XCTAssertEqual(f.login?.timeoutMs, 90_000)
        let other = CreatorFixture(); try other.open(); defer { other.close() }
        other.beforeLoginReturn = { _ in other.elapsed = 120_001 }
        XCTAssertTrue(other.owner.signIn(bunkerURI: "bunker://generated-only")); other.flush()
        XCTAssertEqual(other.login?.startCalls, 0); XCTAssertTrue(other.login?.cancelled == true)
    }
    func testOwnerRetainsAppSlotUntilActualLateObservationCleanup() throws {
        let f = CreatorFixture(); try f.seed(); try f.open()
        let status = CreatorHeldStatus(), unused = CreatorUnusedProbe()
        f.withPublisherContext = { slot in
            let grant = try V4vGrant(V4vFixture.grant(), nowMs: V4vFixture.time)
            let attempt = try V4vObservedAttempt(local: OriginAttemptIdentity(destination: 1, codecEpoch: 1, token: 1, sdkGeneration: 1, sdkAttempt: 1), grant: grant, nonce: V4vFixture.key)
            let gate = OriginConfirmationGate()
            try gate.stage(OriginCurrentAuthority(attempt: attempt, grant: grant, expiresAtNs: f.base + 60_000_000_000), clock: { f.clock().monotonicNs })
            XCTAssertTrue(gate.activate(attempt, clock: { f.clock().monotonicNs }))
            slot.submit(V4vObservationRequest(attempt: attempt, gate: gate, deadlineNs: f.base + 8_000_000_000, make: {
                try V4vOriginObserver(attempt: attempt, profile: V4vFixture.profile(), gate: gate, status: status, media: unused, decoder: unused, owner: f.queue, clock: f.clock, confirm: { _, _ in XCTFail("No confirmation expected") })
            }, completed: { _ in }))
        }
        try f.prepared(); f.withPublisherContext = nil
        weak var retained = f.owner
        f.owner.close(); f.flush(); f.queue.sync { f.client.closed() }; f.flush()
        f.owner = nil; f.flush()
        XCTAssertNotNil(retained, "semantic cancellation cannot release app-wide probe capacity")
        f.queue.sync { status.completeCleanup() }; f.flush()
        XCTAssertNil(retained, "actual late closure releases the retained owner")
    }
    func testCorruptJournalFailsClosedWithoutSignerHTTPOrCapture() throws {
        let f = CreatorFixture(); f.journal.document = .object(["bad": .bool(true)]); try f.open(); defer { f.close() }
        XCTAssertFalse(f.last.ready); XCTAssertNotNil(f.last.error); XCTAssertTrue(f.calls.isEmpty); XCTAssertNil(f.login)
        XCTAssertFalse(f.last.canPrepare); XCTAssertEqual(f.capture.prepared, 0)
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush(); XCTAssertNil(f.login)
    }
    func testLoginIsExplicitAndLateCommitAfterEndRefused() throws {
        let f = CreatorFixture(); try f.open(); defer { f.close() }
        XCTAssertFalse(f.owner.signIn(bunkerURI: String(repeating: "x", count: 8193)))
        XCTAssertFalse(f.owner.selectEvent("../not-an-event")); XCTAssertNil(f.login)
        XCTAssertTrue(f.owner.signIn(bunkerURI: "bunker://generated-only")); f.flush(); let login = try XCTUnwrap(f.login)
        f.owner.end(); f.flush(); XCTAssertTrue(login.cancelled)
        let persona = String(repeating: "d", count: 64)
        let result = try V4vLoginResult(.object(["ok": .bool(true), "pubkey": .string(persona), "registered": .bool(true), "operator": .bool(false), "sessionToken": .string("generated-only"), "sessionTokenMaxAgeSec": .integer(3600)]), persona: persona)
        try f.queue.sync { XCTAssertThrowsError(try login.commit(result)) }
        XCTAssertEqual(f.capture.started, 0); XCTAssertNil(f.last.publicPersona)
    }
    func testEndPreservesAlreadyRunningRecordingAdmission() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.prepared()
        XCTAssertTrue(f.owner.start()); f.flush(); XCTAssertEqual(f.capture.started, 1)
        f.owner.captureChanged(prepared: false, running: true, stopping: false); f.flush()
        f.owner.end(); f.flush()
        XCTAssertNotNil(f.capture.admission.beginRotation(try XCTUnwrap(f.capture.ticket)))
        XCTAssertEqual(f.capture.globalStops, 0)
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
    }
    func testEndBeforeAttachmentCompletionCannotPrepareCamera() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        f.capture.holdAttach = true
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.answer(.success(V4vFixture.answer()))
        f.owner.end(); f.flush(); f.capture.pendingAttach?(true); f.flush()
        XCTAssertEqual(f.capture.prepared, 0); XCTAssertEqual(f.capture.started, 0); XCTAssertEqual(f.capture.globalStops, 0)
    }
    func testEndDuringFactoryRetainsRealCandidateUntilActualCleanup() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        f.beforePublisherReturn = { publisher in publisher.autoClose = false; entered.signal(); release.wait() }
        XCTAssertTrue(f.owner.prepare()); f.flush()
        f.queue.async { f.client.answer(.success(V4vFixture.answer())) }
        entered.wait(); f.owner.end()
        // Actual production-shared admission retains the unfinished factory.
        XCTAssertNil(f.capture.admission.reservePreparation())
        release.signal(); f.flush(); XCTAssertEqual(f.capture.prepared, 0)
        XCTAssertFalse(f.owner.reloadEvents(), "priority End must retain admission while its request is in flight")
        XCTAssertNil(f.capture.admission.reservePreparation())
        XCTAssertFalse(f.publishers[0].cleanupComplete)
        let origin = V4vFixture.origin(2, state: "revoked", recording: "never-started")
        f.answer(.success(V4vFixture.answer(V4vFixture.session(2, phase: "ended", origin: origin, complete: true), grant: nil)))
        XCTAssertFalse(f.owner.canChangeCaptureSettings, "uncertain candidate cleanup still retains quality admission")
        f.queue.sync { f.publishers[0].closed() }; f.flush()
        XCTAssertTrue(f.owner.canChangeCaptureSettings, "a stale cold ticket cannot retain actual closed capacity")
        XCTAssertNotNil(f.capture.admission.reservePreparation())
        XCTAssertEqual(f.capture.globalStops, 0)
    }
    func testInitialIdleEchoBeforeActualAttachmentCompletionDoesNotRetirePublisher() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        f.capture.beforeAttachCompletion = { f.owner.captureChanged(prepared: false, running: false, stopping: false) }
        XCTAssertTrue(f.owner.prepare()); f.flush(); f.answer(.success(V4vFixture.answer()))
        XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.publishers[0].ends, 0)
        // A real Preparing->failure sequence still retires even when coalesced.
        f.owner.captureChanged(prepared: false, running: false, stopping: true)
        f.owner.captureChanged(prepared: false, running: false, stopping: false); f.flush()
        XCTAssertGreaterThan(f.publishers[0].ends, 0)
    }
    func testJournalStageFailureCannotSendPrepareOrConstructPublisher() throws {
        let f = CreatorFixture(); try f.seed(); try f.open(); defer { f.close() }; try f.events()
        f.queue.sync { f.journal.failSave = true }
        XCTAssertTrue(f.owner.prepare()); f.flush(); XCTAssertEqual(f.calls.count, 2)
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertNotNil(f.last.error)
    }
    func testUncertainReadCannotDiscardSavedPendingMutation() throws {
        let f = CreatorFixture(); try f.seed(pending: true); try f.open(); defer { f.close() }
        XCTAssertEqual(f.last.pending, "prepare"); XCTAssertTrue(f.owner.reloadEvents()); f.flush()
        f.answer(.success(.object(["ok": .bool(true), "events": .array([])])))
        XCTAssertEqual(f.last.pending, "prepare"); XCTAssertTrue(f.owner.retryPending()); f.flush()
        let body = try V4vJSON.parse(XCTUnwrap(f.calls.last?.body)).object(required: ["expectedEventGeneration", "idempotencyKey"])
        XCTAssertEqual(try body.text("idempotencyKey"), V4vFixture.key)
    }
    func testRecoveryAcknowledgementNeedsFreshStatusAndSeparateReplacement() throws {
        let f = CreatorFixture(); try f.seed(active: true)
        let e = try V4vSessionEngine(profile: V4vFixture.profile(), store: f.journal, clock: f.clock(), fencePublisher: {})
        try e.requestEnd(clock: f.clock()); try e.continueEnd(key: V4vFixture.key, clock: f.clock())
        let origin = V4vFixture.origin(2, state: "revoked", recording: "incomplete")
        let ended = V4vFixture.session(2, phase: "ended", origin: origin)
        try e.applyResponse(V4vFixture.answer(ended, grant: nil), for: XCTUnwrap(e.pending), clock: f.clock())
        try f.open(); defer { f.close() }
        XCTAssertFalse(f.last.endComplete)
        XCTAssertTrue(f.owner.acknowledgeRecovery()); f.flush(); XCTAssertTrue(f.calls.isEmpty)
        XCTAssertTrue(f.owner.status()); f.flush(); f.answer(.success(V4vFixture.answer(ended, grant: nil)))
        XCTAssertTrue(f.last.canAcknowledge); XCTAssertTrue(f.owner.acknowledgeRecovery()); f.flush()
        let body = try V4vJSON.parse(XCTUnwrap(f.calls.last?.body)).object(required: ["eventGeneration", "revision", "idempotencyKey", "acknowledgement"])
        let ackKey = try body.text("idempotencyKey")
        let recovery: V4vJSON = .object(["state": .string("acknowledged"), "acknowledgementKey": .string(ackKey), "eventGeneration": .integer(1), "revision": .integer(2), "explicitPrepareRequired": .bool(true), "origin": origin])
        f.answer(.success(V4vFixture.answer(V4vFixture.edit(ended, ["recovery": recovery]), grant: nil)))
        XCTAssertTrue(f.publishers.isEmpty); XCTAssertTrue(f.last.canReplace); XCTAssertEqual(f.capture.started, 0)
        XCTAssertTrue(f.owner.prepare(replacingAcknowledged: true)); f.flush()
        let replacement = try V4vJSON.parse(XCTUnwrap(f.calls.last?.body)).object(required: ["expectedEventGeneration", "idempotencyKey"], optional: ["replaces"])
        XCTAssertEqual(try replacement.uint("expectedEventGeneration"), 1); XCTAssertNotNil(replacement["replaces"])
        let successor = String(repeating: "c", count: 32)
        f.answer(.success(V4vFixture.answer(V4vFixture.session(generation: 2, sid: successor), grant: V4vFixture.grant(generation: 2, sid: successor))))
        XCTAssertEqual(f.publishers.count, 1); XCTAssertEqual(f.capture.prepared, 1); XCTAssertEqual(f.capture.started, 0)
    }
}
