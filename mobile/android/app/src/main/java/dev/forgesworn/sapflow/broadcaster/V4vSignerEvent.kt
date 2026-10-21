package dev.forgesworn.sapflow.broadcaster

// Verify-only adaptation of KithMoot (MIT, Copyright 2026 TheCryptoDonkey).
import fr.acinq.secp256k1.Secp256k1
import java.security.MessageDigest
import kotlinx.serialization.json.*

internal data class V4vLoginChallenge(val challenge: String, val origin: String, val app: String, val createdAt: Long) {
    init { require(challenge.matches(Regex("[0-9a-f]{64}")) && app.length in 1..128) }
    fun unsigned(pubkey: String): JsonObject {
        require(pubkey.matches(Regex("[0-9a-f]{64}")))
        return buildJsonObject {
            put("pubkey", pubkey); put("created_at", createdAt); put("kind", 21236)
            put("tags", buildJsonArray { add(buildJsonArray { add("challenge"); add(challenge) }); add(buildJsonArray { add("origin"); add(origin) }); add(buildJsonArray { add("app"); add(app) }) })
            put("content", "")
        }
    }
}
internal object SignedLoginEvent {
    private fun hex(value: String, bytes: Int): ByteArray {
        require(value.matches(Regex("[0-9a-f]{${bytes * 2}}")))
        return ByteArray(bytes) { value.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    }
    fun id(event: JsonObject): String = with(V4vJson) {
        val canonical = buildJsonArray { add(0); add(event.text("pubkey", 64)); add(event.number("created_at", 1)); add(event.number("kind", 21236, 21236)); add(event.getValue("tags")); add(event.text("content", 0)) }
        MessageDigest.getInstance("SHA-256").digest(canonical.toString().toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it.toInt() and 255) }
    }
    fun validate(expected: JsonObject, eventText: String?, signature: String?): JsonObject {
        val signed = if (eventText != null) V4vJson.parse(eventText, 8192) else buildJsonObject {
            for ((key, value) in expected) put(key, value)
            put("id", id(expected)); put("sig", requireNotNull(signature).lowercase())
        }
        require(signed.keys == setOf("pubkey", "created_at", "kind", "tags", "content", "id", "sig"))
        for (key in expected.keys) require(signed[key] == expected[key]) { "Signer changed the login request" }
        with(V4vJson) {
            val digest = id(expected); require(signed.text("id", 64) == digest)
            require(Secp256k1.get().verifySchnorr(hex(signed.text("sig", 128), 64), hex(digest, 32), hex(expected.text("pubkey", 64), 32))) { "Signer signature did not verify" }
        }
        return signed
    }
}

