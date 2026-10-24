import Foundation
import CoreMedia

/// Constant-space rational media cadence. A delayed owner may catch up only a
/// bounded number of real generated frames; it cannot hide a stall by rebasing.
struct CapturePrivacyCadence {
    struct Plan { let videoFrames: Range<Int64>; let audioFramesThrough: UInt64 }
    enum Failure: LocalizedError {
        case clock, overflow, stalled
        var errorDescription: String? { "Native privacy clock/cadence could not be maintained" }
    }
    private(set) var nextVideoFrame: Int64
    private var lastNow: UInt64?
    init(nextVideoFrame: Int64) throws {
        guard nextVideoFrame >= 0 else { throw Failure.clock }
        self.nextVideoFrame = nextVideoFrame
    }
    static func firstVideoFrame(now: CMTime, sealedEnd: CMTime?) throws -> Int64 {
        guard now.isNumeric, now >= .zero, sealedEnd.map({ $0.isNumeric && $0 >= .zero }) ?? true else { throw Failure.clock }
        let due = CMTimeConvertScale(now, timescale: 30, method: .roundTowardNegativeInfinity).value
        let boundary = sealedEnd.map { CMTimeConvertScale($0, timescale: 30, method: .roundTowardPositiveInfinity).value } ?? 0
        return max(due, boundary)
    }
    static func audioTargetIsDue(framesThrough: UInt64, nextInputTime: CMTime) throws -> Bool {
        guard framesThrough <= UInt64(Int64.max), nextInputTime.isNumeric, nextInputTime >= .zero else { throw Failure.clock }
        let firstDue = CMTimeConvertScale(nextInputTime, timescale: 48_000, method: .roundTowardPositiveInfinity)
        guard firstDue.isNumeric, firstDue.value >= 0 else { throw Failure.clock }
        return framesThrough >= UInt64(firstDue.value)
    }
    mutating func plan(elapsedNs: UInt64, video: Bool) throws -> Plan {
        guard lastNow.map({ elapsedNs >= $0 }) ?? true else { throw Failure.clock }
        let seconds = elapsedNs / 1_000_000_000
        let remainder = elapsedNs % 1_000_000_000
        guard seconds <= UInt64(Int64.max / 90_000) else { throw Failure.overflow }
        let audio = seconds * 48_000 + remainder * 48_000 / 1_000_000_000
        let due = video ? Int64(seconds * 30 + remainder * 30 / 1_000_000_000) + 1 : nextVideoFrame
        // An off-grid sealed source may put the first replacement frame in the
        // future. Waiting for it is not rollback; observed wall cadence remains
        // independently checked below.
        let end = max(nextVideoFrame, due)
        guard end - nextVideoFrame <= 4 else { throw Failure.stalled }
        // A timer executing again does not establish freshness after suspension.
        if let lastNow, elapsedNs - lastNow > 200_000_000 { throw Failure.stalled }
        let frames = nextVideoFrame..<end
        nextVideoFrame = end; lastNow = elapsedNs
        return Plan(videoFrames: frames, audioFramesThrough: audio)
    }
}

/// Declared native clock bridge. Capture PTS is converted from the session's
/// synchronizationClock, never numerically compared with uptime/authority ns.
struct CaptureMediaClock {
    let clock: CMClock
    let epoch: CMTime
    init(clock: CMClock, epoch: CMTime) throws {
        guard epoch.isNumeric else { throw CapturePrivacyCadence.Failure.clock }
        self.clock = clock; self.epoch = epoch
    }
    func time(_ pts: CMTime, from source: CMClock) throws -> CMTime {
        let value = CMTimeSubtract(CMSyncConvertTime(pts, from: source, to: clock), epoch)
        guard value.isNumeric, CMTimeCompare(value, .zero) >= 0 else { throw CapturePrivacyCadence.Failure.clock }
        return value
    }
    func now() throws -> CMTime { try time(CMClockGetTime(clock), from: clock) }
}

