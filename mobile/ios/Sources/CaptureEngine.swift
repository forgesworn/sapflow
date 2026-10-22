import AVFoundation
import UIKit

struct CaptureSnapshot {
    var orientation = CaptureOrientation.portrait
    var orientationChoice = CaptureOrientationChoice.automatic
    var profile = CaptureIntent.defaultProfile
    var cameraRequested = CameraPosition.rear
    var cameraActive: CameraPosition?
    var cameraPending = false
    var cameraPreviewMirrored = false
    var cameraControl: CaptureAdmission.CameraSource?
    var cameraCapabilities = CameraCapabilities()
    var cameraValues = CameraNativeValues()
    var cameraControlPending = false
    var cameraRequestedCommand: CameraCommand?
    var cameraDetail = "Camera is not verified"
    var phase = "Idle"
    var detail = "Prepare the camera and microphone to begin."
    var publisher = "Publisher not attached · local recording only"
    var level: Float = -60
    var encoded: UInt64 = 0
    var captureDrops: UInt64 = 0
    var recordingIOBusy = false
    var canStart = false
    var running = false
    var captureElapsedNs: UInt64?
    var broadcastElapsedNs: UInt64?
    var audioInputs: [AudioInputOption] = []
    var requestedAudioInputID: String?
    var activeAudioInput = "Not verified"
    var backgroundAudioEnabled = false
    var systemRecordingControl = "Stop and save is available in Sapflow; system recording controls are unavailable on this SDK."
    var stopControl: AudioShowStopControl?
    var socketKilobitsPerSecond: Double?
}

extension CaptureEngine: V4vCreatorCapturePort {}

final class CaptureRun {
    let profile: CaptureIntent
    let recorder: CompressedRecorder
    let recordingTicket: RecordingCaptureAdmission.Ticket
    let worker: IngressWorker
    var video: HardwareVideoEncoder?
    var audio: NativeAudioEncoder!
    var audioRoute: AudioInputRouting?
    var epoch: CMTime?
    var videoTimeline = CameraVideoTimeline()
    var admitting = false
    var startup: [(sample: CMSampleBuffer, video: Bool)] = []
    var startupBytes = 0
    init(recorder: CompressedRecorder, recordingTicket: RecordingCaptureAdmission.Ticket, orientation: CaptureOrientation, profile: CaptureIntent, ingress: RustEncodedIngress, failure: @escaping (Error) -> Void, publishingFailure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void, meter: @escaping (Float) -> Void) throws {
        self.profile = profile
        self.recorder = recorder; self.recordingTicket = recordingTicket
        worker = IngressWorker(ingress: ingress, failure: publishingFailure, statistics: statistics)
        let recorder = self.recorder; let worker = self.worker
        let output: (CompressedUnit) -> Void = { unit in recorder.offer(unit); worker.offer(unit) }
        if let camera = profile.videoProfile {
            video = try HardwareVideoEncoder(orientation: orientation, profile: camera, output: output, failure: failure)
        }
        audio = try NativeAudioEncoder(output: output, meter: meter, recordingTiming: profile.hasVideo ? nil : { recorder.audioTiming($0) })
    }
}

private final class RecordingRunStop {
    let run: CaptureRun
    let rotationToken: UInt64?
    let rotation: CaptureAdmission.Rotation?
    var publishing: Result<Void, Error>?
    var recording: CompressedRecorder.Completion?
    var completed = false
    init(run: CaptureRun, rotationToken: UInt64?, rotation: CaptureAdmission.Rotation?) {
        self.run = run; self.rotationToken = rotationToken; self.rotation = rotation
    }
}

