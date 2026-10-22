package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioBroadcastWakeLeaseTest {
    private class Native : AudioBroadcastWakeLease.Platform {
        var active = false
        var failAcquire = false
        var failRelease = false
        var acquisitions = 0
        var releases = 0
        override fun held() = active
        override fun acquire(timeoutMs: Long) {
            assertEquals(30_000L, timeoutMs)
            acquisitions++
            if (failAcquire) throw SecurityException("Generated failure")
            active = true
        }
        override fun release() {
            releases++
            if (failRelease) throw IllegalStateException("Generated unknown closure")
            active = false
        }
    }
    private class Fixture {
        val native = Native()
        val driver = AudioBroadcastWakeLease<Any>(native)
        val a = Any()
        init { assertTrue(driver.bind(a)) }
        fun active(now: Long = 0, renew: Boolean = true) = driver.maintain(a, true, now, renew)
    }

    @Test fun preparationNeverAcquiresUntilCallerReportsCommittedEligibility() {
        val f = Fixture()
        repeat(100) { assertTrue(f.driver.maintain(f.a, false, it.toLong())) }
        assertEquals(0, f.native.acquisitions)
        assertFalse(f.native.active)
        assertTrue(f.active(100)); assertTrue(f.native.active)
    }
    @Test fun initialForegroundDepartureAcquiresButRepeatedDepartureCannotRenew() {
        val f = Fixture()
        assertTrue(f.active(0, renew = false))
        assertTrue(f.active(11_000_000_000, renew = false))
        assertEquals(1, f.native.acquisitions)
        assertTrue(f.active(11_000_000_000))
        assertEquals(2, f.native.acquisitions)
    }
    @Test fun oneColdCheckRenewsTimedLeaseWithoutPerMediaWork() {
        val f = Fixture(); assertTrue(f.active())
        repeat(9) { assertTrue(f.active((it + 1) * 1_000_000_000L)) }
        assertEquals(1, f.native.acquisitions)
        assertTrue(f.active(10_000_000_000)); assertEquals(2, f.native.acquisitions)
        assertTrue(f.active(39_999_999_999)); assertEquals(3, f.native.acquisitions)
    }
    @Test fun withheldCheckCannotRetrospectivelyRenewEvenIfNativeStillReportsHeld() {
        val f = Fixture(); assertTrue(f.active())
        assertFalse(f.active(30_000_000_000))
        assertFalse(f.native.active); assertEquals(1, f.native.acquisitions)
        assertFalse(f.active(30_000_000_001)); assertFalse(f.driver.bind(f.a))
    }
    @Test fun platformTimeoutOrEarlyLossIsTerminalAndDoesNotReacquire() {
        val f = Fixture(); assertTrue(f.active()); f.native.active = false
        assertFalse(f.active(1)); assertEquals(1, f.native.acquisitions)
        assertFalse(f.active(2))
    }
    @Test fun blockedSuccessfulRenewalCannotExtendOriginalExpiredLease() {
        val native = Native(); var completed = 0L
        val driver = AudioBroadcastWakeLease<Any>(native) { completed }
        val owner = Any(); check(driver.bind(owner))
        assertTrue(driver.maintain(owner, true, 0))
        completed = 30_000_000_000 // Native acquire returns only at original expiry.
        assertFalse(driver.maintain(owner, true, 10_000_000_000))
        assertFalse(native.active); assertFalse(driver.maintain(owner, true, completed))
    }
    @Test fun eligibilityLossReleasesDuringBlockedApplicationCleanup() {
        val f = Fixture(); assertTrue(f.active())
        assertFalse(f.driver.maintain(f.a, false, 1))
        assertFalse(f.native.active)
        assertFalse(f.driver.bind(f.a)); assertFalse(f.active(2))
        // Still bound to A until actual application cleanup retires it.
        assertFalse(f.driver.bind(Any())) // A must not be replaced before retire.
    }
    @Test fun exactStopIsStickyAndLateOldCommandsCannotAffectSuccessor() {
        val f = Fixture(); assertTrue(f.active()); assertTrue(f.driver.stop(f.a))
        assertFalse(f.active(1)); assertTrue(f.driver.retire(f.a))
        val b = Any(); assertTrue(f.driver.bind(b)); assertTrue(f.driver.maintain(b, true, 2))
        assertFalse(f.driver.stop(f.a)); assertFalse(f.driver.retire(f.a))
        assertFalse(f.driver.maintain(f.a, false, 3)); assertTrue(f.native.active)
    }
    @Test fun destructionAndRepeatedStopReleaseWithoutResurrection() {
        val f = Fixture(); assertTrue(f.active()); f.driver.destroy(); f.driver.destroy()
        assertFalse(f.native.active); assertFalse(f.active(1)); assertFalse(f.driver.bind(f.a))
    }
    @Test fun acquisitionFailureIsTerminalWithoutClaimingPower() {
        val f = Fixture(); f.native.failAcquire = true
        assertFalse(f.active()); assertFalse(f.native.active)
        f.native.failAcquire = false
        assertFalse(f.active(1)); assertEquals(1, f.native.acquisitions)
    }
    @Test fun uncertainNativeReleaseRetainsCapacityUntilActualClosure() {
        val f = Fixture(); assertTrue(f.active()); f.native.failRelease = true
        assertFalse(f.driver.stop(f.a)); assertFalse(f.driver.retire(f.a))
        val b = Any(); assertFalse(f.driver.bind(b)); assertTrue(f.native.active)
        f.native.active = false; f.native.failRelease = false // Actual timed release/closure.
        assertTrue(f.driver.retire(f.a)); assertTrue(f.driver.bind(b))
        assertTrue(f.driver.maintain(b, true, 1))
    }
    @Test fun negativeRollbackAndOverflowClocksNeverExtendAuthority() {
        for (invalid in listOf(-1L, Long.MAX_VALUE)) {
            val f = Fixture(); assertFalse(f.active(invalid)); assertFalse(f.native.active)
        }
        val f = Fixture(); assertTrue(f.active(10)); assertFalse(f.active(9))
        assertFalse(f.native.active); assertEquals(1, f.native.acquisitions)
    }
    @Test fun realStandbyPreparationNeedsPromotionAndCommittedStartThenStopReleasesBeforeCleanup() {
        val admission = CaptureStartAdmission()
        val prep = checkNotNull(admission.reserve(CaptureIntent.AudioOnly48kMono, backgroundAudio = true, initialStandby = true))
        check(admission.claim(prep)); check(admission.beginEpoch(prep, 1)); check(admission.prepared(prep, 1))
        val policy = BackgroundAudioPolicy()
        val owner = checkNotNull(policy.reserve(prep, "01".repeat(16)))
        val native = Native(); val wake = AudioBroadcastWakeLease<BackgroundAudioPolicy.Owner>(native)
        check(wake.bind(owner))
        assertTrue(wake.maintain(owner, admission.continuesInBackground(prep), 0))
        assertFalse(native.active); assertNull(admission.start(prep))
        check(admission.servicePromoted(prep)); check(policy.promote(owner))
        val start = checkNotNull(admission.start(prep))
        assertTrue(wake.maintain(owner, admission.continuesInBackground(prep), 1)); assertFalse(native.active)
        check(admission.commit(start, 2))
        assertTrue(wake.maintain(owner, admission.continuesInBackground(prep), 2)); assertTrue(native.active)
        check(admission.stop(prep, 3))
        assertFalse(wake.maintain(owner, admission.continuesInBackground(prep), 3))
        assertFalse(native.active); assertTrue(admission.owns(prep)) // Cleanup has not completed.
    }
    @Test fun realPolicyRejectsVideoAndDefaultOffWithoutWakeAcquisition() {
        val native = Native(); val wake = AudioBroadcastWakeLease<BackgroundAudioPolicy.Owner>(native)
        for (intent in listOf(CaptureIntent.DEFAULT, CaptureIntent.AudioOnly48kMono)) {
            val prep = checkNotNull(CaptureStartAdmission().reserve(intent))
            BackgroundAudioPolicy().reserve(prep, "02".repeat(16))?.let {
                check(wake.bind(it)); wake.maintain(it, true, 0)
            }
        }
        assertEquals(0, native.acquisitions)
    }
}
