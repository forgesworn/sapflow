import Foundation

/// Cold command admission, independent of camera/media ownership. A successful
/// native cleanup receipt is still required before releasing a stopped intent.
/// No callback, codec, socket or file operation runs under this scalar lock.
final class CaptureAdmission {
    fileprivate final class Identity: Equatable, Sendable {
        static func == (lhs: Identity, rhs: Identity) -> Bool { lhs === rhs }
    }
    struct Preparation: Equatable {
        fileprivate let owner: Identity
        fileprivate let serial: UInt64
        let profile: CaptureIntent
        let audioInputID: String?
        let continuesAudioInBackground: Bool
        let orientationChoice: CaptureOrientationChoice
        let initialOrientation: CaptureOrientation?
        let initialCamera: CameraPosition
        let initialStandby: Bool
    }
    struct OrientationObservation {
        fileprivate let serial: UInt64
        fileprivate let preparation: Preparation?
        let orientation: CaptureOrientation
    }
    struct Start: Equatable {
        fileprivate let serial: UInt64
        let preparation: Preparation
    }
    struct Rotation: Equatable {
        fileprivate let serial: UInt64
        let preparation: Preparation
    }
    struct Stop: Equatable {
        fileprivate let serial: UInt64
        let preparation: Preparation?
    }
    /// Immutable native ownership. Only this Admission can mint a run/token;
    /// callbacks retain the originally submitted value, never a current lookup.
    struct MediaRun: Equatable {
        let preparation: Preparation
        fileprivate let serial: UInt64
    }
    enum MediaKind: Equatable { case raw, slate, zero }
    struct MediaToken: Equatable {
        let run: MediaRun
        let revision: UInt64
        let kind: MediaKind
        fileprivate let serial: UInt64
    }
    enum MediaAdmission: Equatable { case allow, superseded, stopped, failed }
    enum OutputAdmission: Equatable { case current, acceptedPrefix, superseded, failed }
    enum PrivacyDesired: Equatable { case live, standby }
    enum PrivacyPhase: Equatable { case live, armed, enteringStandby, standby, resuming, failed, stopped }
    struct PrivacyRequest: Equatable {
        let preparation: Preparation
        let run: MediaRun?
        let revision: UInt64
        let desired: PrivacyDesired
        let beganAtNs: UInt64
        let deadlineNs: UInt64?
        fileprivate let serial: UInt64
    }
    struct PrivacyView {
        let preparation: Preparation
        let run: MediaRun?
        let revision: UInt64
        let desired: PrivacyDesired
        let phase: PrivacyPhase
        let previewHidden: Bool
        let request: PrivacyRequest?
    }
    enum Sink: UInt8 { case publishing = 1, recording = 2 }
    struct SinkMask: OptionSet, Equatable {
        let rawValue: UInt8
        static let publishing = SinkMask(rawValue: Sink.publishing.rawValue)
        static let recording = SinkMask(rawValue: Sink.recording.rawValue)
        static let both: SinkMask = [.publishing, .recording]
    }
    struct SinkToken: Equatable {
        let run: MediaRun
        let sink: Sink
        fileprivate let serial: UInt64
    }
    /// Trust boundary: only actual coded validation plus the same unit's bounded
    /// branch reservations may construct this receipt. Requested keyframe flags
    /// and a void consume/offer call are not independent-IDR/admission evidence.
    struct MediaBoundary {
        let token: MediaToken
        let pts: Int64
        let duration: UInt32
        let codedIdr: Bool
        let audioGeneration: UInt64?
        let acceptedSinks: SinkMask
    }
    enum Phase { case idle, reserved, attached, permissions, prepared, starting, running, rotating, stopping, retained }
    private let lock = NSLock()
    private let identity = Identity()
    private var serial: UInt64
    private var selectedAudioInputID: String?
    private var selectedCamera = CameraPosition.rear
    private var camera = CameraControlPolicy<Preparation>()
    private var selectedBackgroundAudio = false
    private var selectedInitialStandby = false
    private var privacyDesired = PrivacyDesired.live
    private var privacyPhase = PrivacyPhase.live
    private var privacyRevision: UInt64 = 0
    private var privacyRequest: PrivacyRequest?
    private var mediaRun: MediaRun?
    private var closingMediaRun: MediaRun?
    private var committedMediaRun: MediaRun?
    private var committedMediaStart: Start?
    private var recordingClosureRun: MediaRun?
    private var rawToken: MediaToken?
    private var slateToken: MediaToken?
    private var zeroToken: MediaToken?
    private var mediaStarted = false
    private var mediaFailed = false
    private var terminalBranches: UInt8 = 0
    private var mediaOperationDeadlineNs: UInt64?
    private var mediaOperationBeganAtNs: UInt64?
    private var publishingSink: SinkToken?
    private var recordingSink: SinkToken?
    private var expectedZeroGeneration: UInt64?
    private var slateConfirmed = false
    private var zeroThrough: Int64?
    private var zeroFrames: UInt64 = 0
    private var selectedOrientationChoice = CaptureOrientationChoice.automatic
    private var latestAutoOrientation = CaptureOrientation.portrait
    private var orientationObservation: UInt64?
    private var foreground = true
    private var phase: Phase = .idle
    private var preparation: Preparation?
    private var starting: Start?
    private var rotation: Rotation?
    private var stopping: Stop?
    private var candidatePending = false
    private var stoppedOwnersClean = false
    private var hasCommittedRun = false
    private var timingPreparation: Preparation?
    private var captureBeganAtNs: UInt64?
    private var captureStoppedAtNs: UInt64?
    private var broadcastBeganAtNs: UInt64?
    private var broadcastStoppedAtNs: UInt64?

