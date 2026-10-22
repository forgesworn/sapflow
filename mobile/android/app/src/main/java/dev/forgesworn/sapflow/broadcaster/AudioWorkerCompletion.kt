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
}
