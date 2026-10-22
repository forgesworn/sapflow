package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class ManualQrPolicyTest {
    private val begin = 1_000L
    private fun open() = ManualQrPolicy(begin).also { assertTrue(it.claimOpen(begin)) }
    private fun validResult(policy: ManualQrPolicy) {
        assertTrue(policy.beginDecode(begin)); assertTrue(policy.accept("generated-untrusted-candidate", begin))
    }
    @Test fun permissionOnlyCancellationNeverStartsNativeOrRearms() {
        val p = ManualQrPolicy(begin); p.cancel()
        assertFalse(p.claimOpen(begin)); assertFalse(p.beginDecode(begin))
        assertNull(p.finish(begin)); p.presentationClosed()
        assertNull(requireNotNull(p.finish(begin)).textForDelivery(begin)); assertNull(p.finish(begin))
    }
    @Test fun resultRequiresActualNativeDecoderAndPresentationReceipts() {
        val p = open(); validResult(p)
        assertNull(p.finish(begin)); p.nativeClosed(true); assertNull(p.finish(begin))
        p.presentationClosed(); assertNull(p.finish(begin)) // Actual decoder still owns its array.
        p.decodeReturned(); assertEquals("generated-untrusted-candidate", requireNotNull(p.finish(begin)).textForDelivery(begin))
        assertNull(p.finish(begin)); assertFalse(p.claimOpen(begin))
    }
    @Test fun cancellationAfterCandidateWhileNativeCloseHeldDiscardsIt() {
        val p = open(); validResult(p); p.decodeReturned(); p.cancel(); p.presentationClosed()
        assertNull(p.finish(begin)); assertFalse(p.nativeAndDecoderClosed)
        p.nativeClosed(true); assertNull(requireNotNull(p.finish(begin)).textForDelivery(begin))
    }
    @Test fun originalDeadlineCannotBeExtendedByFirstFrameOrWithheldTimer() {
        val p = open(); assertEquals(begin + 60_000_000_000L, p.deadlineNs)
        assertFalse(p.beginDecode(p.deadlineNs)); assertTrue(p.stopped)
        assertFalse(p.claimOpen(p.deadlineNs)); p.presentationClosed()
        assertNull(p.finish(p.deadlineNs)); p.nativeClosed(true)
        assertNull(requireNotNull(p.finish(p.deadlineNs)).textForDelivery(begin))
    }
    @Test fun successfulResultHeldBeyondOriginalDeadlineHasNoAuthority() {
        val p = open(); validResult(p); p.decodeReturned(); p.nativeClosed(true); p.presentationClosed()
        assertNull(requireNotNull(p.finish(p.deadlineNs)).textForDelivery(begin))
    }
    @Test fun unknownCleanupIsNotReleasedByLaterNoOpOrTime() {
        val p = open(); validResult(p); p.decodeReturned(); p.nativeClosed(false); p.presentationClosed()
        assertTrue(p.cleanupUncertain); assertFalse(p.nativeAndDecoderClosed)
        p.nativeClosed(true); assertNull(p.finish(p.deadlineNs + 1)); assertTrue(p.cleanupUncertain)
    }
    @Test fun decoderAndRateAreCapacityOneAndSingleCandidate() {
        val p = open(); assertTrue(p.beginDecode(begin)); assertFalse(p.beginDecode(begin))
        p.decodeReturned(); assertFalse(p.beginDecode(begin + 199_999_999))
        assertTrue(p.beginDecode(begin + 200_000_000))
        assertFalse(p.accept("x".repeat(2049), begin + 200_000_000)); assertFalse(p.accept("é", begin + 200_000_000))
        assertTrue(p.accept("valid-synthetic-candidate", begin + 200_000_000)); assertFalse(p.accept("second", begin + 200_000_000))
    }
    @Test fun negativeOverflowAndRollbackClocksFailClosed() {
        for (start in listOf(-1L, Long.MAX_VALUE)) assertFalse(ManualQrPolicy(start).claimOpen(start))
        val p = open(); assertFalse(p.beginDecode(begin - 1)); assertFalse(p.beginDecode(begin)); assertTrue(p.stopped)
    }
    @Test fun realPermissionReplyCannotBeLentAcrossRecreationOrReplacement() {
        val replies = ManualQrPermissionReplies<Any>(); val old = Any(); val next = Any()
        assertTrue(replies.reserve(old)); replies.restoreDiscard(); assertTrue(replies.outstanding)
        assertFalse(replies.reserve(next)); assertSame(old, replies.receive())
        assertFalse(replies.outstanding); assertTrue(replies.reserve(next)); assertSame(next, replies.receive())
    }
    @Test fun restoredMarkerHasNoApprovalAndBlocksUntilActualUntaggedReply() {
        val replies = ManualQrPermissionReplies<Any>(); replies.restoreDiscard()
        assertTrue(replies.outstanding); assertNull(replies.pending()); assertFalse(replies.reserve(Any()))
        assertNull(replies.receive()); assertFalse(replies.outstanding); assertTrue(replies.reserve(Any()))
    }
    @Test fun lateOldCallbacksCannotReleaseFreshScannerOrCapture() {
        val gate = CaptureStartAdmission(); val a = requireNotNull(gate.reserveScanner(Any()))
        val old = open(); old.cancel(); old.presentationClosed(); old.nativeClosed(true)
        assertNotNull(old.finish(begin)); assertTrue(gate.scannerClosed(a, true))
        val b = requireNotNull(gate.reserveScanner(Any()))
        assertFalse(gate.scannerClosed(a, true)); assertTrue(gate.ownsScanner(b)); assertNull(gate.reserve())
        assertFalse(gate.scannerClosed(b, false)); assertNull(gate.reserve())
        assertTrue(gate.scannerClosed(b, true)); assertNotNull(gate.reserve())
    }
    @Test fun ordinaryCaptureStopDoesNotManufactureScannerClose() {
        val gate = CaptureStartAdmission(); val scan = requireNotNull(gate.reserveScanner(Any()))
        assertNull(gate.stop()); assertFalse(gate.idle); assertTrue(gate.ownsScanner(scan))
        assertFalse(gate.scannerClosed(scan, false)); assertFalse(gate.idle)
    }

    @Test fun nativeCompletionBeforeDeadlineCannotOfferAfterWithheldUiRelay() {
        val p = open(); validResult(p); p.decodeReturned(); p.nativeClosed(true); p.presentationClosed()
        val reply = requireNotNull(p.finish(p.deadlineNs - 1))
        assertEquals("generated-untrusted-candidate", reply.textForDelivery(p.deadlineNs - 1))
        assertNull(reply.textForDelivery(p.deadlineNs))
        assertNull(reply.textForDelivery(p.deadlineNs + 1))
        assertEquals("QR scan reply (redacted)", reply.toString())
    }
    @Test fun actualUiDeliveryRefusesClockBeforeOriginalBeginAndCannotReviveRejectedCompletion() {
        val p = open(); validResult(p); p.decodeReturned(); p.nativeClosed(true); p.presentationClosed()
        val reply = requireNotNull(p.finish(begin))
        assertNull(reply.textForDelivery(begin - 1))
        assertEquals("generated-untrusted-candidate", reply.textForDelivery(begin))
        val expired = open(); validResult(expired); expired.decodeReturned(); expired.nativeClosed(true); expired.presentationClosed()
        val rejected = requireNotNull(expired.finish(expired.deadlineNs))
        assertNull(rejected.textForDelivery(begin)) // Native expiry never revives via UI clock.
    }
}
