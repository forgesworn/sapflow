package dev.forgesworn.sapflow.broadcaster

import org.junit.After
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.OpenOption
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.*
import java.nio.file.attribute.PosixFilePermission.OWNER_READ
import java.nio.file.attribute.PosixFilePermission.OWNER_WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.util.UUID

/** Real host file IO verifies shared policy and failure composition. This test
 * backend is NOT a production fallback or Android FD/proc/SELinux qualification. */
class RecordingCheckpointStorageTest {
    @get:Rule val temporary = TemporaryFolder()
    private val roots = mutableListOf<HostRoot>()
    @After fun releaseHostTestResources() {
        // Unknown logical owners remain retained. Their simulated failing close
        // already closed its actual host channel; only test-root handles remain.
        roots.forEach { it.releaseHostResources() }
    }
    private class Fixture(val directory: Path, val catalogue: RecordingCatalogue,
        val writer: RecordingCatalogue.Writer, val identity: RecordingCatalogue.Identity,
        val root: HostRoot, val storage: RecordingCheckpointStorage)
    private fun fixture(configure: (HostRoot) -> Unit = {}): Fixture {
        val directory = temporary.newFolder().toPath()
        val catalogue = RecordingCatalogue(directory)
        val identity = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "AUDIO_ONLY_48K_MONO")
        val writer = catalogue.begin(identity)
        val root = HostRoot(directory).also { roots.add(it); configure(it) }
        val storage = writer.checkpointStorage { _, _ -> root }
        return Fixture(directory, catalogue, writer, identity, root, storage)
    }
    private val header get() = ByteArray(32) { 3 }
    private val fragment get() = ByteArray(101) { (it + 1).toByte() }
    private fun prepared(f: Fixture) { f.storage.appendInitial(header); f.storage.appendFragment(fragment) }
    private fun metadata(value: Int = 7) = ByteArray(600) { value.toByte() }
    private fun digest(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    private fun install(f: Fixture, sequence: Long = 1, value: Int = 7): RecordingCheckpointStorage.InstalledCheckpoint {
        var calls = 0
        val installed = f.storage.installCheckpoint(sequence, metadata(value)) { bytes, expected ->
            calls++
            assertArrayEquals(metadata(value), bytes)
            assertEquals(sequence, expected.sequence)
            assertEquals(Files.size(f.directory.resolve(f.identity.filename)), expected.committedOffset)
            assertEquals(32, expected.initialHeaderLength)
            assertEquals(digest(header), expected.initialHeaderSha256)
            assertTrue(expected.recordingBinding.matches(Regex("[0-9a-f]{64}")))
            assertTrue(expected.sourceFileBinding.matches(Regex("[0-9a-f]{64}")))
            assertNotEquals(expected.recordingBinding, expected.sourceFileBinding)
        }
        assertEquals(2, calls) // Before mutation, and after actual slot readback.
        return installed
    }

    @Test fun directoryEntryChangesKeepRootIdentityAndMediaHardlinksAreRejected() {
        val f = fixture(); prepared(f)
        Files.write(f.directory.resolve("generated-entry"), byteArrayOf(9), CREATE_NEW)
        f.storage.checkCapacity()
        f.storage.appendFragment(fragment)
        val media = f.directory.resolve(f.identity.filename)
        Files.createLink(f.directory.resolve("generated-hardlink"), media)
        assertEquals(2L, (Files.getAttribute(media, "unix:nlink", NOFOLLOW_LINKS) as Number).toLong())
        assertThrows(IllegalStateException::class.java) { f.storage.appendFragment(fragment) }
        assertArrayEquals(header + fragment + fragment, Files.readAllBytes(media))
        f.storage.closeIncomplete()
        assertTrue(f.writer.cleanupComplete)
    }

    @Test fun partialWritesUseOriginalFragmentsAndRetainTheExistingSingleLease() {
        val f = fixture { it.maxWrite = 3 }
        prepared(f)
        assertTrue(f.catalogue.cleanupRetained)
        assertThrows(IllegalStateException::class.java) { f.writer.ownersClosed() }
        assertThrows(IllegalStateException::class.java) { f.writer.finalised(1) }
        val checkpoint = install(f)
        assertEquals(101L, checkpoint.expected.fragmentRegionBytes)
        assertEquals(digest(fragment), checkpoint.expected.fragmentRegionSha256)
        assertArrayEquals(header + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        val closed = f.storage.closeIncomplete()
        assertEquals(133L, closed.bytes)
        assertSame(closed, f.storage.closeIncomplete())
        assertFalse(f.catalogue.cleanupRetained)
        assertEquals(RecordingCatalogue.State.INCOMPLETE, f.catalogue.scan().single().state)
    }
    @Test fun atomicallyPrivateWriterReceiptAdmitsTheStrictCheckpointBackend() {
        val privateMode = setOf(OWNER_READ, OWNER_WRITE)
        val f = fixture { it.strictPrivateFiles = true }
        val receipt = f.directory.resolve("session-${f.identity.segmentId}.receipt.json")
        assertEquals(privateMode, Files.getPosixFilePermissions(receipt, NOFOLLOW_LINKS))
        prepared(f); install(f); f.storage.closeIncomplete()
        assertEquals(privateMode, Files.getPosixFilePermissions(receipt, NOFOLLOW_LINKS))
        assertEquals(RecordingCatalogue.State.INCOMPLETE, f.catalogue.scan().single().state)

        // An old/permissive file is not exempted by the optional backend. This
        // changes one fixture file's mode, never the shared process umask.
        val directory = temporary.newFolder().toPath()
        val catalogue = RecordingCatalogue(directory)
        val identity = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "AUDIO_ONLY_48K_MONO")
        val writer = catalogue.begin(identity)
        val prior = directory.resolve("session-${identity.segmentId}.receipt.json")
        Files.setPosixFilePermissions(prior, PosixFilePermissions.fromString("rw-r--r--"))
        val strict = HostRoot(directory).also { roots.add(it); it.strictPrivateFiles = true }
        assertThrows(IllegalStateException::class.java) { writer.checkpointStorage { _, _ -> strict } }
        assertFalse(Files.exists(directory.resolve(identity.filename)))
        assertEquals(PosixFilePermissions.fromString("rw-r--r--"), Files.getPosixFilePermissions(prior, NOFOLLOW_LINKS))
        assertFalse(catalogue.cleanupRetained)
    }
    @Test fun alternatingSlotsAdvanceOnlyAfterDurableReadbackAndKeepTwoNames() {
        val f = fixture(); prepared(f)
        install(f, 1, 11)
        f.storage.appendFragment(fragment)
        install(f, 2, 22)
        f.storage.appendFragment(fragment)
        val last = install(f, 3, 33)
        val prefix = "session-${f.identity.segmentId}"
        assertArrayEquals(metadata(22), Files.readAllBytes(f.directory.resolve("$prefix.checkpoint.0")))
        assertArrayEquals(metadata(33), Files.readAllBytes(f.directory.resolve("$prefix.checkpoint.1")))
        assertFalse(Files.exists(f.directory.resolve("$prefix.checkpoint.tmp")))
        assertEquals(335L, last.expected.committedOffset)
        assertEquals(digest(fragment + fragment + fragment), last.expected.fragmentRegionSha256)
        assertEquals(3, f.root.moves)
        assertEquals(3, f.root.rootSyncs)
        f.storage.closeIncomplete()
    }
    @Test fun sameOwnerFinalMetadataRewriteKeepsFragmentsAndCatalogueIncomplete() {
        val f = fixture(); prepared(f); val checkpoint = install(f)
        val replacement = ByteArray(32) { 5 }
        val closed = f.storage.finishStoredInputs(replacement, checkpoint) { actual, expected ->
            assertArrayEquals(replacement, actual); assertEquals(checkpoint.expected, expected)
        }
        assertTrue(closed.headerRewritten)
        assertArrayEquals(replacement + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        assertEquals(RecordingCatalogue.State.INCOMPLETE, f.catalogue.scan().single().state)
        assertThrows(IllegalStateException::class.java) { f.storage.appendFragment(fragment) }
    }
    @Test fun foreignAndOldInstalledReceiptsCannotRewriteAnotherFrontier() {
        val one = fixture(); prepared(one); val old = install(one)
        one.storage.appendFragment(fragment); install(one, 2)
        val before = Files.readAllBytes(one.directory.resolve(one.identity.filename))
        assertThrows(IllegalStateException::class.java) { one.storage.finishStoredInputs(header, old) { _, _ -> } }
        assertArrayEquals(before, Files.readAllBytes(one.directory.resolve(one.identity.filename)))
        one.storage.closeIncomplete()
        val two = fixture(); prepared(two)
        assertThrows(IllegalStateException::class.java) { two.storage.finishStoredInputs(header, old) { _, _ -> } }
        assertArrayEquals(header + fragment, Files.readAllBytes(two.directory.resolve(two.identity.filename)))
        two.storage.closeIncomplete()
    }
    @Test fun wrongEnvelopeAndOversizeMetadataFailBeforeAnySlotMutation() {
        val f = fixture(); prepared(f)
        assertThrows(IllegalArgumentException::class.java) {
            f.storage.installCheckpoint(1, ByteArray(RecordingCheckpointStorage.MAX_CHECKPOINT + 1)) { _, _ -> }
        }
        assertEquals(0, f.root.creates.count { it.endsWith(".checkpoint.tmp") })
        f.storage.closeIncomplete()
        val second = fixture(); prepared(second)
        assertThrows(IllegalStateException::class.java) {
            second.storage.installCheckpoint(1, metadata()) { _, expected ->
                check(expected.committedOffset == 134L) { "Wrong committed offset" }
            }
        }
        assertEquals(0, second.root.moves)
        assertEquals(0L, second.storage.closeIncomplete().installedSequence)
    }
    @Test fun corruptReadbackCannotPublishInstalledProgress() {
        val f = fixture(); prepared(f); f.root.corruptReadback = true
        assertThrows(IllegalStateException::class.java) { install(f) }
        assertEquals(0L, f.storage.closeIncomplete().installedSequence)
        assertFalse(f.catalogue.cleanupRetained)
    }
    @Test fun zeroProgressAndStorageFailuresKeepThePrefixWithoutFinalisation() {
        val f = fixture(); prepared(f); f.root.zeroWrites = true
        assertThrows(IllegalStateException::class.java) { f.storage.appendFragment(fragment) }
        assertArrayEquals(header + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        assertEquals(0L, f.storage.closeIncomplete().installedSequence)
        for (failure in listOf("media-sync", "rename", "directory-sync")) {
            val another = fixture(); prepared(another); another.root.failure = failure
            assertThrows(IOException::class.java) { install(another) }
            assertEquals(0L, another.storage.closeIncomplete().installedSequence)
            assertArrayEquals(header + fragment, Files.readAllBytes(another.directory.resolve(another.identity.filename)))
        }
    }
    @Test fun unknownMetadataCloseRetainsRootLeaseAndNeverRetriesItsRecycledOwner() {
        val f = fixture(); prepared(f); f.root.failCloseSuffix = ".checkpoint.tmp"
        assertThrows(IOException::class.java) { install(f) }
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertEquals(1, f.root.closeCalls.entries.single { it.key.endsWith(".checkpoint.tmp") }.value)
        assertEquals(0, f.root.rootCloseCalls)
        assertTrue(f.catalogue.cleanupUncertain)
        assertTrue(f.catalogue.cleanupRetained)
        assertThrows(RecordingCatalogue.CatalogueFailure::class.java) { f.catalogue.checkRecordingCapacity() }
    }
    @Test fun unknownMediaCloseRetainsTheSameLeaseAndIsNotRetried() {
        val f = fixture(); prepared(f); f.root.failCloseSuffix = ".mp4"
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertEquals(1, f.root.closeCalls[f.identity.filename])
        assertTrue(f.catalogue.cleanupRetained)
    }
    @Test fun unknownRootCloseRetainsCatalogueAuthorityWithoutAnotherCloseAttempt() {
        val f = fixture(); prepared(f); f.root.failure = "root-close"
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertThrows(IOException::class.java) { f.storage.closeIncomplete() }
        assertEquals(1, f.root.rootCloseCalls)
        assertTrue(f.catalogue.cleanupRetained)
        assertTrue(f.catalogue.cleanupUncertain)
    }
    @Test fun reentrantCloseAndWriterCancellationCannotBypassCompoundOwnership() {
        val f = fixture(); prepared(f)
        assertThrows(IllegalStateException::class.java) {
            f.storage.installCheckpoint(1, metadata()) { _, _ -> f.storage.closeIncomplete() }
        }
        assertEquals(0, f.root.moves); f.storage.closeIncomplete()
        val another = fixture(); prepared(another); another.writer.cancel()
        assertThrows(IllegalStateException::class.java) { another.storage.appendFragment(fragment) }
        assertArrayEquals(header + fragment, Files.readAllBytes(another.directory.resolve(another.identity.filename)))
        another.storage.closeIncomplete(); assertFalse(another.catalogue.cleanupRetained)
    }
    @Test fun initialValidatorCancellationFencesEveryCheckpointMutation() {
        val f = fixture(); prepared(f)
        var validations = 0
        assertThrows(IllegalStateException::class.java) {
            f.storage.installCheckpoint(1, metadata()) { _, _ ->
                validations++; f.writer.cancel()
            }
        }
        assertEquals(1, validations)
        assertEquals(0, f.root.moves)
        assertEquals(0, f.root.creates.count { it.endsWith(".checkpoint.tmp") })
        assertArrayEquals(header + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        assertEquals(0L, f.storage.closeIncomplete().installedSequence)
        assertFalse(f.catalogue.cleanupRetained)
    }
    @Test fun readbackValidatorCancellationCannotIssueNewInstalledProgress() {
        val f = fixture(); prepared(f); install(f)
        f.storage.appendFragment(fragment)
        var validations = 0
        var returned: RecordingCheckpointStorage.InstalledCheckpoint? = null
        assertThrows(IllegalStateException::class.java) {
            returned = f.storage.installCheckpoint(2, metadata(22)) { _, _ ->
                validations++
                if (validations == 2) f.writer.cancel()
            }
        }
        assertEquals(2, validations); assertNull(returned)
        assertEquals(2, f.root.moves) // Published bytes are not an installed receipt.
        assertArrayEquals(header + fragment + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        assertEquals(1L, f.storage.closeIncomplete().installedSequence)
        assertFalse(f.catalogue.cleanupRetained)
        assertEquals(RecordingCatalogue.State.INCOMPLETE, f.catalogue.scan().single().state)
    }
    @Test fun finalHeaderValidatorCancellationCannotRewriteTheOriginalHeader() {
        val f = fixture(); prepared(f); val checkpoint = install(f)
        assertThrows(IllegalStateException::class.java) {
            f.storage.finishStoredInputs(ByteArray(32) { 99.toByte() }, checkpoint) { _, _ -> f.writer.cancel() }
        }
        assertArrayEquals(header + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename)))
        val closure = f.storage.closeIncomplete()
        assertFalse(closure.headerRewritten); assertEquals(1L, closure.installedSequence)
        assertFalse(f.catalogue.cleanupRetained)
    }
    @Test fun namespaceAndStrictRootFreeSpaceBoundsAreCheckedBeforePublishing() {
        val f = fixture(); prepared(f)
        repeat(1015) { Files.write(f.directory.resolve("unrelated-$it"), byteArrayOf(1), CREATE_NEW) }
        f.storage.checkCapacity()
        Files.write(f.directory.resolve("one-too-many"), byteArrayOf(1), CREATE_NEW)
        assertThrows(IllegalStateException::class.java) { f.storage.checkCapacity() }
        f.storage.closeIncomplete()
        for (available in listOf(RecordingCheckpointStorage.STORAGE_FLOOR, -1L)) {
            val next = fixture(); prepared(next); next.root.available = available
            assertThrows(IllegalStateException::class.java) { install(next) }
            assertEquals(0, next.root.moves); next.storage.closeIncomplete()
        }
    }
    @Test fun segmentCapacityAndExistingTemporaryNamesArePreserved() {
        val f = fixture(); prepared(f)
        repeat(255) { Files.write(f.directory.resolve("session-${UUID.randomUUID()}.mp4"), byteArrayOf(1), CREATE_NEW) }
        f.storage.checkCapacity()
        Files.write(f.directory.resolve("session-${UUID.randomUUID()}.mp4"), byteArrayOf(1), CREATE_NEW)
        assertThrows(IllegalStateException::class.java) { f.storage.checkCapacity() }
        f.storage.closeIncomplete()
        val next = fixture(); prepared(next)
        val temp = next.directory.resolve("session-${next.identity.segmentId}.checkpoint.tmp")
        Files.write(temp, byteArrayOf(8), CREATE_NEW)
        assertThrows(IllegalStateException::class.java) { install(next) }
        assertArrayEquals(byteArrayOf(8), Files.readAllBytes(temp)); next.storage.closeIncomplete()
    }
    @Test fun changedRootAndMediaNamesFailWithoutWritingTheirReplacements() {
        val f = fixture(); prepared(f)
        val moved = f.directory.resolveSibling(f.directory.fileName.toString() + "-moved")
        Files.move(f.directory, moved); Files.createDirectory(f.directory)
        assertThrows(IllegalStateException::class.java) { f.storage.appendFragment(fragment) }
        assertArrayEquals(header + fragment, Files.readAllBytes(moved.resolve(f.identity.filename)))
        f.storage.closeIncomplete()
        val next = fixture(); prepared(next)
        val media = next.directory.resolve(next.identity.filename)
        Files.move(media, next.directory.resolve("preserved-original"))
        Files.write(media, byteArrayOf(99), CREATE_NEW)
        assertThrows(IllegalStateException::class.java) { install(next) }
        assertArrayEquals(byteArrayOf(99), Files.readAllBytes(media)); next.storage.closeIncomplete()
    }
    @Test fun finalHeaderLengthAndOriginalReadbackAreMandatory() {
        val f = fixture(); prepared(f); val checkpoint = install(f)
        assertThrows(IllegalArgumentException::class.java) { f.storage.finishStoredInputs(ByteArray(31), checkpoint) { _, _ -> } }
        assertArrayEquals(header + fragment, Files.readAllBytes(f.directory.resolve(f.identity.filename))); f.storage.closeIncomplete()
        val next = fixture(); prepared(next); val installed = install(next)
        FileChannel.open(next.directory.resolve(next.identity.filename), WRITE).use { it.write(ByteBuffer.wrap(byteArrayOf(77)), 0) }
        assertThrows(IllegalStateException::class.java) { next.storage.finishStoredInputs(header, installed) { _, _ -> } }
        next.storage.closeIncomplete()
    }

    private class Pending(val directory: Path, val catalogue: RecordingCatalogue,
        val identity: RecordingCatalogue.Identity, val writer: RecordingCatalogue.Writer, val root: HostRoot)
    private fun pending(): Pending {
        val directory = temporary.newFolder().toPath()
        val catalogue = RecordingCatalogue(directory)
        val identity = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "AUDIO_ONLY_48K_MONO")
        val writer = catalogue.begin(identity)
        return Pending(directory, catalogue, identity, writer, HostRoot(directory).also { roots.add(it) })
    }
    private fun contextName(identity: RecordingCatalogue.Identity) = "session-${identity.segmentId}.append-context"

    @Test fun durableContextPrecedesMediaAndReconstructsTheExactExistingBindings() {
        val f = fixture { it.maxWrite = 3; it.strictPrivateFiles = true }
        val path = f.directory.resolve(contextName(f.identity))
        val encoded = Files.readAllBytes(path)
        val media = checkNotNull(f.root.inspect(f.identity.filename))
        assertEquals(0L, media.bytes)
        val context = RecordingAppendContext.decode(encoded, f.identity, f.root.identity, media)
        val bindings = context.bindings(f.identity, f.root.identity, media)
        assertEquals(digest("sapflow.android.checkpoint.recording.v1\n${f.identity.showId}\n${f.identity.segmentId}\n${f.identity.segmentIndex}\n${f.identity.profileId}\n${context.nonce}".toByteArray(Charsets.UTF_8)), bindings.recording)
        assertEquals(digest("sapflow.android.checkpoint.file.v1\n${f.root.identity.device}\n${f.root.identity.inode}\n${media.device}\n${media.inode}\n${context.nonce}".toByteArray(Charsets.UTF_8)), bindings.sourceFile)
        assertEquals(setOf(OWNER_READ, OWNER_WRITE), Files.getPosixFilePermissions(path, NOFOLLOW_LINKS))
        assertEquals(1, f.root.contextMoves); assertEquals(1, f.root.contextRootSyncs)
        val events = f.root.contextEvents
        assertTrue(events.indexOf("media-sync") < events.indexOf("write"))
        assertTrue(events.indexOf("sync") < events.indexOf("temp-close"))
        assertTrue(events.indexOf("temp-close") < events.indexOf("rename"))
        assertTrue(events.indexOf("rename") < events.indexOf("directory-sync"))
        assertTrue(events.indexOf("directory-sync") < events.indexOf("read"))
        assertTrue(events.indexOf("read") < events.indexOf("read-close"))
        prepared(f); val checkpoint = install(f)
        assertEquals(bindings.recording, checkpoint.expected.recordingBinding)
        assertEquals(bindings.sourceFile, checkpoint.expected.sourceFileBinding)
        f.storage.closeIncomplete()
        // Reconstruct with a new root owner after release, not the old live token.
        val fresh = HostRoot(f.directory).also { roots.add(it) }
        val recovered = RecordingAppendContext.decode(Files.readAllBytes(path), f.identity,
            fresh.identity, checkNotNull(fresh.inspect(f.identity.filename)))
        assertEquals(bindings, recovered.bindings(f.identity, fresh.identity, checkNotNull(fresh.inspect(f.identity.filename))))
        assertArrayEquals(encoded, Files.readAllBytes(path)); fresh.close()
    }

    @Test fun oldOrMalformedContextAndTemporaryNamesAreNeverOverwritten() {
        for (suffix in listOf("", ".tmp")) {
            val f = pending(); val path = f.directory.resolve(contextName(f.identity) + suffix)
            val original = byteArrayOf(91, 7, 23)
            Files.write(path, original, CREATE_NEW)
            assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            assertArrayEquals(original, Files.readAllBytes(path))
            assertFalse(Files.exists(f.directory.resolve(f.identity.filename)))
            assertTrue(f.writer.cleanupComplete); assertEquals(0, f.root.contextMoves)
        }
    }

    @Test fun symlinkAndHardlinkContextNamesRefuseWithoutTouchingTheirTargets() {
        for (hardlink in listOf(false, true)) {
            val f = pending(); val target = f.directory.resolve("preserved-target")
            Files.write(target, byteArrayOf(11, 12), CREATE_NEW)
            val path = f.directory.resolve(contextName(f.identity))
            if (hardlink) Files.createLink(path, target) else Files.createSymbolicLink(path, target.fileName)
            assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            assertArrayEquals(byteArrayOf(11, 12), Files.readAllBytes(target))
            assertFalse(Files.exists(f.directory.resolve(f.identity.filename)))
            assertTrue(f.writer.cleanupComplete)
        }
    }

    @Test fun contextIoFailuresNeverReturnAnAppendOwnerOrCheckpointProgress() {
        for (stage in listOf("create", "write", "sync", "rename", "directory-sync", "open-read", "read", "zero-write")) {
            val f = pending(); f.root.contextFailure = stage
            if (stage == "zero-write") {
                assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            } else {
                assertThrows(IOException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            }
            assertEquals(0L, Files.size(f.directory.resolve(f.identity.filename)))
            assertFalse(Files.exists(f.directory.resolve("session-${f.identity.segmentId}.checkpoint.0")))
            assertFalse(Files.exists(f.directory.resolve("session-${f.identity.segmentId}.checkpoint.1")))
            assertEquals(0, f.root.moves); assertTrue(f.writer.cleanupComplete)
            assertFalse(f.catalogue.cleanupUncertain)
        }
    }

    @Test fun corruptContextReadbackCannotAdmitInitialMedia() {
        val f = pending(); f.root.corruptContextReadback = true
        assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
        assertEquals(0L, Files.size(f.directory.resolve(f.identity.filename)))
        assertEquals(1, f.root.contextMoves); assertEquals(0, f.root.moves)
        assertTrue(f.writer.cleanupComplete)
    }

    @Test fun unknownContextClosesRetainTheirActualOwnerAndLeaseWithoutRetry() {
        for (suffix in listOf(".append-context.tmp", ".append-context")) {
            val f = pending(); f.root.failCloseSuffix = suffix
            assertThrows(IOException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            assertEquals(1, f.root.closeCalls.entries.single { it.key.endsWith(suffix) }.value)
            assertEquals(0, f.root.rootCloseCalls)
            assertTrue(f.catalogue.cleanupRetained); assertTrue(f.catalogue.cleanupUncertain)
            assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            assertEquals(1, f.root.closeCalls.entries.single { it.key.endsWith(suffix) }.value)
            assertEquals(0L, Files.size(f.directory.resolve(f.identity.filename)))
        }
    }

    @Test fun contextBootstrapCancellationAndReentrancyCannotPublishAnOwner() {
        for (cut in listOf("sync", "read")) {
            val f = pending(); var observed = false
            f.root.onContextEvent = { event ->
                if (event == cut && !observed) {
                    observed = true
                    assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
                    f.writer.cancel()
                }
            }
            assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
            assertTrue(observed); assertTrue(f.writer.cleanupComplete)
            assertEquals(0L, Files.size(f.directory.resolve(f.identity.filename)))
            assertEquals(if (cut == "sync") 0 else 1, f.root.contextMoves)
            assertEquals(0, f.root.moves)
        }
    }

    @Test fun changedContextOrMediaIncarnationFencesAllLaterMediaWrites() {
        for (changeMedia in listOf(false, true)) {
            val f = fixture()
            val name = if (changeMedia) f.identity.filename else contextName(f.identity)
            val path = f.directory.resolve(name); val original = Files.readAllBytes(path)
            Files.move(path, f.directory.resolve("preserved-original"))
            Files.write(path, original, CREATE_NEW)
            assertThrows(IllegalStateException::class.java) { f.storage.appendInitial(header) }
            assertArrayEquals(original, Files.readAllBytes(path))
            f.storage.closeIncomplete(); assertTrue(f.writer.cleanupComplete)
        }
        val f = pending(); var changed = false
        f.root.onContextEvent = { event ->
            if (event == "read" && !changed) {
                changed = true
                Files.move(f.directory.resolve(f.identity.filename), f.directory.resolve("original-media"))
                Files.write(f.directory.resolve(f.identity.filename), byteArrayOf(), CREATE_NEW)
            }
        }
        assertThrows(IllegalStateException::class.java) { f.writer.checkpointStorage { _, _ -> f.root } }
        assertTrue(changed); assertEquals(0L, Files.size(f.directory.resolve("original-media")))
        assertTrue(f.writer.cleanupComplete)
    }

    private class HostRoot(private val directory: Path) : RecordingCheckpointStorage.Root {
        override val identity = node(directory)
        private val rootChannel = FileChannel.open(directory, READ)
        private val files = mutableListOf<HostFile>()
        var maxWrite = Int.MAX_VALUE
        var zeroWrites = false
        var corruptReadback = false
        var failure: String? = null
        var failCloseSuffix: String? = null
        var strictPrivateFiles = false
        var available = Long.MAX_VALUE
        // Keep existing checkpoint counters/fault points scoped to their old
        // operation. Context bootstrap has separate cold metadata diagnostics.
        var contextFailure: String? = null
        var corruptContextReadback = false
        var onContextEvent: ((String) -> Unit)? = null
        val contextEvents = mutableListOf<String>()
        var contextMoves = 0
        var contextRootSyncs = 0
        private var contextDirectorySync = false
        private fun contextEvent(name: String) { contextEvents.add(name); onContextEvent?.invoke(name) }
        var rootSyncs = 0
        var rootCloseCalls = 0
        var moves = 0
        val creates = mutableListOf<String>()
        val closeCalls = mutableMapOf<String, Int>()
        override fun verify() { check(rootChannel.isOpen && identity.sameFile(node(directory))) }
        override fun names(limit: Int): List<String> {
            verify(); val names = mutableListOf<String>()
            Files.newDirectoryStream(directory).use { for (p in it) { check(names.size < limit); names.add(p.fileName.toString()) } }
            return names
        }
        override fun inspect(name: String): RecordingCheckpointStorage.Node? {
            val path = directory.resolve(name)
            if (!Files.exists(path, NOFOLLOW_LINKS)) return null
            if (strictPrivateFiles) check(Files.getPosixFilePermissions(path, NOFOLLOW_LINKS).all { it == OWNER_READ || it == OWNER_WRITE }) { "Fixture metadata is not private" }
            return node(path)
        }
        override fun create(name: String): RecordingCheckpointStorage.File {
            verify(); creates.add(name)
            if (name.endsWith(".append-context.tmp")) {
                contextEvent("create")
                if (contextFailure == "create") throw IOException("Injected context create failure")
            }
            val mode = PosixFilePermissions.asFileAttribute(setOf(OWNER_READ, OWNER_WRITE))
            return HostFile(name, FileChannel.open(directory.resolve(name), setOf<OpenOption>(CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS), mode)).also { files.add(it) }
        }
        override fun openRead(name: String): RecordingCheckpointStorage.File {
            verify()
            if (name.endsWith(".append-context")) {
                contextEvent("open-read")
                if (contextFailure == "open-read") throw IOException("Injected context open failure")
            }
            return HostFile(name, FileChannel.open(directory.resolve(name), READ, NOFOLLOW_LINKS)).also { files.add(it) }
        }
        override fun replace(source: String, destination: String) {
            verify()
            val context = destination.endsWith(".append-context")
            if (context) {
                contextEvent("rename")
                if (contextFailure == "rename") throw IOException("Injected context rename failure")
            } else if (failure == "rename") throw IOException("Injected rename failure")
            Files.move(directory.resolve(source), directory.resolve(destination), ATOMIC_MOVE, REPLACE_EXISTING)
            if (context) { contextMoves++; contextDirectorySync = true } else moves++
        }
        override fun sync() {
            verify()
            if (contextDirectorySync) {
                contextEvent("directory-sync")
                if (contextFailure == "directory-sync") throw IOException("Injected context root sync failure")
                rootChannel.force(true); contextRootSyncs++; contextDirectorySync = false
            } else {
                if (failure == "directory-sync") throw IOException("Injected root sync failure")
                rootChannel.force(true); rootSyncs++
            }
        }
        override fun availableBytes() = available
        override fun close() {
            rootCloseCalls++; rootChannel.close()
            if (failure == "root-close") throw IOException("Injected unknown root close after actual release")
        }
        fun releaseHostResources() { files.forEach { if (it.channel.isOpen) it.channel.close() }; if (rootChannel.isOpen) rootChannel.close() }
        private inner class HostFile(private val name: String, val channel: FileChannel) : RecordingCheckpointStorage.File {
            private val original = node(directory.resolve(name))
            private var attempted = false
            private var closeFailure: IOException? = null
            override fun stat() = original.copy(bytes = channel.size())
            override fun read(bytes: ByteArray, offset: Int, count: Int, position: Long): Int {
                if (name.endsWith(".append-context")) {
                    contextEvent("read")
                    if (contextFailure == "read") throw IOException("Injected context read failure")
                }
                val result = channel.read(ByteBuffer.wrap(bytes, offset, count), position)
                if (corruptContextReadback && name.endsWith(".append-context") && result > 0) bytes[offset] = (bytes[offset].toInt() xor 1).toByte()
                if (corruptReadback && name.contains(".checkpoint.") && result > 0) bytes[offset] = (bytes[offset].toInt() xor 1).toByte()
                return result
            }
            override fun write(bytes: ByteArray, offset: Int, count: Int, position: Long): Int {
                if (name.endsWith(".append-context.tmp")) {
                    contextEvent("write")
                    if (contextFailure == "write") throw IOException("Injected context write failure")
                    if (contextFailure == "zero-write") return 0
                }
                if (zeroWrites) return 0
                return channel.write(ByteBuffer.wrap(bytes, offset, minOf(count, maxWrite)), position)
            }
            override fun sync() {
                if (name.endsWith(".append-context.tmp")) {
                    contextEvent("sync")
                    if (contextFailure == "sync") throw IOException("Injected context sync failure")
                }
                if (name.endsWith(".mp4") && channel.size() == 0L) contextEvent("media-sync")
                if (failure == "media-sync" && name.endsWith(".mp4")) throw IOException("Injected media sync failure")
                channel.force(true)
            }
            override fun close() {
                closeFailure?.let { throw it }; if (attempted) return
                attempted = true; closeCalls[name] = (closeCalls[name] ?: 0) + 1; channel.close()
                if (name.endsWith(".append-context.tmp")) contextEvent("temp-close")
                if (name.endsWith(".append-context")) contextEvent("read-close")
                if (failCloseSuffix?.let(name::endsWith) == true) {
                    val error = IOException("Injected unknown close after actual release"); closeFailure = error; throw error
                }
            }
        }
        companion object {
            private fun node(path: Path): RecordingCheckpointStorage.Node {
                val a = Files.readAttributes(path, "unix:dev,ino,mode,nlink,size", NOFOLLOW_LINKS)
                val mode = (a.getValue("mode") as Number).toInt()
                return RecordingCheckpointStorage.Node((a.getValue("dev") as Number).toLong(), (a.getValue("ino") as Number).toLong(),
                    mode and 61440 == 16384, mode and 61440 == 32768, (a.getValue("nlink") as Number).toLong(), (a.getValue("size") as Number).toLong())
            }
        }
    }
}
