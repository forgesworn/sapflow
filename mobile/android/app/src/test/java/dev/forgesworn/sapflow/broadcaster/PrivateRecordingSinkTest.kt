// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.ReadableByteChannel
import java.nio.channels.WritableByteChannel
import java.nio.file.Files
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** Actual file/catalogue/sink closure, with no Rust/native muxer creation. */
class PrivateRecordingSinkTest {
    @get:Rule val temporary = TemporaryFolder()

    @Suppress("UNCHECKED_CAST")
    private fun <T> reference(owner: Any, name: String): AtomicReference<T> =
        owner.javaClass.getDeclaredField(name).let { field ->
            field.isAccessible = true
            field.get(owner) as AtomicReference<T>
        }

    private class UnconfirmedClose(private val actual: FileChannel) : FileChannel() {
        val attempts = AtomicInteger()
        override fun implCloseChannel() {
            attempts.incrementAndGet()
            actual.close()
            throw AssertionError("Injected unconfirmed file-close return")
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

    @Test fun unconfirmedActualFileAbortRetainsLeaseWithoutRetryOrFinalisedReceipt() {
        val root = temporary.root.toPath().resolve("private-recordings")
        val catalogue = RecordingCatalogue(root)
        val identity = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, CaptureProfile.DEFAULT.name)
        val writer = catalogue.begin(identity)
        val file = RecordingFile(root, writer.filename)
        val bytes = byteArrayOf(1, 2, 3, 4)
        file.write(bytes, false)
        val channel = reference<FileChannel?>(file, "channel")
        val actual = checkNotNull(channel.get())
        val fault = UnconfirmedClose(actual)
        channel.set(fault)
        val sink = PrivateRecordingSink(root, catalogue)
        reference<RecordingFile?>(sink, "file").set(file)
        reference<RecordingCatalogue.Writer?>(sink, "receipt").set(writer)

        assertThrows(IllegalStateException::class.java) { sink.close() }
        assertEquals(1, fault.attempts.get())
        assertFalse(actual.isOpen)
        assertNull(channel.get()) // The real abort has already removed it.
        assertSame(file, reference<RecordingFile?>(sink, "file").get())
        assertTrue(catalogue.cleanupRetained)
        assertTrue(catalogue.cleanupUncertain)
        assertFalse(writer.cleanupComplete)

        assertThrows(IllegalStateException::class.java) { sink.close() }
        assertEquals(1, fault.attempts.get())
        assertTrue(catalogue.cleanupRetained)
        assertTrue(catalogue.cleanupUncertain)
        assertFalse(writer.cleanupComplete)
        assertThrows(IllegalStateException::class.java) { writer.finalised(10) }
        assertEquals(RecordingCatalogue.State.WRITING, catalogue.scan().single().state)
        assertArrayEquals(bytes, Files.readAllBytes(root.resolve(writer.filename)))
        assertThrows(IllegalStateException::class.java) {
            RecordingCatalogue(root).begin(identity.copy(segmentId = UUID.randomUUID().toString()))
        }
    }
}
