package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock

/** Control cleanup must report a stalled writer, never wait indefinitely.
 * An interrupted caller (a reader thread being stopped while it reports its
 * own failure) is a cleanup that did not happen yet, not a crash: the flag is
 * kept for the caller and the owner that interrupted it releases instead. */
internal fun ReentrantLock.cleanupWithin(timeoutMs: Long, cleanup: () -> Unit): Boolean {
    val locked = try { tryLock(timeoutMs, TimeUnit.MILLISECONDS) }
    catch (_: InterruptedException) { Thread.currentThread().interrupt(); return false }
    if (!locked) return false
    try { cleanup(); return true }
    finally { unlock() }
}
