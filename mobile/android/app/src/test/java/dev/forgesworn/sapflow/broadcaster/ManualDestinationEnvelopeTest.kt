package dev.forgesworn.sapflow.broadcaster

import java.io.File
import java.util.Base64
import java.util.Random
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ManualDestinationEnvelopeTest {
    private fun document(plaintext: Boolean = false) = """{"version":1,"kind":"sapflow-manual-destination","name":"Generated destination","transport":"rtmp","endpoint":"${if (plaintext) "rtmp" else "rtmps"}://stream.example.invalid/live","streamKey":"generated-private-key","streamID":"","passphrase":"","allowPlaintext":$plaintext}""".toByteArray()
    private fun link(bytes: ByteArray = document()) = ManualDestinationEnvelope.PREFIX + Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
    private fun rejected(text: String) {
        try { ManualDestinationEnvelope.decode(text); fail("Expected redacted refusal") }
        catch (error: ManualProfileFailure) { assertNull(error.cause); assertFalse(error.toString().contains("generated-private-key")) }
    }
    @Test fun sharedEnvelopeAndProfileCorpus() {
        val configured = System.getProperty("sapflow.fixtureRoot")?.let(::File)
        val roots = generateSequence(File(requireNotNull(System.getProperty("user.dir"))).absoluteFile) { it.parentFile }.take(8).toList()
        val file = configured?.resolve("tests/fixtures/manual-destination-link-v1.json")
            ?: roots.map { it.resolve("tests/fixtures/manual-destination-link-v1.json") }.first { it.isFile }
        assertTrue(file.length() in 1..1_048_576)
        val rows = V4vJson.parse(file.readText(), 1_048_576).getValue("cases").jsonArray
        assertTrue(rows.size >= 98)
        rows.forEach { row ->
            val value = row.jsonObject
            val accepted = runCatching {
                val bytes = ManualDestinationEnvelope.decode(value.getValue("text").jsonPrimitive.content)
                try { ManualDestinationProfile.parse(bytes) } finally { bytes.fill(0) }
            }.isSuccess
            assertEquals(value.getValue("id").jsonPrimitive.content, value.getValue("accepted").jsonPrimitive.boolean, accepted)
        }
    }
    @Test fun exactDecodedSizeAndNoncanonicalUnusedBits() {
        val exact = document() + ByteArray(16_384 - document().size) { 32 }
        assertArrayEquals(exact, ManualDestinationEnvelope.decode(link(exact)))
        rejected(link(exact + 32))
        rejected(ManualDestinationEnvelope.PREFIX + "Zh") // Same decoded byte as Zg, different unused bits.
        rejected(ManualDestinationEnvelope.PREFIX + "Zm9")
        rejected(ManualDestinationEnvelope.PREFIX + "A")
        assertArrayEquals(byteArrayOf(102), ManualDestinationEnvelope.decode(ManualDestinationEnvelope.PREFIX + "Zg"))
        rejected(link() + "=")
        rejected("x".repeat(24_577))
    }
    @Test fun alternateSyntaxCannotNormaliseIntoAuthority() {
        val text = link()
        for (candidate in listOf(text.replace("sapflow-manual", "SAPFLOW-MANUAL"), text.replace("import/v1", "import:80/v1"),
            text.replace("import/v1", "name@import/v1"), text.replace("/v1#", "/v1?#"), text + "#", text + "\n",
            text.replace("#", "#%"), " $text", text + "é", "rtmp://stream.example.invalid/live")) rejected(candidate)
    }
    @Test fun coldInvitationRequiresOriginalForegroundAndExplicitReview() {
        val inbox = ManualLinkInbox(); val client = Any(); val selected = requireNotNull(inbox.offer(client, link(), true))
        assertEquals("Manual destination invitation (redacted)", selected.toString())
        assertNull(inbox.consume(client, selected, true)) // Not foreground-admitted yet; consumes refusal.
        val next = requireNotNull(inbox.offer(client, link(document() + 32), true))
        assertSame(next, inbox.foreground(client, true))
        assertEquals(link(document() + 32), inbox.consume(client, next, true))
        assertNull(inbox.consume(client, next, true))
        // A separately delivered identical link is a new untrusted invitation,
        // never authority inherited from the consumed review.
        val reopened = requireNotNull(inbox.offer(client, link(document() + 32), true))
        assertNotSame(next, reopened); assertNull(inbox.consume(client, next, true))
        assertSame(reopened, inbox.foreground(client, true)); assertNotNull(inbox.consume(client, reopened, true))
    }
    @Test fun busyArrivalIsDiscardedRatherThanQueuedForLaterShow() {
        val inbox = ManualLinkInbox(); val client = Any()
        assertNull(inbox.offer(client, link(), false))
        assertNull(inbox.foreground(client, true))
        val selected = requireNotNull(inbox.offer(client, link(), true))
        assertNull(inbox.foreground(client, false))
        assertNull(inbox.foreground(client, true))
        assertNull(inbox.consume(client, selected, true))
    }
    @Test fun oneInvitationAndStaleActivityCannotRelabelSuccessor() {
        val inbox = ManualLinkInbox(); val a = Any(); val b = Any()
        val old = requireNotNull(inbox.offer(a, link(), true))
        assertNull(inbox.offer(b, link(document() + 32), true))
        assertNull(inbox.foreground(b, true)); assertNull(inbox.consume(b, old, true))
        inbox.invalidate(a)
        val next = requireNotNull(inbox.offer(b, link(document() + 32), true))
        inbox.invalidate(a)
        assertSame(next, inbox.foreground(b, true))
        assertNull(inbox.consume(a, old, true))
        assertNotNull(inbox.consume(b, next, true))
    }
    @Test fun backgroundEditOrPrepareCannotResurrectOriginalInvite() {
        for (reason in listOf("background", "edit", "prepare", "other import", "destroy")) {
            val inbox = ManualLinkInbox(); val client = Any()
            val selected = requireNotNull(inbox.offer(client, link(), true))
            assertSame(reason, selected, inbox.foreground(client, true))
            inbox.invalidate(client)
            assertNull(reason, inbox.foreground(client, true))
            assertNull(reason, inbox.consume(client, selected, true))
        }
    }
    @Test fun linkReviewStillRequiresIndependentExactPlaintextConsentAndApply() {
        val inbox = ManualLinkInbox(); val client = Any(); val actions = ManualProfileActions(); var fields = 0
        val invite = requireNotNull(inbox.offer(client, link(document(true)), true))
        assertNotNull(inbox.foreground(client, true))
        val received = requireNotNull(inbox.consume(client, invite, true))
        assertEquals(0, fields); assertNull(actions.review)
        val ticket = requireNotNull(actions.begin(true))
        val profile = ManualDestinationProfile.parse(ManualDestinationEnvelope.decode(received))
        assertTrue(actions.reviewed(ticket, profile, true))
        val review = requireNotNull(actions.review)
        assertFalse(actions.apply(review, true) { fields++ })
        assertTrue(actions.setConsent(review, true))
        assertFalse(actions.apply(review, false) { fields++ }) // Foreground/capture guard rechecked.
        assertTrue(actions.apply(review, true) { fields++ }); assertEquals(1, fields)
        assertFalse(actions.apply(review, true) { fields++ })
    }
    @Test fun actualUncertainIOAndOutstandingPickerBlockRecreatedClient() {
        val admission = ManualProfileIOAdmission(); val picker = ManualProfilePicker<String>(); val actions = ManualProfileActions()
        val a = Any(); val b = Any(); val ticket = requireNotNull(actions.begin(true))
        assertTrue(picker.begin(a, ticket)); assertTrue(picker.hasOutstanding)
        val inbox = ManualLinkInbox()
        assertNull(inbox.offer(b, link(), !admission.busy && !picker.hasOutstanding))
        picker.invalidate(a); picker.returned(b, "discard-only"); assertNotNull(picker.resume(b, false))
        assertFalse(picker.hasOutstanding)
        assertTrue(admission.reserve()); assertTrue(admission.busy)
        admission.finish(false) // Actual failed/unknown provider close retains global admission.
        assertNull(inbox.offer(b, link(), !admission.busy && !picker.hasOutstanding))
        admission.finish(true) // A later no-op must not manufacture real cleanup.
        assertTrue(admission.busy); assertFalse(admission.reserve())
        assertNull(inbox.foreground(b, true))
    }
    @Test fun newIntentPauseCannotReplaceReviewButIdleDeliveryCanReachResume() {
        val inbox = ManualLinkInbox(); val client = Any()
        val old = requireNotNull(inbox.offer(client, link(), true))
        assertNotNull(inbox.foreground(client, true)); assertNotNull(inbox.consume(client, old, true))
        inbox.pause(client, conflictingReview = true)
        assertNull(inbox.offer(client, link(document() + 32), true))
        assertNull(inbox.foreground(client, true))
        // A later idle native delivery can survive its ordinary pause/resume.
        inbox.pause(client, conflictingReview = false)
        val next = requireNotNull(inbox.offer(client, link(document() + 32), true))
        assertSame(next, inbox.foreground(client, true)); assertNotNull(inbox.consume(client, next, true))
    }
    @Test fun coldBackgroundBeforeFirstResumeCannotPromoteOrRearm() {
        val inbox = ManualLinkInbox(); val client = Any()
        val old = requireNotNull(inbox.offer(client, link(), true))
        inbox.pause(client, conflictingReview = false); inbox.invalidate(client)
        assertNull(inbox.foreground(client, true)); assertNull(inbox.consume(client, old, true))
        val reopened = requireNotNull(inbox.offer(client, link(), true))
        assertNotSame(old, reopened); assertNull(inbox.consume(client, old, true))
    }
    @Test fun stoppedIdleNativeDeliveryWaitsForResumeButRestoredLaunchIsRefused() {
        val inbox = ManualLinkInbox(); val client = Any(); val text = link()
        val old = requireNotNull(inbox.offer(client, text, true)); inbox.foreground(client, true)
        inbox.pause(client, conflictingReview = true); inbox.stopped(client)
        assertNull(inbox.offer(client, text, allowed = true, newDelivery = false))
        assertNull(inbox.foreground(client, true))
        // Real onNewIntent can run while the Activity is stopped, before onStart.
        val fresh = requireNotNull(inbox.offer(client, text, allowed = true, newDelivery = true))
        assertFalse(fresh.ready)
        assertNull(inbox.consume(client, old, true))
        assertSame(fresh, inbox.foreground(client, true)); assertNotNull(inbox.consume(client, fresh, true))
        inbox.stopped(client)
        assertNull(inbox.offer(client, text, allowed = false, newDelivery = true)) // Actual capture remains authoritative.
    }
    @Test fun identicalLinkAfterDismissAndEditNeedsFreshReviewAndPlaintextConsent() {
        val inbox = ManualLinkInbox(); val client = Any(); val actions = ManualProfileActions()
        val text = link(document(true))
        val old = requireNotNull(inbox.offer(client, text, true)); inbox.foreground(client, true)
        inbox.invalidate(client) // Dismiss or edit removes only the exact prior invitation.
        val next = requireNotNull(inbox.offer(client, text, true)); inbox.foreground(client, true)
        assertNull(inbox.consume(client, old, true))
        val bytes = ManualDestinationEnvelope.decode(requireNotNull(inbox.consume(client, next, true)))
        val profile = try { ManualDestinationProfile.parse(bytes) } finally { bytes.fill(0) }
        val ticket = requireNotNull(actions.begin(true)); assertTrue(actions.reviewed(ticket, profile, true))
        val review = requireNotNull(actions.review)
        assertFalse(actions.apply(review, true) { fail("A reopened link cannot inherit plaintext consent") })
        assertTrue(actions.setConsent(review, true)); assertTrue(actions.apply(review, true) {})
    }
    @Test fun boundedGeneratedMutationsOnlyReturnBytesOrStaticErrors() {
        val random = Random(0x5a9f10L); val text = link()
        repeat(2048) {
            val chars = text.toCharArray()
            repeat(1 + random.nextInt(4)) { chars[random.nextInt(chars.size)] = random.nextInt(256).toChar() }
            try { val bytes = ManualDestinationEnvelope.decode(String(chars)); assertTrue(bytes.size <= 16_384); bytes.fill(0) }
            catch (error: ManualProfileFailure) { assertNull(error.cause); assertFalse(error.toString().contains("generated-private-key")) }
        }
    }
}
