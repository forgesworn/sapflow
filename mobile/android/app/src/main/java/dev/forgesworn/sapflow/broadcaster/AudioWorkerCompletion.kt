package dev.forgesworn.sapflow.broadcaster

/** Handler-owned completion facts survive deferred Stop. A posted completion
 * may arrive before its Thread has returned from finally; never erase it. */
internal class AudioWorkerCompletion {
    var readerDone = true
        private set
    var pumpDone = true
        private set
    val stopped get() = readerDone && pumpDone
    fun begin() { readerDone = false; pumpDone = false }
    fun readerBegan() { readerDone = false }
    fun readerCompleted() { readerDone = true }
    fun pumpCompleted() { pumpDone = true }
    fun observeStopped(readerAlive: Boolean, pumpAlive: Boolean) {
        readerDone = readerDone || !readerAlive
        pumpDone = pumpDone || !pumpAlive
    }
    /** SAME cold barrier used by the concrete native owner and host race tests.
     * Worker completion messages alone do not mean Thread.finally has returned.
     * Read actual successful submissions only after both real joins; unknown
     * joins/counts stay null, never an invented no-input/EOS receipt. */
    fun joinedInputFrames(reader: Thread?, pump: Thread?, acceptedFrames: java.util.concurrent.atomic.AtomicLong): Long? {
        if (reader === Thread.currentThread() || pump === Thread.currentThread()) return null
        try { reader?.join(1000); pump?.join(1000) }
        catch (error: Exception) {
            if (error is InterruptedException) Thread.currentThread().interrupt()
            return null
        }
        if (reader?.isAlive == true || pump?.isAlive == true) return null
        return acceptedFrames.get().takeIf { it >= 0 }
    }

}
