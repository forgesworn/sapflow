package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaCodec
import dev.forgesworn.sapflow.NativeAudioFileReceipt
import dev.forgesworn.sapflow.NativeAudioGenerationReceipt
import dev.forgesworn.sapflow.NativeRecordingCompletion
import dev.forgesworn.sapflow.NativeRecordingSourceMapping
import dev.forgesworn.sapflow.NativeSampleFanout
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.OpenOption
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.*
import java.nio.file.attribute.PosixFilePermission.OWNER_READ
import java.nio.file.attribute.PosixFilePermission.OWNER_WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference

/** Actual SDK + actual host filesystem POLICY controls. Synthetic compressed
 * bytes/explicit scalar receipts are NOT native codec, decoder or authority
 * qualification. No production IO, muxer or complete producer is mocked. */
private data class Fixture(val directory: Path, val catalogue: RecordingCatalogue,
    val identity: RecordingCatalogue.Identity, val root: HostRoot, val owner: RecordingCheckpointOwner)
private val roots = mutableListOf<HostRoot>()
private fun fixture(base: Path, maxWrite: Int = Int.MAX_VALUE, intent: CaptureIntent = CaptureIntent.AudioOnly48kMono): Fixture {
    val directory = Files.createDirectory(base.resolve(UUID.randomUUID().toString()))
    val catalogue = RecordingCatalogue(directory)
    val identity = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, intent.profileId)
    lateinit var actual: HostRoot
    val owner = RecordingCheckpointOwner.open(catalogue, identity, intent, 0uL,
        maxFragmentBytes = 4096uL, maxFragmentMs = 100u, storageFactory = { path, _ ->
            HostRoot(path).also { actual = it; it.maxWrite = maxWrite; roots.add(it) }
        })
    return Fixture(directory, catalogue, identity, actual, owner)
}
private fun payload() = ByteBuffer.allocateDirect(3).put(byteArrayOf(1, 2, 3)).apply { flip() }
private fun codec() = MediaCodec.aac(ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).apply { flip() })
private inline fun refused(body: () -> Unit): Exception {
    try { body() } catch (error: Exception) { return error }
    error("Expected refusal")
}
private fun pushNine(f: Fixture) {
    codec().use { codec ->
        repeat(9) { index ->
            val tick = index * 1024L
            codec.sample(payload(), tick, tick, 1024u, 48_000u, false).use { f.owner.push(it) }
        }
    }
}
/** Registered synthetic API values, NOT observations extracted from a codec. */
private fun suppliedComplete(f: Fixture) {
    f.owner.generationEnded(NativeAudioGenerationReceipt(1uL, 6400uL, 6400uL, 0uL,
        48_000u, 9216uL, 2112u, 704u, NativeRecordingSourceMapping.NEAREST))
    f.owner.completeAudio(NativeAudioFileReceipt(1uL, 6400uL, 0uL, 9216uL, 2112u, 704u))
}
private fun assertIncomplete(f: Fixture) {
    check(f.owner.cleanupComplete && !f.catalogue.cleanupRetained)
    check(f.catalogue.scan().single().state == RecordingCatalogue.State.INCOMPLETE)
}

private fun assertEmptyUnverified(f: Fixture) {
    check(f.owner.cleanupComplete && !f.catalogue.cleanupRetained)
    val entry = f.catalogue.scan().single()
    check(entry.state == RecordingCatalogue.State.ORPHAN_UNVERIFIED && entry.bytes == 0L)
    refused { f.catalogue.openExport(entry, allowUnverified = true) }
}

