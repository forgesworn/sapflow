package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class KeyframeRecoveryTest {
    @Test fun startupDoesNotRequestAndOverflowRequestsOnlyOncePerTransition() {
        val control = KeyframeRecovery().apply { begin(1) }
        assertFalse(control.observe(1, true, 0uL, 0))
        assertFalse(control.observe(1, false, 0uL, 1))
        assertTrue(control.observe(1, true, 1uL, 2))
        assertFalse(control.observe(1, true, 1uL, 3))
        assertTrue(control.take(1))
        assertFalse(control.take(1))
    }
    @Test fun staleGenerationAndRapidResetCannotRequestAnotherFrame() {
        val control = KeyframeRecovery().apply { begin(4) }
        control.observe(4, false, 0uL, 0)
        assertTrue(control.observe(4, true, 1uL, 1))
        assertFalse(control.take(3))
        assertTrue(control.take(4))
        control.observe(4, false, 1uL, 2)
        assertFalse(control.observe(4, true, 2uL, 499_999_999))
        control.observe(4, false, 2uL, 500_000_000)
        assertTrue(control.observe(4, true, 3uL, 500_000_001))
        control.begin(5)
        assertFalse(control.take(4))
        assertFalse(control.take(5))
        assertFalse(control.observe(4, true, 4uL, 1_000_000_000))
    }
}
