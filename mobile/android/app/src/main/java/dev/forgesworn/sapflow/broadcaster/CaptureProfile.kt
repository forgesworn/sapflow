package dev.forgesworn.sapflow.broadcaster

/** Explicit, immutable capture intent. Rates are bits/s; transport pacing is
 * bytes/s. A grant is a ceiling, never permission to silently change quality. */
internal enum class CaptureProfile(val label: String, val landscapeWidth: Int, val landscapeHeight: Int,
    val videoBps: Int, val minimumPublishBps: Long) {
    HD_720("720p", 1280, 720, 2_500_000, 4_000_000),
    SD_480("480p", 848, 480, 600_000, 1_000_000);

    val audioBps: Int get() = 96_000
    val frameRate: Int get() = 30
    val elementaryBps: Int get() = videoBps + audioBps
    fun requireGrant(maxPublishBps: Long) {
        require(maxPublishBps >= minimumPublishBps) { "$label needs a grant of at least ${minimumPublishBps / 1_000_000} Mbps; choose a supported quality before Prepare" }
    }
    /** Original and retransmitted SRT data share this native wire pacer. The
     * server's admission cap counts unique TS bytes, not all NIC traffic. */
    fun srtPacingBytesPerSecond(maxPublishBps: Long): ULong {
        requireGrant(maxPublishBps)
        return minOf(1_250_000L, maxPublishBps / 8).toULong()
    }

    /** Hardware capability values are supplied by Android, keeping selection
     * testable without pretending capability support is physical acceptance. */
    fun supportedBy(widthAlignment: Int, heightAlignment: Int, minBps: Int, maxBps: Int,
        surface: Boolean, cbr: Boolean, baseline: Boolean, sizeAndRate: (Int, Int, Double) -> Boolean): Boolean =
        widthAlignment > 0 && heightAlignment > 0 &&
            landscapeWidth % widthAlignment == 0 && landscapeHeight % heightAlignment == 0 &&
            landscapeHeight % widthAlignment == 0 && landscapeWidth % heightAlignment == 0 &&
            videoBps in minBps..maxBps && surface && cbr && baseline &&
            sizeAndRate(landscapeWidth, landscapeHeight, frameRate.toDouble()) &&
            sizeAndRate(landscapeHeight, landscapeWidth, frameRate.toDouble())

    companion object {
        val DEFAULT = HD_720
        /** Integer letterbox fit. At 480p the three residual rows/columns are
         * black, not a stretch or a crop of the sensor image. */
        fun fitViewport(width: Int, height: Int, imageWidth: Int, imageHeight: Int): IntArray {
            require(width > 0 && height > 0 && imageWidth > 0 && imageHeight > 0)
            val fitWidth: Int
            val fitHeight: Int
            if (width.toLong() * imageHeight <= height.toLong() * imageWidth) {
                fitWidth = width; fitHeight = maxOf(1, (width.toLong() * imageHeight / imageWidth).toInt())
            } else {
                fitHeight = height; fitWidth = maxOf(1, (height.toLong() * imageWidth / imageHeight).toInt())
            }
            return intArrayOf((width - fitWidth) / 2, (height - fitHeight) / 2, fitWidth, fitHeight)
        }
    }
}

/** Reserves Prepare synchronously before its codec-owner task can run. UI
 * quality selection observes pending even while that Handler is busy. */
internal class CapturePrepareAdmission {
    private class Request(val profile: CaptureProfile) {
        val live = java.util.concurrent.atomic.AtomicBoolean(true)
    }
    private val admitted = java.util.concurrent.atomic.AtomicReference<Request?>()
    val pending: Boolean get() = admitted.get() != null
    // Retain the slot until its queued task drains. A cancelled old task cannot
    // release or adopt a newer Prepare's profile.
    fun cancelPending() { admitted.get()?.live?.set(false) }
    fun submit(profile: CaptureProfile, canPrepare: Boolean, enqueue: (Runnable) -> Boolean,
        prepare: (CaptureProfile) -> Unit): Boolean {
        val request = Request(profile)
        if (!canPrepare || !admitted.compareAndSet(null, request)) return false
        val task = Runnable {
            try { if (admitted.get() === request && request.live.get()) prepare(request.profile) }
            finally { admitted.compareAndSet(request, null) }
        }
        try {
            if (enqueue(task)) return true
        } catch (error: Exception) { admitted.compareAndSet(request, null); throw error }
        admitted.compareAndSet(request, null)
        return false
    }
}