private fun completeSdkInputsRemainUnknownAppAdmission(base: Path) {
    val f = fixture(base, maxWrite = 3)
    f.owner.beginAudio(2112u); pushNine(f); suppliedComplete(f)
    val before = checkNotNull(f.owner.frontier)
    val result = f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE)
    check(result.scope == RecordingCheckpointOwner.Scope.SUBMITTED_SDK_INPUTS_ONLY)
    check(result.applicationAdmission == RecordingCheckpointOwner.ApplicationAdmission.UNKNOWN)
    check(result.completeAudioEof && result.headerRewritten && result.movieTimescale == 720_000u)
    check(result.durationTicks == 96_000uL && result.durationMsCeil == 134uL)
    check(checkNotNull(f.owner.frontier).audioUnits == 9uL && before.owner == f.owner.frontier!!.owner)
    check(Files.size(f.directory.resolve(f.identity.filename)) == result.bytes)
    check(f.root.moves >= 2 && f.root.rootSyncs >= 2)
    assertIncomplete(f)
}
private fun unknownLeadingNeverAdmitsMedia(base: Path) {
    val f = fixture(base)
    codec().use { c -> c.sample(payload(), 0, 0, 1024u, 48_000u, false).use { s -> refused { f.owner.push(s) } } }
    check(f.owner.frontier == null && Files.size(f.directory.resolve(f.identity.filename)) == 0L)
    f.owner.closeIncomplete(); assertEmptyUnverified(f)
}
private fun unknownFileReceiptCannotRequestComplete(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f)
    val prefix = Files.readAllBytes(f.directory.resolve(f.identity.filename))
    refused { f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE) }
    check(Files.readAllBytes(f.directory.resolve(f.identity.filename)).contentEquals(prefix))
    f.owner.closeIncomplete(); assertIncomplete(f)
}
private fun missingGenerationCannotGrantFileReceipt(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f)
    refused { f.owner.completeAudio(NativeAudioFileReceipt(1uL, 6400uL, 0uL, 9216uL, 2112u, 704u)) }
    f.owner.closeIncomplete(); assertIncomplete(f)
}
private fun contradictoryGenerationIsSticky(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f)
    refused { f.owner.generationEnded(NativeAudioGenerationReceipt(1uL, 6401uL, 6400uL, 0uL,
        48_000u, 9216uL, 2112u, 704u, NativeRecordingSourceMapping.NEAREST)) }
    refused { suppliedComplete(f) }
    f.owner.closeIncomplete(); assertIncomplete(f)
}
private fun unknownEofProducesExplicitUntrimmedPrefix(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f)
    val result = f.owner.finishStoredInputs()
    check(result.sdkOutcome == NativeRecordingCompletion.INCOMPLETE_PREFIX && !result.completeAudioEof)
    check(result.applicationAdmission == RecordingCheckpointOwner.ApplicationAdmission.UNKNOWN && result.headerRewritten)
    check(f.owner.frontier!!.audioUnits == 9uL && !f.owner.frontier!!.completeAudioEof)
    assertIncomplete(f)
}
private fun originalTimingViewsUseExistingQueuesAndHandle(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u)
    codec().use { c -> NativeSampleFanout.withAacTimingViews(8192uL, 8192uL, 1000u, false).use { queues ->
        repeat(9) { index ->
            val tick = index * 1024L
            c.sample(payload(), tick, tick, 1024u, 48_000u, false).use { original ->
                original.aacTimingViews(1uL, tick - 2112, if (index < 3) null else (index - 3) * 1024L).use { views ->
                    val outcome = queues.pushAacTimingViews(views, index.toULong())
                    check(outcome.recordingQueued && outcome.publishingQueued == (index >= 3))
                }
            }
            queues.popRecording(index.toULong())!!.use { sample ->
                check(sample.pts() == tick && sample.aacSourceInfo()!!.originalPts == tick)
                f.owner.push(sample)
            }
            if (index >= 3) queues.popPublishing(index.toULong())!!.use { check(it.pts() == (index - 3) * 1024L) }
        }
        check(queues.stats().publishingDrops == 0uL && queues.stats().recordingDrops == 0uL)
    } }
    suppliedComplete(f); f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE); assertIncomplete(f)
}
private fun oldFrontierAndClosedResultRemainImmutable(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f)
    val first = checkNotNull(f.owner.frontier); suppliedComplete(f)
    val result = f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE)
    val bytes = Files.readAllBytes(f.directory.resolve(f.identity.filename))
    check(f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE) === result)
    check(f.owner.closeIncomplete() === result)
    check(Files.readAllBytes(f.directory.resolve(f.identity.filename)).contentEquals(bytes))
    check(first.sequence < f.owner.frontier!!.sequence && first.fileEnd < f.owner.frontier!!.fileEnd)
    check(first.completeAudioEof == false && f.owner.frontier!!.completeAudioEof)
    check(Files.list(f.directory).use { stream -> stream.filter { it.fileName.toString().matches(Regex(".*\\.checkpoint\\.[01]")) }.count() } == 2L)
}
private fun corruptSidecarReadbackNeverPublishes(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); f.root.corruptReadback = true
    refused { pushNine(f) }; check(f.owner.frontier == null)
    f.owner.closeIncomplete(); assertIncomplete(f)
}
private fun cancellationDuringReadbackNeverPublishes(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u)
    f.root.onCheckpointRead = { f.owner.cancel() }
    refused { pushNine(f) }; check(f.owner.frontier == null)
    f.root.onCheckpointRead = null
    f.owner.closeIncomplete(); assertIncomplete(f)
}
private fun wrongWorkerAndReentryAreRejectedWithoutPoisoning(base: Path) {
    val f = fixture(base); val other = AtomicReference<Exception?>()
    val thread = Thread { other.set(refused { f.owner.checkStorage() }) }; thread.start(); thread.join(5000)
    check(!thread.isAlive && other.get() != null)
    f.owner.beginAudio(2112u)
    var checked = 0
    f.root.onCheckpointRead = { refused { f.owner.checkStorage() }; checked++ }
    pushNine(f); suppliedComplete(f); f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE)
    check(checked > 0); assertIncomplete(f)
}
private fun realSdkConstructorFailureClosesExactStorageLease(base: Path) {
    val directory = Files.createDirectory(base.resolve(UUID.randomUUID().toString()))
    val catalogue = RecordingCatalogue(directory)
    val id = RecordingCatalogue.Identity(UUID.randomUUID().toString(), UUID.randomUUID().toString(), 0, "AUDIO_ONLY_48K_MONO")
    var created = false
    refused { RecordingCheckpointOwner.open(catalogue, id, CaptureIntent.AudioOnly48kMono, 0uL,
        maxFragmentBytes = 1uL, storageFactory = { path, _ -> HostRoot(path).also { created = true; roots.add(it) } }) }
    check(created && !catalogue.cleanupRetained)
    val entry = catalogue.scan().single()
    check(entry.state == RecordingCatalogue.State.ORPHAN_UNVERIFIED && entry.bytes == 0L)
    refused { catalogue.openExport(entry, allowUnverified = true) }
}
private fun unknownMediaCloseRetainsLeaseWithoutRetry(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f); f.root.failCloseSuffix = ".mp4"
    val first = refused { f.owner.closeIncomplete() }; val calls = f.root.closeCalls.toMap()
    check(refused { f.owner.closeIncomplete() } === first && f.root.closeCalls == calls)
    check(!f.owner.cleanupComplete && f.catalogue.cleanupRetained && f.catalogue.cleanupUncertain)
    refused { f.catalogue.begin(f.identity.copy(segmentId = UUID.randomUUID().toString())) }
}
private fun unknownRootCloseRetainsLeaseWithoutRetry(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); pushNine(f); f.root.failure = "root-close"
    val first = refused { f.owner.closeIncomplete() }; val calls = f.root.rootCloseCalls
    check(refused { f.owner.closeIncomplete() } === first && f.root.rootCloseCalls == calls && calls == 1)
    check(!f.owner.cleanupComplete && f.catalogue.cleanupRetained && f.catalogue.cleanupUncertain)
}
private fun unknownMetadataCloseRetainsExactPendingOwners(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u); f.root.failCloseSuffix = ".checkpoint.tmp"
    refused { pushNine(f) }; check(f.owner.frontier == null)
    refused { f.owner.closeIncomplete() }; val calls = f.root.closeCalls.toMap()
    refused { f.owner.closeIncomplete() }; check(f.root.closeCalls == calls)
    check(!f.owner.cleanupComplete && f.catalogue.cleanupRetained && f.catalogue.cleanupUncertain)
}
private fun stereoProfileIsRefusedByActualSdk(base: Path) {
    val f = fixture(base); f.owner.beginAudio(2112u)
    MediaCodec.aac(ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x90.toByte())).apply { flip() }).use { c ->
        c.sample(payload(), 0, 0, 1024u, 48_000u, false).use { refused { f.owner.push(it) } }
    }
    check(f.owner.frontier == null); f.owner.closeIncomplete(); assertEmptyUnverified(f)
}
private fun audioVideoSharesOneWorkerAndExactTrackFrontier(base: Path) {
    val f = fixture(base, intent = CaptureIntent.AudioVideo()); f.owner.beginAudio(2112u)
    // Existing client framing fixture, not decodable-media qualification.
    val sps = byteArrayOf(0x67, 0x4d, 0x40, 0x0d, 0xec.toByte(), 0xa0.toByte(), 0xa0.toByte(),
        0xcf.toByte(), 0xcf.toByte(), 0x80.toByte(), 0x88.toByte(), 0, 0, 3, 0, 8, 0,
        0, 3, 1, 0xe0.toByte(), 0x78, 0xa1.toByte(), 0x4c, 0xb0.toByte())
    val pps = byteArrayOf(0x68, 0xef.toByte(), 0xbc.toByte(), 0x80.toByte())
    MediaCodec.h264(ByteBuffer.allocateDirect(sps.size).put(sps).apply { flip() }, ByteBuffer.allocateDirect(pps.size).put(pps).apply { flip() }).use { video ->
        codec().use { audio -> repeat(9) { index ->
            if (index % 2 == 0) {
                val frame = index / 2
                val nal = byteArrayOf(0, 0, 0, 2, if (frame == 0) 0x65.toByte() else 0x41.toByte(), 0x80.toByte())
                video.sample(ByteBuffer.allocateDirect(nal.size).put(nal).apply { flip() }, frame.toLong(), frame.toLong(),
                    1u, 30u, frame == 0).use { f.owner.push(it) }
            }
            val tick = index * 1024L
            audio.sample(payload(), tick, tick, 1024u, 48_000u, false).use { f.owner.push(it) }
        } }
    }
    suppliedComplete(f); val result = f.owner.finishStoredInputs(NativeRecordingCompletion.COMPLETE)
    check(f.owner.frontier!!.audioUnits == 9uL && f.owner.frontier!!.videoUnits == 5uL && f.owner.frontier!!.videoEnd != null)
    check(result.applicationAdmission == RecordingCheckpointOwner.ApplicationAdmission.UNKNOWN); assertIncomplete(f)
}