/// Session configuration, encoded callbacks, disk and FFI work each have an
/// explicit serial owner. Raw camera pixels and microphone PCM stay native.
// Sendability is provided by the control/samples queue ownership documented
// above; no destination replacement is allowed while a run is finalising.
final class CaptureEngine: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureAudioDataOutputSampleBufferDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    private let control = DispatchQueue(label: "sapflow.capture-control", qos: .userInitiated)
    private let recordingIO = DispatchQueue(label: "sapflow.recording-io", qos: .utility)
    private let recordingAdmission = RecordingCaptureAdmission()
    private let recordingBindingLock = NSLock()
    private var recordingBinding: (catalogue: RecordingCatalogue, admission: RecordingLibraryAdmission)?
    private var recordingShow: (CaptureAdmission.Preparation, RecordingCatalogue.Show)? // control only
    private var retainedRecorder: CompressedRecorder?
    private var runStop: RecordingRunStop?
    private var preparedPublisherClosed: (CaptureAdmission.Stop, Bool)?
    private var storageCheckPending = false // one coalesced cold capacity query
    private let samples = DispatchQueue(label: "sapflow.capture-samples", qos: .userInteractive)
    private var run: CaptureRun? // samples queue only
    private var captureGeneration: UInt64 = 0 // control queue only
    private var audioRoute: AudioInputRouting? // control owner; immutable reference captured by CaptureRun
    private var audioPin: (CaptureAdmission.Preparation, AudioInputPin)?
    private var audioEpoch: UInt64 = 0
    private var cameraDevice: CameraDeviceControls? // control only; immutable source delegate owns video callbacks
    private var cameraSwitchTimer: DispatchWorkItem?
    private var deferredOrientation: CaptureOrientation?
    private let cameraCommands = CameraCommandMailbox<CaptureAdmission.CameraSource>()
    private var configured = false // control queue only
    private var configuredProfile: CaptureIntent? // control queue only
    private var orientationPlan = OrientationRestartPlan()
    private var rotationTimeout: DispatchWorkItem?
    private var rotationDeadline: RecordingCaptureAdmission.RotationDeadline?
    private let admission = CaptureAdmission()
    private var preparedIntent: CaptureAdmission.Preparation? // control owner only
    private var rotationIntent: CaptureAdmission.Rotation?
    private var stopIntent: CaptureAdmission.Stop?
    private var rejectedCandidate: (CaptureAdmission.Preparation, EncodedPublishingSink)?
    private var stopping = false
    private var recordingFailureDescription: String?
    private var stopReason: String?
    private var snapshot = CaptureSnapshot() // control queue only
    private var storageTimer: DispatchSourceTimer?
    private var telemetryTimer: DispatchSourceTimer? // one per logical capture, survives rotation
    private var telemetryIntent: CaptureAdmission.Preparation? // control queue only
    private var telemetryReads = TelemetryReadAdmission<CaptureAdmission.Preparation>()
    private var socketRate = SocketOutboundRate()
    private var notifications: [NSObjectProtocol] = []
    private let ingress = RustEncodedIngress()
    private let destinationLock = NSLock()
    private var destinationFence: EncodedPublishingSink?
    var onChange: ((CaptureSnapshot) -> Void)?
    private var lastMeterUpdate = 0.0
    private var cleanupObservation: (CaptureAdmission.Preparation, () -> Void)? // control only, one exact show

    override init() {
        super.init()
        let centre = NotificationCenter.default
        for name in [AVCaptureSession.wasInterruptedNotification, AVCaptureSession.runtimeErrorNotification,
                     AVAudioSession.interruptionNotification, AVAudioSession.mediaServicesWereResetNotification] {
            notifications.append(centre.addObserver(forName: name, object: nil, queue: nil) { [weak self] notification in
                guard let self else { return }
                let event: AudioShowLifecycle.Interruption
                if notification.name == AVAudioSession.interruptionNotification {
                    let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
                    event = type == AVAudioSession.InterruptionType.ended.rawValue ? .audioEnded : .audioBegan
                } else if notification.name == AVAudioSession.mediaServicesWereResetNotification { event = .mediaServicesReset }
                else {
                    guard notification.object as? AVCaptureSession === self.session else { return }
                    event = .captureFailed
                }
                guard let ticket = self.admission.interruption(event) else { return }
                self.stopCaptured(ticket, reason: "Capture interrupted. Return to the foreground and prepare again.")
            })
        }
    }
    deinit { for token in notifications { NotificationCenter.default.removeObserver(token) }; storageTimer?.cancel(); rotationTimeout?.cancel(); telemetryTimer?.cancel() }

    /// Attach a real bounded destination adapter before preparing. No endpoint
    /// or credential is inferred from a display label or bundled configuration.
    private var recordings: (catalogue: RecordingCatalogue, admission: RecordingLibraryAdmission)? {
        recordingBindingLock.lock(); defer { recordingBindingLock.unlock() }; return recordingBinding
    }
    var canChooseCaptureProfile: Bool { recordings != nil && admission.canChooseCaptureProfile && !recordingAdmission.hasOwner }
    var canReservePreparation: Bool { canChooseCaptureProfile }
    /// Root installs the same app-owned catalogue used by the library. No file
    /// work is performed here, and a second instance is never silently opened.
    func setRecordingCatalogue(_ catalogue: RecordingCatalogue, admission gate: RecordingLibraryAdmission,
                               completion: @escaping (Bool) -> Void) {
        control.async {
            guard self.admission.canChooseCaptureProfile, !self.recordingAdmission.hasOwner, !catalogue.cleanupUncertain else { completion(false); return }
            self.recordingBindingLock.lock()
            let accepted = self.recordingBinding == nil
            if accepted { self.recordingBinding = (catalogue, gate) }
            self.recordingBindingLock.unlock()
            completion(accepted)
        }
    }
    func selectCamera(_ position: CameraPosition) -> Bool {
        canChooseCaptureProfile && admission.selectCamera(position)
    }
    func cameraCommand(_ command: CameraCommand, source: CaptureAdmission.CameraSource) {
        guard admission.permitsCameraControls(source) else { return }
        if cameraCommands.offer(source, command) { control.async { self.drainCameraCommands() } }
    }
    private func drainCameraCommands() {
        for request in cameraCommands.take() {
            let source = request.source; let command = request.command
            guard admission.permitsCameraControls(source), let device = cameraDevice, device.source == source else { finishCameraCommand(request); continue }
            guard device.capabilities.accepts(command) else { finishCameraCommand(request); snapshot.cameraDetail = "Camera control is unsupported or outside its current range"; publish(); continue }
            do {
                snapshot.cameraRequestedCommand = command; snapshot.cameraControlPending = true; publish()
                try device.apply(command) { [weak self, weak device] in
                    self?.control.async {
                        guard let self else { return }
                        let confirmed = self.finishCameraCommand(request)
                        guard confirmed, let device, self.admission.permitsCameraControls(source), self.cameraDevice === device else { return }
                        self.snapshot.cameraValues = device.values; self.snapshot.cameraCapabilities = device.capabilities
                        self.snapshot.cameraControlPending = self.cameraCommands.pending(source); self.snapshot.cameraDetail = "Native-reported camera settings; remote picture is not verified"; self.publish()
                    }
                }
            } catch {
                guard admission.permitsCameraControls(source) else { continue }
                finishCameraCommand(request)
                snapshot.cameraControlPending = cameraCommands.pending(source); snapshot.cameraDetail = "Camera setting could not be applied"; publish()
            }
        }
    }
    @discardableResult private func finishCameraCommand(_ request: CameraCommandMailbox<CaptureAdmission.CameraSource>.Request) -> Bool {
        guard let result = cameraCommands.complete(request) else { return false }
        if result.schedule { control.async { self.drainCameraCommands() } }
        return result.confirmed
    }
    func switchCamera(_ position: CameraPosition, source: CaptureAdmission.CameraSource) {
        guard admission.permitsCameraControls(source),
              let request = admission.reserveCameraSwitch(position, source: source, nowNs: DispatchTime.now().uptimeNanoseconds) else { return }
        control.async { self.switchCameraOnControl(request) }
    }
    private func switchCameraOnControl(_ request: CaptureAdmission.CameraSwitch) {
        guard admission.permitsCameraSwitch(request, nowNs: DispatchTime.now().uptimeNanoseconds),
              let old = cameraDevice, let profile = request.owner.profile.videoProfile else {
            if admission.rejectCameraSwitch(request) {
                refreshCameraSnapshot(); snapshot.cameraDetail = "Camera request expired before source retirement"; publish(); applyDeferredOrientation()
            }
            return
        }
        guard !cameraCommands.pending(old.source), samples.sync(execute: { self.run == nil || self.run?.epoch != nil }) else {
            _ = admission.rejectCameraSwitch(request); snapshot.cameraDetail = "Camera settings or capture startup are busy; try switching when ready"; publish(); return
        }
        snapshot.cameraRequested = request.position; snapshot.cameraPending = true; snapshot.cameraControl = nil
        snapshot.canStart = false
        snapshot.cameraDetail = "Checking requested camera; remote picture is not verified"; publish()
        var retired = false
        do {
            let changed = try CameraSourceTransaction.perform(preflight: {
                try CameraDeviceControls.preflight(position: request.position, profile: profile)
            }, retire: { () -> CaptureAdmission.CameraSource? in
                guard self.admission.permitsCameraSwitch(request, nowNs: DispatchTime.now().uptimeNanoseconds) else { return nil }
                // All previously admitted raw work precedes this sample-owner
                // boundary. Publish retirement before any native mutation or
                // flush, so queued audio inherits the original deadline too.
                return try CameraSourceTransaction.onSampleOwner(queue: self.samples, retire: {
                    guard let source = self.admission.retireCameraSwitch(request, nowNs: DispatchTime.now().uptimeNanoseconds) else { return nil }
                    retired = true; return source
                }, seal: { _ in
                    old.pause()
                    if let run = self.run, let encoder = run.video {
                        let duration = try encoder.sealForCameraSwitch()
                        guard run.videoTimeline.beginSourceSwitch(sealedDuration: duration) else {
                            throw CaptureFailure(message: "Camera switch has no valid media boundary")
                        }
                    }
                }, sealFailed: { source, _ in
                    // A real VT/prefix error is a samples-owned media fault,
                    // even if ordinary Stop concurrently claimed cleanup.
                    self.fenceCameraMedia(source.owner, reason: "Video encoder failed while sealing camera switch")
                }, requestKeyframe: { _ in self.run?.video?.requestKeyframe() })
            }, isCurrent: { self.admission.permitsCameraFrame($0) && self.admission.permitsCameraSwitch(request, nowNs: DispatchTime.now().uptimeNanoseconds) }, retirementStarted: { retired }, replace: { candidate, source in
                try self.replaceCamera(candidate, source: source, orientation: self.orientationPlan.desired, profile: profile)
            })
            guard changed else {
                if retired { failCamera(request.owner, reason: "Camera switch was cancelled or expired after producer retirement") }
                else { _ = admission.rejectCameraSwitch(request); refreshCameraSnapshot(); publish(); applyDeferredOrientation() }
                return
            }
            publish()
            let timer = DispatchWorkItem { [weak self] in
                guard let self, self.admission.pendingCameraSwitch == request else { return }
                _ = self.admission.failCameraSwitch(request)
                self.failCamera(request.owner, reason: "Camera switch exceeded six seconds; Stop and prepare again")
            }
            cameraSwitchTimer = timer
            control.asyncAfter(deadline: DispatchTime(uptimeNanoseconds: request.deadlineNs), execute: timer)
        } catch {
            if retired { _ = admission.failCameraSwitch(request); failCamera(request.owner, reason: "Camera switch failed after retiring the old source") }
            else if admission.rejectCameraSwitch(request) {
                refreshCameraSnapshot(); snapshot.cameraDetail = "Selected camera cannot provide the prepared format; current camera unchanged"; publish()
                applyDeferredOrientation()
            }
        }
    }
    private func failCamera(_ preparation: CaptureAdmission.Preparation, reason: String) {
        CameraSourceTransaction.claimFailure(claim: { self.admission.stop(preparation: preparation) }, markIncomplete: {
            self.samples.sync {
                guard let run = self.run, run.recordingTicket.request.preparation == preparation else { return }
                run.admitting = false; run.recorder.markIncomplete(CaptureFailure(message: reason))
            }
        }, stop: { ticket in self.stopCaptured(ticket, reason: reason) })
    }
    private func refreshCameraSnapshot() {
        let source = admission.cameraControlSource
        snapshot.cameraControl = source
        snapshot.cameraPending = admission.pendingCameraSwitch != nil
        snapshot.cameraRequested = admission.pendingCameraSwitch?.position ?? admission.desiredCamera
        snapshot.canStart = admission.currentPhase == .prepared && (source != nil || !snapshot.profile.hasVideo)
        if let source, let device = cameraDevice, device.source == source {
            snapshot.cameraActive = source.position
            snapshot.cameraCapabilities = device.capabilities; snapshot.cameraValues = device.values
        } else { snapshot.cameraActive = nil }
    }
    private func applyDeferredOrientation() {
        guard deferredOrientation != nil else { return }
        deferredOrientation = nil
        if let observation = admission.latestOrientationObservation { applyOrientationObservation(observation) }
    }
    func selectAudioInput(_ id: String?) -> Bool {
        guard canChooseCaptureProfile else { return false }
        return admission.selectAudioInput(id)
    }
    func selectBackgroundAudio(_ enabled: Bool) -> Bool {
        canChooseCaptureProfile && admission.selectBackgroundAudio(enabled)
    }
    func selectOrientationChoice(_ choice: CaptureOrientationChoice) -> Bool {
        canChooseCaptureProfile && admission.selectOrientationChoice(choice)
    }
    var backgroundContinuation: CaptureAdmission.Preparation? { admission.backgroundContinuation }
    /// Admission and the committed Start share one lock. No timer, UI snapshot
    /// or scene callback can grant continuation to a merely starting owner.
    @discardableResult func setForeground(_ active: Bool) -> CaptureAdmission.Preparation? {
        let action = admission.visibilityChanged(foreground: active)
        if let ticket = action.stop { stopCaptured(ticket, reason: "Capture interrupted. Return to the foreground and prepare again.") }
        if active {
            destinationLock.lock(); let publisher = destinationFence; destinationLock.unlock()
            publisher?.revalidateAuthority()
        }
        return action.continuing
    }
    @discardableResult func stop(preparation: CaptureAdmission.Preparation) -> Bool {
        stop(control: AudioShowStopControl(preparation: preparation, admission: admission))
    }
    @discardableResult func stop(control: AudioShowStopControl) -> Bool {
        guard let ticket = control.perform(on: admission) else { return false }
        stopCaptured(ticket, reason: nil)
        return true
    }
    private func stopCaptured(_ ticket: CaptureAdmission.Stop, reason: String?) {
        recordingAdmission.cancel(ticket.preparation); fencePublishing()
        control.async {
            guard self.admission.permitsStop(ticket) else { return }
            self.stopIntent = ticket; self.stopOnControl(reason: reason)
        }
    }
    /// The single listener is an internal app-lifetime journal lease owner.
    /// Admission releases only after every real native/file/candidate closes.
    func whenCleanupComplete(_ ticket: CaptureAdmission.Preparation, completion: @escaping () -> Void) {
        control.async {
            guard self.admission.ownsPreparation(ticket) else { completion(); return }
            guard self.cleanupObservation == nil else { return }
            self.cleanupObservation = (ticket, completion)
        }
    }
    private func notifyCleanupIfReleased() {
        guard let (ticket, completion) = cleanupObservation, !admission.ownsPreparation(ticket) else { return }
        cleanupObservation = nil; completion()
    }
    func refreshAudioInputs(profile: CaptureIntent) {
        control.async {
            guard self.canChooseCaptureProfile else { return }
            do {
                // Inventory is category-dependent; never activate the session just to list it.
                let audio = AVAudioSession.sharedInstance()
                try audio.setCategory(.playAndRecord, mode: profile.hasVideo ? .videoRecording : .measurement,
                                      options: [.defaultToSpeaker, .allowBluetooth])
                let inputs = AudioInputRouting.inventory(audio)
                guard self.canChooseCaptureProfile else { return }
                self.snapshot.audioInputs = inputs
                self.publish()
            } catch { self.snapshot.detail = "Microphone inventory unavailable: \(error.localizedDescription)"; self.publish() }
        }
    }
    private func attachAudioRoute(_ preparation: CaptureAdmission.Preparation) throws -> AudioInputRouting {
        audioRoute?.close(); audioRoute = nil
        if audioPin?.0 != preparation { audioPin = (preparation, AudioInputPin(requestedID: preparation.audioInputID)) }
        guard let pin = audioPin?.1 else { throw CaptureFailure(message: "Microphone selection is unavailable") }
        guard audioEpoch < UInt64.max else { throw CaptureFailure(message: "Microphone owner counter exhausted") }
        audioEpoch += 1
        let owner = AudioInputOwner(preparation: preparation, epoch: audioEpoch)
        let route = try AudioInputRouting.attach(owner: owner, pin: pin, failure: { [weak self] in
            self?.control.async {
                guard let self, self.audioRoute?.owner == owner, self.preparedIntent == preparation,
                      self.admission.currentPreparation == preparation else { return }
                self.samples.sync {
                    guard let run = self.run, run.audioRoute?.owner == owner else { return }
                    run.admitting = false
                    run.recorder.markIncomplete(CaptureFailure(message: "Microphone changed or disconnected"))
                }
                self.snapshot.activeAudioInput = "Microphone changed or disconnected"
                self.stopOnControl(reason: self.audioRoute?.failureDescription)
            }
        }, changed: { [weak self] id in self?.control.async {
            guard let self, self.audioRoute?.owner == owner, self.audioRoute?.permitsPCM() == true else { return }
            self.snapshot.activeAudioInput = self.snapshot.audioInputs.first { $0.id == id }?.name ?? "Verified microphone"
            self.publish()
        } })
        audioRoute = route
        snapshot.audioInputs = AudioInputRouting.inventory()
        snapshot.requestedAudioInputID = preparation.audioInputID
        return route
    }
    func reservePreparation(profile: CaptureIntent = .defaultProfile) -> CaptureAdmission.Preparation? {
        guard let binding = recordings else { return nil }
        return binding.admission.withCaptureReservation {
            guard !self.recordingAdmission.hasOwner else { return nil }
            return self.admission.reservePreparation(profile: profile)
        }
    }
    /// The destination owner separately fences its captured publisher. This
    /// exact camera fence cancels initial approval without stopping a recording
    /// that already began, including its later orientation epochs.
    func cancelPendingPreparation(_ preparation: CaptureAdmission.Preparation) {
        guard let ticket = admission.stopIfPending(preparation) else { return }
        recordingAdmission.cancel(preparation)
        control.async {
            guard self.admission.permitsStop(ticket) else { return }
            self.stopIntent = ticket
            self.stopOnControl(reason: nil)
        }
    }
    /// A factory that failed before creating native owners can retire only its
    /// exact reservation. It cannot bypass an already requested Stop barrier.
    func abandonPreparation(_ ticket: CaptureAdmission.Preparation) {
        control.async {
            let released = self.admission.candidateClosed(ticket, cleanupComplete: true)
            _ = self.admission.failed(ticket, cleanupComplete: true)
            if released { self.admissionStopped() }
            self.notifyCleanupIfReleased()
        }
    }
    func setDestination(_ destination: EncodedPublishingSink?, preparation ticket: CaptureAdmission.Preparation,
                        completion: @escaping (Bool) -> Void = { _ in }) {
        control.async {
            let ownsReservation = self.admission.currentPreparation == ticket && self.admission.currentPhase == .reserved
            guard !self.snapshot.running, !self.stopping, ownsReservation,
                  self.ingress.destination?.cleanupComplete != false, self.admission.attached(ticket) else {
                // A rejected newly constructed coordinator still owns timers.
                // Close it without touching the current destination or successor.
                let pending = self.admission.awaitsCandidate(ticket)
                if pending, let destination, destination !== self.ingress.destination {
                    self.rejectedCandidate = (ticket, destination)
                }
                let finish = { [destination] in
                    self.control.async {
                        guard pending else { return }
                        let clean = destination?.cleanupComplete != false
                        let released = self.admission.candidateClosed(ticket, cleanupComplete: clean)
                        if clean, self.rejectedCandidate?.0 == ticket { self.rejectedCandidate = nil }
                        if ownsReservation { _ = self.admission.failed(ticket, cleanupComplete: clean && self.ingress.destination?.cleanupComplete != false) }
                        if released { self.admissionStopped() }
                        self.notifyCleanupIfReleased()
                    }
                }
                if let destination, destination !== self.ingress.destination {
                    destination.endDestination(); destination.stop(completion: finish)
                } else { finish() }
                completion(false); return
            }
            self.ingress.destination?.endDestination()
            self.ingress.destination = destination
            self.preparedIntent = ticket
            if let orientation = self.admission.orientationAtAttachment(ticket) {
                self.orientationPlan.prepare(orientation)
                self.snapshot.orientation = orientation
            }
            self.snapshot.orientationChoice = ticket.orientationChoice
            self.snapshot.cameraRequested = ticket.initialCamera; self.snapshot.cameraActive = nil
            self.snapshot.cameraControl = nil; self.snapshot.cameraPending = false
            destination?.observeBroadcastEnd { [weak self] instant in self?.admission.broadcastEnded(ticket, nowNs: instant) }
            if self.configuredProfile != ticket.profile { self.configured = false }
            self.snapshot.profile = ticket.profile
            self.snapshot.backgroundAudioEnabled = ticket.continuesAudioInBackground
            self.snapshot.captureElapsedNs = nil; self.snapshot.broadcastElapsedNs = nil; self.snapshot.socketKilobitsPerSecond = nil
            self.destinationLock.lock(); self.destinationFence = destination; self.destinationLock.unlock()
            self.snapshot.publisher = self.ingress.status
            self.publish(); completion(true)
        }
    }
    private func fencePublishing() {
        destinationLock.lock(); let owner = destinationFence; destinationLock.unlock()
        owner?.endDestination()
    }
    private func fenceCodecEpoch() {
        destinationLock.lock(); let owner = destinationFence; destinationLock.unlock()
        owner?.closeCodecEpoch()
    }
    func setOrientation(_ orientation: CaptureOrientation) {
        guard let observation = admission.observeOrientation(orientation) else { return }
        control.async { self.applyOrientationObservation(observation) }
    }
    private func applyOrientationObservation(_ observation: CaptureAdmission.OrientationObservation) {
        guard self.admission.permitsOrientation(observation) else { return }
        guard (self.preparedIntent?.profile ?? self.snapshot.profile).hasVideo else { return }
        if self.admission.pendingCameraSwitch != nil { self.deferredOrientation = observation.orientation; return }
        guard self.orientationPlan.desired != observation.orientation else { return }
        let token = self.orientationPlan.request(observation.orientation, running: self.snapshot.running)
        if let token {
            guard let intent = self.preparedIntent, let child = self.admission.beginRotation(intent) else {
                self.orientationPlan.cancel(); return
            }
            guard let deadline = RecordingCaptureAdmission.RotationDeadline(preparation: intent, nowNs: DispatchTime.now().uptimeNanoseconds) else {
                self.stopOnControl(reason: "Rotation deadline could not be established."); return
            }
            self.rotationDeadline = deadline
            self.rotationIntent = child
            let timeout = DispatchWorkItem { [weak self] in
                guard let self, self.orientationPlan.pending == token else { return }
                self.orientationPlan.cancel(); self.rotationTimeout = nil
                self.stopIntent = self.admission.stop(); self.recordingAdmission.cancel(self.stopIntent?.preparation); self.rotationIntent = nil
                self.fencePublishing()
                self.stopReason = "Rotation finalisation timed out; automatic restart cancelled."
                self.snapshot.detail = "Rotation finalisation exceeded six seconds. Automatic restart cancelled; cleanup may still finish."
                self.publish()
                self.stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled.")
            }
            self.rotationTimeout = timeout
            self.control.asyncAfter(deadline: DispatchTime(uptimeNanoseconds: deadline.deadlineNs), execute: timeout)
            self.stopOnControl(reason: nil)
        } else if !self.stopping && !self.snapshot.running {
            guard self.admission.currentPhase != .stopping, self.admission.currentPhase != .retained else { return }
            do {
                let resumePreview = self.session.isRunning
                if self.configured { try self.applyOrientation() }
                self.snapshot.orientation = observation.orientation
                if resumePreview { self.setSampleDelivery(true); self.session.startRunning() }
                if self.snapshot.canStart { self.snapshot.detail = self.orientationDescription }
                self.publish()
            } catch { self.report(error) }
        }
    }
    private var orientationDescription: String {
        let profile = preparedIntent?.profile ?? snapshot.profile
        guard let camera = profile.videoProfile else { return "AAC-LC 48 kHz mono · 96 kbit/s · microphone only" }
        let dimensions = camera.dimensions(for: orientationPlan.desired)
        return "\(dimensions.width)×\(dimensions.height) at \(camera.frameRate) fps · hardware H.264 · AAC 48 kHz mono"
    }
    private func applyOrientation() throws {
        guard !snapshot.running, !stopping else { throw CaptureFailure(message: "Orientation requires a new encoder epoch") }
        guard (preparedIntent?.profile ?? snapshot.profile).hasVideo else { return }
        setSampleDelivery(false)
        if session.isRunning { session.stopRunning() }
        guard let preparation = preparedIntent, let profile = preparation.profile.videoProfile else { return }
        let dimensions = profile.dimensions(for: orientationPlan.desired)
        let candidate = try CameraDeviceControls.preflight(position: admission.desiredCamera, profile: profile)
        guard let source = admission.initialCameraSource(canvas: .init(width: Int(dimensions.width), height: Int(dimensions.height))) else {
            throw CaptureFailure(message: "Camera preparation was retired")
        }
        try replaceCamera(candidate, source: source, orientation: orientationPlan.desired, profile: profile)
        snapshot.orientation = orientationPlan.desired
    }
    private func setSampleDelivery(_ enabled: Bool) {
        // Stop the old producers before the sample-queue barrier/encoder flush.
        // A later epoch restores delegates only after its orientation is set.
        for output in session.outputs {
            if let video = output as? AVCaptureVideoDataOutput {
                if enabled, let device = cameraDevice, video === device.output { device.deliver(on: samples) }
                else { video.setSampleBufferDelegate(nil, queue: nil) }
            } else if let audio = output as? AVCaptureAudioDataOutput {
                audio.setSampleBufferDelegate(enabled ? self : nil, queue: enabled ? samples : nil)
            }
        }
    }
    private func restartForOrientation(_ child: CaptureAdmission.Rotation) {
        guard admission.permitsRotation(child) else { return }
        guard rotationDeadline?.permits(child.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true else {
            stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled."); return
        }
        do {
            guard AVCaptureDevice.authorizationStatus(for: .video) == .authorized,
                  AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else {
                throw CaptureFailure(message: "Capture permission changed; prepare again")
            }
            guard let start = admission.reserveRotationStart(child) else { return }
            snapshot.canStart = true
            startOnControl(start, keepSavedSegment: true)
            if snapshot.running {
                snapshot.detail = "\(orientationDescription). Rotation began a new recording segment; active publishing reconnects. The previous segment is retained in the private recording library."
                publish()
            }
        } catch { stopOnControl(reason: error.localizedDescription) }
    }
    func prepare() {
        if let ticket = admission.currentPreparation { prepare(ticket); return }
        guard let ticket = reservePreparation() else { return }
        setDestination(nil, preparation: ticket) { [weak self] accepted in if accepted { self?.prepare(ticket) } }
    }
    func prepare(_ ticket: CaptureAdmission.Preparation) {
        guard admission.beginPermissions(ticket) else { return }
        control.async {
            guard self.admission.permitsPreparation(ticket) else { return }
            self.snapshot.phase = "Preparing"
            self.snapshot.detail = ticket.profile.hasVideo ? "Checking camera and microphone permission." : "Checking microphone permission."
            self.snapshot.canStart = false; self.publish()
        }
        Task {
            let camera = ticket.profile.requiresCameraPermission ? await AVCaptureDevice.requestAccess(for: .video) : true
            guard self.admission.permitsPreparation(ticket) else { return }
            let microphone = await AVCaptureDevice.requestAccess(for: .audio)
            guard self.admission.permitsPreparation(ticket) else { return }
            control.async {
                guard self.admission.permitsPreparation(ticket), self.preparedIntent == ticket,
                      !self.snapshot.running, !self.stopping else { return }
                guard camera && microphone else {
                    self.stopOnControl(reason: ticket.profile.hasVideo ? "Camera and microphone permission are required. Enable them in Settings." : "Microphone permission is required. Enable it in Settings."); return
                }
                self.preflightRecording(ticket) {
                  do {
                    guard self.admission.permitsPreparation(ticket), self.preparedIntent == ticket else { return }
                    let audio = AVAudioSession.sharedInstance()
                    try audio.setCategory(.playAndRecord, mode: ticket.profile.hasVideo ? .videoRecording : .measurement, options: [.defaultToSpeaker, .allowBluetooth])
                    try audio.setPreferredSampleRate(48_000)
                    try audio.setActive(true)
                    _ = try AudioInputRouting.activate(configure: {
                        if !self.configured { try self.configure(profile: ticket.profile) }
                        try self.applyOrientation()
                    }, attach: {
                        try self.attachAudioRoute(ticket)
                    }, start: {
                        self.setSampleDelivery(true); self.session.startRunning()
                    })
                    guard self.admission.prepared(ticket) else { self.stopOnControl(reason: nil); return }
                    self.snapshot.phase = "Prepared"
                    self.snapshot.detail = self.orientationDescription
                    self.snapshot.canStart = !ticket.profile.hasVideo || self.admission.cameraControlSource != nil
                    self.snapshot.publisher = self.ingress.status
                    self.publish()
                  } catch { self.stopOnControl(reason: error.localizedDescription) }
                }
            }
        }
    }
    private func preflightRecording(_ preparation: CaptureAdmission.Preparation, completion: @escaping () -> Void) {
        guard let binding = recordings, let work = recordingAdmission.begin(.preflight(preparation)) else {
            stopOnControl(reason: "Recording storage is unavailable or still closing."); return
        }
        snapshot.detail = "Checking recording storage."; publish()
        recordingIO.async {
            var failure: Error?
            if self.recordingAdmission.permits(work), self.admission.permitsPreparation(preparation) {
                do { try binding.catalogue.checkRecordingCapacity(); try self.checkStorage(binding.catalogue, profile: preparation.profile) } catch { failure = error }
            }
            let observedFailure = failure
            self.control.async {
                let allowed = self.recordingAdmission.permits(work) && self.admission.permitsPreparation(preparation)
                self.recordingAdmission.closed(work, cleanupComplete: !binding.catalogue.cleanupUncertain)
                if !allowed { self.finishPreparedStopIfReady(); return }
                if let observedFailure { self.stopOnControl(reason: "Recording storage preflight failed: \(observedFailure.localizedDescription)") }
                else { completion() }
            }
        }
    }
    private func replaceCamera(_ candidate: CameraDeviceControls.Candidate, source: CaptureAdmission.CameraSource,
                               orientation: CaptureOrientation, profile: CaptureProfile) throws {
        guard admission.permitsCameraFrame(source) else { throw CaptureFailure(message: "Camera source was retired") }
        let previous = cameraDevice
        previous?.pause()
        samples.sync {} // old accepted work is ordered before the new source
        let device = CameraDeviceControls(candidate: candidate, source: source, frame: { [weak self] source, sample in
            self?.cameraFrame(source, sample: sample)
        }, dropped: { [weak self] source in
            self?.control.async {
                guard let self, self.admission.permitsCameraControls(source) else { return }
                self.snapshot.captureDrops += 1
            }
        })
        session.beginConfiguration()
        defer { session.commitConfiguration(); withExtendedLifetime(previous) {} }
        cameraDevice = device // retain the new native candidate even if session mutation fails
        if let previous {
            if session.outputs.contains(where: { $0 === previous.output }) { session.removeOutput(previous.output) }
            if session.inputs.contains(where: { $0 === previous.candidate.input }) { session.removeInput(previous.candidate.input) }
        }
        guard session.canAddInput(candidate.input) else { throw CaptureFailure(message: "Selected camera input is unsupported") }
        session.addInput(candidate.input)
        guard session.canAddOutput(device.output) else { throw CaptureFailure(message: "Selected camera output is unsupported") }
        session.addOutput(device.output)
        try device.configure(orientation: orientation, profile: profile)
        device.deliver(on: samples)
        cameraCommands.invalidate(); snapshot.cameraControlPending = false; snapshot.cameraRequestedCommand = nil
        snapshot.cameraActive = nil; snapshot.cameraControl = nil
        snapshot.cameraPreviewMirrored = source.position.previewMirrored
        snapshot.cameraPending = admission.pendingCameraSwitch != nil
        snapshot.cameraDetail = "Waiting for a valid native camera frame; remote picture is not verified"
    }
    /// Runs on the existing samples queue. No extra frame dispatch, pixel copy,
    /// device getter, or allocation is added to the steady-state video path.
    private func cameraFrame(_ source: CaptureAdmission.CameraSource, sample: CMSampleBuffer) {
        let dimensions = CameraFrameGeometry.read(sample)
        switch admission.cameraFrame(source, width: dimensions?.width ?? 0, height: dimensions?.height ?? 0, nowNs: DispatchTime.now().uptimeNanoseconds) {
        case .stale: return
        case .failed:
            fenceCameraMedia(source.owner, reason: "Camera output changed format or missed the switch deadline"); return
        case let .accepted(first):
            if first { control.async {
                guard self.admission.permitsCameraControls(source), self.cameraDevice?.source == source else { return }
                self.cameraSwitchTimer?.cancel(); self.cameraSwitchTimer = nil
                self.refreshCameraSnapshot()
                self.snapshot.cameraDetail = "Native camera verified; remote picture is not verified"
                self.publish(); self.applyDeferredOrientation()
            } }
            processCapture(sample, isVideo: true, source: source)
        }
    }
    /// Samples-owner fault fence precedes the cold cleanup task. A withheld
    /// control queue cannot keep microphone/media admission open after failure.
    private func fenceCameraMedia(_ preparation: CaptureAdmission.Preparation, reason: String) {
        if let run, run.recordingTicket.request.preparation == preparation {
            run.admitting = false; run.startup.removeAll(); run.startupBytes = 0
            run.recorder.markIncomplete(CaptureFailure(message: reason))
        }
        control.async { self.failCamera(preparation, reason: reason) }
    }
    private func configure(profile: CaptureIntent) throws {
        session.beginConfiguration(); defer { session.commitConfiguration() }
        cameraDevice?.pause(); cameraDevice = nil
        for input in session.inputs { session.removeInput(input) }
        for output in session.outputs { session.removeOutput(output) }
        session.automaticallyConfiguresApplicationAudioSession = false
        guard let microphone = AVCaptureDevice.default(for: .audio) else { throw CaptureFailure(message: "Microphone unavailable") }
        let micInput = try AVCaptureDeviceInput(device: microphone)
        guard session.canAddInput(micInput) else { throw CaptureFailure(message: "Microphone input is unsupported") }
        session.addInput(micInput)
        let audio = AVCaptureAudioDataOutput()
        audio.setSampleBufferDelegate(self, queue: samples)
        guard session.canAddOutput(audio) else { throw CaptureFailure(message: "Microphone output unavailable") }
        session.addOutput(audio)
        configuredProfile = profile
        configured = true
    }
    func start() { if let ticket = admission.preparedTicket { start(preparation: ticket) } }
    func start(preparation: CaptureAdmission.Preparation) {
        guard let ticket = admission.reserveStart(preparation) else { return }
        control.async { self.startOnControl(ticket) }
    }
    private func startOnControl(_ ticket: CaptureAdmission.Start, keepSavedSegment: Bool = false) {
        guard admission.permitsStart(ticket), preparedIntent == ticket.preparation,
              snapshot.canStart, !snapshot.running, !stopping else { return }
        guard !keepSavedSegment || rotationDeadline?.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true else {
            stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled."); return
        }
        guard let binding = recordings, let work = recordingAdmission.begin(.segment(ticket)) else {
            stopOnControl(reason: "Recording storage is unavailable or still closing."); return
        }
        let previousShow = recordingShow?.0 == ticket.preparation ? recordingShow?.1 : nil
        let deadline = keepSavedSegment ? rotationDeadline : nil
        snapshot.phase = "Starting"; snapshot.canStart = false
        snapshot.detail = "Preparing the private recording segment."; publish()
        recordingIO.async {
            var recorder: CompressedRecorder?
            var show: RecordingCatalogue.Show?
            var failure: Error?
            var receipt: RecordingCatalogue.Writer?
            do {
                guard self.recordingAdmission.permits(work), self.admission.permitsStart(ticket),
                      deadline.map({ $0.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) }) ?? true else {
                    self.control.async {
                        self.recordingAdmission.closed(work, cleanupComplete: true)
                        if self.admission.permitsStart(ticket) { self.stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled.") }
                        else { self.finishPreparedStopIfReady() }
                    }; return
                }
                try binding.catalogue.checkRecordingCapacity()
                try self.checkStorage(binding.catalogue, profile: ticket.preparation.profile)
                guard self.recordingAdmission.permits(work), self.admission.permitsStart(ticket),
                      deadline.map({ $0.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) }) ?? true else {
                    self.control.async {
                        self.recordingAdmission.closed(work, cleanupComplete: true)
                        if self.admission.permitsStart(ticket) { self.stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled.") }
                        else { self.finishPreparedStopIfReady() }
                    }; return
                }
                let selected = previousShow ?? binding.catalogue.newShow(profile: ticket.preparation.profile)
                show = selected
                let created = try binding.catalogue.begin(identity: selected.nextIdentity())
                receipt = created
                recorder = try CompressedRecorder(receipt: created, queue: self.recordingIO, hasVideo: ticket.preparation.profile.hasVideo) { [weak self] error in
                    self?.control.async {
                        guard let self, self.preparedIntent == ticket.preparation, self.recordingAdmission.contains(work) else { return }
                        self.recordingFailureDescription = error.localizedDescription
                        self.snapshot.detail = "Recording stopped early: \(error.localizedDescription)"; self.publish()
                    }
                }
            } catch {
                failure = error
                if let receipt { receipt.cancel(); do { try receipt.ownersClosed() } catch { receipt.retainUncertainCleanup() } }
            }
            let readyRecorder = recorder; let readyShow = show
            let ownedReceipt = receipt; let creationFailure = failure
            self.control.async {
                guard let recorder = readyRecorder, let show = readyShow else {
                    let clean = ownedReceipt?.cleanupComplete ?? !binding.catalogue.cleanupUncertain
                    self.recordingAdmission.closed(work, cleanupComplete: clean)
                    if self.admission.permitsStart(ticket) { self.stopOnControl(reason: creationFailure.map { "Recording could not be created: \($0.localizedDescription)" } ?? "Recording could not be created.") }
                    else { self.finishPreparedStopIfReady() }
                    return
                }
                guard self.admission.permitsStart(ticket), self.recordingAdmission.permits(work) else {
                    self.closeRejectedRecorder(recorder, work: work); return
                }
                self.recordingShow = (ticket.preparation, show)
                self.snapshot.canStart = true
                self.adoptRecording(recorder, work: work, ticket: ticket, keepSavedSegment: keepSavedSegment)
            }
        }
    }
    private func closeRejectedRecorder(_ recorder: CompressedRecorder, work: RecordingCaptureAdmission.Ticket) {
        recordingAdmission.closing(work)
        recorder.finish { result in self.control.async {
            self.recordingAdmission.closed(work, cleanupComplete: result.cleanupComplete)
            if !result.cleanupComplete { self.retainedRecorder = recorder }
            self.finishPreparedStopIfReady()
        } }
    }
    private func adoptRecording(_ recorder: CompressedRecorder, work: RecordingCaptureAdmission.Ticket, ticket: CaptureAdmission.Start, keepSavedSegment: Bool) {
        guard admission.permitsStart(ticket), recordingAdmission.permits(work), preparedIntent == ticket.preparation,
              snapshot.canStart, !snapshot.running, !stopping else { closeRejectedRecorder(recorder, work: work); return }
        guard !keepSavedSegment || rotationDeadline?.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true else {
            closeRejectedRecorder(recorder, work: work)
            stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled."); return
        }
        self.captureGeneration &+= 1
        let captureToken = self.captureGeneration
        var constructedRun: CaptureRun?
        do {
            self.ingress.reset(hasVideo: ticket.preparation.profile.hasVideo)
            self.recordingFailureDescription = nil
            let run = try CaptureRun(recorder: recorder, recordingTicket: work, orientation: self.orientationPlan.desired, profile: ticket.preparation.profile, ingress: self.ingress,
                failure: { [weak self] error in
                    recorder.markIncomplete(error)
                    self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.stopOnControl(reason: error.localizedDescription)
                } },
                publishingFailure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.snapshot.publisher = "Publisher stopped: \(error.localizedDescription)"
                    self?.snapshot.detail = "Local recording continues. Stop and prepare again to restart publishing."
                    self?.publish()
                } }, statistics: { [weak self] count, status in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.snapshot.encoded = count; self?.snapshot.publisher = status; self?.publish()
                } }, meter: { [weak self] level in self?.meter(level) })
            run.audioRoute = self.audioRoute
            constructedRun = run
            // Once native owners exist, even a winning synchronous Stop must
            // reach their actual worker/recorder cleanup path before admission.
            guard self.admission.permitsStart(ticket),
                  !keepSavedSegment || self.rotationDeadline?.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true,
                  self.recordingAdmission.activate(work) else {
                run.video?.finish(); try? run.audio.finish()
                self.closeRejectedRecorder(recorder, work: work)
                if self.admission.permitsStart(ticket) { self.stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled.") }
                return
            }
            if keepSavedSegment {
                try AVAudioSession.sharedInstance().setActive(true)
                run.audioRoute = try AudioInputRouting.activate(configure: { try self.applyOrientation() }, attach: {
                    try self.attachAudioRoute(ticket.preparation)
                }, start: { self.setSampleDelivery(true); self.session.startRunning() })
            }
            guard self.admission.permitsStart(ticket), self.recordingAdmission.isActive(work),
                  !keepSavedSegment || self.rotationDeadline?.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true else {
                run.video?.finish(); try? run.audio.finish(); self.closeRejectedRecorder(recorder, work: work)
                if self.admission.permitsStart(ticket) { self.stopOnControl(reason: "Rotation finalisation timed out; automatic restart cancelled.") }
                return
            }
            var currentAtExposure = false
            run.worker.startPublishing {
                self.samples.sync {
                    currentAtExposure = self.admission.permitsStart(ticket) &&
                        (!keepSavedSegment || self.rotationDeadline?.permits(ticket.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true)
                    // Register ownership even when cancellation won, so normal
                    // Stop joins this worker's queued failure/close callbacks.
                    // Media remains fenced until the exact Start commits.
                    self.run = run
                }
                self.snapshot.running = true; self.snapshot.canStart = false
            }
            let committedAt = DispatchTime.now().uptimeNanoseconds
            guard currentAtExposure,
                  !keepSavedSegment || self.rotationDeadline?.permits(ticket.preparation, nowNs: committedAt) == true else {
                self.stopOnControl(reason: keepSavedSegment && self.admission.permitsStart(ticket) ? "Rotation finalisation timed out; automatic restart cancelled." : nil); return
            }
            guard self.admission.started(ticket, nowNs: committedAt, broadcasting: self.ingress.destination != nil) else { self.stopOnControl(reason: nil); return }
            self.samples.sync {
                if self.admission.currentPreparation == ticket.preparation, self.admission.currentPhase == .running { run.admitting = true }
            }
            guard self.admission.currentPreparation == ticket.preparation, self.admission.currentPhase == .running else { self.stopOnControl(reason: nil); return }
            if keepSavedSegment, let token = self.orientationPlan.pending {
                _ = self.orientationPlan.complete(token)
                self.rotationTimeout?.cancel(); self.rotationTimeout = nil; self.rotationDeadline = nil
            }
            self.beginTelemetry(ticket.preparation)
            self.snapshot.phase = "Recording"
            self.snapshot.detail = keepSavedSegment ? "Rotation began a new recording segment. Active publishing reconnects; the previous private segment is retained." : run.video.map { "Hardware encoder: \($0.implementation). Saving compressed video and audio locally." } ?? (ticket.preparation.continuesAudioInBackground ? "Saving microphone AAC locally. This started audio show may continue with the screen locked. Return to Sapflow to Stop and save." : "Saving microphone AAC locally. Keep Sapflow in the foreground.")
            self.snapshot.encoded = 0; self.snapshot.captureDrops = 0; self.snapshot.level = -60
            self.snapshot.publisher = self.ingress.status
            self.publish()
            let timer = DispatchSource.makeTimerSource(queue: self.control)
            timer.schedule(deadline: .now() + 3, repeating: 5)
            timer.setEventHandler { [weak self] in
                guard let self, self.captureGeneration == captureToken, self.preparedIntent == ticket.preparation else { return }
                self.checkActiveStorage(run.recorder, work: run.recordingTicket, preparation: ticket.preparation, generation: captureToken)
                self.samples.async {
                    if self.run?.epoch == nil { self.control.async {
                        guard self.captureGeneration == captureToken, self.preparedIntent == ticket.preparation else { return }
                        self.stopOnControl(reason: run.profile.hasVideo ? "Camera or microphone did not deliver its initial sample" : "Microphone did not deliver its initial sample")
                    } }
                }
            }
            self.storageTimer = timer; timer.resume()
        } catch {
            constructedRun?.video?.finish(); try? constructedRun?.audio.finish()
            self.closeRejectedRecorder(recorder, work: work)
            self.fencePublishing(); self.stopOnControl(reason: error.localizedDescription)
        }
    }
    func stop() {
        let ticket = admission.stop()
        recordingAdmission.cancel(ticket.preparation)
        fencePublishing() // synchronous admission fence precedes queued cleanup
        control.async {
            guard self.admission.permitsStop(ticket) else { return }
            self.stopIntent = ticket
            self.orientationPlan.cancel(); self.rotationTimeout?.cancel(); self.rotationTimeout = nil
            self.rotationIntent = nil
            if self.stopping { self.snapshot.detail = "Restart cancelled; finishing the recording."; self.publish() }
            self.stopOnControl(reason: nil)
        }
    }
    private func admissionStopped() {
        stopping = false; preparedIntent = nil; stopIntent = nil; recordingShow = nil; preparedPublisherClosed = nil; rotationDeadline = nil
        snapshot.phase = stopReason == nil ? "Stopped" : "Interrupted"
        snapshot.canStart = false
        notifyCleanupIfReleased()
        publish()
    }
    private func stopOnControl(reason requestedReason: String?) {
        let retiringRoute = audioRoute
        let routeFailed = retiringRoute?.close() == true
        audioRoute = nil
        let reason = routeFailed ? retiringRoute?.failureDescription : requestedReason
        if routeFailed, let retiringRoute {
            self.samples.sync {
                guard let run = self.run, run.audioRoute?.owner == retiringRoute.owner else { return }
                run.admitting = false
                run.recorder.markIncomplete(CaptureFailure(message: "Microphone route changed or disconnected"))
            }
        }
        if let reason { stopReason = reason; orientationPlan.cancel(); rotationTimeout?.cancel(); rotationTimeout = nil }
        let child = rotationIntent
        let rotationAllowed = reason == nil && child.map(admission.permitsRotation) == true && orientationPlan.pending != nil
        if !rotationAllowed {
            stopIntent = admission.stop(); recordingAdmission.cancel(stopIntent?.preparation); rotationIntent = nil
            orientationPlan.cancel()
            stopTelemetry()
        }
        guard !stopping else { return }
        stopReason = reason
        let rotationToken = rotationAllowed ? orientationPlan.pending : nil
        if rotationToken == nil { fencePublishing() } else { fenceCodecEpoch() }
        storageTimer?.cancel(); storageTimer = nil
        snapshot.activeAudioInput = "Not active"
        cameraSwitchTimer?.cancel(); cameraSwitchTimer = nil; deferredOrientation = nil
        cameraCommands.invalidate(); snapshot.cameraControlPending = false
        snapshot.cameraControl = nil; snapshot.cameraActive = nil; snapshot.cameraPending = false
        setSampleDelivery(false)
        if session.isRunning { session.stopRunning() }
        if let device = cameraDevice {
            session.beginConfiguration()
            if session.outputs.contains(where: { $0 === device.output }) { session.removeOutput(device.output) }
            if session.inputs.contains(where: { $0 === device.candidate.input }) { session.removeInput(device.candidate.input) }
            session.commitConfiguration()
            cameraDevice = nil
        }
        snapshot.canStart = false
        guard snapshot.running else {
            snapshot.phase = "Finalising"
            snapshot.detail = reason ?? "Closing the prepared destination."
            publish(); try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            // Prepared/permission-only teardown must also retain a publisher
            // until its actual completion; duplicate Stop joins this barrier.
            stopping = true
            let stopped = stopIntent ?? admission.stop()
            let finish = { self.control.async {
                guard self.admission.permitsStop(stopped) else { return }
                self.preparedPublisherClosed = (stopped, self.ingress.destination?.cleanupComplete != false)
                self.finishPreparedStopIfReady()
            } }
            if let destination = ingress.destination { destination.stop(completion: finish) } else { finish() }
            return
        }
        stopping = true
        snapshot.running = false; snapshot.phase = "Finalising"
        snapshot.publisher = ingress.destination == nil ? "Local capture stopping" : "Publisher stop requested · destination end unconfirmed"
        snapshot.detail = rotationToken == nil ? (reason ?? "Closing the recording safely.") : "Rotating: saving this recording segment. Active publishing will reconnect upright."
        publish()
        samples.async {
            guard let run = self.run else { self.control.async {
                self.stopping = false
                if let ticket = self.stopIntent { _ = self.admission.completeStop(ticket, cleanupComplete: false) }
                self.snapshot.detail = "Capture cleanup unconfirmed; Prepare is blocked."; self.publish()
            }; return }
            self.run = nil
            var flushFailed = false
            do { try run.audio.finish() } catch {
                flushFailed = true
                self.control.async { self.recordingFailureDescription = "AAC flush failed: \(error.localizedDescription)" }
            }
            run.video?.finish()
            self.recordingAdmission.closing(run.recordingTicket)
            let state = RecordingRunStop(run: run, rotationToken: rotationToken, rotation: child)
            self.control.async { self.runStop = state }
            // File finalisation must not wait behind network cleanup. Both real
            // receipts join before rotation or a new capture can be admitted.
            run.recorder.finish(incomplete: flushFailed) { result in self.control.async {
                state.recording = result
                self.recordingAdmission.closed(run.recordingTicket, cleanupComplete: result.cleanupComplete)
                if !result.cleanupComplete { self.retainedRecorder = run.recorder }
                self.finishRunStopIfReady(state)
            } }
            run.worker.finish { result in self.control.async {
                state.publishing = result
                self.finishRunStopIfReady(state)
            } }
        }
    }
    private func finishRunStopIfReady(_ state: RecordingRunStop) {
        guard runStop === state, !state.completed, let recording = state.recording, let publishingResult = state.publishing else { return }
        state.completed = true; stopping = false
        let publishingClean: Bool
        if case .success = publishingResult { publishingClean = ingress.destination?.cleanupComplete != false }
        else { publishingClean = false }
        let clean = recording.cleanupComplete && publishingClean && !recordingAdmission.hasOwner
        if !publishingClean { stopReason = "Publisher cleanup unconfirmed; automatic restart blocked." }
        snapshot.phase = stopReason == nil ? "Stopped" : "Interrupted"
        if !recording.cleanupComplete { snapshot.detail = "Recording cleanup unconfirmed; Prepare is blocked." }
        else if recording.finalised { snapshot.detail = stopReason ?? "Recording finalised. It has not been independently decoded." }
        else { snapshot.detail = "Recording is incomplete. The private original was retained for inspection." }
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        if clean, recording.finalised, recordingFailureDescription == nil,
           let token = state.rotationToken, let child = state.rotation,
           admission.permitsRotation(child), orientationPlan.pending == token,
           rotationDeadline?.permits(child.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) == true {
            runStop = nil; rotationIntent = nil
            restartForOrientation(child)
        } else {
            if state.rotationToken != nil, rotationDeadline?.permits(state.rotation?.preparation ?? state.run.recordingTicket.request.preparation, nowNs: DispatchTime.now().uptimeNanoseconds) != true {
                stopReason = "Rotation finalisation timed out; automatic restart cancelled."
                snapshot.phase = "Interrupted"; snapshot.detail = stopReason!
            }
            rotationTimeout?.cancel(); rotationTimeout = nil; rotationDeadline = nil
            orientationPlan.cancel(); rotationIntent = nil
            let stopped = stopIntent ?? admission.stop()
            _ = admission.completeStop(stopped, cleanupComplete: clean)
            if admission.currentPhase == .idle { preparedIntent = nil; stopIntent = nil; recordingShow = nil; runStop = nil }
            notifyCleanupIfReleased()
            publish()
        }
    }
    private func finishPreparedStopIfReady() {
        guard let (stopped, publisherClean) = preparedPublisherClosed, admission.permitsStop(stopped) else { return }
        guard !recordingAdmission.hasOwner else {
            if recordingAdmission.cleanupRetained {
                _ = admission.completeStop(stopped, cleanupComplete: false)
                stopping = false; snapshot.phase = "Interrupted"
                snapshot.detail = "Recording cleanup unconfirmed; Prepare is blocked."
            } else { snapshot.detail = "Waiting for recording storage cleanup." }
            publish(); return
        }
        let clean = publisherClean && recordings?.catalogue.cleanupUncertain != true
        let released = admission.completeStop(stopped, cleanupComplete: clean)
        if released { admissionStopped() }
        else if admission.currentPhase == .retained {
            stopping = false; snapshot.phase = "Interrupted"
            snapshot.detail = "Recording or publisher cleanup unconfirmed; Prepare is blocked."; publish()
        }
    }
    /// All resource-values filesystem work runs only on recordingIO.
    private func checkStorage(_ catalogue: RecordingCatalogue, profile: CaptureIntent) throws {
        let available = try catalogue.recordingRootURL.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]).volumeAvailableCapacityForImportantUsage
        guard let available, available >= 0, RecordingCaptureAdmission.storageAvailable(profile: profile, bytes: UInt64(available)) else {
            throw CaptureFailure(message: "Recording storage is below its required reserve")
        }
    }
    private func checkActiveStorage(_ recorder: CompressedRecorder, work: RecordingCaptureAdmission.Ticket, preparation: CaptureAdmission.Preparation, generation: UInt64) {
        guard !storageCheckPending else { return }
        storageCheckPending = true
        recorder.checkStorage(profile: preparation.profile) { result in self.control.async {
            self.storageCheckPending = false
            guard self.preparedIntent == preparation, self.captureGeneration == generation, self.snapshot.running else { return }
            if case .failure = result {
                self.recordingFailureDescription = "Recording storage is below its required reserve."
                self.snapshot.detail = "Local recording stopped early because storage is low. Publishing continues."
                self.publish()
                // Stop only recording admission, preserving the accepted prefix
                // and the independent publisher/capture. Stop later joins this
                // same coalesced actual finalisation result.
                self.recordingAdmission.closing(work)
                recorder.finish(incomplete: true) { completion in self.control.async {
                    self.recordingAdmission.closed(work, cleanupComplete: completion.cleanupComplete)
                    if !completion.cleanupComplete { self.retainedRecorder = recorder }
                    guard self.preparedIntent == preparation, self.captureGeneration == generation else { return }
                    self.publish()
                } }
            }
        } }
    }
    private func beginTelemetry(_ intent: CaptureAdmission.Preparation) {
        if telemetryIntent == intent, telemetryTimer != nil { return }
        telemetryTimer?.setEventHandler {}; telemetryTimer?.cancel()
        telemetryIntent = intent; telemetryReads.invalidate(); socketRate = SocketOutboundRate()
        updateElapsed(intent, nowNs: DispatchTime.now().uptimeNanoseconds)
        let timer = DispatchSource.makeTimerSource(queue: control)
        timer.schedule(deadline: .now() + 1, repeating: 1)
        timer.setEventHandler { [weak self] in self?.readTelemetry(intent) }
        telemetryTimer = timer; timer.resume()
    }
    @discardableResult private func updateElapsed(_ intent: CaptureAdmission.Preparation, nowNs: UInt64) -> CaptureElapsedSnapshot {
        let value = admission.elapsed(intent, nowNs: nowNs)
        snapshot.captureElapsedNs = value.captureNs; snapshot.broadcastElapsedNs = value.broadcastNs
        return value
    }
    private func readTelemetry(_ intent: CaptureAdmission.Preparation) {
        guard telemetryIntent == intent else { return }
        let now = DispatchTime.now().uptimeNanoseconds
        let elapsed = updateElapsed(intent, nowNs: now)
        socketRate.expire(nowNs: now)
        snapshot.socketKilobitsPerSecond = elapsed.broadcastRunning ? socketRate.kilobitsPerSecond : nil
        publish() // Other meter/statistics publications use only these cached metrics.
        guard admission.currentPreparation == intent, elapsed.captureRunning, elapsed.broadcastRunning,
              let destination = ingress.destination, let query = telemetryReads.begin(intent) else { return }
        destination.readSocketOutbound { [weak self] sample in
            guard let self else { return }
            self.control.async {
                guard self.telemetryReads.complete(query), self.telemetryIntent == intent,
                      self.admission.currentPreparation == intent else { return }
                let now = DispatchTime.now().uptimeNanoseconds
                let current = self.admission.elapsed(intent, nowNs: now)
                self.socketRate.update(current.broadcastRunning ? sample : nil, nowNs: now)
            }
        }
    }
    private func stopTelemetry() {
        telemetryTimer?.setEventHandler {}; telemetryTimer?.cancel(); telemetryTimer = nil
        telemetryReads.invalidate(); socketRate = SocketOutboundRate()
        if let intent = telemetryIntent { updateElapsed(intent, nowNs: DispatchTime.now().uptimeNanoseconds) }
        snapshot.socketKilobitsPerSecond = nil
    }
    private func report(_ error: Error) { snapshot.phase = "Error"; snapshot.detail = error.localizedDescription; snapshot.canStart = false; publish() }
    private func publish() {
        snapshot.recordingIOBusy = recordingAdmission.isWorking || storageCheckPending
        snapshot.stopControl = admission.currentPreparation.map { AudioShowStopControl(preparation: $0, admission: admission) }
        let value = snapshot
        DispatchQueue.main.async { UIApplication.shared.isIdleTimerDisabled = value.running; self.onChange?(value) }
    }
    private func meter(_ level: Float) {
        // Audio callback only schedules coalesced UI work, never logs per packet.
        let now = ProcessInfo.processInfo.systemUptime
        guard now - lastMeterUpdate >= 0.2 else { return }
        lastMeterUpdate = now
        control.async { self.snapshot.level = level; self.publish() }
    }
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard !(output is AVCaptureVideoDataOutput) else { return }
        processCapture(sampleBuffer, isVideo: false)
    }
    private func processCapture(_ sampleBuffer: CMSampleBuffer, isVideo: Bool, source: CaptureAdmission.CameraSource? = nil) {
        guard let run, run.admitting else { return }
        do {
            guard run.audioRoute?.awaitPCM() == true else { return }
            switch admission.cameraMedia(run.recordingTicket.request.preparation) {
            case .stale: return
            case .failed:
                fenceCameraMedia(run.recordingTicket.request.preparation, reason: "Camera switch exceeded its original deadline"); return
            case .allowed: break
            }
            if let source {
                guard admission.permitsCameraFrame(source), run.recordingTicket.request.preparation == source.owner else { return }
            }
            guard !isVideo || run.profile.hasVideo else { throw CaptureFailure(message: "Unexpected camera sample in audio-only capture") }
            if run.epoch == nil {
                // Preserve early audio while waiting for the first camera
                // callback. Dropping it would silently remove PCM from the
                // shared timeline. Startup alone may retain bounded raw media.
                let bytes = CMSampleBufferGetImageBuffer(sampleBuffer).map { CVPixelBufferGetDataSize($0) } ?? CMSampleBufferGetTotalSampleSize(sampleBuffer)
                guard run.startup.count < 8, bytes <= 12 * 1024 * 1024 - run.startupBytes else {
                    throw CaptureFailure(message: "Camera/microphone startup exceeded its native buffer bound")
                }
                run.startup.append((sampleBuffer, isVideo)); run.startupBytes += bytes
                guard (!run.profile.hasVideo || run.startup.contains(where: { $0.video })), run.startup.contains(where: { !$0.video }) else { return }
                let initial = run.startup.sorted {
                    CMTimeCompare(CMSampleBufferGetPresentationTimeStamp($0.sample), CMSampleBufferGetPresentationTimeStamp($1.sample)) < 0
                }
                run.epoch = CMSampleBufferGetPresentationTimeStamp(initial[0].sample)
                run.startup.removeAll(); run.startupBytes = 0
                for pending in initial { try process(pending.sample, video: pending.video, run: run) }
            } else { try process(sampleBuffer, video: isVideo, run: run) }
        } catch {
            run.admitting = false; run.startup.removeAll(); run.startupBytes = 0
            run.recorder.markIncomplete(error)
            control.async { self.stopOnControl(reason: error.localizedDescription) }
        }
    }
    private func process(_ sample: CMSampleBuffer, video: Bool, run: CaptureRun) throws {
        guard let epoch = run.epoch else { return }
        let time = CMTimeSubtract(CMSampleBufferGetPresentationTimeStamp(sample), epoch)
        guard time.isValid, time.seconds >= 0 else { throw CaptureFailure(message: "Capture clock moved before the session epoch") }
        if video {
            guard let encoder = run.video else { throw CaptureFailure(message: "Video encoder is absent for this capture") }
            guard run.videoTimeline.accept(time) else { throw CaptureFailure(message: "Camera timestamp changed its existing media timeline") }
            try encoder.encode(sample, time: time)
        }
        else { try run.audio.encode(sample, time: time) }
    }
    func captureOutput(_ output: AVCaptureOutput, didDrop sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        control.async { self.snapshot.captureDrops += 1 }
    }
}
