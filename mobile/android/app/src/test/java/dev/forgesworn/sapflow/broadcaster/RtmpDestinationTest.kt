package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class RtmpDestinationTest {
    @Test fun secureDestinationRetainsExplicitApplicationAndPort() {
        val destination = RtmpDestination.parse("rtmps://origin.invalid:8443/live/nested", false)
        assertTrue(destination.tls)
        assertEquals("origin.invalid", destination.host)
        assertEquals(8443, destination.port)
        assertEquals("live/nested", destination.app)
        assertEquals(443, RtmpDestination.parse("rtmps://origin.invalid/live", false).port)
    }

    @Test fun plaintextRequiresAnExplicitDevelopmentGrantAndNeverFallsBack() {
        assertThrows(IllegalArgumentException::class.java) { RtmpDestination.parse("rtmp://origin.invalid/live", false) }
        val destination = RtmpDestination.parse("rtmp://origin.invalid/live", true)
        assertFalse(destination.tls)
        assertEquals(1935, destination.port)
        assertTrue(RtmpDestination.parse("rtmps://origin.invalid/live", true).tls)
    }

    @Test fun userinfoQueriesFragmentsMissingAppsAndInvalidPortsAreRefused() {
        for (url in listOf("https://origin.invalid/live", "rtmps://user:secret@origin.invalid/live", "rtmps://origin.invalid/live?token=placeholder",
            "rtmps://origin.invalid/live#fragment", "rtmps://origin.invalid", "rtmps://origin.invalid:65536/live")) {
            assertThrows(Exception::class.java) { RtmpDestination.parse(url, false) }
        }
        assertThrows(IllegalArgumentException::class.java) { RtmpDestination.parse("x".repeat(4097), false) }
    }

    @Test fun numericIpv6DestinationsDoNotIncludeUriBracketsInTlsIdentity() {
        val destination = RtmpDestination.parse("rtmps://[::1]:443/live", false)
        assertEquals("::1", destination.host)
        assertTrue(destination.tls)
    }
}