fun main(args: Array<String>) {
    require(args.size == 1)
    val directory = Paths.get(args[0]); check(Files.isDirectory(directory))
    val tests = listOf(::completeSdkInputsRemainUnknownAppAdmission, ::unknownLeadingNeverAdmitsMedia,
        ::unknownFileReceiptCannotRequestComplete, ::missingGenerationCannotGrantFileReceipt,
        ::contradictoryGenerationIsSticky, ::unknownEofProducesExplicitUntrimmedPrefix,
        ::originalTimingViewsUseExistingQueuesAndHandle, ::oldFrontierAndClosedResultRemainImmutable,
        ::corruptSidecarReadbackNeverPublishes, ::cancellationDuringReadbackNeverPublishes,
        ::wrongWorkerAndReentryAreRejectedWithoutPoisoning, ::realSdkConstructorFailureClosesExactStorageLease,
        ::unknownMediaCloseRetainsLeaseWithoutRetry, ::unknownRootCloseRetainsLeaseWithoutRetry,
        ::unknownMetadataCloseRetainsExactPendingOwners, ::stereoProfileIsRefusedByActualSdk,
        ::audioVideoSharesOneWorkerAndExactTrackFrontier)
    try { tests.forEach { test -> test(directory); println("SDK recording owner host control passed: ${test.name}") } }
    finally { roots.forEach { it.releaseHostResources() } }
    println("SDK recording owner host controls passed: ${tests.size}")
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
    var strictPrivateFiles = true
    var onCheckpointRead: (() -> Unit)? = null
    var available = Long.MAX_VALUE
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
        val mode = PosixFilePermissions.asFileAttribute(setOf(OWNER_READ, OWNER_WRITE))
        return HostFile(name, FileChannel.open(directory.resolve(name), setOf<OpenOption>(CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS), mode)).also { files.add(it) }
    }
    override fun openRead(name: String): RecordingCheckpointStorage.File {
        verify(); return HostFile(name, FileChannel.open(directory.resolve(name), READ, NOFOLLOW_LINKS)).also { files.add(it) }
    }
    override fun replace(source: String, destination: String) {
        verify(); if (failure == "rename") throw IOException("Injected rename failure")
        Files.move(directory.resolve(source), directory.resolve(destination), ATOMIC_MOVE, REPLACE_EXISTING); moves++
    }
    override fun sync() { verify(); if (failure == "directory-sync") throw IOException("Injected root sync failure"); rootChannel.force(true); rootSyncs++ }
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
            if (name.contains(".checkpoint.")) onCheckpointRead?.invoke()
            val result = channel.read(ByteBuffer.wrap(bytes, offset, count), position)
            if (corruptReadback && name.contains(".checkpoint.") && result > 0) bytes[offset] = (bytes[offset].toInt() xor 1).toByte()
            return result
        }
        override fun write(bytes: ByteArray, offset: Int, count: Int, position: Long): Int {
            if (zeroWrites) return 0
            return channel.write(ByteBuffer.wrap(bytes, offset, minOf(count, maxWrite)), position)
        }
        override fun sync() { if (failure == "media-sync" && name.endsWith(".mp4")) throw IOException("Injected media sync failure"); channel.force(true) }
        override fun close() {
            closeFailure?.let { throw it }; if (attempted) return
            attempted = true; closeCalls[name] = (closeCalls[name] ?: 0) + 1; channel.close()
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
