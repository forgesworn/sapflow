package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.NativeSrtCipher
import dev.forgesworn.sapflow.NativeSrtEvent
import org.junit.Assert.*
import org.junit.Test

/** Dormant real sinks exercise configuration and native event handling without sockets/JNI. */
class SrtRecoveryPolicyTest {
    private fun sink(policy: SrtRecoveryPolicy = SrtRecoveryPolicy(),
        events: (PublishingAttemptEvent) -> Unit = {}) = SrtPublishingSink(
        SrtDestination.parse("srt://127.0.0.1:9000"), "publish:lab", "synthetic-passphrase", false,
        events = events, pacingBytesPerSecond = 325_000uL, recoveryPolicy = policy,
    )

    @Test fun requestedLatencyRejectsBothInvalidBoundariesWithoutClamping() {
        for (value in listOf(Int.MIN_VALUE, -1, 0, 1001, Int.MAX_VALUE)) {
            assertThrows(IllegalArgumentException::class.java) { SrtRecoveryPolicy(value) }
        }
        for (value in listOf(1, 120, 999, 1000)) assertEquals(value, SrtRecoveryPolicy(value).requestedLatencyMs)
        assertEquals(1000, SrtRecoveryPolicy().requestedLatencyMs)
    }

    @Test fun packetRetentionUsesCeilingWithOneSecondFloorAndBoundedMaximum() {
        val boundaries = mapOf(1 to 1000, 120 to 1000, 799 to 1000, 800 to 1000,
            801 to 1002, 802 to 1003, 803 to 1004, 804 to 1005, 999 to 1249, 1000 to 1250)
        boundaries.forEach { (request, age) -> assertEquals(age, SrtRecoveryPolicy(request).maxPacketAgeMs) }
        for (request in 1..1000) {
            val age = SrtRecoveryPolicy(request).maxPacketAgeMs
            assertTrue(age in 1000..1250)
            assertTrue(age * 4 >= request * 5)
            if (age > 1000) assertTrue((age - 1) * 4 < request * 5)
        }
    }

    @Test fun actualSinkConfigurationUsesRequestAndDerivedAgeWithoutChangingOtherBounds() {
        for (request in listOf(1, 120, 800, 801, 1000)) {
            val attempt = sink(SrtRecoveryPolicy(request))
            try {
                val config = attempt.nativeConfig()
                assertEquals(request.toUInt(), config.latencyMs)
                assertEquals(SrtRecoveryPolicy(request).maxPacketAgeMs.toUInt(), config.maxPacketAgeMs)
                assertEquals(1000u, config.maxSampleResidenceMs)
                assertEquals(128u, config.maxBufferedPackets)
                assertEquals(4u * 1024u * 1024u, config.maxBufferedBytes)
                assertEquals(325_000uL, config.bandwidthBytesPerSecond)
                assertEquals(NativeSrtCipher.AES256, config.cipher)
                assertEquals("publish:lab", config.streamId)
            } finally { attempt.close() }
        }
    }

    @Test fun diagnosticsRequireConnectedAndReportActualLargerPeerWithoutRetuningRetention() {
        val events = mutableListOf<PublishingAttemptEvent>()
        val attempt = sink(events = events::add)
        try {
            assertTrue(attempt.diagnostics.contains("latency requested 1000 ms · negotiated pending"))
            assertFalse(attempt.diagnostics.contains("negotiated 1000 ms"))
            val before = attempt.nativeConfig()
            attempt.connected(NativeSrtEvent.Connected(1500u, 1316u))
            assertTrue(attempt.diagnostics.contains("latency requested 1000 ms · negotiated 1500 ms"))
            assertEquals(listOf(PublishingAttemptEvent.Accepted), events)
            assertTrue(attempt.status.contains("media delivery unconfirmed"))
            assertEquals(before, attempt.nativeConfig())
            assertEquals(1250u, attempt.nativeConfig().maxPacketAgeMs)
            // The request cap must never become an implicit peer-negotiation cap.
            attempt.connected(NativeSrtEvent.Connected(65_535u, 1316u))
            assertTrue(attempt.diagnostics.contains("negotiated 65535 ms"))
            assertEquals(before, attempt.nativeConfig())
        } finally { attempt.close() }
    }

    @Test fun freshAttemptKeepsDefaultRequestAndDoesNotInheritEarlierNegotiation() {
        val first = sink()
        first.connected(NativeSrtEvent.Connected(1500u, 1316u))
        first.close()
        val replacement = sink()
        try {
            assertEquals(1000u, replacement.nativeConfig().latencyMs)
            assertEquals(1250u, replacement.nativeConfig().maxPacketAgeMs)
            assertTrue(replacement.diagnostics.contains("latency requested 1000 ms · negotiated pending"))
            replacement.connected(NativeSrtEvent.Connected(1000u, 1316u))
            assertTrue(replacement.diagnostics.contains("latency requested 1000 ms · negotiated 1000 ms"))
        } finally { replacement.close() }
    }
}
