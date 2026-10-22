package dev.forgesworn.sapflow.broadcaster

/** Cold scalar authority only. No callback, renderer, recorder or sink work is
 * performed under this lock. Actual native cleanup releases each preparation. */
internal class CaptureStartAdmission {
    class Preparation internal constructor(val id: Long, val intent: CaptureIntent, val audioInputId: String? = null,
        val backgroundAudio: Boolean = false, val orientation: CaptureOrientationChoice.Selection? = null,
        val cameraChoice: CameraSourceChoice? = null)
    class Start internal constructor(val id: Long, val preparation: Preparation, val epoch: Long)
    private var serial = 0L
    private var current: Preparation? = null
    private var epoch = -1L
    private var selectedCamera: CameraSourceChoice? = null
    private var live = false
    private var candidate = false
    private var committed = false
    private var prepared = false
    private var pending: Start? = null
    private var preserve = false
    private var cleaned = false
    private var cleaning = false
    private var serviceReady = false
    private val elapsed = CaptureElapsed()
    private var timingPreparation: Preparation? = null
    val idle: Boolean @Synchronized get() = current == null
    val preparation: Preparation? @Synchronized get() = current
    val elapsedPreparation: Preparation? @Synchronized get() = timingPreparation
    @Synchronized fun reserve(intent: CaptureIntent = CaptureIntent.DEFAULT, audioInputId: String? = null,
        backgroundAudio: Boolean = false, orientation: CaptureOrientationChoice.Selection? = null, cameraChoice: CameraSourceChoice = CameraSourceChoice.REAR): Preparation? {
        if (current != null || serial == Long.MAX_VALUE) return null
        if (audioInputId != null && !AudioInputOption.validId(audioInputId)) return null
        if (backgroundAudio && intent.hasVideo) return null
        val frozen = if (intent.hasVideo) orientation ?: CaptureOrientationChoice.AUTO.freeze(true, true, CardinalPose.NATURAL) else null
        return Preparation(++serial, intent, audioInputId, backgroundAudio, frozen, cameraChoice.takeIf { intent.hasVideo }).also {
            selectedCamera = it.cameraChoice
            timingPreparation = it; elapsed.reset()
            current = it; epoch = -1; live = true; candidate = true; committed = false
            prepared = false; pending = null; preserve = false; cleaned = false; cleaning = false
            serviceReady = false
        }
    }
    @Synchronized fun claim(value: Preparation): Boolean {
        if (current != value || !candidate || !live) return false
        candidate = false; return true
    }
    /** Rechecked on the actual codec owner, including events queued before
     * Stop, fixed selection or successor admission. Auto may still coalesce
     * poses during its preserved orientation cleanup, as before. */
    @Synchronized fun permitsOrientation(value: Preparation): Boolean = current === value && live &&
        value.intent.hasVideo && value.orientation?.automatic == true
    @Synchronized fun cameraChoice(value: Preparation): CameraSourceChoice? = selectedCamera.takeIf { current === value }
    @Synchronized fun selectCamera(value: Preparation, choice: CameraSourceChoice): Boolean {
        if (!permitsCamera(value)) return false
        selectedCamera = choice; return true
    }
    @Synchronized fun canSwitchCamera(value: Preparation): Boolean = permitsCamera(value) && pending == null && !cleaning
    @Synchronized fun permitsCamera(value: Preparation): Boolean = current === value && live && value.intent.hasVideo
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
        if (current != value || !live || !prepared || pending != null || serial == Long.MAX_VALUE || value.backgroundAudio && !serviceReady) return null
        prepared = false
        return Start(++serial, value, epoch).also { pending = it }
    }
    @Synchronized fun permits(value: Start): Boolean = live && current == value.preparation && epoch == value.epoch && pending == value &&
        (!value.preparation.backgroundAudio || serviceReady)
    /** Promotion is a scalar prerequisite, never inferred from a queued service Intent. */
    @Synchronized fun servicePromoted(value: Preparation): Boolean {
        if (current !== value || !live || !value.backgroundAudio || value.intent.hasVideo) return false
        serviceReady = true; return true
    }
    @Synchronized fun backgroundReady(value: Preparation): Boolean = current === value && live && value.backgroundAudio && serviceReady
    @Synchronized fun continuesInBackground(value: Preparation): Boolean = backgroundReady(value) && committed &&
        !cleaning && pending == null && !value.intent.hasVideo
    /** Exact callback/action fence: never resample and stop a successor. */
    @Synchronized fun stop(value: Preparation, nowNs: Long? = null): Boolean {
        if (current !== value || !live) return false
        stop(nowNs); return true
    }
    /** Unexpected loss of the exact logical service owner, not a native input callback.
     * A normal Stop that already won must not be reclassified as a failed save. */
    @Synchronized fun interrupt(value: Preparation, nowNs: Long? = null): Boolean {
        if (current !== value || !live) return false
        stop(nowNs); return true
    }
    /** After recorder creation, immediately before actual media admission. */
    @Synchronized fun commit(value: Start, nowNs: Long? = null): Boolean {
        if (!permits(value)) return false
        elapsed.commit(nowNs ?: System.nanoTime())
        pending = null; committed = true; return true
    }
    /** End retires only the first pending run, preserving recording rotations. */
    @Synchronized fun cancelInitial(value: Preparation, nowNs: Long? = null): Boolean {
        if (current != value) return false
        elapsed.end(nowNs ?: System.nanoTime())
        if (committed) return false
        live = false; prepared = false; pending = null; preserve = false
        return true
    }
    @Synchronized fun stop(nowNs: Long? = null): Preparation? {
        elapsed.stop(nowNs ?: System.nanoTime())
        live = false; prepared = false; pending = null; preserve = false
        return current
    }
    @Synchronized fun owns(value: Preparation): Boolean = current == value
    @Synchronized fun freezeElapsed(nowNs: Long? = null) = elapsed.stop(nowNs ?: System.nanoTime())
    @Synchronized fun elapsed(value: Preparation?, nowNs: Long?, publisherAttached: Boolean, destinationEndNs: Long?): CaptureElapsedSnapshot {
        if (value !== timingPreparation) return CaptureElapsedSnapshot(null, null, true)
        return elapsed.snapshot(nowNs ?: System.nanoTime(), publisherAttached, destinationEndNs)
    }
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
