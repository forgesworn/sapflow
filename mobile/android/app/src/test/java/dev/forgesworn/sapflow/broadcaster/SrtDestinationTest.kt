package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class SrtDestinationTest {
    @Test fun numericUnicastIpv4AndIpv6KeepExplicitPort() {
        assertEquals("192.0.2.10:8890", SrtDestination.parse("srt://192.0.2.10:8890").nativeAddress)
        val ipv6 = SrtDestination.parse("srt://[2001:db8::1]:9999")
        assertEquals(9999, ipv6.port)
        assertTrue(ipv6.nativeAddress.startsWith("["))
        assertEquals(ipv6.address, ipv6.socketAddress.address)
    }
    @Test fun dnsMulticastWildcardBroadcastAndEmbeddedCredentialsAreRefused() {
        for (value in listOf("srt://origin.invalid:8890", "srt://0.0.0.0:8890", "srt://255.255.255.255:8890",
            "srt://224.0.0.1:8890", "srt://[ff02::1]:8890", "srt://192.0.2.1", "srt://192.0.2.1:0",
            "srt://192.0.2.1:65536", "srt://user@192.0.2.1:8890", "srt://192.0.2.1:8890?passphrase=hidden",
            "srt://192.0.2.1:8890/stream", "rtmp://192.0.2.1:8890", "srt://[fe80::1%25eth0]:8890")) {
            assertThrows(Exception::class.java) { SrtDestination.parse(value) }
        }
    }
    @Test fun encryptionIsDefaultAndPlaintextRequiresAnEmptySecret() {
        SrtDestination.validateCredentials("event", "placeholder-passphrase", false)
        SrtDestination.validateCredentials("", "", true)
        for (secret in listOf("", "short", "x".repeat(80), "é".repeat(40))) {
            assertThrows(IllegalArgumentException::class.java) { SrtDestination.validateCredentials("event", secret, false) }
        }
        assertThrows(IllegalArgumentException::class.java) { SrtDestination.validateCredentials("event", "placeholder-passphrase", true) }
        assertThrows(IllegalArgumentException::class.java) { SrtDestination.validateCredentials("é".repeat(257), "placeholder-passphrase", false) }
        assertThrows(IllegalArgumentException::class.java) { SrtDestination.validateCredentials("bad\u0000id", "placeholder-passphrase", false) }
    }
}
