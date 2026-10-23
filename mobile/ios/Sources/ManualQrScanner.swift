import AVFoundation
import Combine
import UIKit

/// App-lifetime scanner, separate from every capture/encoder session. Published
/// and preview state belong to main; native state belongs to sessionQueue.
final class ManualQrScanner: ObservableObject {
    final class Selection: Identifiable {
        let id = UUID()
        fileprivate let policy: ManualQrPolicy.Ticket
        fileprivate let library: RecordingLibraryAdmission.Ticket
        fileprivate let settings: ManualDestinationSettings.Ticket
        fileprivate var permissionRequested = false, closeScheduled = false
        fileprivate init(policy: ManualQrPolicy.Ticket, library: RecordingLibraryAdmission.Ticket,
                         settings: ManualDestinationSettings.Ticket) {
            self.policy = policy; self.library = library; self.settings = settings
        }
    }
    @Published private(set) var presentation: Selection?
    @Published private(set) var visible = false
    @Published private(set) var message: String?
    private let policy = ManualQrPolicy()
    private let library: RecordingLibraryAdmission
    private let settings: ManualDestinationSettings
    private let canReserve: () -> Bool
    private let sessionQueue = DispatchQueue(label: "sapflow.manual-qr.session", qos: .userInitiated)
    private let metadataQueue = DispatchQueue(label: "sapflow.manual-qr.metadata", qos: .userInitiated)
    private var camera: Camera? // Session owner only.
    private var timeout: DispatchWorkItem? // Main only; never a close receipt.
    private weak var preview: ManualQrPreviewView?
    private var retainedSelf: ManualQrScanner?
    private static var now: UInt64 { DispatchTime.now().uptimeNanoseconds }