    init(initialSerial: UInt64 = 0) { serial = initialSerial }
    private func locked<T>(_ body: () -> T) -> T {
        lock.lock(); defer { lock.unlock() }; return body()
    }
    private func next() -> UInt64? {
        guard serial < UInt64.max else { return nil }
        serial += 1; return serial
    }
    var currentPhase: Phase { locked { phase } }
    var canChooseCaptureProfile: Bool { locked { phase == .idle } }
    func selectOrientationChoice(_ choice: CaptureOrientationChoice) -> Bool {
        locked {
            guard phase == .idle, foreground else { return false }
            selectedOrientationChoice = choice; orientationObservation = nil; return true
        }
    }
    /// Sensor/display input is admitted synchronously, before queueing native
    /// work. Latest-wins serials and exact preparation identity fence late work.
    func observeOrientation(_ orientation: CaptureOrientation) -> OrientationObservation? {
        locked {
            guard foreground, phase != .stopping, phase != .retained,
                  preparation.map({ $0.profile.hasVideo && $0.orientationChoice == .automatic }) ?? (phase == .idle && selectedOrientationChoice == .automatic),
                  let serial = next() else { return nil }
            latestAutoOrientation = orientation; orientationObservation = serial
            return OrientationObservation(serial: serial, preparation: preparation, orientation: orientation)
        }
    }
    func permitsOrientation(_ value: OrientationObservation) -> Bool {
        locked {
            guard foreground, orientationObservation == value.serial, preparation == value.preparation,
                  phase != .stopping, phase != .retained else { return false }
            if let preparation { return preparation.profile.hasVideo && preparation.orientationChoice == .automatic }
            return phase == .idle && selectedOrientationChoice == .automatic
        }
    }
    /// Resume a deferred pose without minting a new sensor serial. A newer
    /// synchronously admitted observation may still be waiting on the owner queue.
    var latestOrientationObservation: OrientationObservation? {
        locked {
            guard foreground, let serial = orientationObservation,
                  let preparation, preparation.profile.hasVideo, preparation.orientationChoice == .automatic,
                  phase != .stopping, phase != .retained, phase != .idle else { return nil }
            return OrientationObservation(serial: serial, preparation: preparation, orientation: latestAutoOrientation)
        }
    }
    /// Auto may have a newer physical pose by the time its queued candidate is
    /// attached. A fixed preparation always uses its immutable initial value.
    func orientationAtAttachment(_ value: Preparation) -> CaptureOrientation? {
        locked {
            guard preparation == value, phase == .attached, value.profile.hasVideo else { return nil }
            return value.orientationChoice == .automatic ? latestAutoOrientation : value.initialOrientation
        }
    }
    typealias CameraSource = CameraControlPolicy<Preparation>.Source
    typealias CameraSwitch = CameraControlPolicy<Preparation>.Switch
    typealias CameraCanvas = CameraControlPolicy<Preparation>.Canvas
    func selectCamera(_ position: CameraPosition) -> Bool {
        locked { guard phase == .idle, foreground else { return false }; selectedCamera = position; return true }
    }
    var desiredCamera: CameraPosition { locked { camera.desired } }
    var pendingCameraSwitch: CameraSwitch? { locked { camera.pending } }
    var cameraControlSource: CameraSource? {
        locked {
            guard foreground, privacyDesired == .live, privacyPhase != .resuming,
                  phase == .prepared || phase == .running, let source = camera.source,
                  camera.permitsControls(source) else { return nil }; return source
        }
    }
    func permitsCameraControls(_ source: CameraSource) -> Bool {
        locked { foreground && privacyDesired == .live && privacyPhase != .resuming && (phase == .prepared || phase == .running) && preparation == source.owner && camera.permitsControls(source) }
    }
    func initialCameraSource(canvas: CameraCanvas) -> CameraSource? {
        locked {
            guard foreground, let preparation, preparation.profile.hasVideo,
                  [.permissions, .prepared, .starting].contains(phase) || phase == .running && privacyPhase == .resuming else { return nil }
            return camera.initial(preparation, canvas: canvas)
        }
    }
    func reserveCameraSwitch(_ position: CameraPosition, source: CameraSource, nowNs: UInt64) -> CameraSwitch? {
        locked {
            guard foreground, privacyDesired == .live, privacyPhase == .live,
                  phase == .prepared || phase == .running, let preparation,
                  preparation == source.owner, camera.permitsControls(source) else { return nil }
            return camera.reserveSwitch(preparation, position: position, nowNs: nowNs)
        }
    }
    func permitsCameraSwitch(_ value: CameraSwitch, nowNs: UInt64) -> Bool {
        locked { foreground && (phase == .prepared || phase == .running) && preparation == value.owner && camera.permits(value, nowNs: nowNs) }
    }
    func retireCameraSwitch(_ value: CameraSwitch, nowNs: UInt64) -> CameraSource? {
        locked {
            guard foreground, phase == .prepared || phase == .running, preparation == value.owner else { return nil }
            return camera.retire(value, nowNs: nowNs)
        }
    }
    func rejectCameraSwitch(_ value: CameraSwitch) -> Bool { locked { camera.reject(value) } }
    func failCameraSwitch(_ value: CameraSwitch) -> Bool { locked { camera.fail(value) } }
    func permitsCameraFrame(_ source: CameraSource) -> Bool {
        locked {
            foreground && preparation == source.owner && (phase == .permissions || phase == .prepared || phase == .starting || phase == .running) &&
                camera.source == source && !camera.failed
        }
    }
    /// Delegates are disabled and the native session stop joins on the control
    /// owner. This scalar retirement rejects old source callbacks immediately.
    func parkPrivacySources(_ value: Preparation) -> Bool {
        locked {
            guard preparation == value, privacyDesired == .standby,
                  phase != .stopping, phase != .retained else { return false }
            let position = camera.desired
            camera.close()
            if value.profile.hasVideo { camera.prepare(value, initial: position) }
            return true
        }
    }
    func privacyBlocksOrientation(_ value: Preparation) -> Bool {
        locked { preparation == value && (privacyPhase == .enteringStandby || privacyPhase == .resuming) }
    }
    func cameraMedia(_ value: Preparation, nowNs: UInt64? = nil) -> CameraControlPolicy<Preparation>.Media {
        locked {
            guard preparation == value, phase == .running else { return .stale }
            guard value.profile.hasVideo else { return .allowed }
            // Steady-state audio uses only the scalar owner gate. Read the
            // monotonic clock solely during a retired-source transition.
            let now = camera.retired && camera.pending != nil ? (nowNs ?? DispatchTime.now().uptimeNanoseconds) : 0
            return camera.media(value, nowNs: now)
        }
    }
    func cameraFrame(_ source: CameraSource, width: Int, height: Int, nowNs: UInt64) -> CameraControlPolicy<Preparation>.Frame {
        locked {
            guard foreground, preparation == source.owner, (phase == .permissions || phase == .prepared || phase == .starting || phase == .running) else { return .stale }
            return camera.frame(source, width: width, height: height, nowNs: nowNs)
        }
    }
    func selectBackgroundAudio(_ enabled: Bool) -> Bool {
        locked { guard phase == .idle, foreground else { return false }; selectedBackgroundAudio = enabled; return true }
    }
    func selectInitialStandby(_ enabled: Bool) -> Bool {
        locked { guard phase == .idle, foreground else { return false }; selectedInitialStandby = enabled; return true }
    }
    var privacyView: PrivacyView? {
        locked {
            guard let preparation else { return nil }
            return PrivacyView(preparation: preparation, run: mediaRun, revision: privacyRevision,
                desired: privacyDesired, phase: privacyPhase,
                previewHidden: privacyDesired == .standby || privacyPhase == .resuming || mediaFailed,
                request: privacyRequest)
        }
    }
    private func privacyFaultLocked() {
        mediaFailed = true; privacyPhase = .failed
    }
    private func privacyDeadlineLocked(_ nowNs: UInt64, prior: UInt64? = nil) -> UInt64? {
        guard nowNs <= UInt64.max - 6_000_000_000 else { privacyFaultLocked(); return nil }
        return [nowNs + 6_000_000_000, prior, camera.pending?.deadlineNs, mediaOperationDeadlineNs].compactMap { $0 }.min()
    }
    private func mintMediaTokensLocked(_ run: MediaRun) -> Bool {
        guard let raw = next(), let slate = next(), let zero = next() else { privacyFaultLocked(); return false }
        rawToken = MediaToken(run: run, revision: privacyRevision, kind: .raw, serial: raw)
        slateToken = MediaToken(run: run, revision: privacyRevision, kind: .slate, serial: slate)
        zeroToken = MediaToken(run: run, revision: privacyRevision, kind: .zero, serial: zero)
        return true
    }
    /// Reserve immutable native identity before any encoder/worker is created.
    /// Registration does not grant input; only the exact committed Start does.
    func reserveMediaRun(_ start: Start, publishing: Bool = true) -> MediaRun? {
        locked {
            guard preparation == start.preparation, starting == start, phase == .starting,
                  mediaRun == nil, closingMediaRun == nil, !mediaFailed, let id = next() else { return nil }
            let value = MediaRun(preparation: start.preparation, serial: id)
            guard mintMediaTokensLocked(value) else { return nil }
            guard let publishingID = next(), let recordingID = next() else { privacyFaultLocked(); return nil }
            publishingSink = SinkToken(run: value, sink: .publishing, serial: publishingID)
            recordingSink = SinkToken(run: value, sink: .recording, serial: recordingID)
            if !publishing { terminalBranches |= Sink.publishing.rawValue }
            mediaRun = value; mediaStarted = false; committedMediaRun = nil; committedMediaStart = nil; recordingClosureRun = nil
            expectedZeroGeneration = nil; slateConfirmed = false; zeroThrough = nil; zeroFrames = 0
            if privacyDesired == .standby {
                guard let command = next() else { privacyFaultLocked(); return nil }
                let pending = privacyPhase == .armed || privacyPhase == .enteringStandby || privacyPhase == .resuming
                let priorBegan = pending ? privacyRequest?.beganAtNs : nil
                let priorDeadline = pending ? privacyRequest?.deadlineNs : nil
                privacyPhase = .armed
                privacyRequest = PrivacyRequest(preparation: start.preparation, run: value, revision: privacyRevision,
                    desired: .standby, beganAtNs: priorBegan ?? 0,
                    deadlineNs: priorDeadline, serial: command)
            }
            return value
        }
    }
    func mediaToken(_ run: MediaRun, kind: MediaKind) -> MediaToken? {
        locked {
            guard mediaRun == run else { return nil }
            switch kind { case .raw: return rawToken; case .slate: return slateToken; case .zero: return zeroToken }
        }
    }
    @discardableResult func commitMediaRun(_ run: MediaRun, start: Start, nowNs: UInt64, broadcasting: Bool) -> Bool {
        locked {
            guard mediaRun == run, !mediaFailed else { return false }
            if let deadline = mediaOperationDeadlineNs,
               nowNs >= deadline || nowNs < (mediaOperationBeganAtNs ?? 0) { privacyFaultLocked(); return false }
            var activation: PrivacyRequest?
            if privacyDesired == .standby {
                guard let prior = privacyRequest, let deadline = privacyDeadlineLocked(nowNs, prior: prior.deadlineNs),
                      nowNs < deadline, prior.deadlineNs == nil || nowNs >= prior.beganAtNs else { privacyFaultLocked(); return false }
                activation = PrivacyRequest(preparation: run.preparation, run: run, revision: privacyRevision,
                    desired: .standby, beganAtNs: prior.deadlineNs == nil ? nowNs : prior.beganAtNs,
                    deadlineNs: deadline, serial: prior.serial)
            }
            guard startedLocked(start, nowNs: nowNs, broadcasting: broadcasting) else { return false }
            mediaStarted = true; committedMediaRun = run; committedMediaStart = start; recordingClosureRun = nil
            if let activation {
                privacyRequest = activation
                privacyPhase = .enteringStandby
            }
            mediaOperationDeadlineNs = nil; mediaOperationBeganAtNs = nil
            return true
        }
    }
    /// Urgent privacy is show-scoped and wins even while native/source/Auto work
    /// is queued. No callback, preview, native close or dispatch occurs here.
    func requestStandby(preparation value: Preparation, nowNs: UInt64) -> PrivacyRequest? {
        locked {
            guard preparation == value, ![.idle, .stopping, .retained].contains(phase), !mediaFailed else { return nil }
            if privacyDesired == .standby, let request = privacyRequest { return request }
            privacyDesired = .standby // Fence raw before any fallible reservation.
            guard privacyRevision < UInt64.max, let id = next() else { privacyFaultLocked(); return nil }
            privacyRevision += 1
            expectedZeroGeneration = nil; slateConfirmed = false; zeroThrough = nil; zeroFrames = 0
            if let mediaRun, !mintMediaTokensLocked(mediaRun) { return nil }
            let needsBudget = mediaStarted || camera.pending != nil || mediaOperationDeadlineNs != nil
            let deadline = needsBudget ? privacyDeadlineLocked(nowNs, prior: privacyRequest?.deadlineNs) : nil
            if needsBudget, deadline == nil { return nil }
            let request = PrivacyRequest(preparation: value, run: mediaRun, revision: privacyRevision,
                desired: .standby, beganAtNs: nowNs, deadlineNs: deadline, serial: id)
            privacyRequest = request; privacyPhase = mediaStarted ? .enteringStandby : .armed
            return request
        }
    }
    /// Explicit displayed-revision approval only. An unknown old AAC/source
    /// retirement cannot be bypassed by a ready Boolean or a second gesture.
    func requestResume(preparation value: Preparation, expectedRun: MediaRun? = nil, expectedRevision: UInt64, nowNs: UInt64) -> PrivacyRequest? {
        locked {
            guard foreground, preparation == value, phase == .running, mediaStarted, !mediaFailed,
                  privacyPhase == .standby, privacyRevision == expectedRevision,
                  expectedRun.map({ $0 == mediaRun }) ?? true,
                  let mediaRun, privacyRevision < UInt64.max, let id = next(),
                  let deadline = privacyDeadlineLocked(nowNs) else { return nil }
            privacyRevision += 1
            guard mintMediaTokensLocked(mediaRun) else { return nil }
            privacyDesired = .live; privacyPhase = .resuming
            let request = PrivacyRequest(preparation: value, run: mediaRun, revision: privacyRevision,
                desired: .live, beganAtNs: nowNs, deadlineNs: deadline, serial: id)
            privacyRequest = request; return request
        }
    }
    enum PrivacyFailureClaim { case media(MediaRun), preparation(Stop) }
    /// Queueing a diagnostic is not fault ownership. Claim only when the exact
    /// request still owns admission at the point of mutation. Stop/supersession
    /// wins normally; a claim that wins first remains sticky through Stop.
    func claimPrivacyFailure(_ value: PrivacyRequest, nowNs: UInt64) -> PrivacyFailureClaim? {
        locked {
            guard preparation == value.preparation, privacyRequest == value,
                  phase != .stopping, phase != .retained, phase != .idle else { return nil }
            if let run = value.run {
                guard mediaRun == run else { return nil }
                privacyFaultLocked(); return .media(run)
            }
            guard mediaRun == nil else { return nil }
            return .preparation(stopLocked(nowNs: nowNs))
        }
    }
    private func requestCurrentLocked(_ value: PrivacyRequest, nowNs: UInt64) -> Bool {
        guard privacyRequest == value, preparation == value.preparation, mediaRun == value.run,
              !mediaFailed, phase != .stopping, phase != .retained else { return false }
        if privacyPhase != .standby, let deadline = value.deadlineNs, nowNs < value.beganAtNs || nowNs >= deadline {
            privacyFaultLocked(); return false
        }
        return true
    }
    func permitsPrivacy(_ value: PrivacyRequest, nowNs: UInt64) -> Bool {
        locked { requestCurrentLocked(value, nowNs: nowNs) }
    }
    func sinkToken(run: MediaRun, kind: Sink) -> SinkToken? {
        locked { guard mediaRun == run else { return nil }; return kind == .publishing ? publishingSink : recordingSink }
    }
    func requiredSinkMask(run: MediaRun) -> SinkMask {
        locked { preparation == run.preparation ? SinkMask(rawValue: SinkMask.both.rawValue & ~terminalBranches) : [] }
    }
    /// Called only after the exact local branch has irreversibly closed input.
    /// Prefix cleanup remains separately owned. Transient retry is not terminal.
    @discardableResult func sinkTerminated(_ value: SinkToken) -> Bool {
        locked {
            guard preparation == value.run.preparation,
                  mediaRun == value.run || mediaRun == nil && closingMediaRun == value.run,
                  value == (value.sink == .publishing ? publishingSink : recordingSink) else { return false }
            terminalBranches |= value.sink.rawValue
            if terminalBranches & SinkMask.both.rawValue == SinkMask.both.rawValue { privacyFaultLocked() }
            return true
        }
    }
    /// Native beginStandby has synchronously joined the prior converter and
    /// returned this newly installed zero-only generation; no guessed padding.
    func zeroOwnerReady(_ request: PrivacyRequest, generation: UInt64, retiredGeneration: UInt64?, nowNs: UInt64) -> Bool {
        locked {
            guard requestCurrentLocked(request, nowNs: nowNs), request.desired == .standby,
                  retiredGeneration != generation else { return false }
            expectedZeroGeneration = generation; return true
        }
    }
    @discardableResult func recordBoundary(_ value: MediaBoundary, nowNs: UInt64) -> Bool {
        locked {
            guard let request = privacyRequest, mediaStarted, privacyPhase == .enteringStandby,
                  requestCurrentLocked(request, nowNs: nowNs), value.token.run == mediaRun,
                  value.token.revision == privacyRevision, value.pts >= 0,
                  value.duration > 0 else { return false }
            let required = SinkMask(rawValue: SinkMask.both.rawValue & ~terminalBranches)
            guard !required.isEmpty else { privacyFaultLocked(); return false }
            guard value.acceptedSinks.intersection(required) == required else { return false }
            switch value.token.kind {
            case .raw: return false
            case .slate:
                guard value.token == slateToken, value.codedIdr else { return false }
                slateConfirmed = true
            case .zero:
                guard value.token == zeroToken, let expectedZeroGeneration,
                      value.audioGeneration == expectedZeroGeneration, value.duration == 1024,
                      value.pts <= Int64.max - 1024 else { return false }
                if let zeroThrough, zeroThrough != value.pts { return false }
                zeroThrough = value.pts + 1024; zeroFrames = min(2048, zeroFrames + 1024)
            }
            guard zeroFrames >= 2048, !value.token.run.preparation.profile.hasVideo || slateConfirmed else { return false }
            privacyPhase = .standby
            return true
        }
    }
    /// Readiness is established on the samples owner before this final scalar
    /// commit. A newer urgent Standby or ordinary Stop wins without a false fault.
    func commitResume(_ request: PrivacyRequest, nowNs: UInt64) -> MediaAdmission {
        locked {
            guard preparation == request.preparation, mediaRun == request.run else { return .superseded }
            if mediaFailed { return .failed }
            if phase == .stopping || phase == .retained { return .stopped }
            guard privacyRequest == request, privacyPhase == .resuming else { return .superseded }
            guard requestCurrentLocked(request, nowNs: nowNs) else { return mediaFailed ? .failed : .superseded }
            privacyPhase = .live; privacyRequest = nil; return .allow
        }
    }
    func allowsInput(_ token: MediaToken, nowNs: UInt64? = nil) -> MediaAdmission {
        locked {
            guard mediaRun == token.run, preparation == token.run.preparation else { return .superseded }
            if mediaFailed { return .failed }
            if phase == .stopping || phase == .retained || !mediaStarted { return .stopped }
            guard token.revision == privacyRevision else { return .superseded }
            if privacyPhase != .standby, let request = privacyRequest, let deadline = request.deadlineNs {
                let now = nowNs ?? DispatchTime.now().uptimeNanoseconds
                if now < request.beganAtNs || now >= deadline { privacyFaultLocked(); return .failed }
            }
            switch token.kind {
            case .raw: return privacyPhase == .live && privacyDesired == .live && rawToken == token ? .allow : .superseded
            case .slate: return token.run.preparation.profile.hasVideo && (privacyDesired == .standby || privacyPhase == .resuming) && slateToken == token ? .allow : .superseded
            case .zero: return privacyDesired == .standby && zeroToken == token || privacyPhase == .resuming && zeroToken == token ? .allow : .superseded
            }
        }
    }
    /// The possibly blocking route/source wait is deliberately OUTSIDE the
    /// scalar lock. It cannot carry a token across a newer privacy fence.
    func rawAfterWait(_ token: MediaToken, wait: () -> Bool) -> MediaAdmission {
        let before = allowsInput(token)
        guard before == .allow else { return before }
        guard wait() else { return .superseded }
        return allowsInput(token)
    }
    /// Call only for actual previously submitted native work retained by the
    /// bounded codec ledger. Prefix is never a sanitised-output confirmation.
    func outputAdmission(_ token: MediaToken, nowNs: UInt64? = nil) -> OutputAdmission {
        locked {
            guard preparation == token.run.preparation, mediaRun == token.run || closingMediaRun == token.run else { return .superseded }
            if mediaFailed { return .failed }
            if token.revision != privacyRevision || phase == .stopping || phase == .rotating || closingMediaRun == token.run { return .acceptedPrefix }
            if privacyPhase != .standby, let request = privacyRequest, let deadline = request.deadlineNs {
                let now = nowNs ?? DispatchTime.now().uptimeNanoseconds
                if now < request.beganAtNs || now >= deadline { privacyFaultLocked(); return .failed }
            }
            return .current
        }
    }
    /// Native callbacks may synchronously latch only scalars. Cleanup/diagnostic
    /// work is dispatched after this method returns; a stale run cannot fail B.
    @discardableResult func nativeFault(_ token: MediaToken) -> Bool {
        locked {
            guard preparation == token.run.preparation, mediaRun == token.run || closingMediaRun == token.run,
                  !mediaFailed else { return false }
            privacyFaultLocked(); return true
        }
    }
    /// A reported native drop may have queued while live before Stop or a
    /// privacy/source fence. Its interval is unknown, so retain loss for THIS
    /// segment. Deliberate discard delegates never invoke this method.
    @discardableResult func nativeDrop(_ token: MediaToken, camera source: CameraSource? = nil) -> Bool {
        locked {
            guard token.kind == .raw, preparation == token.run.preparation,
                  mediaRun == token.run || closingMediaRun == token.run,
                  source.map({ $0.owner == token.run.preparation }) ?? true, !mediaFailed else { return false }
            privacyFaultLocked(); return true
        }
    }
    func hasCommittedMediaRun(_ value: MediaRun, start: Start? = nil) -> Bool {
        locked { committedMediaRun == value && (start.map { committedMediaStart == $0 } ?? true) && (mediaRun == value || closingMediaRun == value) }
    }
    /// Sole scalar finalisation decision for a concrete closed producer run.
    /// Ordinary Stop cancellation does not imply media loss. The native owner
    /// and recorder locks have already fenced callbacks and recording faults.
    func commitRecordingClosure(_ value: MediaRun, start: Start? = nil) -> Bool {
        locked {
            guard committedMediaRun == value, preparation == value.preparation,
                  start.map({ committedMediaStart == $0 }) ?? true,
                  mediaRun == value || closingMediaRun == value, !mediaFailed,
                  recordingClosureRun == nil else { return false }
            recordingClosureRun = value; return true
        }
    }
    func mediaHasFault(_ value: MediaRun) -> Bool {
        locked { preparation == value.preparation && (mediaRun == value || closingMediaRun == value) && mediaFailed }
    }
    /// Called only after the actual old codec/worker owners have closed.
    func mediaRunClosed(_ value: MediaRun) {
        locked {
            if mediaRun == value { mediaRun = nil; mediaStarted = false }
            if closingMediaRun == value { closingMediaRun = nil }
            if committedMediaRun == value { committedMediaRun = nil; committedMediaStart = nil }
            if recordingClosureRun == value { recordingClosureRun = nil }
        }
    }
    /// nil Stop means this exact committed audio show may continue, or the
    /// foreground returned. Prepared/queued Start is synchronously invalidated.
    func visibilityChanged(foreground active: Bool, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> (continuing: Preparation?, stop: Stop?) {
        locked {
            foreground = active
            if active { return (nil, nil) }
            if !mediaFailed, let preparation, AudioShowLifecycle.mayContinue(hasVideo: preparation.profile.hasVideo,
                optedIn: preparation.continuesAudioInBackground, committedRunning: phase == .running) { return (preparation, nil) }
            return (nil, phase == .idle ? nil : stopLocked(nowNs: nowNs))
        }
    }
    var backgroundContinuation: Preparation? {
        locked {
            guard !mediaFailed, let preparation, AudioShowLifecycle.mayContinue(hasVideo: preparation.profile.hasVideo,
                optedIn: preparation.continuesAudioInBackground, committedRunning: phase == .running) else { return nil }
            return preparation
        }
    }
    func ownsPreparation(_ ticket: Preparation) -> Bool { locked { preparation == ticket } }
    func interruption(_ event: AudioShowLifecycle.Interruption, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> Stop? {
        locked {
            guard event != .audioEnded, phase != .idle else { return nil }
            return stopLocked(nowNs: nowNs)
        }
    }
    func selectAudioInput(_ id: String?) -> Bool {
        guard id.map(AudioInputOption.validID) ?? true else { return false }
        return locked {
            guard phase == .idle else { return false }
            selectedAudioInputID = id; return true
        }
    }
    var preparedTicket: Preparation? { locked { phase == .prepared ? preparation : nil } }
    var currentPreparation: Preparation? {
        locked { [.stopping, .retained, .idle].contains(phase) ? nil : preparation }
    }
    func reservePreparation(profile: CaptureIntent = .defaultProfile) -> Preparation? {
        locked {
            guard foreground, phase == .idle, let serial = next() else { return nil }
            let ticket = Preparation(owner: identity, serial: serial, profile: profile, audioInputID: selectedAudioInputID,
                                     continuesAudioInBackground: selectedBackgroundAudio && !profile.hasVideo,
                                     orientationChoice: profile.hasVideo ? selectedOrientationChoice : .automatic,
                                     initialOrientation: profile.hasVideo ? (selectedOrientationChoice.fixedOrientation ?? latestAutoOrientation) : nil,
                                     initialCamera: selectedCamera, initialStandby: selectedInitialStandby)
            orientationObservation = nil
            preparation = ticket; candidatePending = true; hasCommittedRun = false; phase = .reserved
            privacyDesired = ticket.initialStandby ? .standby : .live
            privacyPhase = ticket.initialStandby ? .armed : .live
            privacyRevision = 0; privacyRequest = nil; mediaRun = nil; closingMediaRun = nil
            committedMediaRun = nil; committedMediaStart = nil; recordingClosureRun = nil
            rawToken = nil; slateToken = nil; zeroToken = nil; mediaStarted = false; mediaFailed = false; terminalBranches = 0
            mediaOperationDeadlineNs = nil
            mediaOperationBeganAtNs = nil
            if profile.hasVideo { camera.prepare(ticket, initial: selectedCamera) } else { camera.close() }
            timingPreparation = ticket
            captureBeganAtNs = nil; captureStoppedAtNs = nil
            broadcastBeganAtNs = nil; broadcastStoppedAtNs = nil
            return ticket
        }
    }
    /// The actor calls this only when its exact destination has been adopted.
    func attached(_ ticket: Preparation) -> Bool {
        locked {
            guard preparation == ticket, phase == .reserved else { return false }
            candidatePending = false; phase = .attached; return true
        }
    }
    func beginPermissions(_ ticket: Preparation) -> Bool {
        locked {
            guard preparation == ticket, phase == .attached else { return false }
            phase = .permissions; return true
        }
    }
    func permitsPreparation(_ ticket: Preparation) -> Bool {
        locked { preparation == ticket && phase == .permissions }
    }
    func prepared(_ ticket: Preparation) -> Bool {
        locked {
            guard preparation == ticket, phase == .permissions else { return false }
            phase = .prepared; return true
        }
    }
    func reserveStart(_ ticket: Preparation) -> Start? {
        locked {
            guard preparation == ticket, phase == .prepared, !mediaFailed,
                  !ticket.profile.hasVideo || privacyDesired == .standby || (camera.source == nil || (camera.confirmed && camera.pending == nil && !camera.failed)),
                  let serial = next() else { return nil }
            let value = Start(serial: serial, preparation: ticket)
            starting = value; phase = .starting; return value
        }
    }
    func permitsStart(_ ticket: Start) -> Bool {
        locked { preparation == ticket.preparation && starting == ticket && phase == .starting }
    }
    func started(_ ticket: Start, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds, broadcasting: Bool = true) -> Bool {
        locked { startedLocked(ticket, nowNs: nowNs, broadcasting: broadcasting) }
    }
    private func startedLocked(_ ticket: Start, nowNs: UInt64, broadcasting: Bool) -> Bool {
            guard preparation == ticket.preparation, starting == ticket, phase == .starting else { return false }
            if !hasCommittedRun {
                captureBeganAtNs = nowNs
                if broadcasting && broadcastStoppedAtNs == nil { broadcastBeganAtNs = nowNs }
            }
            starting = nil; hasCommittedRun = true; phase = .running; return true
    }
    func beginRotation(_ ticket: Preparation, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> Rotation? {
        locked {
            guard preparation == ticket, ticket.profile.hasVideo, ticket.orientationChoice == .automatic else { return nil }
            if phase == .rotating { return rotation }
            guard phase == .running, let serial = next() else { return nil }
            guard let deadline = privacyDeadlineLocked(nowNs) else { return nil }
            mediaOperationDeadlineNs = deadline; mediaOperationBeganAtNs = nowNs
            let value = Rotation(serial: serial, preparation: ticket)
            closingMediaRun = mediaRun; mediaRun = nil; mediaStarted = false
            rotation = value; phase = .rotating; return value
        }
    }
    func permitsRotation(_ ticket: Rotation) -> Bool {
        locked { preparation == ticket.preparation && rotation == ticket && phase == .rotating }
    }
    func reserveRotationStart(_ ticket: Rotation) -> Start? {
        locked {
            guard preparation == ticket.preparation, rotation == ticket, phase == .rotating,
                  let serial = next() else { return nil }
            let value = Start(serial: serial, preparation: ticket.preparation)
            rotation = nil; starting = value; phase = .starting; return value
        }
    }
    /// Synchronous invalidation precedes queued cancellation. Duplicate Stop
    /// joins the same cleanup owner; it cannot release that owner's capacity.
    func stop(nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> Stop {
        locked { stopLocked(nowNs: nowNs) }
    }
    func stop(preparation ticket: Preparation, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> Stop? {
        locked { guard preparation == ticket, stopping == nil else { return nil }; return stopLocked(nowNs: nowNs) }
    }
    /// Ending a destination retires its initial camera approval, while an
    /// existing recording keeps its intent through rotation and restart.
    func stopIfPending(_ ticket: Preparation, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> Stop? {
        locked {
            guard preparation == ticket, !hasCommittedRun, phase != .idle else { return nil }
            return stopLocked(nowNs: nowNs)
        }
    }
    private func stopLocked(nowNs: UInt64) -> Stop {
        if captureStoppedAtNs == nil { captureStoppedAtNs = nowNs }
        if broadcastStoppedAtNs == nil { broadcastStoppedAtNs = nowNs }
        if let stopping { return stopping }
        // At serial exhaustion no fresh preparation can ever be admitted.
        // A saturated cleanup ticket still permits the current owner to stop.
        let value = Stop(serial: next() ?? UInt64.max, preparation: preparation)
        stopping = value; starting = nil; rotation = nil; phase = .stopping
        privacyPhase = .stopped; mediaStarted = false
        camera.close()
        return value
    }
    /// Cold destination End/expiry only. Never stops the independent capture or
    /// starts a clock. A retired ticket cannot alter a successor's elapsed time.
    func broadcastEnded(_ ticket: Preparation, nowNs: UInt64) {
        locked {
            guard preparation == ticket, timingPreparation == ticket else { return }
            // The caller first closes the actual publisher admission, including
            // End before a native run exists. This logical mask survives Auto.
            terminalBranches |= Sink.publishing.rawValue
            broadcastStoppedAtNs = min(broadcastStoppedAtNs ?? nowNs, nowNs)
        }
    }
    func elapsed(_ ticket: Preparation, nowNs: UInt64) -> CaptureElapsedSnapshot {
        locked {
            guard timingPreparation == ticket else {
                return CaptureElapsedSnapshot(captureNs: nil, broadcastNs: nil, captureRunning: false, broadcastRunning: false)
            }
            func duration(_ start: UInt64?, _ stop: UInt64?) -> UInt64? {
                guard let start else { return nil }
                let end = stop ?? nowNs
                return end >= start ? end - start : nil
            }
            return CaptureElapsedSnapshot(captureNs: duration(captureBeganAtNs, captureStoppedAtNs),
                broadcastNs: duration(broadcastBeganAtNs, broadcastStoppedAtNs),
                captureRunning: captureBeganAtNs != nil && captureStoppedAtNs == nil,
                broadcastRunning: broadcastBeganAtNs != nil && broadcastStoppedAtNs == nil)
        }
    }
    func permitsStop(_ ticket: Stop) -> Bool {
        locked { stopping == ticket && (phase == .stopping || phase == .retained) }
    }
    func awaitsCandidate(_ ticket: Preparation) -> Bool {
        locked { preparation == ticket && candidatePending }
    }
    /// Factory throw means no owner was created; otherwise only an actual
    /// rejected-candidate close may acknowledge this pre-factory reservation.
    @discardableResult func candidateClosed(_ ticket: Preparation, cleanupComplete: Bool) -> Bool {
        locked {
            guard preparation == ticket, candidatePending else { return false }
            guard cleanupComplete else { phase = .retained; return false }
            candidatePending = false
            return releaseStoppedIfReady()
        }
    }
    private func releaseStoppedIfReady() -> Bool {
        guard stopping != nil, stoppedOwnersClean, !candidatePending else { return false }
        preparation = nil; starting = nil; rotation = nil; stopping = nil
        stoppedOwnersClean = false; hasCommittedRun = false; phase = .idle
        return true
    }
    /// False means cleanup ownership remains retained. A later real completion
    /// for this same owner may release admission, but never auto-start capture.
    @discardableResult func completeStop(_ ticket: Stop, cleanupComplete: Bool) -> Bool {
        locked {
            guard stopping == ticket, phase == .stopping || phase == .retained else { return false }
            stoppedOwnersClean = cleanupComplete
            guard cleanupComplete else { phase = .retained; return false }
            return releaseStoppedIfReady()
        }
    }
    /// Factory/permission/configuration failure retires only its exact intent.
    /// The caller must have released any native owners before reporting true.
    /// A racing Stop owns its own barrier and cannot be bypassed by this path.
    @discardableResult func failed(_ ticket: Preparation, cleanupComplete: Bool) -> Bool {
        locked {
            guard preparation == ticket, stopping == nil, !hasCommittedRun else { return false }
            starting = nil; rotation = nil
            guard cleanupComplete else { phase = .retained; return false }
            candidatePending = false; preparation = nil; hasCommittedRun = false; phase = .idle; return true
        }
    }
}
