package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

internal class V4vCaptureAdmissionTest {
    @Test fun queuedGrantReplyAfterEndCannotAdoptAPublisher() {
        val policy = V4vCaptureAdmission()
        val queuedAnswer = policy.snapshot()
        policy.invalidate() // synchronous End before queued UI reply
        var publishers = 0
        assertThrows(IllegalStateException::class.java) { policy.consume(queuedAnswer); publishers++ }
        assertEquals(0, publishers)
    }
    @Test fun duplicateAnswerCannotCreateSecondOwnerButExplicitFreshPrepareCan() {
        val policy = V4vCaptureAdmission(); val first = policy.snapshot()
        policy.consume(first)
        assertThrows(IllegalStateException::class.java) { policy.consume(first) }
        val next = policy.snapshot(); assertTrue(policy.current(next)); policy.consume(next)
    }
    @Test fun stopExpiryOrBackgroundInvalidationFencesPendingStartPermission() {
        val policy = V4vCaptureAdmission()
        repeat(3) {
            val permit = policy.snapshot(); assertTrue(policy.current(permit))
            policy.invalidate(); assertFalse(policy.current(permit))
        }
    }
    @Test fun lateStartForPreparationACannotStartPreparationBAfterStop() {
        val policy = V4vCaptureAdmission(); val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        policy.consume(policy.snapshot()); val a = policy.bind(1, identity)
        val lateApproval = policy.snapshot()
        policy.invalidate(); policy.consume(policy.snapshot()); val b = policy.bind(2, identity)
        assertFalse(policy.canStart(a, lateApproval, identity))
        assertFalse(policy.canStart(b, lateApproval, identity))
        assertTrue(policy.canStart(b, policy.snapshot(), identity.copy(revision = 2)))
    }
    @Test fun replacementSessionCannotAuthorisePreviouslyPreparedCredentials() {
        val policy = V4vCaptureAdmission(); val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 2)
        policy.consume(policy.snapshot()); val prepared = policy.bind(1, identity)
        assertFalse(policy.canStart(prepared, policy.snapshot(), identity.copy(sessionId = "b".repeat(32), generation = 2)))
        assertFalse(policy.canStart(prepared, policy.snapshot(), identity.copy(revision = 1)))
    }
    @Test fun refreshedApprovalRequiresSameImmutableConfigurationAndLiveExpiry() {
        val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
        fun grant(revision: Long, pass: String = "synthetic", cap: Long = 4_000_000, expires: Long = 100) =
            V4vGrant(identity.copy(revision = revision), "srt://192.0.2.1:8890", "synthetic-stream", pass, 1000, 500, 90000, cap, 524288, expires)
        val prepared = grant(1)
        assertTrue(grant(2).permitsStartOf(prepared, 90))
        assertFalse(grant(2, pass = "changed").permitsStartOf(prepared, 90))
        assertFalse(grant(2, cap = 5_000_000).permitsStartOf(prepared, 90))
        assertFalse(grant(2, expires = 90).permitsStartOf(prepared, 90))
    }
}
