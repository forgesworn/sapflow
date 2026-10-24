import Foundation

/// Public site routing is admitted only by reviewed build settings expanded
/// into this app's Info.plist. No manual destination or received URL participates.
enum V4vCreatorConfiguration {
    enum UnavailableReason: String { case disabled, invalidApproval, incompleteProfile, invalidProfile, nativeSignerUnavailable }
    struct Snapshot: CustomStringConvertible, CustomDebugStringConvertible {
        let profile: V4vConnectorProfile?
        let unavailableReason: UnavailableReason?
        var message: String { profile == nil ? "V4V sign-in is unavailable in this build." : "V4V is configured." }
        var description: String { profile == nil ? "V4V creator configuration (unavailable)" : "V4V creator configuration (configured)" }
        var debugDescription: String { description }
        fileprivate init(profile: V4vConnectorProfile? = nil, reason: UnavailableReason? = nil) {
            self.profile = profile; unavailableReason = reason
        }
    }
    static func load(bundle: Bundle = .main) -> Snapshot {
        #if canImport(SignetAuth)
        let nativeSignerAvailable = true
        #else
        let nativeSignerAvailable = false
        #endif
        return evaluate(info: bundle.infoDictionary ?? [:], nativeSignerAvailable: nativeSignerAvailable)
    }
    /// This input seam permits deterministic contract tests. Production calls
    /// load(), never user defaults, a manual URL field or a remote document.
    static func evaluate(info: [String: Any], nativeSignerAvailable: Bool) -> Snapshot {
        guard let enabled = info["SapflowV4vEnabled"] as? String, ["YES", "NO"].contains(enabled) else {
            return Snapshot(reason: .invalidApproval)
        }
        guard let api = info["SapflowV4vApiOrigin"] as? String,
              let playback = info["SapflowV4vPlaybackOrigin"] as? String,
              let prefix = info["SapflowV4vPlaybackPrefix"] as? String else {
            return Snapshot(reason: .incompleteProfile)
        }
        if enabled == "NO" {
            // Partial/leftover qualification values are not a disabled default.
            guard api.isEmpty, playback.isEmpty, prefix.isEmpty else { return Snapshot(reason: .incompleteProfile) }
            return Snapshot(reason: .disabled)
        }
        guard !api.isEmpty, !playback.isEmpty, !prefix.isEmpty else { return Snapshot(reason: .incompleteProfile) }
        guard let profile = try? V4vConnectorProfile(apiOrigin: api, playbackOrigin: playback, playbackPrefix: prefix) else {
            return Snapshot(reason: .invalidProfile)
        }
        guard nativeSignerAvailable else { return Snapshot(reason: .nativeSignerUnavailable) }
        return Snapshot(profile: profile)
    }
}
