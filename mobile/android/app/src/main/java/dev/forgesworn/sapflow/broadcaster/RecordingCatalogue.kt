package dev.forgesworn.sapflow.broadcaster

import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.OpenOption
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.*
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermission.OWNER_READ
import java.nio.file.attribute.PosixFilePermission.OWNER_WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

/** Private local segments only. A finalised container is not a decoded or
 * published replay. No repair, deletion, upload or automatic orphan adoption.
 * All methods doing IO belong on a cold/recording worker, never a codec/UI callback. */
internal class RecordingCatalogue(directory: Path) {
    enum class Failure { BUSY, CAPACITY, UNSAFE, CHANGED, INELIGIBLE, CANCELLED, IO }
    class CatalogueFailure(val reason: Failure) : IllegalStateException("Private recording unavailable: ${reason.name}")
    enum class State { WRITING, FINALISED, INCOMPLETE, ORPHAN_UNVERIFIED }
    data class Identity(val segmentId: String, val showId: String, val segmentIndex: Long, val profileId: String) {
        init { requireUuid(segmentId); requireUuid(showId); require(segmentIndex >= 0); require(profileId == "UNSPECIFIED" || profileId == "AUDIO_ONLY_48K_MONO" || profileId in CaptureProfile.entries.map { it.name }) }
        val filename: String get() = "session-$segmentId.mp4"
    }
    class Show internal constructor(internal val intent: CaptureIntent) {
        val showId: String = UUID.randomUUID().toString()
        private val next = AtomicLong()
        fun nextIdentity(): Identity {
            val index = next.getAndUpdate { Math.addExact(it, 1) }
            return Identity(UUID.randomUUID().toString(), showId, index, intent.profileId)
        }
    }
    fun newShow(profile: CaptureProfile): Show = newShow(CaptureIntent.AudioVideo(profile))
    fun newShow(intent: CaptureIntent): Show = Show(intent)
    /** Opaque selection from a bounded scan. It grants no remote authority. */
    class Entry internal constructor(val segmentId: String, val identity: Identity?, val state: State,
        val bytes: Long, val durationMs: Long?, val createdAtMs: Long?, internal val stamp: String, internal val receiptStamp: String) {
        override fun toString() = "Private recording segment ($state)"
    }
    private data class Receipt(val identity: Identity, val state: String, val createdMs: Long, val bytes: Long?, val durationMs: Long?, val fileFingerprint: String? = null)
    private enum class Terminal { WRITING, CANCELLED, COMMITTING, CLOSED }
    private val root: Path
    private val directoryKey: String

