enum SrtPublishingLimitFailure: Error { case invalid }

/// Wire pacing is bytes per second; the scoped grant is bits per second.
/// A scoped destination can only reduce the existing manual transport caps.
struct SrtPublishingLimits: Equatable {
    let queueBytes: UInt32
    let bandwidthBytesPerSecond: UInt64
    private init(queueBytes: UInt32, bandwidthBytesPerSecond: UInt64) {
        self.queueBytes = queueBytes; self.bandwidthBytesPerSecond = bandwidthBytesPerSecond
    }
    static let manual = SrtPublishingLimits(queueBytes: 4 * 1024 * 1024, bandwidthBytesPerSecond: 1_250_000)
    static func scoped(maxQueueBytes: UInt64, maxPublishBps: UInt64, captureProfile: CaptureIntent = .defaultProfile) throws -> Self {
        try captureProfile.requireGrant(maxPublishBps: maxPublishBps)
        guard (1500...4_194_304).contains(maxQueueBytes) else { throw SrtPublishingLimitFailure.invalid }
        return Self(queueBytes: UInt32(maxQueueBytes), bandwidthBytesPerSecond: min(manual.bandwidthBytesPerSecond, maxPublishBps / 8))
    }
}
