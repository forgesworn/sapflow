package dev.forgesworn.sapflow.broadcaster

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
}
