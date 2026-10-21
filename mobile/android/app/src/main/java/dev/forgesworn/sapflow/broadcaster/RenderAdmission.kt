package dev.forgesworn.sapflow.broadcaster

/** One scheduled render task plus a scalar latest-frame flag. Producer callback
 * bursts never become a queue of runnables or retained camera frame owners. */
internal class RenderAdmission {
    private var closed = false
    private var scheduled = false
    private var pending = false

    @Synchronized fun offer(): Boolean {
        if (closed) return false
        pending = true
        if (scheduled) return false
        scheduled = true
        return true
    }

    @Synchronized fun begin(): Boolean {
        if (closed || !scheduled || !pending) return false
        pending = false
        return true
    }

    @Synchronized fun finish(): Boolean {
        if (closed || !pending) { scheduled = false; return false }
        return true
    }

    @Synchronized fun close() { closed = true; pending = false }
}
