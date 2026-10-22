import Foundation

/// Cold command admission, independent of camera/media ownership. A successful
/// native cleanup receipt is still required before releasing a stopped intent.
/// No callback, codec, socket or file operation runs under this scalar lock.
final class CaptureAdmission {
    struct Preparation: Equatable {
        fileprivate let serial: UInt64
        let profile: CaptureIntent
        let audioInputID: String?
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
            guard phase == .idle, let serial = next() else { return nil }
            let ticket = Preparation(serial: serial, profile: profile, audioInputID: selectedAudioInputID)
            preparation = ticket; candidatePending = true; hasCommittedRun = false; phase = .reserved
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
            guard preparation == ticket, phase == .prepared, let serial = next() else { return nil }
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
            guard preparation == ticket, ticket.profile.hasVideo else { return nil }
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
