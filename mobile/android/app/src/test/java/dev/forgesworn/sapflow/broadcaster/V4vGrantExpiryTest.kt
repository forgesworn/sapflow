package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

internal class V4vGrantExpiryTest {
    private val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
    @Test fun stalledControlOwnerCannotDelayTheIndependentExpiryFence() {
        var now = 0L; val policy = V4vGrantExpiry { now }
        val ticket = policy.begin(7, identity, 100)
        // Simulate HTTP/FS still blocked on its entirely separate owner.
        now = 100
        assertEquals(7L, policy.expired(ticket)); assertNull(policy.expired(ticket))
        assertNull(policy.refresh(identity.copy(revision = 2), 200))
    }
    @Test fun committedRefreshSupersedesOldAlarmWithoutRestartingDestination() {
        var now = 0L; val policy = V4vGrantExpiry { now }
        val old = policy.begin(7, identity, 100)
        now = 90; val fresh = policy.refresh(identity.copy(revision = 2), 200)!!
        now = 100; assertNull(policy.expired(old)); assertNull(policy.expired(fresh))
        now = 200; assertEquals(7L, policy.expired(fresh))
    }
    @Test fun oldDestinationAndLateCommitCannotStopOrReviveAnotherOwner() {
        var now = 0L; val policy = V4vGrantExpiry { now }
        val old = policy.begin(1, identity, 100)
        policy.clear(); val next = policy.begin(2, identity.copy(sessionId = "b".repeat(32), generation = 2), 300)
        now = 100; assertNull(policy.expired(old)); assertNull(policy.refresh(identity.copy(revision = 3), 400))
        now = 300; assertEquals(2L, policy.expired(next))
        assertNull(policy.refresh(identity.copy(revision = 4), 500))
    }
    @Test fun lateApprovalCannotStartExpiredOwnerWhenAlarmHasNotRun() {
        var now = 0L; val policy = V4vGrantExpiry { now }; val admission = V4vCaptureAdmission()
        policy.begin(7, identity, 100)
        val binding = admission.bind(7, identity)
        val approval = admission.snapshot()
        now = 101 // Do not deliver the independent alarm: simulate scheduler delay.
        val newer = identity.copy(revision = 2)
        assertNull(policy.refresh(newer, 200))
        assertTrue(admission.canStart(binding, approval, newer)) // Revision alone cannot authorise Start.
        assertFalse(policy.admits(binding.owner))
        assertEquals(7L, policy.expiredCurrent())
        admission.invalidate() // The platform fences admission on this exact synchronous result.
        assertFalse(admission.canStart(binding, approval, newer))
        assertNull(policy.expiredCurrent())
    }
    @Test fun timelyCommittedRefreshAllowsCurrentOwnerAfterOriginalDeadline() {
        var now = 0L; val policy = V4vGrantExpiry { now }; val admission = V4vCaptureAdmission()
        val original = policy.begin(7, identity, 100); val binding = admission.bind(7, identity)
        now = 90; val newer = identity.copy(revision = 2)
        assertNotNull(policy.refresh(newer, 200))
        now = 101
        assertNull(policy.expired(original)); assertNull(policy.expiredCurrent())
        assertTrue(policy.admits(binding.owner))
        assertTrue(admission.canStart(binding, admission.snapshot(), newer))
        assertFalse(policy.admits(8))
    }

}
