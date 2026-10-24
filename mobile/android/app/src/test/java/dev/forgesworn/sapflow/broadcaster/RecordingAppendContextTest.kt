package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.security.MessageDigest
import org.junit.Assert.*
import org.junit.Test

/** Pure context integrity/binding controls. No file/native/EOF authority. */
class RecordingAppendContextTest {
    private val identity = RecordingCatalogue.Identity("11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222", 7, "AUDIO_ONLY_48K_MONO")
    private val nonce = "33333333-3333-3333-3333-333333333333"
    private val root = RecordingCheckpointStorage.Node(Long.MIN_VALUE, -3, true, false, 2, 4096)
    private val media = RecordingCheckpointStorage.Node(Long.MAX_VALUE, Long.MIN_VALUE, false, true, 1, 0)
    private fun context() = RecordingAppendContext.create(identity, nonce, root, media)
    private fun decode(bytes: ByteArray) = RecordingAppendContext.decode(bytes, identity, root, media)
    private fun seal(bytes: ByteArray): ByteArray {
        val body = bytes.copyOfRange(0, bytes.size - 32)
        return body + MessageDigest.getInstance("SHA-256").digest(body)
    }

    @Test fun canonicalRoundTripPreservesTheExistingSignedLongDomains() {
        val bytes = context().encode()
        assertTrue(bytes.size in RecordingAppendContext.MIN_BYTES..RecordingAppendContext.MAX_BYTES)
        val decoded = decode(bytes)
        assertArrayEquals(bytes, decoded.encode()); assertEquals(nonce, decoded.nonce)
        val bindings = decoded.bindings(identity, root, media)
        assertEquals("5070b6aba14627239513bd620f18b99e5f40847ba7038639fd9a71375a9ec081", bindings.recording)
        assertEquals("868d871b5bf68bad5ceea096ac6f2d7cad56f1a8a25903a61599f99f6053fe28", bindings.sourceFile)
        // Directory links/size and appended media length are not birth bindings.
        assertEquals(bindings, decoded.bindings(identity, root.copy(links = 5, bytes = 8192), media.copy(bytes = 991)))
    }

    @Test fun everyTruncationOversizeAndTrailingByteRefuses() {
        val bytes = context().encode()
        for (end in 0 until bytes.size) {
            assertThrows(IllegalArgumentException::class.java) { decode(bytes.copyOf(end)) }
        }
        assertThrows(IllegalArgumentException::class.java) { decode(ByteArray(RecordingAppendContext.MAX_BYTES + 1)) }
        val extra = bytes.copyOfRange(0, bytes.size - 32) + byteArrayOf(0) + ByteArray(32)
        assertThrows(IllegalArgumentException::class.java) { decode(seal(extra)) }
    }

    @Test fun checksumProtectsEveryOriginalContextByte() {
        val original = context().encode()
        for (at in original.indices) {
            val changed = original.copyOf(); changed[at] = (changed[at].toInt() xor 1).toByte()
            assertThrows(IllegalArgumentException::class.java) { decode(changed) }
        }
        assertArrayEquals(original, context().encode())
    }

    @Test fun independentlyExpectedIdentityAndDescriptorIncarnationsAreMandatory() {
        val bytes = context().encode()
        for (other in listOf(identity.copy(segmentId = nonce), identity.copy(showId = nonce),
            identity.copy(segmentIndex = 8), identity.copy(profileId = "UNSPECIFIED"))) {
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.decode(bytes, other, root, media) }
        }
        for (other in listOf(root.copy(device = 0), root.copy(inode = 0))) {
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.decode(bytes, identity, other, media) }
        }
        for (other in listOf(media.copy(device = 0), media.copy(inode = 0))) {
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.decode(bytes, identity, root, other) }
        }
    }

    @Test fun checksumValidUnsupportedVersionsAndNoncanonicalFieldsRefuse() {
        val original = context().encode()
        val changes: List<(ByteArray) -> Unit> = listOf(
            { it[0] = 'X'.code.toByte() },
            { ByteBuffer.wrap(it).putInt(8, 2) },
            { ByteBuffer.wrap(it).putInt(12, 2) },
            { it[16] = 'A'.code.toByte() },
            { it[52] = '/'.code.toByte() },
            { ByteBuffer.wrap(it).putLong(124, -1) },
            { ByteBuffer.wrap(it).putInt(132, -1) },
            { ByteBuffer.wrap(it).putInt(132, 65) },
            { it[136] = 0 })
        for (change in changes) {
            val bytes = original.copyOf(); change(bytes)
            assertThrows(IllegalArgumentException::class.java) { decode(seal(bytes)) }
        }
    }

    @Test fun unsafeActualFileTypesAndHardlinksCannotSatisfyContextBindings() {
        val bytes = context().encode()
        for (other in listOf(media.copy(regular = false), media.copy(directory = true),
            media.copy(links = 2), media.copy(bytes = -1))) {
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.decode(bytes, identity, root, other) }
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.create(identity, nonce, root, other) }
        }
        for (other in listOf(root.copy(directory = false), root.copy(regular = true))) {
            assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.decode(bytes, identity, other, media) }
        }
        assertThrows(IllegalArgumentException::class.java) { RecordingAppendContext.create(identity, "not-a-uuid", root, media) }
    }
}
