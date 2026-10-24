package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.util.concurrent.atomic.AtomicReference

/** Worker-owned sequential file position. abort() may close the channel from
 * control work to interrupt a blocked write/force; it never flushes to disk. */
internal class RecordingFile(directory: Path, val filename: String) {
    private val channel: AtomicReference<FileChannel?>
    private enum class CloseDisposition { OPEN, IN_FLIGHT, CLOSED, UNKNOWN }
    private val coldCloseDisposition = AtomicReference(CloseDisposition.OPEN)
    // Exact channel retained after an unconfirmed physical close; no retry.
    private val retainedChannel = AtomicReference<FileChannel?>()
    private var headerBytes: Int? = null

    init {
        require(filename.matches(Regex("session-[a-zA-Z0-9-]+\\.mp4")))
        Files.createDirectories(directory)
        channel = AtomicReference(FileChannel.open(directory.resolve(filename), StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE))
    }

    fun write(bytes: ByteArray, rewriteHeader: Boolean) {
        require(bytes.isNotEmpty() && bytes.size <= 17 * 1024 * 1024) { "Recording write bound exceeded" }
        val target = checkNotNull(channel.get()) { "Recording file closed" }
        val appendPosition = target.position()
        if (rewriteHeader) {
            require(headerBytes == bytes.size) { "Recording header size changed" }
            target.position(0)
        } else if (headerBytes == null) headerBytes = bytes.size
        val source = ByteBuffer.wrap(bytes)
        while (source.hasRemaining()) check(target.write(source) > 0) { "Recording write made no progress" }
        if (rewriteHeader) target.position(appendPosition)
    }

    fun finish() {
        val target = checkNotNull(channel.get()) { "Recording file closed" }
        try { target.force(true) } finally { abort() }
    }

    fun abort() {
        val disposition = coldCloseDisposition.get()
        if (disposition == CloseDisposition.CLOSED) return
        check(disposition == CloseDisposition.OPEN &&
            coldCloseDisposition.compareAndSet(CloseDisposition.OPEN, CloseDisposition.IN_FLIGHT)) {
            "Recording file close is in flight or its ownership is unknown"
        }
        val target = channel.getAndSet(null)
        retainedChannel.set(target)
        try {
            target?.close()
            retainedChannel.set(null)
            coldCloseDisposition.set(CloseDisposition.CLOSED)
        } catch (error: Throwable) {
            coldCloseDisposition.set(CloseDisposition.UNKNOWN)
            throw error
        }
    }
}
