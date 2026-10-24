package dev.forgesworn.sapflow.broadcaster

/** A preflight horizon, never a recording duration limit or a space guarantee.
 * Query off the UI/codec thread and recheck before admitting capture. */
internal object RecordingStorage {
    data class Budget(val horizonSeconds: Long, val payloadBytes: Long, val requiredBytes: Long) {
        init { require(horizonSeconds > 0 && payloadBytes > 0 && requiredBytes >= payloadBytes) }
    }
    enum class State { AVAILABLE, LOW, UNAVAILABLE }
    data class Result(val state: State, val budget: Budget, val availableBytes: Long?)

    fun budget(profile: CaptureProfile, horizonSeconds: Long = 60, reserveBytes: Long = 32L * 1024 * 1024): Budget {
        require(horizonSeconds > 0 && reserveBytes >= 0)
        return budget(CaptureIntent.AudioVideo(profile), horizonSeconds, reserveBytes)
    }
    fun budget(intent: CaptureIntent, horizonSeconds: Long = 60, reserveBytes: Long = 32L * 1024 * 1024): Budget {
        require(horizonSeconds > 0 && reserveBytes >= 0)
        val bits = Math.multiplyExact(intent.elementaryBps.toLong(), horizonSeconds)
        val payload = Math.addExact(bits / 8, if (bits % 8 == 0L) 0 else 1)
        // Account for mux/output writes and bitrate variation without claiming
        // that the target encoder bitrate bounds all future disk consumption.
        val headroom = maxOf(16L * 1024 * 1024, payload / 2)
        return Budget(horizonSeconds, payload, Math.addExact(Math.addExact(payload, headroom), reserveBytes))
    }

    fun check(budget: Budget, available: () -> Long?): Result {
        val bytes = try { available()?.takeIf { it >= 0 } } catch (_: Exception) { null }
        return Result(when { bytes == null -> State.UNAVAILABLE; bytes < budget.requiredBytes -> State.LOW; else -> State.AVAILABLE }, budget, bytes)
    }
}
