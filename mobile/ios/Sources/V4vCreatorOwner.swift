import Foundation

/// The camera owner enforces these exact tickets again after every asynchronous
/// permission/start boundary. Cancelling one ticket must never stop another mode.
protocol V4vCreatorCapturePort: AnyObject {
    var canChooseCaptureProfile: Bool { get }
    func reservePreparation(profile: CaptureIntent) -> CaptureAdmission.Preparation?
    func abandonPreparation(_ ticket: CaptureAdmission.Preparation)
    func setDestination(_ sink: EncodedPublishingSink?, preparation: CaptureAdmission.Preparation, completion: @escaping (Bool) -> Void)
    func prepare(_ ticket: CaptureAdmission.Preparation)
    func start(preparation: CaptureAdmission.Preparation)
    func cancelPendingPreparation(_ ticket: CaptureAdmission.Preparation)
    func whenCleanupComplete(_ ticket: CaptureAdmission.Preparation, completion: @escaping () -> Void)
    func stop()
    @discardableResult func stop(preparation: CaptureAdmission.Preparation) -> Bool
}

protocol V4vCreatorHTTP: AnyObject {
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void)
    func close()
    var cleanupComplete: Bool { get }
}
extension V4vControlClient: V4vCreatorHTTP {}
protocol V4vCreatorAlarm: AnyObject {
    func schedule(atNs deadline: UInt64?, generation: UInt64, fire: @escaping (UInt64) -> Void)
    func cancel()
}
extension NativeDeadlineTimer: V4vCreatorAlarm {}
protocol V4vCreatorLogin: AnyObject {
    func start(restoring: Bool, timeoutMs: UInt32, completion: @escaping (Result<Void, V4vFailure>) -> Void) -> Bool
    func cancel()
    var cleanupComplete: Bool { get }
}
extension V4vSignetLoginOwner: V4vCreatorLogin {}
protocol V4vCreatorPublisher: EncodedPublishingSink {
    func committed(_ permit: V4vPublishingGrantPermit) throws
}
extension V4vObservedPublisher: V4vCreatorPublisher {}

/// One app-lifetime cold owner. The engine, journal, primary HTTP and observer
/// orchestration share `owner`; compressed media never passes through this queue.
final class V4vCreatorOwner: @unchecked Sendable {
    struct Snapshot {
        var ready = false, active = true
        var account = "Not signed in"
        var publicPersona: String?
        var signingIn = false
        var events: [V4vCreatorEvent] = []
        var selectedEvent: String?
        var phase = "Loading"
        var pending: String?
        var expiresAtUnixMs: UInt64?
        var endComplete = false
        var message = "Loading creator state"
        var error: String?
        var canSignIn = false, canCancelSignIn = false, canReloadEvents = false, canSelectEvent = false
        var canPrepare = false, canStart = false, canRefresh = false, canStatus = false, canEnd = false, canEndExisting = false
        var endExistingIdentity: V4vIdentity?
        var canRetry = false, canAcknowledge = false, canReplace = false, closing = false
    }
    typealias HTTPFactory = (@escaping () -> Void) -> V4vCreatorHTTP
    typealias LoginFactory = (String, V4vConnectorProfile, V4vSignetCredentialStore, DispatchQueue,
                              @escaping () -> V4vClock, @escaping (V4vLoginResult) throws -> Void,
                              @escaping () -> Void) throws -> V4vCreatorLogin
    typealias PublisherFactory = (UInt64, V4vPublishingGrantPermit, V4vSessionEngine, DispatchQueue,
                                  V4vObservationSlot, @escaping () -> V4vClock, CaptureIntent) throws -> V4vCreatorPublisher

    /// Scalar admission only. No IO, callbacks, HTTP or camera work under lock.
    private let gate = NSLock()
    private var enabled = true, closed = false, busy = false, endQueued = false
    private var sceneForeground = true
    private var continuingAudio: CaptureAdmission.Preparation?
    private var endNeedsStaging = false
    private var generation: UInt64 = 1
    private var commandSerial: UInt64 = 0, busyCommand: UInt64?
    private var coldTicket: CaptureAdmission.Preparation?
    private var coldPublisher: V4vCreatorPublisher?
    private var coldLogin: V4vCreatorLogin?
    private var latestSnapshot: Snapshot?, snapshotQueued = false
    private var modeUpdateQueued = false
    private let profile: V4vConnectorProfile, journal: V4vJournalStore, credentials: V4vSignetCredentialStore
    private let capture: V4vCreatorCapturePort
    private let owner: DispatchQueue, delivery: DispatchQueue
    private let clock: () -> V4vClock, changed: (Snapshot) -> Void
    private let makeHTTP: HTTPFactory, makeLogin: LoginFactory, makePublisher: PublisherFactory
    private let timer: V4vCreatorAlarm
    private let slot: V4vObservationSlot
    private var engine: V4vSessionEngine?
    private var http: V4vCreatorHTTP?
    private var login: V4vCreatorLogin?
    private var retainedSelf: V4vCreatorOwner?
    private var leaseRetention: V4vCreatorOwner?
    private var showLease: (UUID, CaptureAdmission.Preparation)? // owner queue only
    private var value = Snapshot()
    private var current: V4vCurrentSession?
    private var binding: Binding?
    private var nextDestination: UInt64 = 0
    private var inflight: UInt64?
    private var requestSerial: UInt64 = 0
    private var window: Window?
    private var refreshPlan: RefreshPlan?
    private var capturePrepared = false, captureRunning = false, captureStopping = false
    private var prepareIntent: (generation: UInt64, profile: CaptureIntent)?
    private var ending = false
    private var slotClosed = false
    private var actorCommand: UInt64 = 0
    private var retiringBinding = false
    private var commandStartedNs: UInt64 = 0
    private var lastFreshStatus: (V4vIdentity, UInt64)?
    private struct Binding {
        let ticket: CaptureAdmission.Preparation
        let generation: UInt64
        let publisher: V4vCreatorPublisher
        var permit: V4vPublishingGrantPermit
        var sawCaptureActivity = false
    }
    private struct RefreshPlan: Equatable {
        let due: UInt64
        let generation: UInt64
        let ticket: CaptureAdmission.Preparation
    }
    private struct Window { let end: UInt64; let command: UInt64; var attempts = 0 }
    private enum Read { case events, current(String), status(V4vIdentity), remoteEnd(V4vRemoteEndRead) }
    private enum Purpose { case mutation(V4vOperation), read(Read) }

