package dev.forgesworn.sapflow.broadcaster

/** A system permission reply has no request identity. Keep its original
 * selection, even after cancellation, until that reply is consumed. */
internal class PendingPermissionReply<T : Any> {
    private var pending: T? = null
    val outstanding: Boolean get() = pending != null
    fun reserve(intent: T): Boolean {
        if (pending != null) return false
        pending = intent
        return true
    }
    fun receive(): T? = pending.also { pending = null }
}
