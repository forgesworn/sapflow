package dev.forgesworn.sapflow.broadcaster

/** Explicit internal prerequisite, never native timing/programme authority.
 * Existing application callers retain the publishing-only sample policy. */
internal enum class AudioSampleMode { LEGACY_PUBLISHING, PRESERVE_SOURCE_UNATTESTED }

/** Declared application-coded axis, separate from signed native PTS and the
 * publishing cursor. One instance survives the run's native replacements.
 * A 1024 tick step declares the supported SDK profile; it does not attest how
 * many AUs the platform packed in a callback, H/T, PCM source frames or EOF. */
internal class AudioSourceCursor(initialTick: Long = 0) {
    var nextTick = initialTick; private set
    init { require(initialTick >= 0) }
    fun reserve(publishingTick: Long): Long {
        require(publishingTick == AudioGenerationClock.PRIMING || publishingTick >= 0) { "Stale AAC generation cannot preserve source" }
        val tick = nextTick
        nextTick = Math.addExact(tick, 1024L)
        return tick
    }
}

/** State embedded in each of the EXISTING sixteen transfer slots. Factories
 * are fixed per generation, not allocated per output. The SDK validates one
 * original sample and retains its shared payload in timing views. This owner
 * never queries metadata, copies bytes or creates another handoff queue.
 *
 * A failed native acquisition/close without a documented no-handle rejection
 * quarantines the exact slot and retained handles. No guessed close retry is
 * permitted. The epoch retains every slot even when absent from its free list. */
internal open class AudioTimingTransfer<S : AutoCloseable, V : AutoCloseable, B>(
    private val makeOriginal: (B, Long) -> S,
    private val makeViews: (S, Long, Long?) -> V,
    private val knownRejection: (Throwable) -> Boolean,
    private val onOriginalReturned: () -> Unit = {},
    private val onAcquisitionsReturned: () -> Unit = {},
) : AutoCloseable {
    private var original: S? = null
    private var legacy: S? = null
    private var views: V? = null
    var tick = 0L; private set
    @Volatile var ownershipUnknown = false; private set
    val reusable: Boolean get() = !ownershipUnknown && original == null && legacy == null && views == null
    val sample: S get() { check(!ownershipUnknown); return checkNotNull(legacy) }
    val timingViews: V get() { check(!ownershipUnknown && original == null); return checkNotNull(views) }

    fun attachLegacy(sample: S, publishingTick: Long) {
        check(reusable)
        require(publishingTick >= 0)
        legacy = sample; tick = publishingTick
    }

    /** Owns acquisitions from their first attempt. Publishing PRIMING becomes
     * null only in the approved paired API; the original payload is preserved. */
    fun preserve(buffer: B, sourceTick: Long, nativePtsUs: Long, publishingTick: Long) {
        check(reusable)
        require(sourceTick >= 0 && sourceTick <= Long.MAX_VALUE - 1024L)
        require(publishingTick == AudioGenerationClock.PRIMING || publishingTick >= 0)
        tick = publishingTick
        ownershipUnknown = true
        try { original = makeOriginal(buffer, sourceTick) }
        catch (error: Throwable) { if (knownRejection(error)) ownershipUnknown = false; throw error }
        ownershipUnknown = false
        onOriginalReturned() // Acquisition was latched before any diagnostic callback.
        try {
            ownershipUnknown = true
            views = makeViews(checkNotNull(original), nativePtsUs,
                if (publishingTick == AudioGenerationClock.PRIMING) null else publishingTick)
            ownershipUnknown = false
        } catch (error: Throwable) {
            if (knownRejection(error)) {
                ownershipUnknown = false
                try { closeOriginal() } catch (cleanup: Throwable) { error.addSuppressed(cleanup) }
            }
            throw error
        }
        acquisitionsReturned() // Both handles returned; the original close is still pending.
        closeOriginal() // Views retain the original shared payload, not this wrapper.
    }

    /** Called after construction, so a fixed transfer slot can read its current
     * serial rather than capture an initial constructor argument in a lambda. */
    protected open fun acquisitionsReturned() { onAcquisitionsReturned() }

    private fun closeOriginal() {
        val handle = checkNotNull(original)
        ownershipUnknown = true
        handle.close()
        original = null
        ownershipUnknown = false
    }

    override fun close() {
        check(!ownershipUnknown) { "AAC native handle ownership is unknown; no close retry" }
        check(original != null || views != null || legacy != null) { "AAC transfer has no owner" }
        if (original != null) closeOriginal()
        val handle: AutoCloseable = views ?: legacy ?: return
        ownershipUnknown = true
        handle.close()
        views = null; legacy = null
        ownershipUnknown = false
    }
}

/** Nullable publishing is intentional, not queue loss or a privacy-ready AU. */
internal object AudioSourceAdmission {
    fun publishingWithheld(hasViews: Boolean, tick: Long): Boolean = hasViews && tick == AudioGenerationClock.PRIMING
    fun countPublishingDrop(hasViews: Boolean, tick: Long, publishingQueued: Boolean): Boolean =
        !publishingQueued && !publishingWithheld(hasViews, tick)
}

/** Actual epoch/controller retirement barrier. Queue accounting can reach zero
 * after an owner close throws; that must not make its retained slot droppable.
 * Called with the fixed slot array, without allocating a collection or querying
 * native metadata. This is a nonzero retirement fence, not a unique AU count.
 * Native callback and handoff barriers still apply before any closure receipt. */
internal object AudioSourceRetirement {
    fun outstanding(handoffCount: Int, slots: Array<out AudioTimingTransfer<*, *, *>>): Int {
        require(handoffCount >= 0)
        var count = handoffCount
        var index = 0
        while (index < slots.size) {
            if (slots[index].ownershipUnknown) count = Math.addExact(count, 1)
            index++
        }
        return count
    }
}
