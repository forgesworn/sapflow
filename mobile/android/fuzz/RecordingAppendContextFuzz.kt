package dev.forgesworn.sapflow.broadcaster

import java.security.MessageDigest

/** Jazzer-compatible byte-array target; no Jazzer dependency or native facts.
 * Compile in the same Kotlin module as the actual production context codec. */
object RecordingAppendContextFuzz {
    private val identity = RecordingCatalogue.Identity("11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222", 7, "AUDIO_ONLY_48K_MONO")
    private val root = RecordingCheckpointStorage.Node(Long.MIN_VALUE, -3, true, false, 2, 4096)
    private val media = RecordingCheckpointStorage.Node(Long.MAX_VALUE, Long.MIN_VALUE, false, true, 1, 0)
    private const val FIRST_NONCE = "33333333-3333-3333-3333-333333333333"
    private const val SECOND_NONCE = "44444444-4444-4444-4444-444444444444"

    private fun seed(nonce: String) = RecordingAppendContext.create(identity, nonce, root, media).encode()
    private fun sha(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)
    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it) }

    private fun refused(bytes: ByteArray, expected: RecordingCatalogue.Identity,
        actualRoot: RecordingCheckpointStorage.Node, actualMedia: RecordingCheckpointStorage.Node) {
        try {
            RecordingAppendContext.decode(bytes, expected, actualRoot, actualMedia)
        } catch (_: IllegalArgumentException) { return }
        error("Accepted context without its independently expected identity/descriptors")
    }

    private fun exercise(bytes: ByteArray) {
        val value = try {
            RecordingAppendContext.decode(bytes, identity, root, media)
        } catch (_: IllegalArgumentException) { return }
        // Assertions sit OUTSIDE the parser's expected-refusal catch.
        check(value.identity == identity && value.encode().contentEquals(bytes))
        val bindings = value.bindings(identity, root, media)
        val expectedRecording = "sapflow.android.checkpoint.recording.v1\n${identity.showId}\n${identity.segmentId}\n${identity.segmentIndex}\n${identity.profileId}\n${value.nonce}"
        val expectedSource = "sapflow.android.checkpoint.file.v1\n${root.device}\n${root.inode}\n${media.device}\n${media.inode}\n${value.nonce}"
        check(bindings.recording == hex(sha(expectedRecording.toByteArray(Charsets.UTF_8))))
        check(bindings.sourceFile == hex(sha(expectedSource.toByteArray(Charsets.UTF_8))))
        refused(bytes, identity.copy(segmentIndex = 8), root, media)
        refused(bytes, identity, root.copy(inode = root.inode xor 1L), media)
        refused(bytes, identity, root, media.copy(inode = media.inode xor 1L))
        refused(bytes, identity, root, media.copy(links = 2))
    }

    @JvmStatic fun fuzzerTestOneInput(data: ByteArray) {
        // One beyond the production bound exercises oversize rejection; never
        // allocate/copy an unbounded engine input or mutate its original bytes.
        if (data.size > RecordingAppendContext.MAX_BYTES + 1) return
        val original = data.copyOf()
        exercise(data)
        val generated = seed(FIRST_NONCE)
        val bodySize = generated.size - 32
        // Bounded three-byte records mutate a valid generated envelope before
        // repairing ONLY its checksum, reaching semantic/version/binding paths.
        var at = 0
        while (at + 2 < data.size) {
            val position = (((data[at].toInt() and 255) shl 8) or (data[at + 1].toInt() and 255)) % bodySize
            generated[position] = data[at + 2]
            at += 3
        }
        val digest = sha(generated.copyOfRange(0, bodySize))
        digest.copyInto(generated, bodySize)
        exercise(generated)
        check(data.contentEquals(original)) { "Parser or target mutated original fuzz input" }
    }

    /** Exactly two synthetic seed replays. This is NOT coverage-guided fuzzing. */
    @JvmStatic fun main(args: Array<String>) {
        require(args.isEmpty())
        val seeds = arrayOf(seed(FIRST_NONCE), seed(SECOND_NONCE))
        val hashes = arrayOf("d64fd6a770a04e1929212921a08d4151826766a549aa063568c7a94100dc3d9c", "fd85f258c1e2b1ffbc0b9728fcfc3ee58c63d1d74d1cec5f6a12f886ccdaf95a")
        for (index in seeds.indices) {
            check(hex(sha(seeds[index])) == hashes[index]) { "Synthetic replay seed changed" }
            fuzzerTestOneInput(seeds[index])
        }
        println("RecordingAppendContextFuzz: 2 synthetic seed replays; coverageGuided=false")
    }
}
