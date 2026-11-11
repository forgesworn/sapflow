enum SrtPublishingLimitFailure: Error { case invalid }

/// Mobile recovery trades a longer receive buffer for more time to repair loss.
/// Packet retention follows the public SRT draft section 4.6 recommendation,
/// bounded independently of any larger latency requested by the peer.
struct SrtRecoveryPolicy: Equatable {
    let requestedLatencyMs: UInt32
    var maxPacketAgeMs: UInt32 { max(1000, (requestedLatencyMs * 5 + 3) / 4) }
    static let mobile = SrtRecoveryPolicy(validatedLatencyMs: 1000)

    init(requestedLatencyMs: UInt32) throws {
        guard (1...1000).contains(requestedLatencyMs) else { throw SrtPublishingLimitFailure.invalid }
        self.init(validatedLatencyMs: requestedLatencyMs)
    }
    private init(validatedLatencyMs: UInt32) { requestedLatencyMs = validatedLatencyMs }
}

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
