package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.ReadableByteChannel
import java.nio.channels.WritableByteChannel
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import java.nio.file.Files
import java.nio.file.FileAlreadyExistsException
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class RecordingFileTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun finalHeaderRewritePreservesEveryFragmentAndAppendPosition() {
        val directory = temporary.root.toPath()
        val output = RecordingFile(directory, "session-test.mp4")
        output.write(ByteArray(32) { 1 }, false)
        val fragment = ByteArray(100) { it.toByte() }
        output.write(fragment, false)
        val finalHeader = ByteArray(32) { 2 }
        output.write(finalHeader, true)
        output.write(byteArrayOf(9), false)
        output.finish()
        assertArrayEquals(finalHeader + fragment + byteArrayOf(9), Files.readAllBytes(directory.resolve("session-test.mp4")))
        assertThrows(IllegalStateException::class.java) { output.write(byteArrayOf(1), false) }
    }

    @Test fun exclusiveCreationAndHeaderIdentityProtectTheExistingPrefix() {
        val directory = temporary.root.toPath()
        val output = RecordingFile(directory, "session-test.mp4")
        val header = ByteArray(32) { 3 }
        output.write(header, false)
        assertThrows(FileAlreadyExistsException::class.java) { RecordingFile(directory, "session-test.mp4") }
        assertThrows(IllegalArgumentException::class.java) { output.write(ByteArray(31), true) }
        output.abort()
        output.abort()
        assertArrayEquals(header, Files.readAllBytes(directory.resolve("session-test.mp4")))
    }
    @Suppress("UNCHECKED_CAST")
    private fun channelReference(file: RecordingFile, name: String): AtomicReference<FileChannel?> =
        RecordingFile::class.java.getDeclaredField(name).let { field ->
            field.isAccessible = true
            field.get(file) as AtomicReference<FileChannel?>
        }

    private class HeldUnconfirmedClose(private val actual: FileChannel) : FileChannel() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val attempts = AtomicInteger()
        override fun implCloseChannel() {
            attempts.incrementAndGet()
            entered.countDown()
            try { check(release.await(5, TimeUnit.SECONDS)) { "Close fixture was not released" } }
            finally { actual.close() }
            throw AssertionError("Injected unconfirmed close return")
        }
        override fun read(dst: ByteBuffer) = actual.read(dst)
        override fun read(dsts: Array<out ByteBuffer>, offset: Int, length: Int) = actual.read(dsts, offset, length)
        override fun read(dst: ByteBuffer, position: Long) = actual.read(dst, position)
        override fun write(src: ByteBuffer) = actual.write(src)
        override fun write(srcs: Array<out ByteBuffer>, offset: Int, length: Int) = actual.write(srcs, offset, length)
        override fun write(src: ByteBuffer, position: Long) = actual.write(src, position)
        override fun position() = actual.position()
        override fun position(newPosition: Long): FileChannel { actual.position(newPosition); return this }
        override fun size() = actual.size()
        override fun truncate(size: Long): FileChannel { actual.truncate(size); return this }
        override fun force(metaData: Boolean) = actual.force(metaData)
        override fun transferTo(position: Long, count: Long, target: WritableByteChannel) = actual.transferTo(position, count, target)
        override fun transferFrom(src: ReadableByteChannel, position: Long, count: Long) = actual.transferFrom(src, position, count)
        override fun map(mode: MapMode, position: Long, size: Long): MappedByteBuffer = actual.map(mode, position, size)
        override fun lock(position: Long, size: Long, shared: Boolean): FileLock = actual.lock(position, size, shared)
        override fun tryLock(position: Long, size: Long, shared: Boolean): FileLock? = actual.tryLock(position, size, shared)
    }

    @Test fun concurrentAbortCannotClaimWorkerCloseOrRetryAnUnknownChannel() {
        val output = RecordingFile(temporary.root.toPath(), "session-close-race.mp4")
        val bytes = byteArrayOf(2, 4, 6)
        output.write(bytes, false)
        val channel = channelReference(output, "channel")
        val actual = checkNotNull(channel.get())
        val held = HeldUnconfirmedClose(actual)
        channel.set(held)
        val failure = AtomicReference<Throwable?>()
        val worker = Thread({
            try { output.finish() } catch (error: Throwable) { failure.set(error) }
        }, "recording-file-close-test")
        worker.start()
        try {
            assertTrue(held.entered.await(5, TimeUnit.SECONDS))
            assertNull(channel.get())
            assertSame(held, channelReference(output, "retainedChannel").get())
            assertThrows(IllegalStateException::class.java) { output.abort() }
            assertEquals(1, held.attempts.get())
            assertTrue(actual.isOpen)
        } finally {
            held.release.countDown()
            worker.join(5000)
        }
        assertFalse(worker.isAlive)
        assertTrue(failure.get() is AssertionError)
        assertFalse(actual.isOpen)
        assertSame(held, channelReference(output, "retainedChannel").get())
        assertThrows(IllegalStateException::class.java) { output.abort() }
        assertEquals(1, held.attempts.get())
        assertArrayEquals(bytes, Files.readAllBytes(temporary.root.toPath().resolve("session-close-race.mp4")))
    }

}
