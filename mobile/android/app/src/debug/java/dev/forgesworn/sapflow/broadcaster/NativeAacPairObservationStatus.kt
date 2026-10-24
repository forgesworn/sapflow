// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

/** Cold lab reporting only. Neither cleanup nor completion certifies content. */
internal class NativeAacPairObservationStatus(private val plannedKeys: List<String>) {
    init { require(plannedKeys.size == 6 && plannedKeys.toSet().size == 6) }

    data class Observation(
        val acquiredResourcesClosed: Boolean,
        val failure: String?,
        val inputEos: Boolean,
        val outputEos: Boolean,
        val streamClosed: Boolean,
        val pcmAvailable: Boolean,
    ) {
        val completed: Boolean get() = acquiredResourcesClosed && failure == null &&
            inputEos && outputEos && streamClosed && pcmAvailable
    }

    var observationsRecorded = 0
        private set
    var completedObservations = 0
        private set
    var firstFailure: String? = null
        private set
    var mayContinue = true
        private set
    val allDecoderObservationsComplete: Boolean get() = observationsRecorded == plannedKeys.size &&
        completedObservations == plannedKeys.size && firstFailure == null && mayContinue

    fun record(key: String, observation: Observation) {
        check(mayContinue) { "An acquired native/file owner remains unknown" }
        check(observationsRecorded < plannedKeys.size && plannedKeys[observationsRecorded] == key) {
            "Expected each registered case/role observation once in order"
        }
        observationsRecorded++
        if (observation.completed) completedObservations++
        else if (firstFailure == null) firstFailure = key + ": " +
            (observation.failure ?: if (!observation.acquiredResourcesClosed) "Acquired resource closure unknown"
                else "Decoder observation incomplete: actual EOS and closed PCM required")
        if (!observation.acquiredResourcesClosed) mayContinue = false
    }

    companion object {
        fun pcmReadbackAllowed(acquisitionsKnown: Boolean, streamAcquired: Boolean, streamClosed: Boolean): Boolean =
            acquisitionsKnown && (!streamAcquired || streamClosed)

        fun acquiredResourcesClosed(
            acquisitionsKnown: Boolean,
            decoderAcquired: Boolean, decoderReleased: Boolean,
            extractorAcquired: Boolean, extractorReleased: Boolean,
            streamAcquired: Boolean, streamClosed: Boolean,
            nativeIndexUncertain: Boolean,
        ): Boolean = acquisitionsKnown && !nativeIndexUncertain &&
            (!decoderAcquired || decoderReleased) && (!extractorAcquired || extractorReleased) &&
            (!streamAcquired || streamClosed)
    }
}
