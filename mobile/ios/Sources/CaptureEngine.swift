import AVFoundation
import UIKit

struct CaptureSnapshot {
    var orientation = CaptureOrientation.portrait
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

final class IngressWorker {
    private let queue = DispatchQueue(label: "sapflow.encoded-ingress", qos: .userInitiated)
    private let budget = EncodedWorkBudget(bytes: 4 * 1024 * 1024, count: 128, span: 1)
    let ingress: RustEncodedIngress
    private let failure: (Error) -> Void
    private let statistics: (UInt64, String) -> Void
    private var lastStatistics = 0.0
    private var failed = false
    init(ingress: RustEncodedIngress, failure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void) { self.ingress = ingress; self.failure = failure; self.statistics = statistics }
    func offer(_ unit: CompressedUnit) {
        guard let reservation = budget.reserve(bytes: unit.byteCount) else {
            if budget.close() { queue.async { self.fail(CaptureFailure(message: "Encoded SDK ingress exceeded its bounded backlog")) } }
            return
        }
        queue.async {
            defer { self.budget.release(reservation) }
            guard !self.failed else { return }
            do {
                try self.ingress.accept(unit)
                let now = ProcessInfo.processInfo.systemUptime
                if now - self.lastStatistics >= 0.5 {
                    self.lastStatistics = now
                    self.statistics(self.ingress.accepted, self.ingress.status)
                }
            } catch { if self.budget.close() { self.fail(error) } }
        }
    }
    private func fail(_ error: Error) {
        guard !failed else { return }
        failed = true
        ingress.destination?.endDestination()
        failure(error)
    }
    func finish(_ completion: @escaping (Result<Void, Error>) -> Void) {
        budget.close()
        queue.async {
            if let destination = self.ingress.destination { destination.finishEpoch(completion: completion) }
            else { completion(.success(())) }
        }
    }
}

final class CaptureRun {
    let recorder: CompressedRecorder
    let worker: IngressWorker
    var video: HardwareVideoEncoder!
    var audio: NativeAudioEncoder!
    var epoch: CMTime?
    var admitting = true
    var startup: [(sample: CMSampleBuffer, video: Bool)] = []
    var startupBytes = 0
    init(url: URL, orientation: CaptureOrientation, ingress: RustEncodedIngress, failure: @escaping (Error) -> Void, publishingFailure: @escaping (Error) -> Void, recordingFailure: @escaping (Error) -> Void, statistics: @escaping (UInt64, String) -> Void, meter: @escaping (Float) -> Void) throws {
        recorder = try CompressedRecorder(url: url, onFailure: recordingFailure)
        worker = IngressWorker(ingress: ingress, failure: publishingFailure, statistics: statistics)
        let recorder = self.recorder; let worker = self.worker
        let output: (CompressedUnit) -> Void = { unit in recorder.offer(unit); worker.offer(unit) }
        video = try HardwareVideoEncoder(orientation: orientation, output: output, failure: failure)
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
    private var orientationPlan = OrientationRestartPlan()
    private var rotationTimeout: DispatchWorkItem?
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
                self?.control.async {
                    guard let self else { return }
                    if notification.name == AVAudioSession.routeChangeNotification && !self.snapshot.running { return }
                    self.stopOnControl(reason: "Capture interrupted. Return to the foreground and prepare again.")
                }
            })
        }
    }
    deinit { for token in notifications { NotificationCenter.default.removeObserver(token) }; storageTimer?.cancel(); rotationTimeout?.cancel() }

    /// Attach a real bounded destination adapter before preparing. No endpoint
    /// or credential is inferred from a display label or bundled configuration.
    func setDestination(_ destination: EncodedPublishingSink?, completion: @escaping (Bool) -> Void = { _ in }) {
        control.async {
            guard !self.snapshot.running, !self.stopping else { completion(false); return }
            guard self.ingress.destination?.cleanupComplete != false else {
                self.snapshot.detail = "Previous publisher cleanup is unconfirmed; Prepare is blocked."
                self.publish(); completion(false); return
            }
            self.ingress.destination?.endDestination()
            self.ingress.destination = destination
            self.destinationLock.lock(); self.destinationFence = destination; self.destinationLock.unlock()
            self.snapshot.publisher = self.ingress.status
            self.publish(); completion(true)
        }
    }
    private func fencePublishing() {
        destinationLock.lock(); let owner = destinationFence; destinationLock.unlock()
        owner?.endDestination()
    }
    func setOrientation(_ orientation: CaptureOrientation) {
        control.async {
            guard self.orientationPlan.desired != orientation else { return }
            let token = self.orientationPlan.request(orientation, running: self.snapshot.running)
            if let token {
                let timeout = DispatchWorkItem { [weak self] in
                    guard let self, self.orientationPlan.pending == token else { return }
                    self.orientationPlan.cancel(); self.rotationTimeout = nil
                    self.fencePublishing()
                    self.stopReason = "Rotation finalisation timed out; automatic restart cancelled."
                    self.snapshot.detail = "Rotation finalisation exceeded six seconds. Automatic restart cancelled; cleanup may still finish."
                    self.publish()
                }
                self.rotationTimeout = timeout
                self.control.asyncAfter(deadline: .now() + 6, execute: timeout)
                self.stopOnControl(reason: nil)
            } else if !self.stopping && !self.snapshot.running {
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
        "\(orientationPlan.desired.width)×\(orientationPlan.desired.height) at 30 fps · hardware H.264 · AAC 48 kHz mono"
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
    private func restartForOrientation() {
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
            snapshot.canStart = true
            startOnControl(keepSavedSegment: true)
            if snapshot.running {
                snapshot.detail = "\(orientationDescription). Rotation began a new publishing session and recording segment; previous segment remains in Files."
                publish()
            }
        } catch { report(error) }
    }
    func prepare() {
        Task {
            let camera = await AVCaptureDevice.requestAccess(for: .video)
            let microphone = await AVCaptureDevice.requestAccess(for: .audio)
            control.async {
                guard camera && microphone else { self.report(CaptureFailure(message: "Camera and microphone permission are required. Enable them in Settings.")); return }
                guard !self.snapshot.running, !self.stopping else { return }
                do {
                    try self.checkStorage()
                    let audio = AVAudioSession.sharedInstance()
                    try audio.setCategory(.playAndRecord, mode: .videoRecording, options: [.defaultToSpeaker, .allowBluetooth])
                    try audio.setPreferredSampleRate(48_000)
                    try audio.setActive(true)
                    if !self.configured { try self.configure() }
                    try self.applyOrientation()
                    self.setSampleDelivery(true)
                    self.session.startRunning()
                    self.snapshot.phase = "Prepared"
                    self.snapshot.detail = self.orientationDescription
                    self.snapshot.canStart = true
                    self.snapshot.publisher = self.ingress.status
                    self.publish()
                } catch { self.report(error) }
            }
        }
    }
    private func configure() throws {
        session.beginConfiguration(); defer { session.commitConfiguration() }
        for input in session.inputs { session.removeInput(input) }
        for output in session.outputs { session.removeOutput(output) }
        session.automaticallyConfiguresApplicationAudioSession = false
        guard session.canSetSessionPreset(.hd1280x720) else { throw CaptureFailure(message: "720p capture is unavailable") }
        session.sessionPreset = .hd1280x720
        guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back),
              let microphone = AVCaptureDevice.default(for: .audio) else { throw CaptureFailure(message: "Camera or microphone unavailable") }
        let cameraInput = try AVCaptureDeviceInput(device: camera)
        let micInput = try AVCaptureDeviceInput(device: microphone)
        guard session.canAddInput(cameraInput), session.canAddInput(micInput) else { throw CaptureFailure(message: "Capture inputs are unsupported") }
        session.addInput(cameraInput); session.addInput(micInput)
        try camera.lockForConfiguration()
        defer { camera.unlockForConfiguration() }
        guard camera.activeFormat.videoSupportedFrameRateRanges.contains(where: { $0.minFrameRate <= 30 && $0.maxFrameRate >= 30 }) else { throw CaptureFailure(message: "720p30 is unsupported on this camera") }
        camera.activeVideoMinFrameDuration = CMTime(value: 1, timescale: 30)
        camera.activeVideoMaxFrameDuration = CMTime(value: 1, timescale: 30)
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
        configured = true
    }
    func start() { control.async { self.startOnControl() } }
    private func startOnControl(keepSavedSegment: Bool = false) {
        guard self.snapshot.canStart, !self.snapshot.running, !self.stopping else { return }
        self.captureGeneration &+= 1
        let captureToken = self.captureGeneration
        do {
            try self.checkStorage()
            let folder = try FileManager.default.url(for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let url = folder.appendingPathComponent("Sapflow-\(UUID().uuidString).mov")
            self.ingress.reset()
            self.recordingFailureDescription = nil
            let run = try CaptureRun(url: url, orientation: self.orientationPlan.desired, ingress: self.ingress,
                failure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken else { return }
                    self?.stopOnControl(reason: error.localizedDescription)
                } },
                publishingFailure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken else { return }
                    self?.snapshot.publisher = "Publisher stopped: \(error.localizedDescription)"
                    self?.snapshot.detail = "Local recording continues. Stop and prepare again to restart publishing."
                    self?.publish()
                } }, recordingFailure: { [weak self] error in self?.control.async {
                    guard self?.captureGeneration == captureToken else { return }
                    self?.recordingFailureDescription = error.localizedDescription
                    self?.snapshot.detail = "Recording failed: \(error.localizedDescription)"; self?.publish()
                } }, statistics: { [weak self] count, status in self?.control.async {
                    guard self?.captureGeneration == captureToken else { return }
                    self?.snapshot.encoded = count; self?.snapshot.publisher = status; self?.publish()
                } }, meter: { [weak self] level in self?.meter(level) })
            try self.ingress.destination?.start()
            self.samples.sync { self.run = run }
            self.snapshot.running = true; self.snapshot.canStart = false
            self.snapshot.phase = "Recording"
            self.snapshot.detail = "Hardware encoder: \(run.video.implementation). Saving compressed video and audio locally."
            if !keepSavedSegment { self.snapshot.recordingURL = nil }
            self.snapshot.encoded = 0; self.snapshot.captureDrops = 0; self.snapshot.level = -60
            self.snapshot.publisher = self.ingress.status
            self.publish()
            let timer = DispatchSource.makeTimerSource(queue: self.control)
            timer.schedule(deadline: .now() + 3, repeating: 5)
            timer.setEventHandler { [weak self] in
                guard let self, self.captureGeneration == captureToken else { return }
                do { try self.checkStorage() } catch { self.stopOnControl(reason: error.localizedDescription) }
                self.samples.async {
                    if self.run?.epoch == nil { self.control.async {
                        guard self.captureGeneration == captureToken else { return }
                        self.stopOnControl(reason: "Camera or microphone did not deliver its initial sample")
                    } }
                }
            }
            self.storageTimer = timer; timer.resume()
        } catch { self.fencePublishing(); self.report(error) }
    }
    func stop() {
        fencePublishing() // synchronous admission fence precedes queued cleanup
        control.async {
            self.orientationPlan.cancel(); self.rotationTimeout?.cancel(); self.rotationTimeout = nil
            if self.stopping { self.snapshot.detail = "Restart cancelled; finishing the recording."; self.publish() }
            self.stopOnControl(reason: nil)
        }
    }
    private func stopOnControl(reason: String?) {
        if let reason { stopReason = reason; orientationPlan.cancel(); rotationTimeout?.cancel(); rotationTimeout = nil }
        guard !stopping else { return }
        stopReason = reason
        let rotationToken = orientationPlan.pending
        if rotationToken == nil { fencePublishing() }
        storageTimer?.cancel(); storageTimer = nil
        setSampleDelivery(false)
        if session.isRunning { session.stopRunning() }
        snapshot.canStart = false
        guard snapshot.running else {
            snapshot.phase = reason == nil ? "Stopped" : "Interrupted"
            snapshot.detail = reason ?? "Prepare to start a new session."
            publish(); try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            return
        }
        stopping = true
        snapshot.running = false; snapshot.phase = "Finalising"
        snapshot.publisher = ingress.destination == nil ? "Local capture stopping" : "Publisher stop requested · destination end unconfirmed"
        snapshot.detail = rotationToken == nil ? (reason ?? "Closing the recording safely.") : "Rotating: saving this segment and restarting the publisher upright."
        publish()
        samples.async {
            guard let run = self.run else { self.control.async { self.stopping = false }; return }
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
                        if case .success = result, self.recordingFailureDescription == nil, let rotationToken, self.orientationPlan.complete(rotationToken) != nil {
                            self.restartForOrientation()
                        } else {
                            self.orientationPlan.cancel()
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
