package dev.forgesworn.sapflow.broadcaster

/** Control-owner diagnostics only. Messages are for the UI, never the log.
 * Log entries accept fixed codes and scalar ownership facts, not media or paths. */
internal class CaptureLifecycleDiagnostics {
    enum class Code {
        PREPARING, PREPARED, STARTED, ORIENTATION_REQUEST, STOP_REQUEST, CLEANUP_COMPLETED,
        CAPTURE_FAILURE, RECORDING_FAILURE, GPU_FAILURE, AUDIO_READER_FAILURE, AUDIO_PUMP_FAILURE,
        AUDIO_OWNER_FAILURE, VIDEO_ENCODER_FAILURE, ENCODER_EOS_TIMEOUT, AAC_EOS_FAILURE,
        VIDEO_EOS_FAILURE, ORIENTATION_TIMEOUT, CLEANUP_TIMEOUT, AAC_QUIESCENCE_FAILURE,
        CLEANUP_FAILURE, RECORDING_FINALISATION_FAILURE,
    }
    data class Context(val epoch: Long, val generation: Long, val pose: CardinalPose,
        val stage: CaptureStageName, val flags: Int)
    enum class CaptureStageName { IDLE, PREPARING, PREPARED, CAPTURING, STOPPING, ERROR }
    data class Entry(val code: Code, val context: Context) {
        fun logLine(): String = "code=${code.name} epoch=${context.epoch} generation=${context.generation} pose=${context.pose.name} stage=${context.stage.name} flags=${context.flags}"
    }
    data class Failure(val code: Code, val message: String, val context: Context)
    private val entries = ArrayDeque<Entry>()
    private val recordedFailures = mutableSetOf<Code>()
    var firstFailure: Failure? = null
        private set
    private var epoch = -1L

    /** Only an actual new capture epoch rearms first-failure admission. History
     * remains bounded across manual retries, so Prepare does not erase evidence. */
    fun beginEpoch(epoch: Long) {
        require(epoch >= 0 && epoch > this.epoch)
        this.epoch = epoch
        firstFailure = null
        recordedFailures.clear()
    }

    fun transition(code: Code, context: Context): Entry? {
        if (context.epoch != epoch) return null
        return Entry(code, context).also {
            entries.addLast(it)
            if (entries.size > 16) entries.removeFirst()
        }
    }

    fun failure(code: Code, message: String, context: Context): Entry? {
        if (context.epoch != epoch) return null
        if (firstFailure == null) firstFailure = Failure(code, message.take(240), context)
        // Repeated owner callbacks cannot create per-frame failure logs.
        return if (recordedFailures.add(code)) transition(code, context) else null
    }

    fun message(transitionReason: String): String = firstFailure?.message ?: transitionReason
    val transitionCount: Int get() = entries.size
    fun history(): List<Entry> = entries.toList()
}
