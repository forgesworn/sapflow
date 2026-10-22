import SwiftUI
import AVFoundation

@main
struct SapflowBroadcasterApp: App {
    @StateObject private var model = BroadcasterModel()
    var body: some Scene { WindowGroup { BroadcasterView(model: model) } }
}

final class BroadcasterModel: ObservableObject {
    @Published var snapshot = CaptureSnapshot()
    @Published var destinationMode = BroadcasterDestinationMode.recording {
        didSet {
            guard destinationMode != oldValue else { return }
            destinationSelection.select(destinationMode)
            updateCreatorActivity()
        }
    }
    @Published private(set) var creator = V4vCreatorOwner.Snapshot()
    @Published private(set) var creatorSetupError: String?
    @Published var bunkerURI = ""
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
    private lazy var destinationSelection = BroadcasterDestinationSelection { [weak engine] in engine?.cancelPendingPreparation($0) }
    private var hasOrientation = false
    private var foreground = false
    private var creatorOwner: V4vCreatorOwner?
    let creatorConfiguration = V4vCreatorConfiguration.load()
    let engine = CaptureEngine()
    init() {
        profiles = profileStore.load()
        engine.onChange = { [weak self] snapshot in
            guard let self else { return }
            self.snapshot = snapshot; self.updateCreatorCapture()
        }
        if creatorConfiguration.profile != nil {
            V4vCreatorProduction.open(configuration: creatorConfiguration, capture: engine,
                changed: { [weak self] in self?.creator = $0 }, completion: { [weak self] result in
                    guard let self else { if case let .success(owner) = result { owner.close() }; return }
                    switch result {
                    case let .success(owner): self.creatorOwner = owner; self.updateCreatorActivity()
                    case let .failure(error): self.creatorSetupError = error.description
                    }
                })
        }
    }
    deinit { creatorOwner?.close() }
    var recordingOnly: Bool { destinationMode == .recording }
    var captureBusy: Bool { snapshot.running || snapshot.phase == "Finalising" || snapshot.phase == "Preparing" }
    var canPrepare: Bool {
        !captureBusy && !snapshot.canStart && (destinationMode != .v4v || creator.canPrepare)
    }
    var canStart: Bool { snapshot.canStart && (destinationMode != .v4v || creator.canStart) }
    func setForeground(_ active: Bool) { foreground = active; updateCreatorActivity() }
    private func updateCreatorActivity() {
        updateCreatorCapture()
        creatorOwner?.setActive(destinationMode == .v4v && foreground)
    }
    private func updateCreatorCapture() {
        guard destinationMode == .v4v else { return }
        creatorOwner?.captureChanged(prepared: snapshot.canStart, running: snapshot.running,
                                     stopping: snapshot.phase == "Finalising" || snapshot.phase == "Preparing")
    }
    func signIn(restoring: Bool) {
        if creatorOwner?.signIn(bunkerURI: bunkerURI, restoring: restoring) == true { bunkerURI = "" }
    }
    func cancelSignIn() { creatorOwner?.cancelSignIn() }
    func reloadEvents() { creatorOwner?.reloadEvents() }
    func selectEvent(_ id: String?) { if let id { creatorOwner?.selectEvent(id) } }
    func refreshCreator() { creatorOwner?.refresh() }
    func checkCreatorStatus() { creatorOwner?.status() }
    func retryCreatorAction() { creatorOwner?.retryPending() }
    func endBroadcast() { creatorOwner?.end() }
    func acknowledgeRecovery() { creatorOwner?.acknowledgeRecovery() }
    func prepareReplacement() { updateOrientation(); creatorOwner?.prepare(replacingAcknowledged: true) }
    func updateOrientation() {
        if let orientation = CaptureOrientation.device(UIDevice.current.orientation) {
            hasOrientation = true; engine.setOrientation(orientation)
        } else if !hasOrientation, let orientation = UIApplication.shared.connectedScenes
            .compactMap({ ($0 as? UIWindowScene)?.interfaceOrientation }).compactMap(CaptureOrientation.interface).first {
            hasOrientation = true; engine.setOrientation(orientation)
        }
        // Face-up/down and unknown readings retain the last valid orientation.
    }
    func start() {
        if destinationMode == .v4v { updateOrientation(); creatorOwner?.start(); return }
        guard let preparation = destinationSelection.preparation else { return }
        updateOrientation(); engine.start(preparation: preparation)
    }
    func stop() {
        destinationSelection.stop()
        if destinationMode == .v4v, let creatorOwner { creatorOwner.stopCapture() }
        else { engine.stop() }
    }
    func prepare() {
        if destinationMode == .v4v { updateOrientation(); creatorOwner?.prepare(); return }
        guard let ticket = engine.reservePreparation() else { return }
        guard destinationSelection.reserve(ticket) else { engine.abandonPreparation(ticket); return }
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
            if let sink, !destinationSelection.installManual(ticket, end: { sink.endDestination() }) {
                engine.cancelPendingPreparation(ticket)
                engine.setDestination(sink, preparation: ticket) { _ in }
                return
            }
            engine.setDestination(sink, preparation: ticket) { [weak engine] accepted in
                if accepted { engine?.prepare(ticket) }
            }
        } catch {
            engine.abandonPreparation(ticket)
            destinationSelection.forgetFailedReservation(ticket)
            destinationError = error.localizedDescription
        }
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
            profileName = profile.name; destinationMode = .manual; destinationError = nil
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
    @ObservedObject var model: BroadcasterModel
    @Environment(\.scenePhase) private var scenePhase
    @State private var confirmEnd = false
    @State private var confirmRecovery = false
    private var controlsLocked: Bool { model.captureBusy || model.snapshot.canStart }
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
        .onAppear {
            UIDevice.current.beginGeneratingDeviceOrientationNotifications(); model.updateOrientation()
            model.setForeground(scenePhase == .active)
        }
        .onDisappear { model.setForeground(false); UIDevice.current.endGeneratingDeviceOrientationNotifications() }
        .onChange(of: scenePhase) { phase in
            if phase == .active { model.setForeground(true) }
            else if phase == .background { model.setForeground(false) }
            // A camera permission prompt may make the scene inactive briefly.
        }
        .onReceive(NotificationCenter.default.publisher(for: UIDevice.orientationDidChangeNotification)) { _ in model.updateOrientation() }
        .confirmationDialog("End this V4V broadcast?", isPresented: $confirmEnd, titleVisibility: .visible) {
            Button("End broadcast", role: .destructive) { model.endBroadcast() }
        } message: {
            Text("Publishing will end. A local recording already in progress will continue until you choose Stop and save.")
        }
        .confirmationDialog("Acknowledge the incomplete replay?", isPresented: $confirmRecovery, titleVisibility: .visible) {
            Button("Acknowledge incomplete replay", role: .destructive) { model.acknowledgeRecovery() }
        } message: {
            Text("The service could not confirm a complete replay. Acknowledge this before preparing a replacement broadcast.")
        }
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
                destinationForm
                if model.destinationMode == .manual, let error = model.destinationError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
                HStack {
                    Button("Prepare") { model.prepare() }.disabled(!model.canPrepare).accessibilityIdentifier("capture.prepare")
                    Button(model.recordingOnly ? "Start recording" : "Start broadcast") { model.start() }.disabled(!model.canStart).accessibilityIdentifier("capture.start")
                }.buttonStyle(.borderedProminent)
                Button("Stop and save") { model.stop() }
                    .buttonStyle(.bordered).disabled(!model.snapshot.running && !model.snapshot.canStart && model.snapshot.phase != "Finalising" && model.snapshot.phase != "Preparing").accessibilityIdentifier("capture.stop")
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
        Picker("Destination", selection: $model.destinationMode) {
            ForEach(BroadcasterDestinationMode.allCases, id: \.self) { Text($0.rawValue).tag($0) }
        }.disabled(controlsLocked).accessibilityIdentifier("destination.mode")
        if model.destinationMode == .v4v { creatorForm }
        if model.destinationMode == .manual { manualForm.disabled(controlsLocked) }
    }
    @ViewBuilder private var creatorForm: some View {
        Text("Streaming to V4V is always free.").font(.caption.bold())
        if model.creatorConfiguration.profile == nil {
            Text(model.creatorConfiguration.message).font(.callout).foregroundStyle(.secondary)
        } else if let error = model.creatorSetupError {
            Text(error).font(.callout).foregroundStyle(.red)
        } else {
            Text(model.creator.account).font(.headline)
            if let persona = model.creator.publicPersona {
                Text(String(persona.prefix(12)) + "…").font(.caption.monospaced()).privacySensitive()
            }
            Text(model.creator.message).font(.callout)
            Text("Session: \(model.creator.phase)").font(.caption).foregroundStyle(.secondary)
            if model.creator.phase == "ended" {
                Text(model.creator.endComplete ? "End confirmed" : "End unconfirmed; check the session or retry the saved action.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if model.creator.pending != nil {
                Text("Confirming a saved action. Retry uses that same action.").font(.caption).foregroundStyle(.secondary)
            }
            if let error = model.creator.error { Text(error).font(.caption).foregroundStyle(.red) }
            if model.creator.signingIn {
                ProgressView("Waiting for sign-in")
                Button("Cancel sign-in") { model.cancelSignIn() }.disabled(!model.creator.canCancelSignIn)
            } else {
                SecureField("Remote signer bunker URI", text: $model.bunkerURI)
                    .textInputAutocapitalization(.never).autocorrectionDisabled().textFieldStyle(.roundedBorder)
                    .privacySensitive().disabled(!model.creator.canSignIn).accessibilityIdentifier("v4v.bunker-uri")
                HStack {
                    Button("Connect signer") { model.signIn(restoring: false) }
                        .disabled(!model.creator.canSignIn || model.bunkerURI.isEmpty).accessibilityIdentifier("v4v.sign-in")
                    Button("Use saved signer") { model.signIn(restoring: true) }
                        .disabled(!model.creator.canSignIn).accessibilityIdentifier("v4v.sign-in-saved")
                }.buttonStyle(.bordered)
            }
            HStack {
                Button("Load events") { model.reloadEvents() }.disabled(!model.creator.canReloadEvents)
                Button("Check session") { model.checkCreatorStatus() }.disabled(!model.creator.canStatus)
            }.buttonStyle(.bordered)
            if !model.creator.events.isEmpty {
                Picker("Event", selection: Binding(get: { model.creator.selectedEvent }, set: { model.selectEvent($0) })) {
                    if model.creator.selectedEvent == nil { Text("Choose an event").tag(Optional<String>.none) }
                    ForEach(model.creator.events, id: \.id) { event in
                        Text(event.title.flatMap { $0.isEmpty ? nil : $0 } ?? event.id).tag(Optional(event.id))
                    }
                }.disabled(!model.creator.canSelectEvent).accessibilityIdentifier("v4v.event")
            }
            HStack {
                Button("Refresh session") { model.refreshCreator() }.disabled(!model.creator.canRefresh)
                Button("Retry saved action") { model.retryCreatorAction() }.disabled(!model.creator.canRetry)
            }.buttonStyle(.bordered)
            Button("End broadcast", role: .destructive) { confirmEnd = true }
                .disabled(!model.creator.canEnd).accessibilityIdentifier("v4v.end")
            if model.creator.canAcknowledge {
                Button("Acknowledge incomplete replay", role: .destructive) { confirmRecovery = true }
                    .accessibilityIdentifier("v4v.acknowledge-recovery")
            }
            if model.creator.canReplace {
                Button("Prepare replacement broadcast") { model.prepareReplacement() }
                    .buttonStyle(.bordered).accessibilityIdentifier("v4v.prepare-replacement")
            }
            Text("Sign-in and event selection do not start the camera. Choose Prepare, then Start broadcast when ready.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
    private var manualForm: some View {
        Group {
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
