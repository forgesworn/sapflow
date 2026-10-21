package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock

/** Control cleanup must report a stalled writer, never wait indefinitely. */
internal fun ReentrantLock.cleanupWithin(timeoutMs: Long, cleanup: () -> Unit): Boolean {
    if (!tryLock(timeoutMs, TimeUnit.MILLISECONDS)) return false
    try { cleanup(); return true }
    finally { unlock() }
}
