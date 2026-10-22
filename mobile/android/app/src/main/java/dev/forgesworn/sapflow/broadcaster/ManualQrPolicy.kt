package dev.forgesworn.sapflow.broadcaster

/** One scanner invocation. No native calls, callbacks or IO under this lock.
 * Cancellation retires result authority; real closure separately releases capacity. */
internal class ManualQrPolicy(val beganNs: Long) {
    companion object { const val DURATION_NS = 60_000_000_000L }
    val deadlineNs: Long = if (beganNs >= 0 && beganNs <= Long.MAX_VALUE - DURATION_NS) beganNs + DURATION_NS else -1
    private var live = deadlineNs >= 0
    private var opening = false
    private var nativeClean = true
    private var decoding = false
    private var presentationClosed = false
    private var retained = false
    private var delivered = false
    private var candidate: String? = null
    private var nextDecodeNs = beganNs

    private fun timely(nowNs: Long): Boolean = deadlineNs >= 0 && nowNs >= beganNs && nowNs < deadlineNs
    @Synchronized fun permits(nowNs: Long): Boolean {
        if (!timely(nowNs)) { live = false; candidate = null }
        return live && !retained
    }
    @Synchronized fun claimOpen(nowNs: Long): Boolean {
        if (!permits(nowNs) || opening || !nativeClean) return false
        opening = true; nativeClean = false
        return true
    }
    @Synchronized fun beginDecode(nowNs: Long): Boolean {
        if (!permits(nowNs) || !opening || nativeClean || decoding || nowNs < nextDecodeNs) return false
        decoding = true
        nextDecodeNs = if (nowNs <= Long.MAX_VALUE - 200_000_000L) nowNs + 200_000_000L else Long.MAX_VALUE
        return true
    }
    @Synchronized fun accept(text: String, nowNs: Long): Boolean {
        if (!decoding || !permits(nowNs) || text.isEmpty() || text.length > 2048 || !text.all { it.code in 0..127 }) return false
        candidate = text; live = false
        return true
    }
    @Synchronized fun decodeReturned() { decoding = false }
    @Synchronized fun cancel() { live = false; candidate = null }
    @Synchronized fun nativeClosed(clean: Boolean) {
        opening = false
        if (clean) nativeClean = true else { retained = true; candidate = null; live = false }
    }
    @Synchronized fun presentationClosed() { presentationClosed = true }
    val stopped: Boolean @Synchronized get() = !live
    val nativeAndDecoderClosed: Boolean @Synchronized get() = nativeClean && !opening && !decoding && !retained
    val cleanupUncertain: Boolean @Synchronized get() = retained
    /** A result remains subject to original deadline at actual final delivery.
     * Cancellation/Stop wins immediately even while native close is withheld. */
    @Synchronized fun finish(nowNs: Long): Reply? {
        if (delivered || !nativeAndDecoderClosed || !presentationClosed || live) return null
        delivered = true
        val text = candidate.takeIf { timely(nowNs) }
        candidate = null
        return Reply(text)
    }
    class Reply(val text: String?) { override fun toString() = "QR scan reply (redacted)" }
}

/** Runtime permission replies carry no native request identity. Keep one exact
 * old invocation (or restoration discard obligation) until the real OS reply. */
internal class ManualQrPermissionReplies<T : Any> {
    private var pending: T? = null
    private var discard = false
    val outstanding: Boolean @Synchronized get() = pending != null || discard
    @Synchronized fun pending(): T? = pending
    @Synchronized fun reserve(ticket: T): Boolean {
        if (outstanding) return false
        pending = ticket; return true
    }
    @Synchronized fun restoreDiscard() { if (pending == null) discard = true }
    @Synchronized fun receive(): T? {
        val original = pending; pending = null; discard = false; return original
    }
}