    init {
        Files.createDirectories(directory)
        check(!Files.isSymbolicLink(directory)) { "Recording directory is unsafe" }
        root = directory.toRealPath()
        val attrs = Files.readAttributes(root, BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        check(attrs.isDirectory && attrs.fileKey() != null) { "Recording directory is unavailable" }
        directoryKey = attrs.fileKey().toString()
    }

    private fun checkRoot() {
        val attrs = Files.readAttributes(root, BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        check(attrs.isDirectory && attrs.fileKey()?.toString() == directoryKey) { "Recording directory changed" }
    }
    private fun path(id: String): Path { requireUuid(id); checkRoot(); return root.resolve("session-$id.mp4") }
    private fun receiptPath(id: String) = root.resolve("session-$id.receipt.json")
    private fun attributes(p: Path): BasicFileAttributes {
        val attrs = Files.readAttributes(p, BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        ensure(attrs.isRegularFile && attrs.fileKey() != null && !Files.isSymbolicLink(p), Failure.UNSAFE)
        ensure((Files.getAttribute(p, "unix:nlink", NOFOLLOW_LINKS) as Number).toLong() == 1L, Failure.UNSAFE)
        return attrs
    }
    private fun stamp(p: Path): Pair<Long, String> {
        val a = attributes(p)
        return a.size() to "${a.fileKey()}:${a.size()}:${a.lastModifiedTime()}"
    }
    private fun readSmall(p: Path, max: Int = MAX_RECEIPT): ByteArray {
        attributes(p)
        FileChannel.open(p, READ, NOFOLLOW_LINKS).use { channel ->
            check(channel.size() in 1..max.toLong()) { "Recording metadata exceeds its bound" }
            val result = ByteBuffer.allocate(max + 1)
            while (result.hasRemaining()) { val n = channel.read(result); if (n < 0) break; check(n > 0) }
            check(result.position() <= max) { "Recording metadata exceeds its bound" }
            return result.array().copyOf(result.position())
        }
    }
    private fun encode(r: Receipt): ByteArray = buildJsonObject {
        put("version", 1); put("segmentId", r.identity.segmentId); put("showId", r.identity.showId)
        put("segmentIndex", r.identity.segmentIndex); put("profileId", r.identity.profileId)
        put("createdAtMs", r.createdMs); put("state", r.state)
        r.bytes?.let { put("bytes", it) }; r.durationMs?.let { put("durationMs", it) }
        r.fileFingerprint?.let { put("fileFingerprint", it) }
    }.toString().toByteArray(Charsets.UTF_8)

    private fun decode(bytes: ByteArray, id: String): Receipt {
        // Our flat canonical format also refuses duplicate keys, nesting and
        // permissive UTF-8 replacement; arbitrary media never enters this parser.
        val text = bytes.toString(Charsets.UTF_8)
        require(text.toByteArray(Charsets.UTF_8).contentEquals(bytes) && text.count { it == '{' } == 1 && text.count { it == '}' } == 1)
        val o = Json.parseToJsonElement(text).jsonObject
        require(o.keys == setOf("version", "segmentId", "showId", "segmentIndex", "profileId", "createdAtMs", "state") ||
            o.keys == setOf("version", "segmentId", "showId", "segmentIndex", "profileId", "createdAtMs", "state", "bytes", "durationMs", "fileFingerprint"))
        fun number(key: String): Long { val p = o.getValue(key).jsonPrimitive; require(!p.isString); return requireNotNull(p.longOrNull) }
        fun string(key: String): String { val p = o.getValue(key).jsonPrimitive; require(p.isString); return p.content }
        require(number("version") == 1L)
        val identity = Identity(string("segmentId"), string("showId"), number("segmentIndex"), string("profileId"))
        require(identity.segmentId == id)
        val state = string("state"); require(state in setOf("writing", "finalised", "incomplete"))
        val created = number("createdAtMs"); require(created > 0)
        val size = o["bytes"]?.let { number("bytes").also { n -> require(n > 0) } }
        val duration = o["durationMs"]?.let { number("durationMs").also { n -> require(n >= 0) } }
        val fingerprint = o["fileFingerprint"]?.let { string("fileFingerprint").also { s -> require(s.matches(Regex("[0-9a-f]{64}"))) } }
        require((size != null) == (duration != null) && (size != null) == (fingerprint != null) && (state != "finalised" || size != null))
        val result = Receipt(identity, state, created, size, duration, fingerprint)
        require(encode(result).contentEquals(bytes))
        return result
    }

    private fun writeReceipt(r: Receipt) {
        checkRoot()
        val target = receiptPath(r.identity.segmentId)
        if (Files.exists(target, NOFOLLOW_LINKS)) attributes(target)
        val temporary = root.resolve(".receipt-${UUID.randomUUID()}.tmp")
        try {
            val bytes = encode(r); check(bytes.size <= MAX_RECEIPT)
            // Request private permissions atomically at creation, independent
            // of whether the process umask would remove group/other access.
            // An unsupported provider fails closed; there is no weaker open.
            val privateMode = PosixFilePermissions.asFileAttribute(setOf(OWNER_READ, OWNER_WRITE))
            FileChannel.open(temporary, setOf<OpenOption>(CREATE_NEW, WRITE, NOFOLLOW_LINKS), privateMode).use { c ->
                val b = ByteBuffer.wrap(bytes); while (b.hasRemaining()) check(c.write(b) > 0); c.force(true)
            }
            Files.move(temporary, target, ATOMIC_MOVE, REPLACE_EXISTING)
            FileChannel.open(root, READ, NOFOLLOW_LINKS).use { it.force(true) }
            check(readSmall(target).contentEquals(bytes)) { "Recording receipt was not confirmed" }
        } finally { Files.deleteIfExists(temporary) }
    }

    private class ProcessOwner(val label: String) { var channel: FileChannel? = null; @Volatile var closeFailed = false; var lockKey: String? = null; var retainedResource: Any? = null }
    private class Lease(val channel: FileChannel, val lock: FileLock, private val key: String, private val process: ProcessOwner) : AutoCloseable {
        private var closeFailure: Exception? = null
        @Volatile private var closedReceipt = false
        val cleanupComplete: Boolean get() = closedReceipt
        fun markUncertain(resource: Any? = null) { process.closeFailed = true; if (resource != null) process.retainedResource = resource }
        @Synchronized override fun close() {
            if (closedReceipt) return
            closeFailure?.let { throw it }
            var error: Exception? = null
            try { if (lock.isValid) lock.release() } catch (failure: Exception) { error = failure }
            try { channel.close() } catch (failure: Exception) { if (error == null) error = failure else error.addSuppressed(failure) }
            closeFailure = error; process.closeFailed = error != null; error?.let { throw it }
            if (channel.isOpen || lock.isValid) {
                val unknown = CatalogueFailure(Failure.IO); closeFailure = unknown; process.closeFailed = true; throw unknown
            }
            closedReceipt = true
            synchronized(processOwners) { if (processOwners[key] === process) processOwners.remove(key) }
        }
    }
    private fun lease(label: String): Lease {
        checkRoot()
        // POSIX record locks can disappear when ANY same-process descriptor for
        // their inode closes. Reserve before opening, and never probe/reopen an
        // inode with a local owner. The registry is bounded and retains failed
        // actual closure; Java FileLock.isValid alone is not sufficient proof.
        val process = ProcessOwner(label)
        synchronized(processOwners) {
            ensure(!processOwners.containsKey(directoryKey), Failure.BUSY)
            ensure(processOwners.size < MAX_PROCESS_OWNERS, Failure.CAPACITY)
            processOwners[directoryKey] = process
        }
        val p = root.resolve(LOCK_NAME)
        var channel: FileChannel? = null
        try {
            // Refuse a renamed/aliased locally held lock BEFORE opening it.
            // Merely detecting overlap after open and then closing would drop
            // the earlier process's POSIX lock too.
            val before = if (Files.exists(p, NOFOLLOW_LINKS)) attributes(p).fileKey().toString() else null
            synchronized(processOwners) {
                ensure(before == null || processOwners.values.none { it !== process && it.lockKey == before }, Failure.BUSY)
                process.lockKey = before
            }
            val c = FileChannel.open(p, CREATE, READ, WRITE, NOFOLLOW_LINKS)
            channel = c; process.channel = c
            val after = attributes(p).fileKey().toString()
            synchronized(processOwners) {
                if (processOwners.values.any { it !== process && it.lockKey == after }) {
                    // An unexpected namespace race opened an already-held
                    // inode. Retain this FD; closing it would revoke its peer.
                    process.closeFailed = true
                    throw CatalogueFailure(Failure.UNSAFE)
                }
                ensure(before == null || before == after, Failure.CHANGED)
                process.lockKey = after
            }
            val lock = try { c.tryLock() } catch (_: OverlappingFileLockException) { null }
            ensure(lock != null, Failure.BUSY)
            checkNotNull(lock)
            val result = Lease(c, lock, directoryKey, process)
            try {
                c.truncate(0); val b = ByteBuffer.wrap(label.toByteArray(Charsets.US_ASCII))
                while (b.hasRemaining()) check(c.write(b) > 0)
                c.force(true); return result
            } catch (error: Exception) { result.close(); throw error }
        } catch (error: Exception) {
            var cleaned = true
            try { if (!process.closeFailed) channel?.close() } catch (failure: Exception) { error.addSuppressed(failure); cleaned = false; process.closeFailed = true }
            if (cleaned && !process.closeFailed) synchronized(processOwners) { if (processOwners[directoryKey] === process) processOwners.remove(directoryKey) }
            throw error
        }
    }
    private fun activeWriter(): String? {
        val text = synchronized(processOwners) { processOwners[directoryKey]?.label } ?: return null
        // A foreign process's writing receipt is unconfirmed, not finalised.
        // Export must obtain the real cross-process lock before touching media.
        return text.removePrefix("write:").takeIf { text.startsWith("write:") && UUID_PATTERN.matches(it) }
    }
    /** Cold scalar only: includes failed/unknown cleanup even when no lease
     * could be returned to the caller. Never probes the lock inode. */
    val cleanupRetained: Boolean get() = synchronized(processOwners) { processOwners.containsKey(directoryKey) }
    /** Unlike cleanupRetained, a healthy live recorder is not an IO failure. */
    val cleanupUncertain: Boolean get() = synchronized(processOwners) { processOwners[directoryKey]?.closeFailed == true }

    /** Cold preflight only: no segment/receipt creation and no reservation
     * across Prepare. The recorder rechecks under its own writer lease. */
    fun checkRecordingCapacity() {
        val held = lease("preflight")
        try { ensure(scan().size < MAX_SEGMENTS, Failure.CAPACITY) }
        finally { held.close() }
    }

    fun begin(identity: Identity, createdAtMs: Long = System.currentTimeMillis()): Writer {
        require(createdAtMs > 0)
        val held = lease("write:${identity.segmentId}")
        try {
            ensure(scan().size < MAX_SEGMENTS, Failure.CAPACITY)
            check(!Files.exists(path(identity.segmentId), NOFOLLOW_LINKS) && !Files.exists(receiptPath(identity.segmentId), NOFOLLOW_LINKS)) { "Recording identity already exists" }
            val receipt = Receipt(identity, "writing", createdAtMs, null, null)
            writeReceipt(receipt)
            return Writer(identity, createdAtMs, { held.close() }, { held.cleanupComplete }, { held.markUncertain(it) })
        } catch (error: Exception) { held.close(); throw error }
    }

    inner class Writer internal constructor(identity: Identity, createdAtMs: Long,
        private val release: () -> Unit, private val released: () -> Boolean, private val uncertain: (Any?) -> Unit) {
        private val receipt = Receipt(identity, "writing", createdAtMs, null, null)
        private val terminal = AtomicReference(Terminal.WRITING)
        private val compound = AtomicBoolean(false)
        private val compoundGuard = Any()
        val filename: String get() = receipt.identity.filename
        val cleanupComplete: Boolean get() = terminal.get() == Terminal.CLOSED && released()
        /** Nonblocking cancellation. The caller separately proves file/mux owner closure. */
        fun cancel() { terminal.compareAndSet(Terminal.WRITING, Terminal.CANCELLED) }
        fun retainUncertainCleanup() { uncertain(null) }
        /** Optional cold child of THIS writer lease. The current recorder does
         * not call this; no independent reserve owner or worker is introduced. */
        internal fun checkpointStorage(
            factory: (Path, (Any) -> Unit) -> RecordingCheckpointStorage.Root = { path, retain -> AndroidRecordingStorageIO.open(path, retain) },
        ): RecordingCheckpointStorage {
            synchronized(compoundGuard) {
                check(terminal.get() == Terminal.WRITING && compound.compareAndSet(false, true))
            }
            var io: RecordingCheckpointStorage.Root? = null
            var transferred = false
            try {
                checkRoot()
                val opened = factory(root) { resource -> uncertain(resource) }; io = opened
                checkRoot(); check(terminal.get() == Terminal.WRITING)
                transferred = true
                return RecordingCheckpointStorage.open(receipt.identity, opened,
                    checkLease = { check(terminal.get() == Terminal.WRITING); checkRoot() },
                    checkAdmission = { check(terminal.get() == Terminal.WRITING) },
                    releaseLease = {
                        synchronized(compoundGuard) {
                            check(compound.compareAndSet(true, false)); cancel()
                        }
                        ownersClosed()
                    },
                    retainUncertain = { resource -> uncertain(resource) })
            } catch (error: Exception) {
                if (!transferred) {
                    var safe = !cleanupUncertain
                    try { io?.close() } catch (cleanup: Exception) { error.addSuppressed(cleanup); uncertain(io); safe = false }
                    if (safe) {
                        synchronized(compoundGuard) { compound.set(false); cancel() }
                        try { ownersClosed() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
                    }
                }
                throw error
            }
        }
        /** Called only after actual mux finish/drain and file force/close. */
        fun finalised(durationMs: Long) {
            require(durationMs >= 0)
            synchronized(compoundGuard) {
                check(!compound.get()) { "Checkpoint child still owns this lease" }
                check(terminal.compareAndSet(Terminal.WRITING, Terminal.COMMITTING)) { "Recording was cancelled" }
            }
            try {
                val file = stamp(path(receipt.identity.segmentId)); check(file.first > 0)
                writeReceipt(receipt.copy(state = "finalised", bytes = file.first, durationMs = durationMs, fileFingerprint = digest(file.second.toByteArray(Charsets.UTF_8))))
            } finally { terminal.set(Terminal.CLOSED); release() }
        }
        /** No metadata rewrite on the abort path. A released writing receipt
         * scans as incomplete; an uncertain owner must retain this lease. */
        fun ownersClosed() {
            val shouldRelease = synchronized(compoundGuard) {
                check(!compound.get()) { "Checkpoint child still owns this lease" }
                cancel()
                terminal.compareAndSet(Terminal.CANCELLED, Terminal.CLOSED) || terminal.get() == Terminal.CLOSED
            }
            if (shouldRelease) release()
        }
    }

    fun scan(): List<Entry> {
        checkRoot()
        val ids = sortedSetOf<String>()
        Files.newDirectoryStream(root).use { entries ->
            var count = 0
            for (p in entries) {
                ensure(++count <= MAX_DIRECTORY_ENTRIES, Failure.CAPACITY)
                val name = p.fileName.toString()
                val id = when { name.endsWith(".receipt.json") -> name.removePrefix("session-").removeSuffix(".receipt.json")
                    name.endsWith(".mp4") -> name.removePrefix("session-").removeSuffix(".mp4")
                    else -> continue }
                if (!name.startsWith("session-") || !UUID_PATTERN.matches(id)) continue
                ids.add(id); ensure(ids.size <= MAX_SEGMENTS, Failure.CAPACITY)
            }
        }
        val active = activeWriter()
        return ids.map { entry(it, active) }
    }
    private fun entry(id: String, active: String?): Entry {
        val p = path(id)
        val file = if (Files.exists(p, NOFOLLOW_LINKS)) stamp(p) else 0L to "missing"
        val metadata = receiptPath(id)
        val metadataStamp = if (Files.exists(metadata, NOFOLLOW_LINKS)) stamp(metadata).second else "missing"
        val bytes = try { readSmall(metadata) } catch (_: java.nio.file.NoSuchFileException) { null }
            catch (_: IllegalArgumentException) { null } catch (_: IllegalStateException) { null }
        val receipt = bytes?.let { runCatching { decode(it, id) }.getOrNull() }
        val state = when {
            receipt == null -> State.ORPHAN_UNVERIFIED
            receipt.state == "writing" && active == id -> State.WRITING
            file.first == 0L -> State.ORPHAN_UNVERIFIED
            receipt.state == "finalised" && receipt.bytes == file.first && receipt.fileFingerprint == digest(file.second.toByteArray(Charsets.UTF_8)) -> State.FINALISED
            receipt.state == "finalised" -> State.ORPHAN_UNVERIFIED
            else -> State.INCOMPLETE
        }
        return Entry(id, receipt?.identity, state, file.first, receipt?.durationMs, receipt?.createdMs, file.second,
            digest((metadataStamp + ":" + (bytes?.let(::digest) ?: "unverified")).toByteArray(Charsets.UTF_8)))
    }

    /** Own this lease through the complete copy and close it in finally/use.
     * Document-provider IO may block: the UI owner supplies cancellation/close. */
    fun openExport(selected: Entry, allowUnverified: Boolean = false): Export {
        ensure(selected.state != State.WRITING && selected.bytes > 0 && (allowUnverified || selected.state == State.FINALISED), Failure.INELIGIBLE)
        val held = lease("export:${selected.segmentId}")
        var inputOwned = false
        try {
            val current = entry(selected.segmentId, null); checkSame(selected, current)
            val input = FileChannel.open(path(selected.segmentId), READ, NOFOLLOW_LINKS)
            val export = Export(selected, input, { held.close() }, { held.cleanupComplete }, onUncertain = { held.markUncertain() })
            inputOwned = true
            try { check(input.size() == selected.bytes); checkSame(selected, entry(selected.segmentId, null)); return export }
            catch (error: Exception) {
                try { export.close() } catch (failure: Exception) { error.addSuppressed(failure) }
                // An uncertain input close retains the shared lease too.
                if (!export.cleanupComplete) throw CatalogueFailure(Failure.IO)
                throw error
            }
        } catch (error: Exception) { if (!inputOwned) held.close(); throw error }
    }
    private fun checkSame(a: Entry, b: Entry) {
        ensure(a.segmentId == b.segmentId && a.identity == b.identity && a.state == b.state && a.bytes == b.bytes && a.stamp == b.stamp && a.receiptStamp == b.receiptStamp, Failure.CHANGED)
    }
    inner class Export internal constructor(private val entry: Entry, private val input: FileChannel,
        private val release: () -> Unit, private val released: () -> Boolean,
        private val closeInput: () -> Unit = { input.close() }, private val onUncertain: () -> Unit = {}) : AutoCloseable {
        private val copied = AtomicBoolean(false)
        private var closeFailure: Exception? = null
        @Volatile private var closedReceipt = false
        val cleanupComplete: Boolean get() = closedReceipt
        fun copyTo(output: OutputStream, cancelled: () -> Boolean = { false }): Long {
            check(copied.compareAndSet(false, true)) { "Recording export already attempted" }
            val buffer = ByteBuffer.allocate(64 * 1024); var total = 0L
            while (total < entry.bytes) {
                ensure(!cancelled(), Failure.CANCELLED)
                buffer.clear(); buffer.limit(minOf(buffer.capacity().toLong(), entry.bytes - total).toInt())
                val n = input.read(buffer); check(n > 0) { "Recording export was truncated" }
                output.write(buffer.array(), 0, n); total += n
            }
            check(!cancelled() && input.read(ByteBuffer.allocate(1)) < 0) { "Recording grew during export" }
            checkSame(entry, this@RecordingCatalogue.entry(entry.segmentId, null))
            return total
        }
        @Synchronized override fun close() {
            if (closedReceipt) return
            closeFailure?.let { throw it }
            var error: Exception? = null
            try { closeInput() } catch (failure: Exception) { error = failure }
            if (error == null && input.isOpen) error = CatalogueFailure(Failure.IO)
            // Never admit a new writer while actual input closure is unknown.
            if (error == null) try { release() } catch (failure: Exception) { error = failure }
            closeFailure = error; error?.let { onUncertain(); throw it }
            if (input.isOpen || !released()) { val unknown = CatalogueFailure(Failure.IO); closeFailure = unknown; onUncertain(); throw unknown }
            closedReceipt = true
        }
    }
    companion object {
        private const val MAX_RECEIPT = 4096
        private const val MAX_SEGMENTS = 256
        private const val MAX_DIRECTORY_ENTRIES = 1024
        private const val LOCK_NAME = ".recording-writer.lock"
        private const val MAX_PROCESS_OWNERS = 16
        private val processOwners = mutableMapOf<String, ProcessOwner>()
        private val UUID_PATTERN = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        private fun requireUuid(id: String) { require(UUID_PATTERN.matches(id)) }
        private fun ensure(condition: Boolean, failure: Failure) { if (!condition) throw CatalogueFailure(failure) }
        fun failureReason(error: Exception): Failure = (error as? CatalogueFailure)?.reason ?: Failure.IO
        private fun digest(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}
