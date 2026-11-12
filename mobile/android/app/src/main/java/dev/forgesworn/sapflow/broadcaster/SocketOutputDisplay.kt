package dev.forgesworn.sapflow.broadcaster

/** Project only the finished reading for display; sampler and controller stay raw. */
internal fun CaptureSnapshot.forDisplay(): CaptureSnapshot {
    val output = socketOutput
    return if (output.phase == SocketOutputPhase.CLOSED && output.ended && output.sampleAgeMs != null) {
        copy(socketOutput = output.copy(sampleAgeMs = null))
    } else this
}

internal fun socketOutputReadingLabel(output: SocketOutputView?): String =
    if (output?.phase == SocketOutputPhase.CLOSED && output.ended) "Final reading"
    else "sample age ${output?.sampleAgeMs?.let { "${it}ms" } ?: "Unknown"}"
