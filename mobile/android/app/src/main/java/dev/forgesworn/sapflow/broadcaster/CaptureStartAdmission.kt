package dev.forgesworn.sapflow.broadcaster

/** Cold scalar authority only. No callback, renderer, recorder or sink work is
 * performed under this lock. Actual native cleanup releases each preparation. */
internal class CaptureStartAdmission {
    class Preparation internal constructor(val id: Long)
    class Start internal constructor(val id: Long, val preparation: Preparation, val epoch: Long)
    private var serial = 0L
    private var current: Preparation? = null
    private var epoch = -1L
    private var live = false
    private var candidate = false
    private var committed = false
    private var prepared = false
    private var pending: Start? = null
    private var preserve = false
    private var cleaned = false
    private var cleaning = false
    val idle: Boolean @Synchronized get() = current == null
    val preparation: Preparation? @Synchronized get() = current
    @Synchronized fun reserve(): Preparation? {
        if (current != null || serial == Long.MAX_VALUE) return null
        return Preparation(++serial).also {
            current = it; epoch = -1; live = true; candidate = true; committed = false
            prepared = false; pending = null; preserve = false; cleaned = false; cleaning = false
        }
    }
    @Synchronized fun claim(value: Preparation): Boolean {
        if (current != value || !candidate || !live) return false
        candidate = false; return true
    }
    /** Only a queued Prepare that never constructed owners may abandon. */
    @Synchronized fun abandon(value: Preparation) {
        if (current != value || !candidate) return
        candidate = false; live = false; cleaned = true; releaseIfClean()
    }
    @Synchronized fun beginEpoch(value: Preparation, generation: Long): Boolean {
        if (current != value || !live || candidate || cleaning) return false
        epoch = generation; prepared = false; pending = null; cleaned = false; preserve = false
        return true
    }
    @Synchronized fun prepared(value: Preparation, generation: Long): Boolean {
        if (current != value || !live || epoch != generation || cleaning) return false
        prepared = true; return true
    }
    @Synchronized fun start(value: Preparation): Start? {
        if (current != value || !live || !prepared || pending != null || serial == Long.MAX_VALUE) return null
        prepared = false
        return Start(++serial, value, epoch).also { pending = it }
    }
    @Synchronized fun permits(value: Start): Boolean = live && current == value.preparation && epoch == value.epoch && pending == value
    /** After recorder creation, immediately before actual media admission. */
    @Synchronized fun commit(value: Start): Boolean {
        if (!permits(value)) return false
        pending = null; committed = true; return true
    }
    /** End retires only the first pending run, preserving recording rotations. */
    @Synchronized fun cancelInitial(value: Preparation): Boolean {
        if (current != value || committed) return false
        live = false; prepared = false; pending = null; preserve = false
        return true
    }
    @Synchronized fun stop(): Preparation? {
        live = false; prepared = false; pending = null; preserve = false
        return current
    }
    @Synchronized fun owns(value: Preparation): Boolean = current == value
    @Synchronized fun beginCleanup(value: Preparation, rotate: Boolean) {
        if (current != value) return
        preserve = rotate && live
        if (!preserve) live = false
        prepared = false; pending = null; cleaned = false; cleaning = true
    }
    @Synchronized fun cleanup(value: Preparation, complete: Boolean) {
        if (current != value) return
        cleaned = complete
        if (complete && preserve && live && !candidate) { cleaning = false; return }
        releaseIfClean()
    }
    private fun releaseIfClean() {
        if (cleaned && !candidate) { current = null; pending = null; prepared = false; live = false }
    }
}
