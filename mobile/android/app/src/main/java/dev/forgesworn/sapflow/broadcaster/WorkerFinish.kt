package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Enqueue finalisation behind the sole in-flight media task. abort must close
 * owned IO promptly and may run on the control caller after the deadline. */
internal fun ThreadPoolExecutor.finishWithin(graceMs: Long, finish: () -> Unit, abort: () -> Unit): Boolean {
    val complete = AtomicBoolean(false)
    try {
        execute {
            try { finish(); complete.set(true) }
            catch (_: Exception) { runCatching(abort) }
        }
        shutdown()
        if (awaitTermination(graceMs, TimeUnit.MILLISECONDS)) return complete.get()
    } catch (_: Exception) { /* Abort also covers rejected finalisation. */ }
    runCatching(abort)
    shutdownNow()
    runCatching { awaitTermination(1, TimeUnit.SECONDS) }
    return false
}
