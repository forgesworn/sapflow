package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import android.os.Process
import android.system.Os
import android.system.OsConstants
import java.io.FileDescriptor
import java.io.FileInputStream
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.io.File
import java.io.FileNotFoundException
import java.security.KeyStore
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import kotlinx.serialization.json.JsonObject

/** Adapted from MIT KithMoot EncryptedRoomStorage (2026 TheCryptoDonkey).
 * No missing-existing/corrupt reset, no plaintext preferences, no persona key. */
internal class V4vSessionStore(context: Context) : V4vJournalStore {
    private val base = File(context.applicationContext.noBackupFilesDir, "v4v-connector.v1.vault")
    private val file = AtomicFile(base)
    private val alias = "sapflow.v4v.connector.v1"
    private val envelope = V4vJournalCipher(::key)
    private val maxBytes = 256 * 1024
    private fun keys() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    private fun key(create: Boolean): SecretKey {
        val store = keys()
        store.getKey(alias, null)?.let { return it as SecretKey }
        check(create) { "V4V journal key unavailable" }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true).build()); generateKey()
        }
    }
    private fun checkPaths() {
        for (path in listOf(base, File(base.path + ".bak"), File(base.path + ".new")))
            check(!Files.isSymbolicLink(path.toPath())) { "V4V journal path is unsafe" }
    }
    private fun openPrivateDirectory(): FileDescriptor {
        val directory = Os.open(base.parentFile!!.absolutePath, OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW, 0)
        try {
            val stat = Os.fstat(directory)
            check(OsConstants.S_ISDIR(stat.st_mode) && stat.st_uid == Process.myUid() && (stat.st_mode and 0x3f) == 0) { "V4V journal directory is unsafe" }
            return directory
        } catch (error: Exception) { Os.close(directory); throw error }
    }
    @Synchronized override fun load(): JsonObject? {
        val directory = openPrivateDirectory()
        try {
            checkPaths()
            val input = try { file.openRead() } catch (error: FileNotFoundException) {
                check(!base.exists() && !File(base.path + ".bak").exists() && !keys().containsAlias(alias)) { "Existing V4V journal unavailable" }
                return null
            }
            val sealed = input.use { require(it.channel.size() in 29..(maxBytes + 64).toLong()); it.readBytes().also { bytes -> require(bytes.size <= maxBytes + 64) } }
            val plain = envelope.open(sealed)
            return try { V4vJson.parse(plain.toString(Charsets.UTF_8), maxBytes) } finally { plain.fill(0) }
        } finally { Os.close(directory) }
    }
    @Synchronized override fun save(document: JsonObject) {
        val plain = document.toString().toByteArray(Charsets.UTF_8)
        require(plain.size <= maxBytes)
        val sealed = try { envelope.seal(plain) } finally { plain.fill(0) }
        checkPaths()
        // Hold and validate the selected app-private parent before replacing any
        // file; the public O_NOFOLLOW flag refuses a substituted final directory.
        val directory = openPrivateDirectory()
        try {
            V4vDurableCommit(replace = { bytes ->
                checkPaths()
                val output = file.startWrite()
                try { output.write(bytes); Os.fchmod(output.fd, 0x180); output.fd.sync(); file.finishWrite(output) }
                catch (error: Exception) { file.failWrite(output); throw error }
            }, readCommitted = {
                checkPaths()
                FileInputStream(base).use { input ->
                    require(input.channel.size() <= maxBytes + 64)
                    val bytes = ByteArrayOutputStream(); val buffer = ByteArray(4096)
                    while (true) { val n = input.read(buffer); if (n < 0) break; require(bytes.size() + n <= maxBytes + 64); bytes.write(buffer, 0, n) }
                    bytes.toByteArray()
                }
            }, syncDirectory = { Os.fsync(directory) }).write(sealed)
        } finally { Os.close(directory) }
    }
}
