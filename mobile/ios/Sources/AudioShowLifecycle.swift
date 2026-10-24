import Foundation
import Darwin

/// Authority time includes suspended/device-sleep time. This clock must never
/// timestamp PCM, encoded samples, media ageing or socket-rate measurements.
enum AudioShowAuthorityClock {
    private static let timebase: mach_timebase_info_data_t = {
        var value = mach_timebase_info_data_t()
        precondition(mach_timebase_info(&value) == KERN_SUCCESS && value.denom != 0)
        return value
    }()
    static func nowNs() -> UInt64 {
        let ticks = mach_continuous_time(), numerator = UInt64(timebase.numer), denominator = UInt64(timebase.denom)
        let whole = (ticks / denominator).multipliedReportingOverflow(by: numerator)
        let fraction = (ticks % denominator) * numerator / denominator
        let result = whole.partialValue.addingReportingOverflow(fraction)
        return whole.overflow || result.overflow ? UInt64.max : result.partialValue
    }
}

/// Evaluated inside CaptureAdmission's existing scalar lock, at the same
/// boundary as Start/Stop. Visibility is never a replacement capture authority.
enum AudioShowLifecycle {
    enum Interruption: Equatable { case audioBegan, audioEnded, mediaServicesReset, captureFailed }
    static func mayContinue(hasVideo: Bool, optedIn: Bool, committedRunning: Bool) -> Bool {
        optedIn && !hasVideo && committedRunning
    }
    /// Dispatch timers use uptime; authority deadlines may include suspension.
    /// Schedule a relative wake, then the actual owner rechecks authority time.
    static func remaining(deadline: UInt64, now: UInt64) -> UInt64 { deadline > now ? deadline - now : 0 }
    static func sceneVisibility(active: Bool, background: Bool) -> Bool? {
        active ? true : background ? false : nil
    }
}

/// One cold-created V4V authority, shared by all attempts of that exact show.
/// No payload/queue/timer is retained. A check authorises the immediate owned
/// submission only; bytes already handed to Network.framework cannot be recalled.
final class AudioShowOutputAuthority {
    private let check: () -> Bool
    // Installed once, before a transport can be constructed or started.
    var denied: () -> Void = {}
    init(check: @escaping () -> Bool) { self.check = check }
    func permits() -> Bool {
        guard check() else { denied(); return false }; return true
    }
    @discardableResult func submit(_ work: () -> Void) -> Bool {
        guard permits() else { return false }; work(); return true
    }
}
