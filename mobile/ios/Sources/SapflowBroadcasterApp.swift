import SwiftUI
import AVFoundation

@main
struct SapflowBroadcasterApp: App {
    var body: some Scene { WindowGroup { BroadcasterView() } }
}

final class BroadcasterModel: ObservableObject {
    @Published var snapshot = CaptureSnapshot()
    @Published var recordingOnly = true
    @Published var transport = ManualTransport.rtmp
    @Published var endpoint = ""
    @Published var srtStreamID = ""
    @Published var srtPassphrase = ""
    @Published var srtPlaintext = false
    @Published var streamKey = ""
    @Published var profileName = ""
    @Published var allowPlaintext = false
    @Published var profiles: [SavedDestinationProfile] = []
    @Published var selectedProfile: UUID?
    @Published var destinationError: String?
    private let profileStore = DestinationProfileStore()
    private var hasOrientation = false
    let engine = CaptureEngine()
    init() {
        profiles = profileStore.load()
        engine.onChange = { [weak self] snapshot in self?.snapshot = snapshot }
    }
    func updateOrientation() {
        if let orientation = CaptureOrientation.device(UIDevice.current.orientation) {
            hasOrientation = true; engine.setOrientation(orientation)
        } else if !hasOrientation, let orientation = UIApplication.shared.connectedScenes
            .compactMap({ ($0 as? UIWindowScene)?.interfaceOrientation }).compactMap(CaptureOrientation.interface).first {
            hasOrientation = true; engine.setOrientation(orientation)
        }
        // Face-up/down and unknown readings retain the last valid orientation.
    }
    func start() { updateOrientation(); engine.start() }
    func prepare() {
        updateOrientation()
        do {
            destinationError = nil
            let sink: EncodedPublishingSink?
            if recordingOnly { sink = nil }
            else if transport == .srt {
                let destination = try srtDestination()
                sink = try ReconnectingPublisher(source: .srtPeerAcknowledgedWatermark) { ManualSrtPublisher(destination: destination, observe: $0) }
            } else {
                let destination = try destination()
                sink = try ReconnectingPublisher(source: .rtmpPeerAcknowledgedWatermark) { ManualRtmpPublisher(destination: destination, observe: $0) }
            }
            engine.setDestination(sink) { [weak self] accepted in if accepted { self?.engine.prepare() } }
        } catch { destinationError = error.localizedDescription }
    }
    private func destination() throws -> ManualDestination {
        try ManualDestination(endpoint: endpoint, streamKey: streamKey, allowPlaintext: allowPlaintext)
    }
    private func srtDestination() throws -> ManualSrtDestination {
        try ManualSrtDestination(endpoint: endpoint, streamID: srtStreamID, passphrase: srtPassphrase, allowPlaintext: srtPlaintext)
    }
    func loadProfile(_ id: UUID?) {
        guard let id, let profile = profiles.first(where: { $0.id == id }) else { return }
        do {
            if profile.transport == "srt" {
                let secret = try profileStore.srtSecret(for: id)
                transport = .srt; srtStreamID = secret.streamID; srtPassphrase = secret.passphrase
                srtPlaintext = profile.allowPlaintext
            } else {
                transport = .rtmp; streamKey = try profileStore.key(for: id)
                allowPlaintext = profile.allowPlaintext
            }
            endpoint = profile.endpoint
            profileName = profile.name; recordingOnly = false; destinationError = nil
        } catch { streamKey = ""; destinationError = error.localizedDescription }
    }
    func saveProfile() {
        do {
            if transport == .srt {
                selectedProfile = try profileStore.saveSrt(id: selectedProfile, name: profileName, destination: srtDestination(), allowPlaintext: srtPlaintext)
            } else {
                selectedProfile = try profileStore.save(id: selectedProfile, name: profileName, destination: destination(), allowPlaintext: allowPlaintext)
            }
            profiles = profileStore.load(); destinationError = nil
        } catch { destinationError = error.localizedDescription }
    }
    func deleteProfile() {
        guard let selectedProfile else { return }
        do {
            try profileStore.delete(selectedProfile)
            profiles = profileStore.load(); self.selectedProfile = nil; streamKey = ""; srtStreamID = ""; srtPassphrase = ""; destinationError = nil
        } catch { destinationError = error.localizedDescription }
    }
}

struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession
    let orientation: CaptureOrientation
    final class Surface: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    }
    func makeUIView(context: Context) -> Surface {
        let view = Surface()
        view.preview.session = session
        view.preview.videoGravity = .resizeAspect
        if let connection = view.preview.connection, connection.isVideoOrientationSupported { connection.videoOrientation = orientation.capture }
        return view
    }
    func updateUIView(_ view: Surface, context: Context) {
        if let connection = view.preview.connection, connection.isVideoOrientationSupported {
            connection.videoOrientation = orientation.capture
        }
    }
}

struct BroadcasterView: View {
    @StateObject private var model = BroadcasterModel()
    private var controlsLocked: Bool { model.snapshot.running || model.snapshot.phase == "Finalising" }
    var body: some View {
        GeometryReader { geometry in
            Group {
                if geometry.size.height > geometry.size.width {
                    VStack(spacing: 12) {
                        preview.frame(maxWidth: .infinity).frame(height: geometry.size.height * 0.42)
                        controls
                    }
                } else {
                    HStack(spacing: 16) {
                        preview.frame(maxWidth: .infinity, maxHeight: .infinity)
                        controls.frame(width: min(380, geometry.size.width * 0.47))
                    }
                }
            }.padding()
        }.background(Color(.systemBackground))
        .onAppear { UIDevice.current.beginGeneratingDeviceOrientationNotifications(); model.updateOrientation() }
        .onDisappear { UIDevice.current.endGeneratingDeviceOrientationNotifications() }
        .onReceive(NotificationCenter.default.publisher(for: UIDevice.orientationDidChangeNotification)) { _ in model.updateOrientation() }
    }
    private var preview: some View {
        CameraPreview(session: model.engine.session, orientation: model.snapshot.orientation)
            .background(Color.black)
            .overlay(alignment: .topLeading) {
                Text("LOCAL PREVIEW").font(.caption.bold()).padding(8).background(.black.opacity(0.6)).foregroundStyle(.white).padding()
            }
    }
    private var controls: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Sapflow").font(.largeTitle.bold())
                Text(model.snapshot.phase).font(.headline)
                Text(model.snapshot.detail).font(.callout)
                Label(model.snapshot.publisher, systemImage: model.recordingOnly ? "record.circle" : "antenna.radiowaves.left.and.right")
                    .font(.caption).foregroundStyle(.secondary)
                ProgressView(value: Double(model.snapshot.level + 60), total: 60).accessibilityLabel("Microphone level")
                Text("Mic \(Int(model.snapshot.level)) dBFS · encoded \(model.snapshot.encoded) · capture drops \(model.snapshot.captureDrops)").font(.caption.monospacedDigit())
                destinationForm.disabled(controlsLocked || model.snapshot.canStart)
                if let error = model.destinationError { Text(error).font(.caption).foregroundStyle(.red) }
                HStack {
                    Button("Prepare") { model.prepare() }.disabled(controlsLocked).accessibilityIdentifier("capture.prepare")
                    Button(model.recordingOnly ? "Start recording" : "Start broadcast") { model.start() }.disabled(!model.snapshot.canStart).accessibilityIdentifier("capture.start")
                }.buttonStyle(.borderedProminent)
                Button("Stop and save") { model.engine.stop() }
                    .buttonStyle(.bordered).disabled(!model.snapshot.running && !model.snapshot.canStart && model.snapshot.phase != "Finalising").accessibilityIdentifier("capture.stop")
                if let url = model.snapshot.recordingURL {
                    ShareLink(item: url) { Label("Export recording", systemImage: "square.and.arrow.up") }
                    Text("Saved in Files → On My iPhone → Sapflow").font(.caption).foregroundStyle(.secondary)
                }
                Text("Keep the app in the foreground. Turning the phone restarts publishing and saves a new recording segment. Locking the phone or an audio interruption ends capture.")
                    .font(.caption).foregroundStyle(.secondary)
            }.padding(.vertical)
        }.scrollDismissesKeyboard(.interactively)
    }
    @ViewBuilder private var destinationForm: some View {
        Toggle("Local recording only", isOn: $model.recordingOnly).accessibilityIdentifier("destination.recording-only")
        if !model.recordingOnly {
            Text("Manual destination · bounded automatic reconnect").font(.caption.bold())
            if !model.profiles.isEmpty {
                Picker("Saved profile", selection: $model.selectedProfile) {
                    Text("Unsaved destination").tag(Optional<UUID>.none)
                    ForEach(model.profiles) { profile in Text(profile.name).tag(Optional(profile.id)) }
                }.onChange(of: model.selectedProfile) { model.loadProfile($0) }
            }
            Picker("Transport", selection: $model.transport) {
                Text("RTMPS / RTMP").tag(ManualTransport.rtmp)
                Text("SRT caller").tag(ManualTransport.srt)
            }.pickerStyle(.segmented)
            if model.transport == .srt {
                TextField("srt://192.0.2.1:9000", text: $model.endpoint).accessibilityIdentifier("destination.endpoint")
                    .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL).textFieldStyle(.roundedBorder)
                SecureField("SRT stream ID (optional)", text: $model.srtStreamID).accessibilityIdentifier("destination.srt-stream-id")
                    .textInputAutocapitalization(.never).autocorrectionDisabled().textFieldStyle(.roundedBorder).privacySensitive()
                SecureField("SRT passphrase", text: $model.srtPassphrase).accessibilityIdentifier("destination.srt-passphrase")
                    .textInputAutocapitalization(.never).autocorrectionDisabled().textFieldStyle(.roundedBorder).privacySensitive()
                Toggle("Explicit plaintext SRT", isOn: $model.srtPlaintext).font(.caption)
                Text(model.srtPlaintext ? "Clear the passphrase to send plaintext media. There is no automatic downgrade." : "AES-256 media encryption. Stream ID/control remain clear; SRT does not authenticate peer identity.")
                    .font(.caption).foregroundStyle(model.srtPlaintext ? .orange : .secondary)
                Text("Numeric IPv4/IPv6 peer only · 120 ms SRT latency").font(.caption).foregroundStyle(.secondary)
            } else {
                TextField("rtmps://stream.example.invalid/live", text: $model.endpoint).accessibilityIdentifier("destination.endpoint")
                    .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL).textFieldStyle(.roundedBorder)
                SecureField("Stream key (kept separate from URL)", text: $model.streamKey).accessibilityIdentifier("destination.stream-key")
                    .textInputAutocapitalization(.never).autocorrectionDisabled().textFieldStyle(.roundedBorder).privacySensitive()
                Toggle("Allow plaintext RTMP for this destination", isOn: $model.allowPlaintext).font(.caption)
                if model.allowPlaintext { Text("rtmp:// sends the stream key and media without encryption. RTMPS will never downgrade automatically.").font(.caption).foregroundStyle(.orange) }
            }
            TextField("Profile name", text: $model.profileName).textFieldStyle(.roundedBorder)
            HStack {
                Button("Save profile") { model.saveProfile() }
                if model.selectedProfile != nil { Button("Delete", role: .destructive) { model.deleteProfile() } }
            }.buttonStyle(.bordered)
            Text("Save keeps destination secrets in this device's unlocked Keychain. Local recording continues through uplink loss. Automatic reconnect shares a 90-second outage window; repeated failures do not extend it. Timely media acknowledgement can restore retries. If retries exhaust while sending, that attempt may continue; a later failure needs a new Prepare. Sending does not confirm decoded playback.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}
