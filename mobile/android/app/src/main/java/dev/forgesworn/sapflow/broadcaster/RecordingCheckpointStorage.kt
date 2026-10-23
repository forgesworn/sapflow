package dev.forgesworn.sapflow.broadcaster

import java.security.MessageDigest
import java.util.UUID

/** Cold, single-worker checked storage. No muxer, media queue, AAC facts or
 * finalised app admission. Root and every open file remain actual IO owners. */
internal class RecordingCheckpointStorage private constructor(
    private val identity: RecordingCatalogue.Identity,
    private val root: Root,
    private val checkLease: () -> Unit,
    /** Pure scalar read of this exact Writer's cancellation state. */
    private val checkAdmission: () -> Unit,
    private val releaseLease: () -> Unit,
    private val retainUncertain: (Any) -> Unit,
) {
    data class Node(val device: Long, val inode: Long, val directory: Boolean, val regular: Boolean,
                    val links: Long, val bytes: Long) {
        fun sameFile(other: Node) = device == other.device && inode == other.inode &&
            directory == other.directory && regular == other.regular &&
            // Directory link counts may change when our own entries are added.
            // Regular files still bind their link count and must have one link.
            (directory || links == other.links)
    }
    interface File {
        fun stat(): Node
        fun read(bytes: ByteArray, offset: Int, count: Int, position: Long): Int
        fun write(bytes: ByteArray, offset: Int, count: Int, position: Long): Int
        fun sync()
        /** Each implementation attempts the native close at most once. */
        fun close()
    }
    interface Root {
        val identity: Node
        fun verify()
        fun names(limit: Int): List<String>
        fun inspect(name: String): Node?
        fun create(name: String): File
        fun openRead(name: String): File
        fun replace(source: String, destination: String)
        fun sync()
        fun availableBytes(): Long
        fun close()
    }
    /** Exact current storage frontier supplied to a future SDK envelope checker.
     * The checker must check the SDK encoding/decoded token against ALL fields.
     * The storage layer does not invent or parse an AAC/checkpoint receipt. */
    data class ExpectedCheckpoint(val sequence: Long, val committedOffset: Long,
        val fragmentRegionBytes: Long, val initialHeaderLength: Int,
        val initialHeaderSha256: String, val fragmentRegionSha256: String,
        val recordingBinding: String, val sourceFileBinding: String)
    class InstalledCheckpoint private constructor(private val owner: Any,
        val expected: ExpectedCheckpoint, val metadataSha256: String) {
        internal fun belongsTo(value: Any) = owner === value
        companion object {
            internal fun issue(owner: Any, expected: ExpectedCheckpoint, hash: String) =
                InstalledCheckpoint(owner, expected, hash)
        }
    }
    /** bytes counts accepted whole chunks. A failed partial append may leave
     * additional uncommitted file bytes; it never advances this frontier. */
    data class Closure(val bytes: Long, val installedSequence: Long, val headerRewritten: Boolean)

    private val worker = Thread.currentThread()
    private val owner = Any()
    private val run = UUID.randomUUID().toString()
    private val prefix = "session-${identity.segmentId}"
    private val mediaName = identity.filename
    private val receiptName = "$prefix.receipt.json"
    private val slots = arrayOf("$prefix.checkpoint.0", "$prefix.checkpoint.1")
    private val temporary = "$prefix.checkpoint.tmp"
    private val reserved = setOf(mediaName, receiptName, slots[0], slots[1], temporary, "$prefix.receipt.tmp")
    private val slotNodes = arrayOfNulls<Node>(2)
    private var media: File? = null
    private var mediaIdentity: Node? = null
    private var metadata: File? = null
    private var initialLength = 0
    private var initialHash = ""
    private var offset = 0L
    private var regionBytes = 0L
    private val regionHash = MessageDigest.getInstance("SHA-256")
    private var installed: InstalledCheckpoint? = null
    private var failed = false
    private var rewritten = false
    private var closeAttempted = false
    private var closed: Closure? = null
    private var closeFailure: Exception? = null
    private val recordingBinding = digest("sapflow.android.checkpoint.recording.v1\n${identity.showId}\n${identity.segmentId}\n${identity.segmentIndex}\n${identity.profileId}\n$run".toByteArray(Charsets.UTF_8))
    private var sourceFileBinding = ""
    private var busy = false

    private fun onWorker() { check(Thread.currentThread() === worker) { "Checkpoint storage has another worker" } }
    private fun active() {
        onWorker(); check(!closeAttempted && !failed && !rewritten) { "Checkpoint storage is not accepting writes" }
        checkLease(); root.verify()
        mediaIdentity?.let { expected ->
            val actual = checkNotNull(media).stat()
            check(expected.sameFile(actual) && actual.bytes == offset) { "Owned media descriptor changed" }
            val named = root.inspect(mediaName)
            check(named != null && actual.sameFile(named) && named.bytes == offset) { "Owned media name changed" }
        }
    }
    private inline fun <T> operation(body: () -> T): T {
        onWorker(); check(!busy) { "Checkpoint storage operation is already active" }; busy = true
        try { active(); return body() }
        catch (error: Exception) { failed = true; throw error }
        finally { busy = false }
    }
    private fun regular(value: Node, size: Long? = null) {
        check(value.regular && !value.directory && value.links == 1L && value.bytes >= 0 &&
            (size == null || value.bytes == size)) { "Unsafe storage file" }
    }
    private fun currentCapacity() {
        val names = root.names(MAX_ENTRIES)
        check(names.size == names.toSet().size && names.size + reserved.count { it !in names } <= MAX_ENTRIES) { "Checkpoint namespace capacity exceeded" }
        val segments = names.mapNotNull { name ->
            when {
                name.matches(Regex("session-[0-9a-f-]{36}\\.mp4")) -> name.removePrefix("session-").removeSuffix(".mp4")
                name.matches(Regex("session-[0-9a-f-]{36}\\.receipt\\.json")) -> name.removePrefix("session-").removeSuffix(".receipt.json")
                else -> null
            }
        }.toSet()
        check(segments.size <= MAX_SEGMENTS) { "Checkpoint segment capacity exceeded" }
        check(root.availableBytes() > STORAGE_FLOOR) { "Checkpoint storage is unavailable or below its floor" }
        checkSlots()
        check(root.inspect(temporary) == null && root.inspect("$prefix.receipt.tmp") == null) { "Checkpoint temporary name exists" }
    }
    private fun checkSlots(publishedIndex: Int? = null, publishedNode: Node? = null) {
        for (index in slots.indices) {
            val named = root.inspect(slots[index])
            val owned = if (publishedIndex == index) checkNotNull(publishedNode) else slotNodes[index]
            check(if (owned == null) named == null else named != null && owned.sameFile(named) && owned.bytes == named.bytes) { "Checkpoint slot changed" }
        }
    }
    private fun revalidate(expected: ExpectedCheckpoint, publishedIndex: Int? = null, publishedNode: Node? = null) {
        active()
        check(expected.committedOffset == offset && expected.fragmentRegionBytes == regionBytes &&
            expected.initialHeaderLength == initialLength && expected.initialHeaderSha256 == initialHash &&
            expected.fragmentRegionSha256 == prefixDigest() && expected.recordingBinding == recordingBinding &&
            expected.sourceFileBinding == sourceFileBinding) { "Checkpoint frontier changed" }
        checkSlots(publishedIndex, publishedNode)
    }
    private fun writeAll(file: File, bytes: ByteArray, start: Long) {
        var done = 0
        while (done < bytes.size) {
            checkAdmission()
            val count = file.write(bytes, done, bytes.size - done, Math.addExact(start, done.toLong()))
            check(count in 1..(bytes.size - done)) { "Storage write made no bounded progress" }
            done += count
        }
    }
    private fun readExact(file: File, size: Int): ByteArray {
        require(size in 1..MAX_CHECKPOINT)
        val bytes = ByteArray(size)
        var done = 0
        while (done < bytes.size) {
            val count = file.read(bytes, done, bytes.size - done, done.toLong())
            check(count in 1..(bytes.size - done)) { "Storage read made no bounded progress" }
            done += count
        }
        return bytes
    }
    private fun prefixDigest(): String {
        val snapshot = regionHash.clone() as MessageDigest
        check(snapshot !== regionHash) { "Digest snapshot aliases its live owner" }
        return snapshot.digest().hex()
    }

    /** Fixed metadata header only. Existing encoded payload arrays are never copied. */
    fun appendInitial(header: ByteArray) = operation {
        require(header.size in 1..MAX_HEADER); check(initialLength == 0 && offset == 0L)
        val stable = header.copyOf() // Bounded container metadata, not encoded media.
        writeAll(checkNotNull(media), stable, 0)
        checkNotNull(media).sync()
        regular(checkNotNull(media).stat(), stable.size.toLong())
        check(readExact(checkNotNull(media), stable.size).contentEquals(stable)) { "Initial header readback changed" }
        initialLength = stable.size; initialHash = digest(stable); offset = stable.size.toLong()
    }
    /** One original SDK fragment buffer on the existing recorder worker. */
    fun appendFragment(bytes: ByteArray) = operation {
        require(bytes.size in 1..MAX_FRAGMENT); check(initialLength > 0)
        val next = Math.addExact(offset, bytes.size.toLong())
        val nextRegion = Math.addExact(regionBytes, bytes.size.toLong())
        writeAll(checkNotNull(media), bytes, offset)
        regular(checkNotNull(media).stat(), next)
        regionHash.update(bytes) // Original buffer, once; no payload clone or staging queue.
        offset = next; regionBytes = nextRegion
    }
    fun checkCapacity() = operation { currentCapacity() }

    /** Cold preparation from the actual appended descriptor frontier. This is
     * not installed progress and cannot authorise recovery or later mutation. */
    fun prepareCheckpoint(sequence: Long): ExpectedCheckpoint = operation {
        val expected = expectedCheckpoint(sequence)
        checkAdmission()
        expected
    }
    private fun expectedCheckpoint(sequence: Long): ExpectedCheckpoint {
        check(initialLength > 0 && regionBytes > 0 && sequence in 1..0xffff_ffffL &&
            sequence == (installed?.expected?.sequence ?: 0L) + 1) { "Checkpoint sequence is not the next frontier" }
        currentCapacity()
        return ExpectedCheckpoint(sequence, offset, regionBytes, initialLength, initialHash,
            prefixDigest(), recordingBinding, sourceFileBinding)
    }

    /** Cold envelope validation is mandatory before mutation AND after readback.
     * This hook must be supplied by the future SDK token owner, not AU callbacks. */
    fun installCheckpoint(sequence: Long, bytes: ByteArray,
        validate: (ByteArray, ExpectedCheckpoint) -> Unit): InstalledCheckpoint = operation {
        require(bytes.size in MIN_CHECKPOINT..MAX_CHECKPOINT)
        val expected = expectedCheckpoint(sequence)
        val stable = bytes.copyOf() // Bounded checkpoint metadata only.
        val metadataHash = digest(stable)
        validate(stable, expected)
        check(digest(stable) == metadataHash) { "Checkpoint validator mutated its input" }
        // Validators are arbitrary cold callbacks and may cancel the Writer.
        // Recheck actual ownership after them, then the scalar immediately
        // before each new mutation. Never execute callback/IO under a lock.
        revalidate(expected); currentCapacity(); checkAdmission()
        checkNotNull(media).sync()
        revalidate(expected); checkAdmission()
        val temp = root.create(temporary); metadata = temp
        regular(temp.stat(), 0)
        writeAll(temp, stable, 0); checkAdmission(); temp.sync()
        val tempNode = temp.stat(); regular(tempNode, stable.size.toLong())
        temp.close(); metadata = null
        root.verify()
        val namedTemp = root.inspect(temporary)
        check(namedTemp != null && tempNode.sameFile(namedTemp) && tempNode.bytes == namedTemp.bytes)
        val index = (sequence and 1).toInt()
        val prior = root.inspect(slots[index]); val known = slotNodes[index]
        check(if (known == null) prior == null else prior != null && known.sameFile(prior) && known.bytes == prior.bytes)
        revalidate(expected); checkAdmission()
        root.replace(temporary, slots[index]); checkAdmission(); root.sync()
        val readback = root.openRead(slots[index]); metadata = readback
        val actualNode = readback.stat(); regular(actualNode, stable.size.toLong())
        check(tempNode.sameFile(actualNode)) { "Published slot is not our temporary descriptor" }
        val actual = readExact(readback, stable.size)
        check(actual.contentEquals(stable) && digest(actual) == metadataHash) { "Checkpoint readback changed" }
        validate(actual, expected)
        check(digest(actual) == metadataHash) { "Checkpoint readback validator mutated its input" }
        revalidate(expected, index, actualNode)
        check(digest(readExact(readback, stable.size)) == metadataHash) { "Checkpoint bytes changed during validation" }
        readback.close(); metadata = null
        revalidate(expected, index, actualNode)
        val published = root.inspect(slots[index])
        check(published != null && actualNode.sameFile(published) && published.bytes == actualNode.bytes)
        check(root.inspect(temporary) == null)
        val result = InstalledCheckpoint.issue(owner, expected, metadataHash)
        // Linearisation: this final pure scalar check admits the immutable
        // progress receipt. Later cancellation fences future work. No IO or
        // arbitrary callback follows this check before publication/return.
        checkAdmission()
        slotNodes[index] = actualNode
        installed = result
        result
    }

    /** Optional same-owner terminal metadata replacement. Does not finalise app
     * admission. Caller must compare this header with the exact SDK token first. */
    fun finishStoredInputs(header: ByteArray, checkpoint: InstalledCheckpoint,
        validateHeader: (ByteArray, ExpectedCheckpoint) -> Unit): Closure {
        operation {
            check(checkpoint.belongsTo(owner) && installed === checkpoint && checkpoint.expected.committedOffset == offset)
            require(header.size == initialLength && initialLength > 0)
            val stable = header.copyOf()
            val expectedHeaderHash = digest(stable)
            validateHeader(stable, checkpoint.expected)
            check(digest(stable) == expectedHeaderHash) { "Final header validator mutated its input" }
            revalidate(checkpoint.expected)
            check(installed === checkpoint && root.inspect(temporary) == null)
            check(digest(readExact(checkNotNull(media), initialLength)) == initialHash) { "Original header changed" }
            revalidate(checkpoint.expected); checkAdmission()
            writeAll(checkNotNull(media), stable, 0); checkAdmission(); checkNotNull(media).sync()
            check(readExact(checkNotNull(media), stable.size).contentEquals(stable)) { "Final header readback changed" }
            regular(checkNotNull(media).stat(), offset)
            revalidate(checkpoint.expected)
            check(root.inspect(temporary) == null)
            // Same scalar progress linearisation as checkpoint publication.
            checkAdmission()
            rewritten = true
        }
        return closeIncomplete()
    }
    /** No raw descriptor retry after unknown close. This owner remains retained
     * by the catalogue if any metadata/media/root closure is unconfirmed. */
    fun closeIncomplete(): Closure {
        onWorker(); check(!busy) { "Checkpoint storage operation is still active" }
        closed?.let { return it }; closeFailure?.let { throw it }
        check(!closeAttempted); closeAttempted = true
        try {
            var error: Exception? = null
            try { metadata?.close(); metadata = null } catch (failure: Exception) { error = failure }
            try { media?.close(); media = null } catch (failure: Exception) { if (error == null) error = failure else error.addSuppressed(failure) }
            if (error == null) try { root.close() } catch (failure: Exception) { error = failure }
            error?.let { throw it }
            releaseLease()
            return Closure(offset, installed?.expected?.sequence ?: 0L, rewritten).also { closed = it }
        } catch (error: Exception) {
            closeFailure = error; retainUncertain(this); throw error
        }
    }
    private fun open() {
        try {
            check(root.identity.directory && !root.identity.regular)
            // A copyable digest is a runtime prerequisite, never assumed from a host test.
            check(prefixDigest() == digest(ByteArray(0)))
            currentCapacity()
            root.inspect(receiptName)?.let { regular(it); check(it.bytes in 1..4096) }
                ?: error("Existing writer receipt is unavailable")
            check(root.inspect(mediaName) == null)
            media = root.create(mediaName)
            val node = checkNotNull(media).stat(); regular(node, 0)
            val named = root.inspect(mediaName)
            check(named != null && node.sameFile(named) && named.bytes == 0L)
            mediaIdentity = node
            sourceFileBinding = digest("sapflow.android.checkpoint.file.v1\n${root.identity.device}\n${root.identity.inode}\n${node.device}\n${node.inode}\n$run".toByteArray(Charsets.UTF_8))
        } catch (error: Exception) {
            failed = true
            try { closeIncomplete() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
            throw error
        }
    }
    companion object {
        const val MAX_HEADER = 65_536
        const val MIN_CHECKPOINT = 545
        const val MAX_CHECKPOINT = 66_080
        const val MAX_FRAGMENT = 16 * 1024 * 1024
        const val MAX_ENTRIES = 1024
        const val MAX_SEGMENTS = 256
        const val STORAGE_FLOOR = 200L * 1024 * 1024 + 256L * 1024
        internal fun open(identity: RecordingCatalogue.Identity, root: Root, checkLease: () -> Unit,
            checkAdmission: () -> Unit, releaseLease: () -> Unit, retainUncertain: (Any) -> Unit): RecordingCheckpointStorage {
            val owner = try { RecordingCheckpointStorage(identity, root, checkLease, checkAdmission, releaseLease, retainUncertain) }
            catch (error: Exception) {
                try { root.close(); releaseLease() }
                catch (cleanup: Exception) { error.addSuppressed(cleanup); retainUncertain(root) }
                throw error
            }
            owner.open(); return owner
        }
        private fun digest(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).hex()
        private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }
    }
}
