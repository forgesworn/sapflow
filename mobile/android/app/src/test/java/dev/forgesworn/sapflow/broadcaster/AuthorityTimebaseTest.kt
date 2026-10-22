package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AuthorityTimebaseTest {
    @Test fun suspendedTimeExpiresGrantEvenWhenMediaClockAndTimerHaveNotAdvanced() {
        var boot = 10_000_000_000L
        var media = 10_000_000_000L
        val clock = AuthorityTimebase(100_000) { boot }
        val expiry = V4vGrantExpiry(clock::nowNs)
        val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        val first = expiry.begin(1, identity, boot + 90_000_000_000L)
        boot += 91_000_000_000L // No timer callback delivered during deep sleep.
        assertEquals(10_000_000_000L, media)
        assertEquals(191_000L, clock.nowMs())
        assertFalse(expiry.admits(1))
        assertNull(expiry.refresh(identity.copy(revision = 2), boot + 90_000_000_000L))
        assertEquals(1L, expiry.expired(first))
        media += 1_000 // Advancing a codec clock never revives grant authority.
        assertEquals(10_000_001_000L, media)
        assertFalse(expiry.admits(1))
    }

    @Test fun newerOwnerIsNotStoppedByPreSuspendOldAlarm() {
        var boot = 1L; val clock = AuthorityTimebase(1_000) { boot }
        val expiry = V4vGrantExpiry(clock::nowNs)
        val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        val old = expiry.begin(1, identity, 10)
        boot = 10; assertEquals(1L, expiry.expiredCurrent())
        val next = expiry.begin(2, identity.copy(revision = 2), 100)
        assertNull(expiry.expired(old)); assertTrue(expiry.admits(2))
        boot = 100; assertEquals(2L, expiry.expired(next))
    }

    @Test fun authorityClockCannotRollBackOrOverflowWallTime() {
        var boot = 10L; val clock = AuthorityTimebase(Long.MAX_VALUE) { boot }
        boot = 9
        assertThrows(IllegalArgumentException::class.java) { clock.nowNs() }
        boot = 1_000_010
        assertThrows(ArithmeticException::class.java) { clock.nowMs() }
    }
}
