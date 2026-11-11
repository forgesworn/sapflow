package dev.forgesworn.sapflow.broadcaster

/** Immutable mobile request, not a limit on the latency a peer can negotiate.
 * Retention follows the request only; a larger peer value never extends it. */
internal data class SrtRecoveryPolicy(val requestedLatencyMs: Int = 1000) {
    init { require(requestedLatencyMs in 1..1000) { "SRT requested latency must be 1..1000 ms" } }

    // Integer ceiling of 1.25 times the request, with a one-second floor.
    val maxPacketAgeMs: Int get() = maxOf(1000, (5 * requestedLatencyMs + 3) / 4)
}
