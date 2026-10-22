import Foundation

/// Cold command admission, independent of camera/media ownership. A successful
/// native cleanup receipt is still required before releasing a stopped intent.
/// No callback, codec, socket or file operation runs under this scalar lock.
final class CaptureAdmission {
    struct Preparation: Equatable {
        fileprivate let serial: UInt64
        let profile: CaptureIntent
        let audioInputID: String?
        let continuesAudioInBackground: Bool
        let orientationChoice: CaptureOrientationChoice
        let initialOrientation: CaptureOrientation?
        let initialCamera: CameraPosition
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
    enum Phase { case idle, reserved, attached, permissions, prepared, starting, running, rotating, stopping, retained }
    private let lock = NSLock()
    private var serial: UInt64
    private var selectedAudioInputID: String?
    private var selectedCamera = CameraPosition.rear
    private var camera = CameraControlPolicy<Preparation>()
    private var selectedBackgroundAudio = false
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
            guard foreground, phase == .prepared || phase == .running, let source = camera.source,
                  camera.permitsControls(source) else { return nil }; return source
        }
    }
    func permitsCameraControls(_ source: CameraSource) -> Bool {
        locked { foreground && (phase == .prepared || phase == .running) && preparation == source.owner && camera.permitsControls(source) }
    }
    func initialCameraSource(canvas: CameraCanvas) -> CameraSource? {
        locked {
            guard foreground, let preparation, preparation.profile.hasVideo,
                  [.permissions, .prepared, .starting].contains(phase) else { return nil }
            return camera.initial(preparation, canvas: canvas)
        }
    }
    func reserveCameraSwitch(_ position: CameraPosition, source: CameraSource, nowNs: UInt64) -> CameraSwitch? {
        locked {
            guard foreground, phase == .prepared || phase == .running, let preparation,
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
    /// nil Stop means this exact committed audio show may continue, or the
    /// foreground returned. Prepared/queued Start is synchronously invalidated.
    func visibilityChanged(foreground active: Bool, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds) -> (continuing: Preparation?, stop: Stop?) {
        locked {
            foreground = active
            if active { return (nil, nil) }
            if let preparation, AudioShowLifecycle.mayContinue(hasVideo: preparation.profile.hasVideo,
                optedIn: preparation.continuesAudioInBackground, committedRunning: phase == .running) { return (preparation, nil) }
            return (nil, phase == .idle ? nil : stopLocked(nowNs: nowNs))
        }
    }
    var backgroundContinuation: Preparation? {
        locked {
            guard let preparation, AudioShowLifecycle.mayContinue(hasVideo: preparation.profile.hasVideo,
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
            let ticket = Preparation(serial: serial, profile: profile, audioInputID: selectedAudioInputID,
                                     continuesAudioInBackground: selectedBackgroundAudio && !profile.hasVideo,
                                     orientationChoice: profile.hasVideo ? selectedOrientationChoice : .automatic,
                                     initialOrientation: profile.hasVideo ? (selectedOrientationChoice.fixedOrientation ?? latestAutoOrientation) : nil,
                                     initialCamera: selectedCamera)
            orientationObservation = nil
            preparation = ticket; candidatePending = true; hasCommittedRun = false; phase = .reserved
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
            guard preparation == ticket, phase == .prepared,
                  !ticket.profile.hasVideo || (camera.source == nil || (camera.confirmed && camera.pending == nil && !camera.failed)),
                  let serial = next() else { return nil }
            let value = Start(serial: serial, preparation: ticket)
            starting = value; phase = .starting; return value
        }
    }
    func permitsStart(_ ticket: Start) -> Bool {
        locked { preparation == ticket.preparation && starting == ticket && phase == .starting }
    }
    func started(_ ticket: Start, nowNs: UInt64 = DispatchTime.now().uptimeNanoseconds, broadcasting: Bool = true) -> Bool {
        locked {
            guard preparation == ticket.preparation, starting == ticket, phase == .starting else { return false }
            if !hasCommittedRun {
                captureBeganAtNs = nowNs
                if broadcasting && broadcastStoppedAtNs == nil { broadcastBeganAtNs = nowNs }
            }
            starting = nil; hasCommittedRun = true; phase = .running; return true
        }
    }
    func beginRotation(_ ticket: Preparation) -> Rotation? {
        locked {
            guard preparation == ticket, ticket.profile.hasVideo, ticket.orientationChoice == .automatic else { return nil }
            if phase == .rotating { return rotation }
            guard phase == .running, let serial = next() else { return nil }
            let value = Rotation(serial: serial, preparation: ticket)
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
        camera.close()
        return value
    }
    /// Cold destination End/expiry only. Never stops the independent capture or
    /// starts a clock. A retired ticket cannot alter a successor's elapsed time.
    func broadcastEnded(_ ticket: Preparation, nowNs: UInt64) {
        locked {
            guard preparation == ticket, timingPreparation == ticket else { return }
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
