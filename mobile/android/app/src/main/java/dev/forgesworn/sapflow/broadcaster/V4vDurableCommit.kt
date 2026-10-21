package dev.forgesworn.sapflow.broadcaster

import java.security.MessageDigest

/** AtomicFile.finishWrite has a void/log-only rename failure path. Never
 * acknowledge a journal mutation until its actual committed bytes and directory sync succeed. */
internal class V4vDurableCommit(private val replace: (ByteArray) -> Unit,
    private val readCommitted: () -> ByteArray, private val syncDirectory: () -> Unit) {
    fun write(bytes: ByteArray) {
        replace(bytes)
        check(MessageDigest.isEqual(bytes, readCommitted())) { "V4V journal replacement was not confirmed" }
        syncDirectory()
    }
}
