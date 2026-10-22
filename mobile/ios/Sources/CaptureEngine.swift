import AVFoundation
import UIKit

struct CaptureSnapshot {
    var orientation = CaptureOrientation.portrait
    var profile = CaptureProfile.defaultProfile
    var phase = "Idle"
    var detail = "Prepare the camera and microphone to begin."
    var publisher = "Publisher not attached · local recording only"
    var level: Float = -60
    var encoded: UInt64 = 0
    var captureDrops: UInt64 = 0
    var recordingURL: URL?
    var canStart = false
    var running = false
}

extension CaptureEngine: V4vCreatorCapturePort {}

final class CaptureRun {
    let profile: CaptureProfile
    let recorder: CompressedRecorder
    let worker: IngressWorker
    var video: HardwareVideoEncoder!
    var audio: NativeAudioEncoder!
    var epoch: CMTime?
    var admitting = true
    var startup: [(sample: CMSampleBuffer, video: Bool)] = []
    var startupBytes = 0
    init(url: URL, orientation: CaptureOrientation, profile: CaptureProfile, ingress: RustEncodedIngress, failure: @escaping (Error) -> Void, publishingFailure: @escaping (Error) -> Void, recordingFailure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void, meter: @escaping (Float) -> Void) throws {
        self.profile = profile
        recorder = try CompressedRecorder(url: url, onFailure: recordingFailure)
        worker = IngressWorker(ingress: ingress, failure: publishingFailure, statistics: statistics)
        let recorder = self.recorder; let worker = self.worker
        let output: (CompressedUnit) -> Void = { unit in recorder.offer(unit); worker.offer(unit) }
        video = try HardwareVideoEncoder(orientation: orientation, profile: profile, output: output, failure: failure)
        audio = try NativeAudioEncoder(output: output, meter: meter)
    }
}