/// Bridge only from the ORIGINAL fresh source PTS. The residual is explicit and
/// bounded by half a source frame; the raw sample is never timestamp-rebased.
enum CaptureResumeBridge {
    struct Plan { let zeroFrames: UInt32; let residual: CMTime }
    static func plan(actualPTS: CMTime, nextInputTime: CMTime, sourceRate: UInt32) throws -> Plan? {
        guard actualPTS.isNumeric, nextInputTime.isNumeric, sourceRate > 0, sourceRate <= 192_000 else {
            throw CapturePrivacyCadence.Failure.clock
        }
        let delta = CMTimeSubtract(actualPTS, nextInputTime)
        if CMTimeCompare(delta, .zero) < 0 { return nil } // Old probe, not input.
        let frames = CMTimeConvertScale(delta, timescale: Int32(sourceRate), method: .roundHalfAwayFromZero)
        guard frames.isNumeric, frames.value >= 0, frames.value <= 4 * 4096 else { throw CapturePrivacyCadence.Failure.stalled }
        let residual = CMTimeSubtract(delta, CMTime(value: frames.value, timescale: Int32(sourceRate)))
        let magnitude = CMTimeAbsoluteValue(residual)
        guard CMTimeCompare(magnitude, CMTime(value: 1, timescale: Int32(sourceRate * 2))) <= 0 else {
            throw CapturePrivacyCadence.Failure.clock
        }
        return Plan(zeroFrames: UInt32(frames.value), residual: residual)
    }
}

/// One latest, unadmitted picture; PCM is never retained while waiting for it.
/// Only a current microphone callback can commit cutover. Replacement pauses in
/// stages so native source PTS can catch its real cursors without rebasing.
struct CaptureResumeCutover<Picture> {
    let hasVideo: Bool
    private(set) var sourceReady = false
    private var picture: (value: Picture, pts: CMTime)?
    init(hasVideo: Bool) { self.hasVideo = hasVideo }
    var generatesSlate: Bool { !sourceReady }
    var generatesZero: Bool { !sourceReady || hasVideo && picture == nil }
    mutating func sourceStarted() { sourceReady = true }
    mutating func offerPicture(_ value: Picture, pts: CMTime, cutoff: CMTime, sealedEnd: CMTime) -> Bool {
        guard sourceReady, pts.isNumeric, pts >= cutoff, pts >= sealedEnd,
              picture.map({ pts >= $0.pts }) ?? true else { return false }
        picture = (value, pts); return true
    }
    enum AudioDecision { case wait, cutover(Picture?) }
    mutating func currentAudio(pts: CMTime, cutoff: CMTime, nextInput: CMTime, sealedVideoEnd: CMTime) -> AudioDecision {
        guard sourceReady, pts.isNumeric, pts >= cutoff, pts >= nextInput else { return .wait }
        guard hasVideo else { return .cutover(nil) }
        guard let candidate = picture else { return .wait }
        // Never commit an old cached picture after its source stalls. This is
        // the existing 250ms encoded sample-duration ceiling in
        // crates/sapflow-client/src/mpegts.rs::MuxConfig::default, not a PTS shift.
        guard candidate.pts >= sealedVideoEnd,
              CMTimeAbsoluteValue(CMTimeSubtract(pts, candidate.pts)) <= CMTime(value: 1, timescale: 4) else {
            picture = nil; return .wait
        }
        return .cutover(candidate.value)
    }
}

/// close() linearises native route fault versus intentional retirement. A real
/// pre-close fault remains sticky even if its diagnostic callback is withheld.
enum CapturePrivacyRouteRetirement {
    static func close(_ route: AudioInputRouting, admission: CaptureAdmission,
                      token: CaptureAdmission.MediaToken?) -> Bool {
        let failed = route.close()
        if failed, let token { _ = admission.nativeFault(token) }
        return failed
    }
}

/// One samples-owner timer, independent of camera/microphone/UI callbacks.
/// DispatchSource coalesces wakeups; the rational planner bounds actual work.
final class CapturePrivacyDriver {
    private let timer: DispatchSourceTimer
    private var stopped = false
    init(queue: DispatchQueue, tick: @escaping () -> Void) {
        timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(10), leeway: .milliseconds(1))
        timer.setEventHandler(handler: tick)
        timer.resume()
    }
    func stop() {
        guard !stopped else { return }; stopped = true
        timer.setEventHandler {}; timer.cancel()
    }
    deinit { timer.cancel() }
}
