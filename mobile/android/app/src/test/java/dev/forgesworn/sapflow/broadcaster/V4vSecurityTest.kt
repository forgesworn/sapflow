package dev.forgesworn.sapflow.broadcaster

import fr.acinq.secp256k1.Secp256k1
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import javax.crypto.spec.SecretKeySpec

internal class V4vSecurityTest {
    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it.toInt() and 255) }
    @Test fun signerFullEventMustMatchEveryRequestedFieldAndVerifyBip340() {
        val secp = Secp256k1.get(); val secret = ByteArray(32).also { it[31] = 1 }
        val pub = hex(secp.pubkeyCreate(secret).copyOfRange(1, 33))
        val expected = V4vLoginChallenge("a".repeat(64), "https://creator.invalid", "Fixture", 1234).unsigned(pub)
        val id = SignedLoginEvent.id(expected)
        val digest = ByteArray(32) { id.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
        val sig = hex(secp.signSchnorr(digest, secret, ByteArray(32)))
        val signed = buildJsonObject { for ((k, v) in expected) put(k, v); put("id", id); put("sig", sig) }
        assertEquals(signed, SignedLoginEvent.validate(expected, signed.toString(), null))
        for ((key, changed) in listOf("pubkey" to JsonPrimitive("f".repeat(64)), "kind" to JsonPrimitive(1), "created_at" to JsonPrimitive(1235), "content" to JsonPrimitive("different"), "tags" to buildJsonArray { })) {
            val wrong = JsonObject(signed.toMutableMap().also { it[key] = changed })
            assertThrows(IllegalArgumentException::class.java) { SignedLoginEvent.validate(expected, wrong.toString(), null) }
        }
        assertThrows(IllegalArgumentException::class.java) { SignedLoginEvent.validate(expected, null, "0".repeat(128)) }
    }
    @Test fun journalCipherAuthenticatesVersionNonceBodyAndKeyWithoutCreatingReadKey() {
        var creations = 0; var reads = 0
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val cipher = V4vJournalCipher({ create -> if (create) creations++ else reads++; key }, 256)
        val text = "synthetic bearer and scoped secret".toByteArray()
        val sealed = cipher.seal(text)
        assertFalse(sealed.toString(Charsets.UTF_8).contains("synthetic"))
        assertArrayEquals(text, cipher.open(sealed)); assertEquals(1, creations); assertEquals(1, reads)
        for (index in listOf(0, 1, 13, sealed.lastIndex)) {
            val corrupt = sealed.clone(); corrupt[index] = (corrupt[index].toInt() xor 1).toByte()
            assertThrows(Exception::class.java) { cipher.open(corrupt) }
        }
        assertThrows(Exception::class.java) { V4vJournalCipher({ SecretKeySpec(ByteArray(32) { 8 }, "AES") }, 256).open(sealed) }
        assertThrows(Exception::class.java) { cipher.open(sealed.copyOf(28)) }
        assertThrows(Exception::class.java) { cipher.seal(ByteArray(257)) }
    }
    @Test fun parserRejectsDuplicateIncludingEscapedKeysAndNumericStringRevision() {
        for (text in listOf("{\"a\":1,\"a\":2}", "{\"a\":1,\"\\u0061\":2}")) assertThrows(Exception::class.java) { V4vJson.parse(text) }
        val o = V4vJson.parse("{\"revision\":\"1\"}")
        assertThrows(Exception::class.java) { with(V4vJson) { o.number("revision", 1) } }
    }
    @Test fun durableOperationRejectsArbitraryFieldsAndEndpointPaths() {
        assertThrows(Exception::class.java) { V4vOperation(V4vOperationKind.REFRESH, "event-fixture", "a".repeat(32), buildJsonObject { put("revision", 1); put("idempotencyKey", "b".repeat(32)); put("url", "https://elsewhere.invalid") }) }
        assertThrows(Exception::class.java) { V4vOperation(V4vOperationKind.PREPARE, "../other", null, buildJsonObject { put("expectedEventGeneration", 0); put("idempotencyKey", "b".repeat(32)) }) }
    }
    @Test fun silentRenameFailureOrDirectorySyncFailureCannotAcknowledgeCommit() {
        val old = "old persisted journal".toByteArray(); val fresh = "new pending operation".toByteArray()
        var actual = old; var synced = false
        val silentRenameFailure = V4vDurableCommit({ /* platform logged a failed rename */ }, { actual }, { synced = true })
        assertThrows(IllegalStateException::class.java) { silentRenameFailure.write(fresh) }
        assertArrayEquals(old, actual); assertFalse(synced)
        val failedSync = V4vDurableCommit({ actual = it.clone() }, { actual }, { throw java.io.IOException("synthetic fsync failure") })
        assertThrows(java.io.IOException::class.java) { failedSync.write(fresh) }
        assertArrayEquals(fresh, actual) // On reopen, exact saved request remains; caller never sent it.
    }
}