/// Session configuration, encoded callbacks, disk and FFI work each have an
/// explicit serial owner. Raw camera pixels and microphone PCM stay native.
// Sendability is provided by the control/samples queue ownership documented
// above; no destination replacement is allowed while a run is finalising.
final class CaptureEngine: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureAudioDataOutputSampleBufferDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    private let control = DispatchQueue(label: "sapflow.capture-control", qos: .userInitiated)
    private let samples = DispatchQueue(label: "sapflow.capture-samples", qos: .userInteractive)
    private var run: CaptureRun? // samples queue only
    private var captureGeneration: UInt64 = 0 // control queue only
    private var configured = false // control queue only
    private var configuredProfile: CaptureProfile? // control queue only
    private var orientationPlan = OrientationRestartPlan()
    private var rotationTimeout: DispatchWorkItem?
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
    private var notifications: [NSObjectProtocol] = []
    private let ingress = RustEncodedIngress()
    private let destinationLock = NSLock()
    private var destinationFence: EncodedPublishingSink?
    var onChange: ((CaptureSnapshot) -> Void)?
    private var lastMeterUpdate = 0.0

    override init() {
        super.init()
        let centre = NotificationCenter.default
        for name in [AVCaptureSession.wasInterruptedNotification, AVCaptureSession.runtimeErrorNotification,
                     AVAudioSession.interruptionNotification, AVAudioSession.routeChangeNotification,
                     UIApplication.didEnterBackgroundNotification] {
            notifications.append(centre.addObserver(forName: name, object: nil, queue: nil) { [weak self] notification in
                guard let self else { return }
                if notification.name == AVAudioSession.routeChangeNotification && self.admission.currentPhase != .running { return }
                let ticket = self.admission.stop()
                self.fencePublishing()
                self.control.async {
                    guard self.admission.permitsStop(ticket) else { return }
                    self.stopIntent = ticket
                    self.stopOnControl(reason: "Capture interrupted. Return to the foreground and prepare again.")
                }
            })
        }
    }
    deinit { for token in notifications { NotificationCenter.default.removeObserver(token) }; storageTimer?.cancel(); rotationTimeout?.cancel() }

    /// Attach a real bounded destination adapter before preparing. No endpoint
    /// or credential is inferred from a display label or bundled configuration.
    var canChooseCaptureProfile: Bool { admission.canChooseCaptureProfile }
    func reservePreparation(profile: CaptureProfile = .defaultProfile) -> CaptureAdmission.Preparation? { admission.reservePreparation(profile: profile) }
    /// The destination owner separately fences its captured publisher. This
    /// exact camera fence cancels initial approval without stopping a recording
    /// that already began, including its later orientation epochs.
    func cancelPendingPreparation(_ preparation: CaptureAdmission.Preparation) {
        guard let ticket = admission.stopIfPending(preparation) else { return }
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
            if self.configuredProfile != ticket.profile { self.configured = false }
            self.snapshot.profile = ticket.profile
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
        control.async {
            guard self.orientationPlan.desired != orientation else { return }
            let token = self.orientationPlan.request(orientation, running: self.snapshot.running)
            if let token {
                guard let intent = self.preparedIntent, let child = self.admission.beginRotation(intent) else {
                    self.orientationPlan.cancel(); return
                }
                self.rotationIntent = child
                let timeout = DispatchWorkItem { [weak self] in
                    guard let self, self.orientationPlan.pending == token else { return }
                    self.orientationPlan.cancel(); self.rotationTimeout = nil
                    self.stopIntent = self.admission.stop(); self.rotationIntent = nil
                    self.fencePublishing()
                    self.stopReason = "Rotation finalisation timed out; automatic restart cancelled."
                    self.snapshot.detail = "Rotation finalisation exceeded six seconds. Automatic restart cancelled; cleanup may still finish."
                    self.publish()
                }
                self.rotationTimeout = timeout
                self.control.asyncAfter(deadline: .now() + 6, execute: timeout)
                self.stopOnControl(reason: nil)
            } else if !self.stopping && !self.snapshot.running {
                guard self.admission.currentPhase != .stopping, self.admission.currentPhase != .retained else { return }
                do {
                    let resumePreview = self.session.isRunning
                    if self.configured { try self.applyOrientation() }
                    self.snapshot.orientation = orientation
                    if resumePreview { self.setSampleDelivery(true); self.session.startRunning() }
                    if self.snapshot.canStart { self.snapshot.detail = self.orientationDescription }
                    self.publish()
                } catch { self.report(error) }
            }
        }
    }
    private var orientationDescription: String {
        let profile = preparedIntent?.profile ?? snapshot.profile
        let dimensions = profile.dimensions(for: orientationPlan.desired)
        return "\(dimensions.width)×\(dimensions.height) at \(profile.frameRate) fps · hardware H.264 · AAC 48 kHz mono"
    }
    private func applyOrientation() throws {
        guard !snapshot.running, !stopping else { throw CaptureFailure(message: "Orientation requires a new encoder epoch") }
        setSampleDelivery(false)
        if session.isRunning { session.stopRunning() }
        guard let output = session.outputs.compactMap({ $0 as? AVCaptureVideoDataOutput }).first,
              let connection = output.connection(with: .video), connection.isVideoOrientationSupported else {
            throw CaptureFailure(message: "Camera cannot rotate its encoded pixel buffers")
        }
        // AVCaptureVideoDataOutput physically rotates buffers in hardware.
        // Configure while stopped; the encoder receives actual upright pixels.
        connection.videoOrientation = orientationPlan.desired.capture
        snapshot.orientation = orientationPlan.desired
    }
    private func setSampleDelivery(_ enabled: Bool) {
        // Stop the old producers before the sample-queue barrier/encoder flush.
        // A later epoch restores delegates only after its orientation is set.
        for output in session.outputs {
            if let video = output as? AVCaptureVideoDataOutput {
                video.setSampleBufferDelegate(enabled ? self : nil, queue: enabled ? samples : nil)
            } else if let audio = output as? AVCaptureAudioDataOutput {
                audio.setSampleBufferDelegate(enabled ? self : nil, queue: enabled ? samples : nil)
            }
        }
    }
    private func restartForOrientation(_ child: CaptureAdmission.Rotation) {
        guard admission.permitsRotation(child) else { return }
        do {
            guard AVCaptureDevice.authorizationStatus(for: .video) == .authorized,
                  AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else {
                throw CaptureFailure(message: "Capture permission changed; prepare again")
            }
            try checkStorage()
            try applyOrientation()
            try AVAudioSession.sharedInstance().setActive(true)
            setSampleDelivery(true)
            session.startRunning()
            guard let start = admission.reserveRotationStart(child) else { return }
            snapshot.canStart = true
            startOnControl(start, keepSavedSegment: true)
            if snapshot.running {
                snapshot.detail = "\(orientationDescription). Rotation began a new recording segment; active publishing reconnects. The previous segment remains in Files."
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
            self.snapshot.detail = "Checking camera and microphone permission."
            self.snapshot.canStart = false; self.publish()
        }
        Task {
            let camera = await AVCaptureDevice.requestAccess(for: .video)
            guard self.admission.permitsPreparation(ticket) else { return }
            let microphone = await AVCaptureDevice.requestAccess(for: .audio)
            guard self.admission.permitsPreparation(ticket) else { return }
            control.async {
                guard self.admission.permitsPreparation(ticket), self.preparedIntent == ticket,
                      !self.snapshot.running, !self.stopping else { return }
                guard camera && microphone else {
                    self.stopOnControl(reason: "Camera and microphone permission are required. Enable them in Settings."); return
                }
                do {
                    try self.checkStorage()
                    let audio = AVAudioSession.sharedInstance()
                    try audio.setCategory(.playAndRecord, mode: .videoRecording, options: [.defaultToSpeaker, .allowBluetooth])
                    try audio.setPreferredSampleRate(48_000)
                    try audio.setActive(true)
                    if !self.configured { try self.configure(profile: ticket.profile) }
                    try self.applyOrientation()
                    self.setSampleDelivery(true)
                    self.session.startRunning()
                    guard self.admission.prepared(ticket) else { self.stopOnControl(reason: nil); return }
                    self.snapshot.phase = "Prepared"
                    self.snapshot.detail = self.orientationDescription
                    self.snapshot.canStart = true
                    self.snapshot.publisher = self.ingress.status
                    self.publish()
                } catch { self.stopOnControl(reason: error.localizedDescription) }
            }
        }
    }
    private func configure(profile: CaptureProfile) throws {
        session.beginConfiguration(); defer { session.commitConfiguration() }
        for input in session.inputs { session.removeInput(input) }
        for output in session.outputs { session.removeOutput(output) }
        session.automaticallyConfiguresApplicationAudioSession = false
        guard session.canSetSessionPreset(profile.sessionPreset) else { throw CaptureFailure(message: "\(profile.label) capture is unavailable") }
        session.sessionPreset = profile.sessionPreset
        guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back),
              let microphone = AVCaptureDevice.default(for: .audio) else { throw CaptureFailure(message: "Camera or microphone unavailable") }
        let cameraInput = try AVCaptureDeviceInput(device: camera)
        let micInput = try AVCaptureDeviceInput(device: microphone)
        guard session.canAddInput(cameraInput), session.canAddInput(micInput) else { throw CaptureFailure(message: "Capture inputs are unsupported") }
        session.addInput(cameraInput); session.addInput(micInput)
        try camera.lockForConfiguration()
        defer { camera.unlockForConfiguration() }
        let rate = Double(profile.frameRate)
        guard camera.activeFormat.videoSupportedFrameRateRanges.contains(where: { $0.minFrameRate <= rate && $0.maxFrameRate >= rate }) else { throw CaptureFailure(message: "\(profile.label) is unsupported on this camera") }
        camera.activeVideoMinFrameDuration = CMTime(value: 1, timescale: profile.frameRate)
        camera.activeVideoMaxFrameDuration = CMTime(value: 1, timescale: profile.frameRate)
        let video = AVCaptureVideoDataOutput()
        video.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        video.alwaysDiscardsLateVideoFrames = true
        video.setSampleBufferDelegate(self, queue: samples)
        let audio = AVCaptureAudioDataOutput()
        audio.setSampleBufferDelegate(self, queue: samples)
        guard session.canAddOutput(video), session.canAddOutput(audio) else { throw CaptureFailure(message: "Capture outputs unavailable") }
        session.addOutput(video); session.addOutput(audio)
        guard let connection = video.connection(with: .video), connection.isVideoOrientationSupported else {
            throw CaptureFailure(message: "Camera cannot produce physically oriented video buffers")
        }
        connection.videoOrientation = orientationPlan.desired.capture
        snapshot.orientation = orientationPlan.desired
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
              self.snapshot.canStart, !self.snapshot.running, !self.stopping else { return }
        self.captureGeneration &+= 1
        let captureToken = self.captureGeneration
        do {
            try self.checkStorage()
            let folder = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let url = folder.appendingPathComponent("Sapflow-\(UUID().uuidString).mov")
            self.ingress.reset()
            self.recordingFailureDescription = nil
            let run = try CaptureRun(url: url, orientation: self.orientationPlan.desired, profile: ticket.preparation.profile, ingress: self.ingress,
                failure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.stopOnControl(reason: error.localizedDescription)
                } },
                publishingFailure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.snapshot.publisher = "Publisher stopped: \(error.localizedDescription)"
                    self?.snapshot.detail = "Local recording continues. Stop and prepare again to restart publishing."
                    self?.publish()
                } }, recordingFailure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.recordingFailureDescription = error.localizedDescription
                    self?.snapshot.detail = "Recording failed: \(error.localizedDescription)"; self?.publish()
                } }, statistics: { [weak self] count, status in self?.control.async {
                    guard self?.captureGeneration == captureToken, self?.preparedIntent == ticket.preparation else { return }
                    self?.snapshot.encoded = count; self?.snapshot.publisher = status; self?.publish()
                } }, meter: { [weak self] level in self?.meter(level) })
            // Once native owners exist, even a winning synchronous Stop must
            // reach their actual worker/recorder cleanup path before admission.
            run.worker.startPublishing {
                self.samples.sync { self.run = run }
                self.snapshot.running = true; self.snapshot.canStart = false
            }
            guard self.admission.started(ticket) else { self.stopOnControl(reason: nil); return }
            self.snapshot.phase = "Recording"
            self.snapshot.detail = "Hardware encoder: \(run.video.implementation). Saving compressed video and audio locally."
            if !keepSavedSegment { self.snapshot.recordingURL = nil }
            self.snapshot.encoded = 0; self.snapshot.captureDrops = 0; self.snapshot.level = -60
            self.snapshot.publisher = self.ingress.status
            self.publish()
            let timer = DispatchSource.makeTimerSource(queue: self.control)
            timer.schedule(deadline: .now() + 3, repeating: 5)
            timer.setEventHandler { [weak self] in
                guard let self, self.captureGeneration == captureToken, self.preparedIntent == ticket.preparation else { return }
                do { try self.checkStorage() } catch { self.stopOnControl(reason: error.localizedDescription) }
                self.samples.async {
                    if self.run?.epoch == nil { self.control.async {
                        guard self.captureGeneration == captureToken, self.preparedIntent == ticket.preparation else { return }
                        self.stopOnControl(reason: "Camera or microphone did not deliver its initial sample")
                    } }
                }
            }
            self.storageTimer = timer; timer.resume()
        } catch { self.fencePublishing(); self.stopOnControl(reason: error.localizedDescription) }
    }
    func stop() {
        let ticket = admission.stop()
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
        stopping = false; preparedIntent = nil; stopIntent = nil
        snapshot.phase = stopReason == nil ? "Stopped" : "Interrupted"
        snapshot.canStart = false
        publish()
    }
    private func stopOnControl(reason: String?) {
        if let reason { stopReason = reason; orientationPlan.cancel(); rotationTimeout?.cancel(); rotationTimeout = nil }
        let child = rotationIntent
        let rotationAllowed = reason == nil && child.map(admission.permitsRotation) == true && orientationPlan.pending != nil
        if !rotationAllowed {
            stopIntent = admission.stop(); rotationIntent = nil
            orientationPlan.cancel()
        }
        guard !stopping else { return }
        stopReason = reason
        let rotationToken = rotationAllowed ? orientationPlan.pending : nil
        if rotationToken == nil { fencePublishing() } else { fenceCodecEpoch() }
        storageTimer?.cancel(); storageTimer = nil
        setSampleDelivery(false)
        if session.isRunning { session.stopRunning() }
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
                let clean = self.ingress.destination?.cleanupComplete != false
                let released = self.admission.completeStop(stopped, cleanupComplete: clean)
                if released { self.admissionStopped() }
                else if self.admission.currentPhase == .retained {
                    self.stopping = false; self.snapshot.phase = "Interrupted"
                    self.snapshot.detail = "Publisher cleanup unconfirmed; Prepare is blocked."; self.publish()
                }
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
            do { try run.audio.finish() } catch {
                self.control.async { self.recordingFailureDescription = "AAC flush failed: \(error.localizedDescription)" }
            }
            run.video.finish()
            run.worker.finish { publishingResult in
                // A retained network owner blocks rotation, never file flush.
                if case .failure = publishingResult {
                    self.control.async {
                        self.orientationPlan.cancel()
                        self.stopReason = "Publisher cleanup unconfirmed; automatic restart blocked."
                    }
                }
                run.recorder.finish { result in
                    self.control.async {
                        self.stopping = false
                        let completedReason = self.stopReason
                        self.snapshot.phase = completedReason == nil ? "Stopped" : "Interrupted"
                        switch result {
                        case let .success(url):
                            self.snapshot.recordingURL = url
                            self.snapshot.detail = completedReason ?? self.recordingFailureDescription.map { "Recording stopped early: \($0). Accepted prefix saved." } ?? "Recording saved. Export it below or from Files."
                        case let .failure(error): self.snapshot.detail = "Recording finalisation failed: \(error.localizedDescription)"
                        }
                        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
                        self.rotationTimeout?.cancel(); self.rotationTimeout = nil
                        if case .success = result, self.recordingFailureDescription == nil,
                           case .success = publishingResult, let rotationToken, let child,
                           self.admission.permitsRotation(child), self.orientationPlan.complete(rotationToken) != nil {
                            self.rotationIntent = nil
                            self.restartForOrientation(child)
                        } else {
                            self.orientationPlan.cancel(); self.rotationIntent = nil
                            let stopped = self.stopIntent ?? self.admission.stop()
                            let clean: Bool
                            if case .success = publishingResult { clean = self.ingress.destination?.cleanupComplete != false } else { clean = false }
                            _ = self.admission.completeStop(stopped, cleanupComplete: clean)
                            if self.admission.currentPhase == .idle { self.preparedIntent = nil; self.stopIntent = nil }
                            self.publish()
                        }
                    }
                }
            }
        }
    }
    private func checkStorage() throws {
        let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let available = try folder.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]).volumeAvailableCapacityForImportantUsage ?? 0
        guard available > 200 * 1024 * 1024 else { throw CaptureFailure(message: "Less than 200 MiB storage available. Free space before recording.") }
    }
    private func report(_ error: Error) { snapshot.phase = "Error"; snapshot.detail = error.localizedDescription; snapshot.canStart = false; publish() }
    private func publish() { let value = snapshot; DispatchQueue.main.async { UIApplication.shared.isIdleTimerDisabled = value.running; self.onChange?(value) } }
    private func meter(_ level: Float) {
        // Audio callback only schedules coalesced UI work, never logs per packet.
        let now = ProcessInfo.processInfo.systemUptime
        guard now - lastMeterUpdate >= 0.2 else { return }
        lastMeterUpdate = now
        control.async { self.snapshot.level = level; self.publish() }
    }
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let run, run.admitting else { return }
        do {
            let isVideo = output is AVCaptureVideoDataOutput
            if run.epoch == nil {
                // Preserve early audio while waiting for the first camera
                // callback. Dropping it would silently remove PCM from the
                // shared timeline. Startup alone may retain bounded raw media.
                let bytes = CMSampleBufferGetImageBuffer(sampleBuffer).map { CVPixelBufferGetDataSize($0) } ?? CMSampleBufferGetTotalSampleSize(sampleBuffer)
                guard run.startup.count < 8, bytes <= 12 * 1024 * 1024 - run.startupBytes else {
                    throw CaptureFailure(message: "Camera/microphone startup exceeded its native buffer bound")
                }
                run.startup.append((sampleBuffer, isVideo)); run.startupBytes += bytes
                guard run.startup.contains(where: { $0.video }), run.startup.contains(where: { !$0.video }) else { return }
                let initial = run.startup.sorted {
                    CMTimeCompare(CMSampleBufferGetPresentationTimeStamp($0.sample), CMSampleBufferGetPresentationTimeStamp($1.sample)) < 0
                }
                run.epoch = CMSampleBufferGetPresentationTimeStamp(initial[0].sample)
                run.startup.removeAll(); run.startupBytes = 0
                for pending in initial { try process(pending.sample, video: pending.video, run: run) }
            } else { try process(sampleBuffer, video: isVideo, run: run) }
        } catch {
            run.admitting = false; run.startup.removeAll(); run.startupBytes = 0
            control.async { self.stopOnControl(reason: error.localizedDescription) }
        }
    }
    private func process(_ sample: CMSampleBuffer, video: Bool, run: CaptureRun) throws {
        guard let epoch = run.epoch else { return }
        let time = CMTimeSubtract(CMSampleBufferGetPresentationTimeStamp(sample), epoch)
        guard time.isValid, time.seconds >= 0 else { throw CaptureFailure(message: "Capture clock moved before the session epoch") }
        if video { try run.video.encode(sample, time: time) }
        else { try run.audio.encode(sample, time: time) }
    }
    func captureOutput(_ output: AVCaptureOutput, didDrop sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        control.async { self.snapshot.captureDrops += 1 }
    }
}