    init(profile: V4vConnectorProfile, journal: V4vJournalStore, credentials: V4vSignetCredentialStore,
         capture: V4vCreatorCapturePort, owner: DispatchQueue, clock: @escaping () -> V4vClock,
         delivery: DispatchQueue = .main, changed: @escaping (Snapshot) -> Void,
         alarm: V4vCreatorAlarm? = nil,
         makeHTTP: HTTPFactory? = nil, makeLogin: LoginFactory? = nil, makePublisher: PublisherFactory? = nil) {
        self.profile = profile; self.journal = journal; self.credentials = credentials; self.capture = capture
        self.owner = owner; self.clock = clock; self.delivery = delivery; self.changed = changed
        self.makeHTTP = makeHTTP ?? { V4vControlClient(profile: profile, onClosed: $0) }
        self.makeLogin = makeLogin ?? { uri, profile, credentials, queue, clock, commit, cleanupHint in
            V4vSignetLoginOwner(profile: profile, credentials: credentials, delivery: queue,
                makeSigner: { saved, remaining in
                    #if canImport(SignetAuth)
                    return try V4vNativeSignet(bunkerURI: uri, restoredCredential: saved, remainingMs: remaining)
                    #else
                    throw V4vFailure.unavailable
                    #endif
                }, makeHTTP: { V4vControlClient(profile: profile, onClosed: $0) },
                clock: { clock().monotonicNs }, unixSeconds: { clock().wallMs / 1000 },
                onCleanupComplete: cleanupHint, commit: commit)
        }
        self.makePublisher = makePublisher ?? { destination, permit, engine, queue, slot, clock, captureProfile in
            try V4vObservedPublisher(destination: destination, permit: permit, profile: profile,
                engine: engine, owner: queue, slot: slot, clock: clock, captureProfile: captureProfile)
        }
        timer = alarm ?? NativeDeadlineTimer(queue: owner, now: { clock().monotonicNs })
        slot = V4vObservationSlot(owner: owner, clock: clock)
        owner.async { [self] in bootstrap() }
    }
    private func bootstrap() {
        do {
            engine = try V4vSessionEngine(profile: profile, store: journal, clock: clock(), fencePublisher: { [weak self] in self?.fenceCaptured() })
            value.ready = true; value.message = "Choose an event or reconcile the saved session"
            if let session = engine?.session { value.selectedEvent = session.identity.eventId }
            value.account = (try? engine?.creatorEventsRequest(clock: clock())) != nil ? "Saved session" : "Not signed in"
            publish()
        } catch { fail(error) }
    }
    /// One ordinary command is admitted before dispatch. Bool is admission,
    /// never HTTP success, authority, capture readiness or decoded proof.
    private func command(allowBackgroundRefresh: Bool = false, _ action: @escaping (UInt64) throws -> Void) -> Bool {
        let started = clock().monotonicNs
        gate.lock()
        guard !closed, enabled, (sceneForeground || (allowBackgroundRefresh && continuingAudio != nil && continuingAudio == coldTicket)),
              !busy, !endQueued, commandSerial < UInt64.max else { gate.unlock(); return false }
        commandSerial += 1; let admitted = commandSerial
        busy = true; busyCommand = admitted; let token = generation; gate.unlock()
        owner.async { [self] in
            gate.lock()
            let admittedHere = !closed && enabled && generation == token && (sceneForeground ||
                (allowBackgroundRefresh && continuingAudio != nil && continuingAudio == coldTicket))
            gate.unlock()
            guard admittedHere else { releaseCommand(admitted); return }
            commandStartedNs = started; actorCommand = admitted
            do { try action(token) } catch { fail(error); releaseCommand() }
        }
        return true
    }
    private func live(_ token: UInt64) -> Bool {
        gate.lock(); defer { gate.unlock() }; return !closed && enabled && token == generation
    }
    private func state() -> (active: Bool, closed: Bool, busy: Bool, generation: UInt64, ending: Bool, foreground: Bool) {
        gate.lock(); defer { gate.unlock() }; return (enabled, closed, busy, generation, endQueued, sceneForeground)
    }
    private func releaseCommand(_ captured: UInt64? = nil) {
        let expected = captured ?? actorCommand
        gate.lock(); if busyCommand == expected { busy = false; busyCommand = nil }; gate.unlock(); publish()
        armRefresh()
    }
    private func beginWindow() throws {
        let end = commandStartedNs.addingReportingOverflow(60_000_000_000)
        guard !end.overflow else { throw V4vFailure.bounds }
        window = Window(end: end.partialValue, command: actorCommand)
    }
    @discardableResult func signIn(bunkerURI: String, restoring: Bool = false) -> Bool {
        guard bunkerURI.utf8.count <= 8192, !bunkerURI.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return false }
        return command { [self] token in
            let admitted = actorCommand
            let sum = commandStartedNs.addingReportingOverflow(120_000_000_000)
            guard !sum.overflow else { throw V4vFailure.bounds }
            let loginDeadline = sum.partialValue
            _ = try remainingLoginMs(deadline: loginDeadline)
            guard value.ready, engine != nil, !capturePrepared, !captureRunning, !captureStopping, binding == nil,
                  login == nil || login?.cleanupComplete == true else { throw V4vFailure.busy }
            let next = try makeLogin(bunkerURI, profile, credentials, owner, clock, { [weak self] result in
                guard let self, self.live(token), self.clock().monotonicNs < loginDeadline, let engine = self.engine else { throw V4vFailure.closed }
                try engine.installAuthenticatedSession(pubkey: result.persona, token: result.token, maxAgeSeconds: result.maxAgeSeconds, clock: self.clock())
                self.value.publicPersona = result.persona; self.value.account = "Signed in"
            }, { [weak self] in self?.owner.async { [weak self] in self?.finishClose() } })
            let remaining: UInt32
            do { remaining = try remainingLoginMs(deadline: loginDeadline) }
            catch { next.cancel(); throw error }
            gate.lock()
            let registered = !closed && enabled && sceneForeground && generation == token
            if registered { coldLogin = next }
            gate.unlock()
            guard registered else { next.cancel(); throw V4vFailure.closed }
            login = next; value.signingIn = true; publish()
            guard next.start(restoring: restoring, timeoutMs: remaining, completion: { [weak self, weak next] result in
                guard let self else { return }
                self.owner.async {
                    guard self.login === next else { return }
                    self.value.signingIn = false
                    if self.live(token) { if case let .failure(error) = result { self.fail(error) } }
                    self.releaseCommand(admitted); self.finishClose()
                }
            }) else {
                // start(false) admits no owners. Keeping this unadmitted object
                // would wait forever for a cleanup event it cannot produce.
                next.cancel(); if login === next { login = nil }
                gate.lock(); if coldLogin === next { coldLogin = nil }; gate.unlock()
                value.signingIn = false; throw V4vFailure.busy
            }
        }
    }
    private func remainingLoginMs(deadline: UInt64) throws -> UInt32 {
        let now = clock().monotonicNs
        guard now >= commandStartedNs, now < deadline, (deadline - now) / 1_000_000 > 0 else { throw V4vFailure.unavailable }
        return UInt32(min(120_000, (deadline - now) / 1_000_000))
    }
    func cancelSignIn() { gate.lock(); let next = coldLogin; gate.unlock(); next?.cancel() }
    @discardableResult func reloadEvents() -> Bool { command { [self] _ in try read(.events) } }
    @discardableResult func selectEvent(_ id: String) -> Bool {
        guard (try? V4vIdentity.event(id)) != nil else { return false }
        return command { [self] _ in
            guard binding == nil, !capturePrepared, !captureRunning, !captureStopping,
                  value.events.contains(where: { $0.id == id }), engine?.pending == nil else { throw V4vFailure.busy }
            value.selectedEvent = id; current = nil; try read(.current(id))
        }
    }
    @discardableResult func status() -> Bool {
        command { [self] _ in guard let identity = engine?.session?.identity else { throw V4vFailure.terminal }; try read(.status(identity)) }
    }
    /// The explicit remote-End control is the only path from a collection row
    /// to local durable state. It first performs a short-lived exact-SID read.
    @discardableResult func endExisting(expected: V4vIdentity) -> Bool {
        command { [self] _ in
            guard let engine, engine.session == nil, engine.pending == nil, binding == nil,
                  !capturePrepared, !captureRunning, !captureStopping,
                  let selected = value.selectedEvent, let remote = current?.receipt,
                  remote.identity == expected, remote.identity.eventId == selected,
                  remote.phase == "active", !remote.terminal, !remote.endComplete else { throw V4vFailure.conflict }
            let read = try engine.remoteEndStatusRequest(identity: remote.identity, clock: clock())
            try beginWindow(); send(read.call, purpose: .read(.remoteEnd(read)))
        }
    }
    /// This scalar admission is authoritative even while the UI snapshot lags.
    var canChangeCaptureSettings: Bool {
        gate.lock(); let free = !closed && !busy && !endQueued; gate.unlock()
        // A failed or late candidate may have no adopted Binding. Only the
        // actual capture cleanup barrier can release its preparation capacity.
        return free && capture.canChooseCaptureProfile
    }
    @discardableResult func prepare(captureProfile: CaptureIntent = .defaultProfile, replacingAcknowledged: Bool = false) -> Bool {
        command { [self] token in
            guard let engine, binding == nil, !capturePrepared, !captureRunning, !captureStopping, engine.pending == nil,
                  let selected = value.selectedEvent else { throw V4vFailure.busy }
            // A saved acknowledged grant can prepare a fresh local camera epoch;
            // restore cannot supply this permit until a fresh durable refresh.
            if let session = engine.session, !session.terminal, session.identity.eventId == selected {
                try attach(engine.publishingGrant(clock: clock()), token: token, captureProfile: captureProfile); releaseCommand(); return
            }
            if let remote = current?.receipt {
                guard let local = engine.session, local.identity.sameSession(remote.identity) else { throw V4vFailure.conflict }
            }
            let expectedGeneration: UInt64
            if let local = engine.session, local.identity.eventId == selected { expectedGeneration = local.identity.generation }
            else if let current, current.receipt == nil { expectedGeneration = current.generation }
            else { throw V4vFailure.conflict }
            try engine.stagePrepare(event: selected, expectedGeneration: expectedGeneration, key: nonce(), replacingAcknowledged: replacingAcknowledged, clock: clock())
            prepareIntent = (token, captureProfile)
            do { try beginWindow(); pump() }
            catch { prepareIntent = nil; throw error }
        }
    }
    @discardableResult func start() -> Bool {
        command { [self] _ in
            guard let engine, let binding, capturePrepared, !captureRunning, !captureStopping, engine.pending == nil, !engine.terminalIntent else { throw V4vFailure.terminal }
            let permit = try engine.publishingGrant(clock: clock())
            guard binding.permit.grant.immutableMatches(permit.grant), clock().monotonicNs < binding.permit.expiresAtNs else { throw V4vFailure.terminal }
            try binding.ticket.profile.requireGrant(maxPublishBps: permit.grant.maxPublishBps)
            capture.start(preparation: binding.ticket); releaseCommand()
        }
    }
    @discardableResult func refresh() -> Bool {
        command(allowBackgroundRefresh: true) { [self] _ in
            guard let engine else { throw V4vFailure.closed }
            try engine.stageRefresh(key: nonce(), clock: clock()); try beginWindow(); pump()
        }
    }
    @discardableResult func retryPending() -> Bool {
        command { [self] _ in guard engine?.pending != nil || engine?.terminalIntent == true else { throw V4vFailure.terminal }; try beginWindow(); pump() }
    }
    @discardableResult func acknowledgeRecovery() -> Bool {
        command { [self] _ in
            guard let engine, let identity = engine.session?.identity, let fresh = lastFreshStatus,
                  fresh.0 == identity, clock().monotonicNs >= fresh.1, clock().monotonicNs - fresh.1 <= 8_000_000_000 else { throw V4vFailure.conflict }
            try engine.stageAcknowledgement(key: nonce(), clock: clock()); try beginWindow(); pump()
        }
    }
    /// Cold fences happen before this can wait behind journal IO or consent.
    /// The exact old publisher/ticket remain captured; never call global Stop.
    func end() {
        let started = clock().monotonicNs
        gate.lock()
        guard !closed, !endQueued else { gate.unlock(); return }
        generation &+= 1; endQueued = true; endNeedsStaging = true
        let ticket = coldTicket, publisher = coldPublisher, login = coldLogin
        gate.unlock()
        if let ticket { capture.cancelPendingPreparation(ticket) }
        publisher?.endDestination(); login?.cancel()
        owner.async { [self] in
            gate.lock(); endNeedsStaging = false; gate.unlock()
            prepareIntent = nil; refreshPlan = nil; ending = true
            commandStartedNs = started
            do { guard let engine else { throw V4vFailure.journal }; try engine.requestEnd(clock: clock()); if window == nil { try beginWindow() }; if inflight == nil { pump() } }
            catch { fail(error); completeWindow() }
            publish()
        }
    }
    func stopCapture() {
        let cold = state(); guard cold.active, !cold.closed else { return }
        gate.lock(); let ticket = coldTicket; gate.unlock()
        guard let ticket, capture.stop(preparation: ticket) else { return }
        captureStopped(preparation: ticket)
    }
    /// The app's exact capture Stop already won. Never look up a different
    /// capture or publisher when this command eventually reaches the owner.
    func captureStopped(preparation ticket: CaptureAdmission.Preparation) {
        gate.lock()
        guard coldTicket == ticket, !closed else { gate.unlock(); return }
        generation &+= 1; let publisher = coldPublisher
        continuingAudio = nil
        gate.unlock()
        publisher?.endDestination()
        owner.async { [self] in
            guard binding?.ticket == ticket else { return }
            prepareIntent = nil; refreshPlan = nil; retireBinding(); publish()
        }
    }
    func setActive(_ active: Bool) {
        gate.lock(); guard !closed, enabled != active else { gate.unlock(); return }; enabled = active; gate.unlock()
        if !active { retireAuthority() }
        gate.lock(); guard !modeUpdateQueued else { gate.unlock(); return }; modeUpdateQueued = true; gate.unlock()
        owner.async { [self] in
            gate.lock(); modeUpdateQueued = false; let active = enabled; gate.unlock()
            prepareIntent = nil
            if !active { refreshPlan = nil; retireBinding() }
            else if engine?.pending != nil || engine?.terminalIntent == true { if window == nil { commandStartedNs = clock().monotonicNs; try? beginWindow() }; pump() }
            publish()
        }
    }
    /// Called by the single app model, never by view disappearance. The exact
    /// CaptureAdmission committed-run decision controls existing publishing;
    /// this does not permit sign-in, event selection or a new background Start.
    @discardableResult func setSceneForeground(_ active: Bool, continuing ticket: CaptureAdmission.Preparation?) -> Bool {
        gate.lock()
        let awakening = active && !sceneForeground
        sceneForeground = active
        continuingAudio = !active && ticket?.continuesAudioInBackground == true && ticket == coldTicket ? ticket : nil
        let permitted = active || continuingAudio != nil
        let signing = !active ? coldLogin : nil
        let publisher = awakening ? coldPublisher : nil
        gate.unlock()
        signing?.cancel()
        publisher?.revalidateAuthority()
        if awakening { owner.async { [weak self] in self?.armRefresh() } }
        return permitted
    }
    private func retireAuthority() {
        gate.lock(); generation &+= 1; let ticket = coldTicket, publisher = coldPublisher, login = coldLogin; gate.unlock()
        if let ticket { capture.cancelPendingPreparation(ticket) }; publisher?.endDestination(); login?.cancel()
    }
    private func fenceCaptured() {
        gate.lock(); let ticket = coldTicket, publisher = coldPublisher; gate.unlock()
        if let ticket { capture.cancelPendingPreparation(ticket) }; publisher?.endDestination()
    }
    func captureChanged(prepared: Bool, running: Bool, stopping: Bool) {
        // At most one pending scalar update, even if a UI sends repeated changes.
        gate.lock(); latestCapture = (prepared, running, stopping)
        captureActivityPending = captureActivityPending || prepared || running || stopping
        guard !captureUpdateQueued else { gate.unlock(); return }; captureUpdateQueued = true; gate.unlock()
        owner.async { [self] in
            gate.lock(); let next = latestCapture, sawActivity = captureActivityPending
            captureActivityPending = false; captureUpdateQueued = false; gate.unlock()
            capturePrepared = next.0; captureRunning = next.1; captureStopping = next.2
            if sawActivity { binding?.sawCaptureActivity = true }
            // setDestination emits its previous Idle snapshot before its
            // completion schedules Prepare. That echo is not a failed epoch.
            if !next.0 && !next.1 && !next.2, binding?.sawCaptureActivity == true { retireBinding() }
            publish()
        }
    }
    private var latestCapture = (false, false, false)
    private var captureUpdateQueued = false
    private var captureActivityPending = false

    private func attach(_ permit: V4vPublishingGrantPermit, token: UInt64, captureProfile: CaptureIntent) throws {
        // Reject an insufficient grant before creating a camera or publisher
        // owner. The selected profile is never inferred from a service cap.
        try captureProfile.requireGrant(maxPublishBps: permit.grant.maxPublishBps)
        guard live(token), binding == nil, let engine, let ticket = capture.reservePreparation(profile: captureProfile) else { throw V4vFailure.busy }
        if ticket.continuesAudioInBackground {
            do {
                gate.lock(); let foreground = sceneForeground; gate.unlock()
                guard foreground, showLease == nil, let leasing = journal as? V4vJournalShowLeasing else { throw V4vFailure.journal }
                let lease = UUID()
                try leasing.acquireShowLease(owner: lease)
                showLease = (lease, ticket); leaseRetention = self
                capture.whenCleanupComplete(ticket) { [weak self] in self?.owner.async { [weak self] in
                    guard let self, self.showLease?.0 == lease, self.showLease?.1 == ticket else { return }
                    leasing.releaseShowLease(owner: lease)
                    self.showLease = nil; self.leaseRetention = nil; self.finishClose()
                } }
            } catch { capture.abandonPreparation(ticket); throw error }
        }
        guard nextDestination < UInt64.max else { capture.abandonPreparation(ticket); throw V4vFailure.bounds }
        gate.lock()
        let mayConstruct = !closed && enabled && generation == token
        if mayConstruct { coldTicket = ticket }
        gate.unlock()
        guard mayConstruct else { capture.abandonPreparation(ticket); throw V4vFailure.closed }
        nextDestination += 1
        let publisher: V4vCreatorPublisher
        do { publisher = try makePublisher(nextDestination, permit, engine, owner, slot, clock, captureProfile) }
        catch { capture.abandonPreparation(ticket); throw error }
        gate.lock()
        let accepted = !closed && enabled && generation == token
        if accepted { coldTicket = ticket; coldPublisher = publisher }
        gate.unlock()
        guard accepted else {
            // A successfully constructed candidate owns real native/timer
            // cleanup. CaptureEngine retains this exact rejected candidate and
            // releases its reservation only after actual stop completion.
            publisher.endDestination()
            capture.setDestination(publisher, preparation: ticket) { _ in }
            throw V4vFailure.closed
        }
        binding = Binding(ticket: ticket, generation: token, publisher: publisher, permit: permit)
        capture.setDestination(publisher, preparation: ticket) { [weak self, weak publisher] accepted in
            guard let self else { return }
            self.owner.async {
                guard let publisher, self.binding?.publisher === publisher else { return }
                guard accepted, self.live(token), self.clock().monotonicNs < permit.expiresAtNs else {
                    self.capture.cancelPendingPreparation(ticket); self.retireBinding(); self.publish(); return
                }
                self.capture.prepare(ticket); self.scheduleRefresh(permit); self.publish()
            }
        }
    }
    private func retireBinding() {
        guard let old = binding, !retiringBinding else { return }; retiringBinding = true
        refreshPlan = nil
        old.publisher.endDestination()
        // Keep its borrowed/native owners until actual stop completion. No
        // successor can be attached while this binding still occupies capacity.
        old.publisher.stop { [weak self, weak publisher = old.publisher] in
            guard let self else { return }
            self.owner.async {
                guard let publisher, self.binding?.publisher === publisher, publisher.cleanupComplete else { return }
                self.binding = nil; self.retiringBinding = false
                self.gate.lock()
                if self.coldPublisher === publisher { self.coldTicket = nil; self.coldPublisher = nil }
                self.gate.unlock(); self.publish(); self.finishClose()
            }
        }
    }
    private func read(_ kind: Read) throws {
        guard let engine else { throw V4vFailure.closed }
        let call: V4vHTTPCall
        switch kind {
        case .events: call = try engine.creatorEventsRequest(clock: clock())
        case let .current(event): call = try engine.currentRequest(event: event, clock: clock())
        case .status: call = try engine.sessionStatusRequest(clock: clock())
        case let .remoteEnd(read): call = read.call
        }
        try beginWindow(); send(call, purpose: .read(kind))
    }
    private func pump() {
        guard inflight == nil, let engine, window != nil, !state().closed else { return }
        do {
            guard let window, clock().monotonicNs < window.end, window.attempts < 4 else { throw V4vFailure.unavailable }
            if let request = try engine.pendingRequest(clock: clock()) {
                send(V4vHTTPCall(request), purpose: .mutation(request.operation)); return
            }
            if engine.terminalIntent {
                if engine.session?.endComplete == true || engine.session?.recoveryState != nil { ending = false; completeWindow(); return }
                guard let session = engine.session else { ending = false; completeWindow(); return }
                try engine.continueEnd(key: nonce(), clock: clock())
                if engine.pending != nil { pump(); return }
                send(try engine.sessionStatusRequest(clock: clock()), purpose: .read(.status(session.identity))); return
            }
            completeWindow()
        } catch { fail(error); completeWindow() }
    }
    private func ensureHTTP() throws -> V4vCreatorHTTP {
        if let http { guard !http.cleanupComplete else { self.http = nil; return try ensureHTTP() }; return http }
        let created = makeHTTP { [weak self] in self?.owner.async { [weak self] in self?.finishClose() } }
        http = created; return created
    }
    private func send(_ call: V4vHTTPCall, purpose: Purpose) {
        do {
            guard inflight == nil, var window, clock().monotonicNs < window.end, window.attempts < 4 else { throw V4vFailure.unavailable }
            let client = try ensureHTTP(); window.attempts += 1; self.window = window
            requestSerial &+= 1; let serial = requestSerial, token = state().generation
            inflight = serial; publish()
            client.execute(call) { [weak self] answer in
                self?.owner.async { [weak self] in self?.received(answer, purpose: purpose, serial: serial, token: token) }
            }
        } catch { fail(error); completeWindow() }
    }
    private func received(_ answer: Result<V4vJSON, V4vFailure>, purpose: Purpose, serial: UInt64, token: UInt64) {
        guard inflight == serial, let engine else { return }; inflight = nil
        if state().closed { finishClose(); return }
        do {
            let json = try answer.get()
            let withinWindow = window.map { clock().monotonicNs < $0.end } ?? false
            value.error = nil
            switch purpose {
            case let .mutation(operation):
                // Late mutation acknowledgements still durably resolve the
                // EXACT pending operation. Cold End already fenced adoption.
                try engine.applyResponse(json, for: operation, clock: clock())
                // The server operation is now durably acknowledged. Local
                // capture/publisher failure cannot retry that HTTP mutation or
                // supply fresh capture approval from a later retry timer.
                do {
                    if !engine.terminalIntent, let permit = try? engine.publishingGrant(clock: clock()) {
                        if let binding { try binding.publisher.committed(permit); self.binding?.permit = permit; scheduleRefresh(permit) }
                        else if withinWindow, let intent = prepareIntent, intent.generation == token, live(token) {
                            try attach(permit, token: token, captureProfile: intent.profile)
                        }
                    }
                } catch { fail(error); completeWindow(); return }
                if engine.terminalIntent { pump() } else { completeWindow() }
            case let .read(kind):
                switch kind {
                case .events:
                    let parsed = try V4vCreatorEvents(json)
                    if live(token) { value.events = parsed.events; value.account = value.publicPersona == nil ? "Saved session" : "Signed in" }
                case let .current(event):
                    let parsed = try V4vCurrentSession(json, event: event)
                    if live(token), value.selectedEvent == event { current = parsed }
                case let .status(identity):
                    let parsed = try V4vCreatorSessionStatus(json, requested: identity)
                    // A status can advance this exact durable scope but cannot
                    // discard pending, restore a grant or supply capture intent.
                    try engine.applyStatus(json, clock: clock())
                    if live(token), engine.pending == nil { lastFreshStatus = (parsed.receipt.identity, clock().monotonicNs) }
                case let .remoteEnd(read):
                    guard withinWindow, live(token), value.selectedEvent == read.identity.eventId,
                          current?.receipt?.identity == read.identity, engine.session == nil else { throw V4vFailure.conflict }
                    try engine.commitRemoteEnd(json, for: read, clock: clock())
                    current = nil; ending = true; pump(); publish(); return
                }
                if ending, engine.session?.endComplete != true, engine.session?.recoveryState == nil { retry(purpose: nil) }
                else { completeWindow() }
            }
        } catch {
            fail(error)
            let retryable = (error as? V4vFailure) == .unavailable
            if retryable { retry(purpose: purpose) } else { completeWindow() }
        }
        publish()
    }
    private func retry(purpose: Purpose?) {
        guard let window, window.attempts < 4 else { completeWindow(); return }
        let now = clock().monotonicNs
        let delay = UInt64(1 << min(3, max(0, window.attempts - 1))) * 1_000_000_000
        guard now < window.end, delay < window.end - now else { completeWindow(); return }
        timer.schedule(atNs: now + delay, generation: requestSerial) { [weak self] serial in
            guard let self, self.requestSerial == serial, !self.state().closed else { return }
            self.timer.cancel()
            // No new inactive read/refresh; exact uncertain operations and End
            // remain reconcilable independently of UI mode.
            if self.engine?.pending != nil || self.engine?.terminalIntent == true { self.pump() }
            else if self.state().active, case let .read(kind)? = purpose {
                do {
                    guard let engine = self.engine else { throw V4vFailure.closed }
                    let call: V4vHTTPCall
                    switch kind {
                    case .events: call = try engine.creatorEventsRequest(clock: self.clock())
                    case let .current(event): call = try engine.currentRequest(event: event, clock: self.clock())
                    case .status: call = try engine.sessionStatusRequest(clock: self.clock())
                    case let .remoteEnd(old):
                        let read = try engine.remoteEndStatusRequest(identity: old.identity, clock: self.clock())
                        self.send(read.call, purpose: .read(.remoteEnd(read))); return
                    }
                    self.send(call, purpose: .read(kind))
                } catch { self.fail(error); self.completeWindow() }
            } else { self.completeWindow() }
        }
    }
    private func scheduleRefresh(_ permit: V4vPublishingGrantPermit) {
        let remaining = permit.grant.expiresMs - permit.grant.refreshMs
        let reserve = remaining.multipliedReportingOverflow(by: 1_000_000)
        guard !reserve.overflow, permit.expiresAtNs >= reserve.partialValue, let binding else {
            refreshPlan = nil; return
        }
        refreshPlan = RefreshPlan(due: permit.expiresAtNs - reserve.partialValue,
                                  generation: binding.generation, ticket: binding.ticket)
        armRefresh()
    }
    private func armRefresh() {
        guard let plan = refreshPlan else { return }
        let cold = state()
        gate.lock()
        let modePermitsRefresh = !closed && enabled && !endQueued &&
            (sceneForeground || (continuingAudio != nil && continuingAudio == coldTicket))
        gate.unlock()
        guard cold.generation == plan.generation, binding?.ticket == plan.ticket else {
            refreshPlan = nil; return
        }
        guard window == nil, inflight == nil, cold.active, !cold.closed, !cold.busy,
              modePermitsRefresh, engine?.pending == nil, engine?.terminalIntent == false else { return }
        if clock().monotonicNs >= plan.due {
            beginAutomaticRefresh(plan)
            return
        }
        timer.schedule(atNs: plan.due, generation: plan.generation) { [weak self] token in
            guard let self else { return }
            self.timer.cancel()
            guard token == plan.generation else { return }
            self.beginAutomaticRefresh(plan)
        }
    }
    /// Keep the deadline until refresh is durably staged. If another command
    /// won admission after the alarm was queued, its release re-enters
    /// `armRefresh`; foreground activation does the same for a withheld alarm.
    private func beginAutomaticRefresh(_ plan: RefreshPlan) {
        guard refreshPlan == plan, live(plan.generation), window == nil, inflight == nil,
              binding?.ticket == plan.ticket else { return }
        _ = command(allowBackgroundRefresh: true) { [self] admitted in
            do {
                guard admitted == plan.generation, refreshPlan == plan, live(admitted), window == nil, inflight == nil,
                      binding?.ticket == plan.ticket, let engine, engine.pending == nil, !engine.terminalIntent else {
                    throw V4vFailure.conflict
                }
                _ = try engine.publishingGrant(clock: clock())
                try engine.stageRefresh(key: nonce(), clock: clock())
                refreshPlan = nil
                try beginWindow(); pump()
            } catch {
                // A failed authority/journal/expiry check must not spin an
                // overdue alarm. Command contention never enters this block,
                // so it retains the deadline for release-time reconciliation.
                refreshPlan = nil
                throw error
            }
        }
    }
    private func completeWindow() {
        let completedCommand = window?.command ?? actorCommand
        window = nil; timer.cancel(); prepareIntent = nil
        gate.lock()
        if busyCommand == completedCommand { busy = false; busyCommand = nil }
        if !endNeedsStaging { endQueued = false }
        gate.unlock()
        armRefresh(); publish()
    }
    private func fail(_ error: Error) {
        if (error as? CaptureProfileFailure) == .insufficientPublishCap {
            value.error = "The destination limit is too low for the selected quality. Choose a lower quality and Prepare again."
            value.message = "Selected quality exceeds the destination limit"
            publish(); return
        }
        value.error = (error as? V4vFailure ?? .unavailable).description
        value.message = engine?.pending != nil ? "Operation unconfirmed; retry the saved operation" :
            ((error as? V4vFailure) == .conflict ? "Session conflict; reconcile saved state or resolve the existing remote session in the service" : "Creator action could not complete")
        publish()
    }
    private func nonce() -> String { UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased() }
    private func publish() {
        let cold = state(); value.active = cold.active; value.closing = cold.closed
        value.phase = engine?.session?.phase ?? (value.ready ? "No session" : "Unavailable")
        value.pending = engine?.pending?.kind.rawValue; value.expiresAtUnixMs = engine?.session?.expiresMs
        value.endComplete = engine?.session?.endComplete == true
        let free = value.ready && cold.active && cold.foreground && !cold.closed && !cold.busy && !cold.ending
        let authenticated = (try? engine?.creatorEventsRequest(clock: clock())) != nil
        let idleCapture = !capturePrepared && !captureRunning && !captureStopping && binding == nil
        value.canSignIn = free && idleCapture && (login == nil || login?.cleanupComplete == true)
        value.canCancelSignIn = value.signingIn
        value.canReloadEvents = free && authenticated
        value.canSelectEvent = free && authenticated && idleCapture && engine?.pending == nil
        let usableGrant = (try? engine?.publishingGrant(clock: clock())) != nil
        let eligibleCurrent = current.map { remote in remote.receipt == nil || engine?.session?.identity.sameSession(remote.receipt!.identity) == true } ?? false
        value.canPrepare = free && authenticated && idleCapture && value.selectedEvent != nil && engine?.pending == nil &&
            (usableGrant || (eligibleCurrent && engine?.session == nil) || engine?.session?.endComplete == true)
        value.canStart = free && capturePrepared && !captureRunning && !captureStopping && binding != nil && (try? engine?.publishingGrant(clock: clock())) != nil
        value.canRefresh = free && authenticated && engine?.pending == nil && engine?.session?.phase == "active" && engine?.terminalIntent == false
        value.canStatus = free && authenticated && engine?.session != nil
        value.canEnd = value.ready && !cold.closed && (engine?.pending != nil || (engine?.session != nil && engine?.session?.endComplete == false))
        value.canEndExisting = free && authenticated && idleCapture && engine?.session == nil && engine?.pending == nil &&
            current?.receipt?.identity.eventId == value.selectedEvent && current?.receipt?.phase == "active" &&
            current?.receipt?.terminal == false && current?.receipt?.endComplete == false
        value.endExistingIdentity = value.canEndExisting ? current?.receipt?.identity : nil
        value.canRetry = free && authenticated && (engine?.pending != nil || (engine?.terminalIntent == true && engine?.session?.endComplete == false && engine?.session?.recoveryState == nil))
        value.canAcknowledge = free && authenticated && engine?.pending == nil && engine?.session?.origin?.recoveryOffered == true && engine?.session?.recoveryState == nil && lastFreshStatus?.0 == engine?.session?.identity
        value.canReplace = free && authenticated && idleCapture && engine?.session?.recoveryState == "acknowledged"
        gate.lock(); latestSnapshot = value
        guard !snapshotQueued else { gate.unlock(); return }; snapshotQueued = true; gate.unlock()
        delivery.async { [weak self] in
            guard let self else { return }
            self.gate.lock(); let snapshot = self.latestSnapshot; self.snapshotQueued = false; self.gate.unlock()
            if let snapshot { self.changed(snapshot) }
        }
    }
    func close() {
        gate.lock(); guard !closed else { gate.unlock(); return }; closed = true; generation &+= 1; gate.unlock()
        retireAuthority()
        owner.async { [self] in
            retainedSelf = self; timer.cancel(); window = nil; prepareIntent = nil; refreshPlan = nil
            slot.close { [weak self] in self?.slotClosed = true; self?.finishClose() }
            engine?.close(); http?.close(); login?.cancel(); retireBinding(); publish(); finishClose()
        }
    }
    private func finishClose() {
        guard state().closed, showLease == nil, slotClosed, binding == nil, (http == nil || http?.cleanupComplete == true),
              (login == nil || login?.cleanupComplete == true) else { return }
        retainedSelf = nil
    }
}
