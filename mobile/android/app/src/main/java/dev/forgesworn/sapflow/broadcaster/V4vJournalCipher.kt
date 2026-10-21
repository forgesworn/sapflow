package dev.forgesworn.sapflow.broadcaster

import kotlinx.serialization.json.JsonObject
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Pure authenticated envelope; only the Android adapter supplies a non-exportable key. */
internal class V4vJournalCipher(private val key: (Boolean) -> SecretKey, private val maxBytes: Int = 256 * 1024) {
    private val aad = "Sapflow V4V connector journal v1".toByteArray()
    fun seal(plain: ByteArray): ByteArray {
        require(plain.size <= maxBytes)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding"); cipher.init(Cipher.ENCRYPT_MODE, key(true)); cipher.updateAAD(aad)
        require(cipher.iv.size == 12)
        return byteArrayOf(1) + cipher.iv + cipher.doFinal(plain)
    }
    fun open(sealed: ByteArray): ByteArray {
        require(sealed.size in 29..maxBytes + 29 && sealed[0] == 1.toByte())
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, sealed.copyOfRange(1, 13))); cipher.updateAAD(aad)
        return cipher.doFinal(sealed, 13, sealed.size - 13)
    }
}

internal interface V4vJournalStore {
    fun load(): JsonObject?
    fun save(document: JsonObject)
}