    init(library: RecordingLibraryAdmission, settings: ManualDestinationSettings,
         canReserve: @escaping () -> Bool) {
        self.library = library; self.settings = settings; self.canReserve = canReserve
        settings.onScanInvalidated = { [weak self] ticket in
            guard let self, let selection = self.presentation, selection.settings == ticket else { return }
            self.cancel(selection)
        }
    }
    var canScan: Bool { presentation == nil && settings.canScan && library.idle && canReserve() }
    func isPresented(_ selection: Selection) -> Bool {
        presentation === selection && visible && policy.claimModal(selection.policy)
    }
    func begin() {
        guard canScan, let edit = settings.beginScan() else { return }
        guard let lease = library.reserveCameraScan(canReserve: canReserve) else {
            _ = settings.completeScan(edit, delivery: nil); return
        }
        guard let token = policy.begin(now: Self.now) else {
            _ = library.finish(lease, cleanupComplete: true)
            _ = settings.completeScan(edit, delivery: nil); return
        }
        let selection = Selection(policy: token, library: lease, settings: edit)
        retainedSelf = self; presentation = selection; visible = true; message = nil
        let alarm = DispatchWorkItem { [weak self, weak selection] in
            guard let self, let selection, self.presentation === selection,
                  self.policy.expire(selection.policy, now: Self.now) else { return }
            self.message = "QR scan timed out. Close the scanner and try again."
            self.close(selection)
        }
        timeout = alarm
        DispatchQueue.main.asyncAfter(deadline: .now() + 60, execute: alarm)
    }
    /// Called only by the exact presented sheet, never by model recomposition.
    func presented(_ selection: Selection) {
        guard presentation === selection, !selection.permissionRequested,
              settings.scanIsCurrent(selection.settings), policy.permits(selection.policy, now: Self.now) else { return }
        selection.permissionRequested = true
        let reply: (Bool) -> Void = { [weak self, selection] granted in
            guard let self, self.presentation === selection else { return }
            guard self.settings.scanIsCurrent(selection.settings),
                  self.policy.permits(selection.policy, now: Self.now), granted else {
                if !granted, self.policy.permits(selection.policy, now: Self.now) {
                    self.message = "Camera permission is required for QR scanning."
                }
                self.cancel(selection)
                self.completeIfReady(selection) // Other close receipts may already be in.
                return
            }
            self.sessionQueue.async { self.open(selection) }
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: DispatchQueue.main.async { reply(true) }
        case .notDetermined:
            if !ManualQrPermissionRequest.begin(policy: policy, ticket: selection.policy, now: Self.now,
                request: { AVCaptureDevice.requestAccess(for: .video, completionHandler: $0) },
                deliver: { DispatchQueue.main.async(execute: $0) }, reply: reply) { cancel(selection) }
        default: DispatchQueue.main.async { reply(false) }
        }
    }
    func attachPreview(_ view: ManualQrPreviewView, selection: Selection) {
        guard presentation === selection else { return }
        preview = view
        presented(selection)
    }
    func requestDismissal(_ selection: Selection) {
        guard presentation === selection, visible else { return }
        cancel(selection)
    }
    func dismissed(_ selection: Selection) {
        guard presentation === selection else { return }
        if visible { cancel(selection) }
        policy.modalClosed(selection.policy)
        completeIfReady(selection)
    }
    func cancel(_ selection: Selection) {
        guard presentation === selection else { return }
        policy.cancel(selection.policy)
        close(selection)
    }
    private func close(_ selection: Selection) {
        guard presentation === selection else { return }
        visible = false; timeout?.cancel(); timeout = nil
        guard !selection.closeScheduled else { return }
        selection.closeScheduled = true
        sessionQueue.async {
            // A claimed open may be queued before cancellation. This queue
            // orders its real construction/stop, not an isRunning snapshot.
            if let camera = self.camera {
                ManualQrCleanup.close(stop: {
                    camera.session.stopRunning(); return !camera.session.isRunning
                }, detach: { camera.output.setMetadataObjectsDelegate(nil, queue: nil) }, afterCallbacks: { barrier in
                    self.metadataQueue.async { self.sessionQueue.async(execute: barrier) }
                }, release: {
                    camera.session.beginConfiguration()
                    for input in camera.session.inputs { camera.session.removeInput(input) }
                    for output in camera.session.outputs { camera.session.removeOutput(output) }
                    camera.session.commitConfiguration()
                    self.camera = nil
                }, complete: { self.nativeClosed(selection, clean: $0) })
            } else { self.nativeClosed(selection, clean: true) }
        }
    }
    private func nativeClosed(_ selection: Selection, clean: Bool) {
        DispatchQueue.main.async {
            guard self.presentation === selection else { return }
            self.preview?.previewLayer.session = nil
            self.policy.nativeClosed(selection.policy, clean: clean)
            if !clean {
                _ = self.library.finish(selection.library, cleanupComplete: false)
                self.message = "The QR camera has not closed safely. Capture remains unavailable."
            }
            self.completeIfReady(selection)
        }
    }
    private func completeIfReady(_ selection: Selection) {
        guard presentation === selection, let result = policy.takeCompletion(selection.policy, now: Self.now) else { return }
        let allowed = library.finish(selection.library, cleanupComplete: true)
        let delivery = allowed && library.canDeliver(selection.library) ? result.delivery : nil
        preview = nil; presentation = nil; retainedSelf = nil
        _ = settings.completeScan(selection.settings, delivery: delivery)
    }
    private func open(_ selection: Selection) {
        guard policy.claimOpen(selection.policy, now: Self.now) else {
            DispatchQueue.main.async { self.cancel(selection) }; return
        }
        guard AVCaptureDevice.authorizationStatus(for: .video) == .authorized,
              policy.permits(selection.policy, now: Self.now) else {
            DispatchQueue.main.async { self.cancel(selection) }; return
        }
        do {
            let camera = Camera()
            self.camera = camera // Retain even a partially configured owner.
            try camera.configure(metadataQueue: metadataQueue) { [weak self, weak selection] text in
                guard let self, let selection, self.policy.permits(selection.policy, now: Self.now),
                      ManualQrPolicy.validatedCandidate(text),
                      self.policy.candidate(text, ticket: selection.policy, now: Self.now) else { return }
                DispatchQueue.main.async { self.close(selection) }
            }
            guard policy.permits(selection.policy, now: Self.now),
                  AVCaptureDevice.authorizationStatus(for: .video) == .authorized else {
                DispatchQueue.main.async { self.cancel(selection) }; return
            }
            camera.session.startRunning()
            guard camera.session.isRunning else { throw Failure.unavailable }
            DispatchQueue.main.async {
                guard self.presentation === selection, self.policy.permits(selection.policy, now: Self.now) else { return }
                self.preview?.previewLayer.session = camera.session
            }
        } catch {
            DispatchQueue.main.async {
                guard self.presentation === selection else { return }
                self.message = "QR camera scanning is unavailable. Nothing was imported."
                self.cancel(selection)
            }
        }
    }
    private enum Failure: Error { case unavailable }
    private final class Camera: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        let session = AVCaptureSession(), output = AVCaptureMetadataOutput()
        private var receive: ((String) -> Void)?
        func configure(metadataQueue: DispatchQueue, receive: @escaping (String) -> Void) throws {
            self.receive = receive
            session.automaticallyConfiguresApplicationAudioSession = false
            session.usesApplicationAudioSession = false
            session.beginConfiguration(); defer { session.commitConfiguration() }
            guard session.canSetSessionPreset(.vga640x480),
                  let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) else { throw Failure.unavailable }
            session.sessionPreset = .vga640x480
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input) else { throw Failure.unavailable }
            session.addInput(input)
            guard session.canAddOutput(output) else { throw Failure.unavailable }
            session.addOutput(output)
            guard output.availableMetadataObjectTypes.contains(.qr) else { throw Failure.unavailable }
            output.setMetadataObjectsDelegate(self, queue: metadataQueue)
            output.metadataObjectTypes = [.qr]
        }
        func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject], from connection: AVCaptureConnection) {
            for object in metadataObjects.prefix(4) {
                guard let code = object as? AVMetadataMachineReadableCodeObject, code.type == .qr,
                      let text = code.stringValue, ManualQrPolicy.boundedASCII(text) else { continue }
                receive?(text)
            }
        }
    }
}
