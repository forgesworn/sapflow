package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.security.MessageDigest

/** Bounded cold metadata, not a checkpoint, file capability or native receipt.
 * The caller must obtain expected identity and Nodes from its checked owner.
 * The checksum detects corruption; it does not authenticate same-UID writers. */
internal class RecordingAppendContext private constructor(
    val identity: RecordingCatalogue.Identity,
    val nonce: String,
    private val rootDevice: Long, private val rootInode: Long,
    private val mediaDevice: Long, private val mediaInode: Long,
) {
    data class Bindings(val recording: String, val sourceFile: String)

    /** EXACT existing v1 digests. No checkpoint-supplied expected bindings. */
    fun bindings(expectedIdentity: RecordingCatalogue.Identity,
        actualRoot: RecordingCheckpointStorage.Node,
        actualMedia: RecordingCheckpointStorage.Node): Bindings {
        checkNodes(actualRoot, actualMedia)
        require(identity == expectedIdentity && rootDevice == actualRoot.device && rootInode == actualRoot.inode &&
            mediaDevice == actualMedia.device && mediaInode == actualMedia.inode) { "Append context binding changed" }
        return Bindings(
            digest("sapflow.android.checkpoint.recording.v1\n${identity.showId}\n${identity.segmentId}\n${identity.segmentIndex}\n${identity.profileId}\n$nonce".toByteArray(Charsets.UTF_8)).hex(),
            digest("sapflow.android.checkpoint.file.v1\n$rootDevice\n$rootInode\n$mediaDevice\n$mediaInode\n$nonce".toByteArray(Charsets.UTF_8)).hex())
    }

    fun encode(): ByteArray {
        val profile = identity.profileId.toByteArray(Charsets.US_ASCII)
        require(profile.size in 1..MAX_PROFILE && profile.toString(Charsets.US_ASCII) == identity.profileId)
        val body = ByteBuffer.allocate(FIXED_BODY + profile.size)
        body.put(MAGIC).putInt(1).putInt(1) // Context version, existing binding-domain version.
        body.put(nonce.toByteArray(Charsets.US_ASCII))
        body.put(identity.segmentId.toByteArray(Charsets.US_ASCII))
        body.put(identity.showId.toByteArray(Charsets.US_ASCII))
        body.putLong(identity.segmentIndex).putInt(profile.size).put(profile)
        body.putLong(rootDevice).putLong(rootInode).putLong(mediaDevice).putLong(mediaInode)
        check(!body.hasRemaining())
        return body.array() + digest(body.array())
    }

    companion object {
        // Fixed-width ASCII UUIDs, bounded ASCII profile and four signed stat scalars.
        private val MAGIC = "SFAC0001".toByteArray(Charsets.US_ASCII)
        private const val FIXED_BODY = 8 + 4 + 4 + 36 * 3 + 8 + 4 + 8 * 4
        private const val MAX_PROFILE = 64
        const val MIN_BYTES = FIXED_BODY + 1 + 32
        const val MAX_BYTES = FIXED_BODY + MAX_PROFILE + 32
        private val uuid = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

        fun create(identity: RecordingCatalogue.Identity, nonce: String,
            actualRoot: RecordingCheckpointStorage.Node, actualMedia: RecordingCheckpointStorage.Node): RecordingAppendContext {
            require(uuid.matches(nonce)); checkNodes(actualRoot, actualMedia)
            return RecordingAppendContext(identity, nonce, actualRoot.device, actualRoot.inode,
                actualMedia.device, actualMedia.inode)
        }

        /** Validates one canonical bounded context against independently known
         * identity and actual descriptors. This does not reopen or adopt files. */
        fun decode(bytes: ByteArray, expectedIdentity: RecordingCatalogue.Identity,
            actualRoot: RecordingCheckpointStorage.Node, actualMedia: RecordingCheckpointStorage.Node): RecordingAppendContext {
            require(bytes.size in MIN_BYTES..MAX_BYTES)
            val body = bytes.copyOfRange(0, bytes.size - 32)
            require(digest(body).contentEquals(bytes.copyOfRange(body.size, bytes.size))) { "Append context checksum changed" }
            val input = ByteBuffer.wrap(body)
            val magic = ByteArray(8); input.get(magic)
            require(magic.contentEquals(MAGIC) && input.int == 1 && input.int == 1) { "Unsupported append context" }
            fun ascii(count: Int): String {
                require(count >= 0 && input.remaining() >= count)
                val value = ByteArray(count); input.get(value)
                require(value.all { it.toInt() in 32..126 })
                return value.toString(Charsets.US_ASCII)
            }
            val nonce = ascii(36); require(uuid.matches(nonce))
            val segment = ascii(36); val show = ascii(36)
            require(input.remaining() >= 12)
            val index = input.long; val profileSize = input.int
            require(profileSize in 1..MAX_PROFILE && input.remaining() == profileSize + 32)
            val identity = RecordingCatalogue.Identity(segment, show, index, ascii(profileSize))
            val value = RecordingAppendContext(identity, nonce, input.long, input.long, input.long, input.long)
            require(!input.hasRemaining())
            value.bindings(expectedIdentity, actualRoot, actualMedia)
            require(value.encode().contentEquals(bytes)) { "Noncanonical append context" }
            return value
        }

        private fun checkNodes(root: RecordingCheckpointStorage.Node, media: RecordingCheckpointStorage.Node) {
            require(root.directory && !root.regular)
            require(media.regular && !media.directory && media.links == 1L && media.bytes >= 0)
        }
        private fun digest(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)
        private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }
    }
}
