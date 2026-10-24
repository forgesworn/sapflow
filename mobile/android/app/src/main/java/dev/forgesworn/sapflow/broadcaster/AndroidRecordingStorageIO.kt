package dev.forgesworn.sapflow.broadcaster

import android.os.ParcelFileDescriptor
import android.os.Build
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructStat
import java.io.FileDescriptor
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/** Android public-API descriptor backend. No pathname/NIO IO fallback. The
 * /proc root bridge must pass every runtime identity check. Its Android/SELinux
 * behaviour needs separate qualification; host policy tests do not prove it.
 * Directory enumeration alone uses a bounded stream of the retained FD path. */
internal class AndroidRecordingStorageIO private constructor(
    private val namedRoot: Path, private val retain: (Any) -> Unit,
) : RecordingCheckpointStorage.Root {
    private val worker = Thread.currentThread()
    private var bootstrap: FileDescriptor? = null
    private var bootstrapCloseAttempted = false
    private var parcel: ParcelFileDescriptor? = null
    // Keep the raw wrapper and numeric context reachable BEFORE detach. A
    // throwing/unsupported transfer cannot leave ownership only in locals.
    private var bridgeAlias: FileDescriptor? = null
    private var bridgeNumber = -1
    private var bridgeDetachedNumber = -1
    private var bridgeTransferAttempted = false
    private var descriptor: FileDescriptor? = null
    private var number = -1
    private var originalIdentity: RecordingCheckpointStorage.Node? = null
    private var closeAttempted = false
    private var closeFailure: Exception? = null
    private var uncertain = false
    private var uncertainEnumeration: Any? = null
    private val files = mutableSetOf<OwnedFile>() // At most media + one metadata owner.
    override val identity get() = checkNotNull(originalIdentity)
    private val anchor get() = "/proc/self/fd/$number"

    private fun worker() { check(Thread.currentThread() === worker) { "Storage descriptor has another worker" } }
    private fun quarantine(error: Exception): Nothing {
        uncertain = true; retain(this); throw error
    }
    private fun initialise() {
        if (Build.VERSION.SDK_INT < 30) throw UnsupportedOperationException("Checked descriptor storage requires public API30 fd flag inspection")
        try {
            val beforeStat = Os.lstat(namedRoot.toString())
            check(OsConstants.S_ISDIR(beforeStat.st_mode) && beforeStat.st_uid == Os.getuid())
            val before = node(beforeStat)
            check(before.directory && !before.regular)
            val original = Os.open(namedRoot.toString(), OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW or OsConstants.O_CLOEXEC, 0)
            bootstrap = original
            closeOnExec(original)
            check(before.sameFile(node(Os.fstat(original)))) { "Recording root changed while opening" }
            check(before.sameFile(node(Os.lstat(namedRoot.toString()))))
            // Explicit opt-in provisioning under the existing Writer lease.
            // Tighten the actual retained directory FD, never a replacement path.
            // Existing legacy catalogue creation may have inherited mode0755.
            Os.fchmod(original, 448) // 0700
            val provisioned = Os.fstat(original); privateOwner(provisioned)
            check(provisioned.st_mode and 511 == 448)
            val owned = ParcelFileDescriptor.dup(original); parcel = owned
            val n = owned.fd; bridgeNumber = n; check(n >= 0)
            val borrowed = owned.fileDescriptor
            // dup2(fd,fd) is a no-op: no descriptor is closed or replaced.
            // AOSP returns a distinct Java FD wrapper. Check that assumption
            // BEFORE detach, then transfer ownership away from PFD's quiet close.
            val alias = Os.dup2(borrowed, n)
            bridgeAlias = alias
            check(alias !== borrowed && alias.valid()) { "Public descriptor bridge is unsupported" }
            check(before.sameFile(node(Os.fstat(alias))))
            closeOnExec(alias)
            bridgeTransferAttempted = true
            val detached = owned.detachFd()
            bridgeDetachedNumber = detached
            check(detached == n && alias.valid()) { "Descriptor ownership transfer failed" }
            parcel = null; descriptor = alias; number = n
            bridgeAlias = null; bridgeNumber = -1; bridgeDetachedNumber = -1
            bootstrapCloseAttempted = true
            Os.close(original) // Exactly one checked close, never PFD.close().
            bootstrap = null
            originalIdentity = node(Os.fstat(alias))
            verify()
        } catch (error: Exception) {
            // An untransferred PFD cannot supply a checked close receipt. Keep
            // it reachable; never quietly close then release catalogue authority.
            var cleanup: Exception? = null
            if (bootstrap != null && bootstrapCloseAttempted) uncertain = true
            if (parcel != null) uncertain = true
            if (bridgeAlias != null || bridgeTransferAttempted && number < 0) uncertain = true
            if (!bootstrapCloseAttempted) bootstrap?.let {
                bootstrapCloseAttempted = true
                try { Os.close(it); bootstrap = null } catch (failure: Exception) { cleanup = failure; uncertain = true }
            }
            if (descriptor != null) {
                closeAttempted = true
                try { Os.close(checkNotNull(descriptor)); descriptor = null } catch (failure: Exception) { cleanup = failure; uncertain = true }
            }
            cleanup?.let(error::addSuppressed)
            if (uncertain) retain(this)
            throw error
        }
    }
    override fun verify() {
        worker(); check(!closeAttempted && !uncertain && number >= 0)
        val actualStat = Os.fstat(checkNotNull(descriptor)); privateOwner(actualStat)
        closeOnExec(checkNotNull(descriptor))
        check(actualStat.st_mode and 511 == 448) { "Retained recording root is not mode0700" }
        val actual = node(actualStat)
        check(identity.sameFile(actual) && actual.directory && !actual.regular)
        // Original namespace changes fail closed. Child IO is anchored to the
        // retained descriptor, not to this potentially replaced pathname.
        check(actual.sameFile(node(Os.lstat(namedRoot.toString())))) { "Named recording root changed" }
        check(actual.sameFile(node(Os.stat(anchor)))) { "Retained root proc access is unavailable" }
    }
    private fun child(name: String): String {
        require(name.matches(Regex("[a-zA-Z0-9._-]{1,96}")) && name != "." && name != "..")
        return "$anchor/$name"
    }
    override fun names(limit: Int): List<String> {
        verify(); require(limit in 1..RecordingCheckpointStorage.MAX_ENTRIES)
        val answer = ArrayList<String>()
        val stream = Files.newDirectoryStream(Paths.get(anchor))
        var problem: Exception? = null
        try {
            for (path in stream) {
                check(answer.size < limit) { "Recording directory exceeds its bound" }
                answer.add(path.fileName.toString())
            }
        } catch (error: Exception) { problem = error; throw error }
        finally {
            try { stream.close() }
            catch (error: Exception) {
                uncertainEnumeration = stream
                problem?.addSuppressed(error)
                quarantine(error)
            }
        }
        verify(); return answer
    }
    override fun inspect(name: String): RecordingCheckpointStorage.Node? {
        verify()
        return try { Os.lstat(child(name)).let { privateOwner(it); node(it) } }
        catch (error: ErrnoException) { if (error.errno == OsConstants.ENOENT) null else throw error }
    }
    private fun open(name: String, create: Boolean): RecordingCheckpointStorage.File {
        verify(); check(files.size < 2) { "Descriptor ownership bound exceeded" }
        val flags = (if (create) OsConstants.O_RDWR or OsConstants.O_CREAT or OsConstants.O_EXCL else OsConstants.O_RDONLY) or
            OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW
        val fd = Os.open(child(name), flags, if (create) 384 else 0) // 0600
        val owned = OwnedFile(fd, name); files.add(owned)
        try {
            val actual = owned.stat()
            closeOnExec(fd)
            check(actual.regular && !actual.directory && actual.links == 1L && (!create || actual.bytes == 0L))
            val named = inspect(name); check(named != null && actual.sameFile(named) && actual.bytes == named.bytes)
            return owned
        } catch (error: Exception) {
            try { owned.close() } catch (cleanup: Exception) { error.addSuppressed(cleanup) }
            throw error
        }
    }
    override fun create(name: String) = open(name, true)
    override fun openRead(name: String) = open(name, false)
    override fun replace(source: String, destination: String) {
        verify(); Os.rename(child(source), child(destination)); verify()
    }
    override fun sync() { verify(); Os.fsync(checkNotNull(descriptor)); verify() }
    override fun availableBytes(): Long {
        verify(); val value = Os.fstatvfs(checkNotNull(descriptor))
        check(value.f_bavail >= 0 && value.f_frsize > 0)
        return Math.multiplyExact(value.f_bavail, value.f_frsize)
    }
    override fun close() {
        worker(); closeFailure?.let { throw it }
        if (closeAttempted) return
        if (uncertain || files.isNotEmpty()) quarantine(IllegalStateException("Storage descriptor cleanup remains unknown"))
        closeAttempted = true
        try { Os.close(checkNotNull(descriptor)); descriptor = null }
        catch (error: Exception) { closeFailure = error; quarantine(error) }
        // Never fstat/probe/retry a closed numeric descriptor: it may be reused.
    }
    private inner class OwnedFile(private val fd: FileDescriptor, private val name: String) : RecordingCheckpointStorage.File {
        private var attempted = false
        private var failure: Exception? = null
        private fun usable() { verify(); check(!attempted && fd.valid()) }
        override fun stat(): RecordingCheckpointStorage.Node {
            usable(); val actual = Os.fstat(fd); privateOwner(actual); return node(actual)
        }
        override fun read(bytes: ByteArray, offset: Int, count: Int, position: Long): Int {
            usable(); require(offset >= 0 && count > 0 && offset <= bytes.size - count && position >= 0)
            return Os.pread(fd, bytes, offset, count, position)
        }
        override fun write(bytes: ByteArray, offset: Int, count: Int, position: Long): Int {
            usable(); require(offset >= 0 && count > 0 && offset <= bytes.size - count && position >= 0)
            return Os.pwrite(fd, bytes, offset, count, position)
        }
        override fun sync() { usable(); Os.fsync(fd) }
        override fun close() {
            worker(); failure?.let { throw it }; if (attempted) return
            attempted = true
            try { Os.close(fd); files.remove(this) }
            catch (error: Exception) { failure = error; quarantine(error) }
        }
    }
    companion object {
        fun open(root: Path, retain: (Any) -> Unit): AndroidRecordingStorageIO =
            AndroidRecordingStorageIO(root, retain).also { it.initialise() }
        private fun node(stat: StructStat) = RecordingCheckpointStorage.Node(stat.st_dev, stat.st_ino,
            OsConstants.S_ISDIR(stat.st_mode), OsConstants.S_ISREG(stat.st_mode), stat.st_nlink, stat.st_size)
        private fun privateOwner(stat: StructStat) {
            check(stat.st_uid == Os.getuid() && stat.st_mode and 63 == 0) { "Recording storage is not private to this app" }
        }
        private fun closeOnExec(fd: FileDescriptor) {
            if (Build.VERSION.SDK_INT < 30) throw UnsupportedOperationException("Public fd flag inspection is unavailable")
            check(Os.fcntlInt(fd, OsConstants.F_GETFD, 0) and OsConstants.FD_CLOEXEC != 0) { "Storage descriptor is not close-on-exec" }
        }
    }
}
