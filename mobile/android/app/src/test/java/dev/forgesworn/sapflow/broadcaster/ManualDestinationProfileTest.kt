package dev.forgesworn.sapflow.broadcaster

import java.io.ByteArrayInputStream
import java.io.File
import java.util.Random
import javax.crypto.spec.SecretKeySpec
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ManualDestinationProfileTest {
    private fun json(endpoint: String = "rtmps://stream.example.invalid/live", plaintext: Boolean = false): ByteArray = buildJsonObject {
        put("version", 1); put("kind", "sapflow-manual-destination"); put("name", "Generated destination"); put("transport", "rtmp")
        put("endpoint", endpoint); put("streamKey", "generated-private-key"); put("streamID", ""); put("passphrase", ""); put("allowPlaintext", plaintext)
    }.toString().toByteArray()
    private fun profile(plaintext: Boolean = false) = ManualDestinationProfile.parse(json(if (plaintext) "rtmp://stream.example.invalid/live" else "rtmps://stream.example.invalid/live", plaintext))
    private fun failure(action: () -> Unit) {
        try { action(); fail("Expected a redacted rejection") } catch (error: ManualProfileFailure) {
            assertEquals("Manual destination action could not be confirmed. Check saved profiles before retrying.", error.message)
            assertNull(error.cause)
        }
    }
    @Test fun sharedGeneratedContractCorpus() {
        val configured = System.getProperty("sapflow.fixtureRoot")?.let(::File)
        val roots = generateSequence(File(requireNotNull(System.getProperty("user.dir"))).absoluteFile) { it.parentFile }.take(8).toList()
        val file = configured?.resolve("tests/fixtures/manual-destination-v1.json")
            ?: roots.map { it.resolve("tests/fixtures/manual-destination-v1.json") }.first { it.isFile }
        assertTrue(file.length() in 1..1_048_576)
        val cases = V4vJson.parse(file.readText(), 1_048_576).getValue("cases").jsonArray
        assertTrue(cases.size >= 42)
        for (row in cases) {
            val value = row.jsonObject
            val bytes = value.getValue("json").jsonPrimitive.content.toByteArray()
            assertEquals(value.getValue("id").jsonPrimitive.content, value.getValue("accepted").jsonPrimitive.boolean,
                runCatching { ManualDestinationProfile.parse(bytes) }.isSuccess)
        }
    }
    @Test fun invalidUtf8AndActualByteBound() {
        failure { ManualDestinationProfile.parse(byteArrayOf(0xc3.toByte(), 0x28)) }
        failure { ManualDestinationProfile.parse(ByteArray(16_385) { 32 }) }
        val exact = json() + ByteArray(16_384 - json().size) { 32 }
        assertEquals("rtmp", ManualDestinationProfile.parse(exact).transport)
    }
    @Test fun lexicalVersionAndEscapedDuplicateAreRejected() {
        for (token in listOf("1.0", "1e0", "\"1\"", "true", "null")) failure {
            ManualDestinationProfile.parse(json().toString(Charsets.UTF_8).replace("\"version\":1", "\"version\":$token").toByteArray())
        }
        failure { ManualDestinationProfile.parse((json().toString(Charsets.UTF_8).dropLast(1) + ",\"\\u006eame\":\"Other\"}").toByteArray()) }
    }
    @Test fun reviewCannotExposePathOrCredentialsOrAuthorProvidedName() {
        val profile = ManualDestinationProfile.fields("generated-name-key", "rtmp", "rtmps://stream.example.invalid/private-app", "generated-private-key", "", "", false)
        assertEquals("stream.example.invalid", profile.review.host); assertEquals(443, profile.review.port)
        for (text in listOf(profile.toString(), profile.review.toString())) {
            assertFalse(text.contains("generated-name-key")); assertFalse(text.contains("private-app")); assertFalse(text.contains("generated-private-key"))
        }
    }
    @Test fun finiteSeededByteMutationHasOnlyStaticErrors() {
        val random = Random(0x51afeL); val original = json()
        repeat(2048) {
            val bytes = original.copyOf()
            repeat(1 + random.nextInt(4)) { bytes[random.nextInt(bytes.size)] = random.nextInt(256).toByte() }
            try { ManualDestinationProfile.parse(bytes) } catch (error: ManualProfileFailure) {
                assertFalse(error.toString().contains("generated-private-key")); assertNull(error.cause)
            }
        }
    }
    @Test fun cancelledPlaintextReviewCannotAuthoriseSuccessor() {
        val actions = ManualProfileActions(); var applied = 0
        val a = requireNotNull(actions.begin(true)); assertTrue(actions.reviewed(a, profile(true), true)); val old = requireNotNull(actions.review)
        actions.invalidate()
        val b = requireNotNull(actions.begin(true)); assertTrue(actions.reviewed(b, profile(true), true)); val next = requireNotNull(actions.review)
        assertFalse(actions.cancel(old)); assertSame(next, actions.review)
        assertFalse(actions.apply(old, true) { applied++ })
        assertFalse(actions.setConsent(old, true)) // A delayed checkbox must not authorise B.
        assertFalse(actions.apply(next, true) { applied++ })
        assertTrue(actions.setConsent(next, true))
        assertTrue(actions.apply(next, true) { applied++ }); assertEquals(1, applied)
        assertFalse(actions.apply(next, true) { applied++ })
    }
    @Test fun secureImportNeverCarriesPlaintextPermissionToAppliedFields() {
        val actions = ManualProfileActions(); val p = ManualDestinationProfile.parse(json(plaintext = true))
        assertTrue(p.allowPlaintext); assertFalse(p.review.plaintext)
        val t = requireNotNull(actions.begin(true)); actions.reviewed(t, p, true)
        var plaintext = true
        assertTrue(actions.apply(requireNotNull(actions.review), true) { plaintext = it.review.plaintext })
        assertFalse(plaintext)
    }
    @Test fun heldReadRetainsCapacityButCannotChangeEditedOrPreparedFields() {
        val actions = ManualProfileActions(); val old = requireNotNull(actions.begin(true))
        actions.invalidate(); assertTrue(actions.busy); assertNull(actions.begin(true))
        assertFalse(actions.reviewed(old, profile(), true)); assertNull(actions.review); assertFalse(actions.busy)
        val next = requireNotNull(actions.begin(true))
        assertFalse(actions.finish(old, true)); assertTrue(actions.busy)
        assertFalse(actions.reviewed(next, profile(), false)); assertNull(actions.review)
    }
    @Test fun pendingPermissionStorageOrServiceAdmissionRejectsReviewAndApply() {
        val actions = ManualProfileActions(); assertNull(actions.begin(false))
        val ticket = requireNotNull(actions.begin(true)); assertTrue(actions.reviewed(ticket, profile(), true))
        var fields = 0
        assertFalse(actions.apply(requireNotNull(actions.review), false) { fields++ })
        actions.invalidate(); assertNull(actions.review); assertEquals(0, fields)
    }
    @Test fun applyInvokesFieldsCallbackExactlyOnce() {
        val actions = ManualProfileActions(); var fields = 0
        val ticket = requireNotNull(actions.begin(true)); actions.reviewed(ticket, profile(), true)
        val review = requireNotNull(actions.review)
        assertTrue(actions.apply(review, true) { fields++ })
        assertEquals(1, fields)
        assertFalse(actions.apply(review, true) { fields++ })
        assertEquals(1, fields)
    }
    @Test fun pickerDepartureDefersExactReplyUntilForegroundResume() {
        val actions = ManualProfileActions(); val picker = ManualProfilePicker<String>(); val client = Any(); val ticket = requireNotNull(actions.begin(true))
        assertTrue(picker.begin(client, ticket)); assertTrue(picker.expectedDeparture(client))
        assertNull(picker.resume(client, true)) // Picker departure is not an ordinary background cancellation.
        picker.returned(client, "synthetic-document"); assertTrue(actions.busy)
        val result = requireNotNull(picker.resume(client, true)); assertSame(ticket, result.ticket); assertTrue(result.valid)
        assertTrue(actions.reviewed(requireNotNull(result.ticket), profile(), true)); assertNull(picker.resume(client, true))
    }
    @Test fun stalePickerCannotLendItsResultOrPlaintextConsentToB() {
        val actions = ManualProfileActions(); val picker = ManualProfilePicker<String>(); val client = Any(); val a = requireNotNull(actions.begin(true))
        picker.begin(client, a); picker.invalidate(client); actions.invalidate(); assertFalse(picker.expectedDeparture(client))
        assertNull(actions.begin(true)); assertFalse(picker.begin(client, a))
        picker.returned(client, "old"); val old = requireNotNull(picker.resume(client, true)); assertFalse(old.valid)
        assertFalse(actions.finish(requireNotNull(old.ticket), false)); assertFalse(actions.busy)
        val b = requireNotNull(actions.begin(true)); assertTrue(picker.begin(client, b)); picker.returned(client, null)
        val cancelled = requireNotNull(picker.resume(client, true)); assertNull(cancelled.value); actions.finish(requireNotNull(cancelled.ticket), false)
        assertNull(actions.review); assertFalse(actions.busy)
    }
    @Test fun foregroundReturnDuringPreparationCannotReadOrReviewPickerResult() {
        val actions = ManualProfileActions(); val picker = ManualProfilePicker<String>(); val client = Any(); val ticket = requireNotNull(actions.begin(true))
        picker.begin(client, ticket); picker.returned(client, "old"); val result = requireNotNull(picker.resume(client, false))
        assertFalse(result.valid); assertFalse(actions.finish(requireNotNull(result.ticket), false)); assertNull(actions.review)
    }
    @Test fun recreatedOrOtherActivityCannotAdoptOldPickerOrLaunchOverIt() {
        val picker = ManualProfilePicker<String>(); val a = Any(); val b = Any()
        val actionsA = ManualProfileActions(); val actionsB = ManualProfileActions()
        val old = requireNotNull(actionsA.begin(true)); assertTrue(picker.begin(a, old))
        picker.invalidate(a); actionsA.invalidate()
        val next = requireNotNull(actionsB.begin(true)); assertFalse(picker.begin(b, next))
        actionsB.finish(next, false)
        picker.returned(b, "old-result")
        val delivered = requireNotNull(picker.resume(b, true)); assertFalse(delivered.valid); assertSame(old, delivered.ticket)
        assertNull(actionsB.review)
        val actual = requireNotNull(actionsB.begin(true)); assertTrue(picker.begin(b, actual))
        assertFalse(picker.expectedDeparture(a)); assertTrue(picker.expectedDeparture(b))
    }
    @Test fun restoredProcessMarkerOnlyDiscardsAndPersistsUntilActualReply() {
        val picker = ManualProfilePicker<String>(); val client = Any()
        picker.restoreDiscard(client); assertTrue(picker.outstandingFor(client)); assertFalse(picker.expectedDeparture(client))
        assertNull(picker.resume(client, true))
        val again = Any(); picker.restoreDiscard(again); assertTrue(picker.outstandingFor(again))
        picker.returned(again, "old-result")
        val delivered = requireNotNull(picker.resume(again, true)); assertFalse(delivered.valid); assertNull(delivered.ticket)
    }
    @Test fun actualIoCapacityRemainsUntilClosureAndUnknownCloseIsSticky() {
        val slot = ManualProfileIOAdmission(); assertTrue(slot.reserve()); assertFalse(slot.reserve())
        slot.finish(true); assertTrue(slot.reserve())
        slot.finish(false); assertFalse(slot.reserve())
        slot.finish(true); assertFalse(slot.reserve()) // A duplicate cannot claim an uncertain close succeeded.
    }
    @Test fun logicalTimeoutReportsBeforeBlockedCancellationAndRejectsLateSuccess() {
        val slot = ManualProfileIOAdmission(); assertTrue(slot.reserve())
        val job = ManualProfileIOJob<String>(slot, 5)
        assertNull(job.timeout(4)); assertTrue(requireNotNull(job.timeout(5)).isFailure)
        assertFalse(slot.reserve()) // Timer delivery does not prove closure.
        assertNull(job.complete(Result.success("late-secret-profile"), 6, true))
        assertFalse(slot.reserve()) // Worker closed but cancellation Binder is still held.
        job.cancellationReturned(); assertTrue(slot.reserve())
    }
    @Test fun cancellationReturnAloneCannotReleaseHeldReadOrUncertainClose() {
        val slot = ManualProfileIOAdmission(); assertTrue(slot.reserve())
        val job = ManualProfileIOJob<String>(slot, 5)
        assertTrue(requireNotNull(job.timeout(5)).isFailure); job.cancellationReturned()
        assertFalse(slot.reserve())
        assertNull(job.complete(Result.success("late-profile"), 7, false)); assertFalse(slot.reserve())
    }
    @Test fun withheldTimerLateCompletionCannotExposeProfileOrAffectSuccessor() {
        val slot = ManualProfileIOAdmission(); assertTrue(slot.reserve())
        val old = ManualProfileIOJob<String>(slot, 5)
        assertTrue(requireNotNull(old.complete(Result.success("expired-profile"), 5, true)).isFailure)
        assertTrue(slot.reserve()); val next = ManualProfileIOJob<String>(slot, 20)
        assertNull(old.timeout(6)); assertNull(old.complete(Result.success("duplicate"), 6, true))
        assertFalse(slot.reserve())
        assertEquals("current", requireNotNull(next.complete(Result.success("current"), 10, true)).getOrThrow())
        assertTrue(slot.reserve())
    }
    @Test fun documentByteAndOriginalDeadlineBoundsAreEnforced() {
        assertArrayEquals(json(), ManualProfileImport.read(ByteArrayInputStream(json()), { 1 }, 2))
        assertTrue(runCatching { ManualProfileImport.read(ByteArrayInputStream(ByteArray(16_385)), { 1 }, 2) }.isFailure)
        var clock = 0L
        assertTrue(runCatching { ManualProfileImport.read(ByteArrayInputStream(json()), { ++clock }, 2) }.isFailure)
    }
    private class Bytes : ManualProfileBytes {
        var data: ByteArray? = null; var writes = 0; var fail = false; var failAfterCommit = false
        override fun read() = data?.copyOf()
        override fun write(bytes: ByteArray) { writes++; if (fail) throw IllegalStateException("generated storage failure"); data = bytes.copyOf(); if (failAfterCommit) throw IllegalStateException("generated sync failure") }
    }
    private fun repository(bytes: Bytes, key: ByteArray = ByteArray(32) { 19 }): ManualDestinationProfiles {
        val cipher = V4vJournalCipher({ SecretKeySpec(key, "AES") }, ManualDestinationProfiles.MAX_STORE)
        return ManualDestinationProfiles(bytes, cipher::seal, cipher::open)
    }
    @Test fun encryptedSaveReloadDeleteDoesNotPersistPlaintext() {
        val bytes = Bytes(); val store = repository(bytes); assertTrue(store.list().isEmpty())
        val id = store.save(null, profile()); val sealed = requireNotNull(bytes.data)
        assertFalse(sealed.toString(Charsets.ISO_8859_1).contains("generated-private-key"))
        assertEquals("generated-private-key", repository(bytes).load(id).streamKey)
        store.delete(id); assertTrue(repository(bytes).list().isEmpty()); assertNotNull(bytes.data)
    }
    @Test fun corruptWrongKeyOversizeAndInvalidDocumentNeverResetOrWrite() {
        val bytes = Bytes(); repository(bytes).save(null, profile()); val valid = requireNotNull(bytes.data).copyOf(); val writes = bytes.writes
        failure { repository(bytes, ByteArray(32) { 20 }).list() }; assertEquals(writes, bytes.writes)
        bytes.data = valid.copyOf().also { it[it.lastIndex] = (it.last().toInt() xor 1).toByte() }
        failure { repository(bytes).list() }; assertEquals(writes, bytes.writes)
        bytes.data = ByteArray(ManualDestinationProfiles.MAX_STORE + 65)
        failure { repository(bytes).save(null, profile()) }; assertEquals(writes, bytes.writes)
        bytes.data = valid
        val bad = ManualDestinationProfiles(bytes, { it }, { "{\"version\":1,\"profiles\":[{}]}".toByteArray() })
        failure { bad.list() }; failure { bad.save(null, profile()) }; assertEquals(writes, bytes.writes)
    }
    @Test fun writeFailureRetainsPoisonedOwnerAndCannotBeSilentlyRetried() {
        val bytes = Bytes(); val store = repository(bytes); val id = store.save(null, profile())
        bytes.fail = true; failure { store.delete(id) }; val writes = bytes.writes
        bytes.fail = false; failure { store.save(id, profile(true)) }; failure { store.list() }; assertEquals(writes, bytes.writes)
        assertEquals(1, repository(bytes).list().size)
    }
    @Test fun sixteenProfileLimitStillAllowsExplicitDelete() {
        val bytes = Bytes(); val store = repository(bytes); val ids = (0 until 16).map { store.save(null, profile()) }
        failure { store.save(null, profile()) }; assertEquals(16, store.list().size)
        store.delete(ids[0]); assertEquals(15, repository(bytes).list().size)
        assertEquals(32, store.save(null, profile()).length)
    }
    @Test fun unconfirmedPostCommitFailureDoesNotClaimOldDataUnchanged() {
        val bytes = Bytes(); val store = repository(bytes); val id = store.save(null, profile())
        bytes.failAfterCommit = true; failure { store.delete(id) }
        assertTrue(repository(bytes).list().isEmpty()) // The operation really committed despite no acknowledgement.
        failure { store.list() } // Same poisoned owner cannot silently retry.
    }
}
