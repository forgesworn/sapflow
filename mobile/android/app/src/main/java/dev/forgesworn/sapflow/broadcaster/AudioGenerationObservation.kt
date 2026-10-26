package dev.forgesworn.sapflow.broadcaster

/** Bounded scalar diagnostics for one actual codec generation. Never an AAC
 * timing/EOF receipt or an admission decision. No media bytes are retained. */
internal class AudioGenerationObservation(private val generation: Long) {
    enum class Phase { FORMAT, OUTPUT_EOS }
    enum class Trim { LEADING, PADDING }
    enum class State { UNSEEN, API_UNAVAILABLE, ABSENT, VALUE, WRONG_TYPE, READ_FAILED, UNSUPPORTED, CONFLICT }
    data class TrimSnapshot(val api: Int, val state: State, val reportedValue: Int?)
    data class Snapshot(
        val generation: Long,
        val formatLeading: TrimSnapshot, val formatPadding: TrimSnapshot,
        val eosLeading: TrimSnapshot, val eosPadding: TrimSnapshot,
        val submittedBuffers: Long, val submittedFrames: Long,
        val submittedRawFrames: Long, val submittedZeroFrames: Long,
        val inputEosQueued: Long,
        val outputBuffers: Long, val nonConfigDataBuffers: Long, val configBuffers: Long,
        val emptyBuffers: Long, val preStartBuffers: Long, val nonConfigBytes: Long,
        val outputFlagsOr: Int, val outputEosBuffers: Long,
        val firstNonConfigPtsUs: Long?, val lastNonConfigPtsUs: Long?, val ptsWentBackwards: Boolean,
        val constructedSamples: Long, val primingFilteredBuffers: Long, val staleFilteredBuffers: Long,
        val handoffAttempts: Long, val handoffOfferReturns: Long,
        val handoffCallbackEntries: Long, val handoffCallbackReturns: Long,
        val overflow: Boolean, val invalidDiagnostic: Boolean, val trimConflict: Boolean,
        val producersJoined: Boolean, val callbackBarrier: Boolean, val nativeStopReturned: Boolean,
        val handoffIdle: Boolean,
        // Buffer observations only, including malformed/partial/early data.
        // No counter is an AU count, coded extent or completeness authority.
        val cardinalityUnknownBuffers: Long, val partialOutputBuffers: Long,
        val otherOutputFlagsBuffers: Long, val invalidOutputRanges: Long,
        val earlyDataBuffers: Long, val nonemptyOutputEosBuffers: Long,
        val checkedBorrowedOutputRanges: Long, val invalidBorrowedOutputRanges: Long,
        val selectedProfile: AudioAacProfile.Snapshot? = null,
    )
    private class Slot {
        var api = 0
        var state = State.UNSEEN
        var value = 0
        var hasValue = false
        fun snapshot() = TrimSnapshot(api, state, if (hasValue) value else null)
    }
    private val formatLeading = Slot()
    private val formatPadding = Slot()
    private val eosLeading = Slot()
    private val eosPadding = Slot()
    private var frozen: Snapshot? = null
    private val profile = AudioAacProfile.Observation(this, generation)
    private var submittedBuffers = 0L
    private var submittedFrames = 0L
    private var rawFrames = 0L
    private var zeroFrames = 0L
    private var inputEosQueued = 0L
    private var outputBuffers = 0L
    private var dataBuffers = 0L
    private var configBuffers = 0L
    private var emptyBuffers = 0L
    private var preStartBuffers = 0L
    private var dataBytes = 0L
    private var flagsOr = 0
    private var outputEosBuffers = 0L
    private var hasPts = false
    private var firstPts = 0L
    private var lastPts = 0L
    private var ptsWentBackwards = false
    private var constructedSamples = 0L
    private var primingFiltered = 0L
    private var staleFiltered = 0L
    private var handoffAttempts = 0L
    private var handoffReturns = 0L
    private var callbackEntries = 0L
    private var callbackReturns = 0L
    private var overflow = false
    private var invalid = generation <= 0
    private var trimConflict = false
    private var producersJoined = false
    private var callbackBarrier = false
    private var stopReturned = false
    private var handoffIdle = false
    private var cardinalityUnknownBuffers = 0L
    private var partialOutputBuffers = 0L
    private var otherOutputFlagsBuffers = 0L
    private var invalidOutputRanges = 0L
    private var earlyDataBuffers = 0L
    private var nonemptyOutputEosBuffers = 0L
    private var checkedBorrowedOutputRanges = 0L
    private var invalidBorrowedOutputRanges = 0L

    private fun active(value: Long) = value == generation && frozen == null
    private fun add(value: Long, amount: Long): Long {
        if (amount < 0) { invalid = true; return value }
        if (amount > Long.MAX_VALUE - value) { overflow = true; return Long.MAX_VALUE }
        return value + amount
    }
    private fun slot(phase: Phase, trim: Trim) = when (phase) {
        Phase.FORMAT -> if (trim == Trim.LEADING) formatLeading else formatPadding
        Phase.OUTPUT_EOS -> if (trim == Trim.LEADING) eosLeading else eosPadding
    }

    @Synchronized fun profileConfigured(value: Long, facts: AudioAacProfile.Setup): Boolean =
        if (!active(value) || callbackBarrier) false else profile.configured(this, value, facts)
    @Synchronized fun profileFormat(value: Long, phase: Phase, facts: AudioAacProfile.Format): Boolean =
        if (!active(value) || callbackBarrier) false else profile.format(this, value,
            if (phase == Phase.FORMAT) AudioAacProfile.Phase.OUTPUT_FORMAT else AudioAacProfile.Phase.OUTPUT_EOS, facts)

    /** Cold optional metadata only. Missing and malformed remain distinct;
     * a reported zero is never substituted for either. No effective trim is
     * derived, even when both phases happen to report matching values. */
    @Synchronized fun trim(value: Long, phase: Phase, trim: Trim, api: Int, state: State, reported: Int = 0) {
        if (!active(value)) return
        if (callbackBarrier) { invalid = true; return }
        val target = slot(phase, trim)
        var observed = if (api < 30) State.API_UNAVAILABLE else state
        if (observed == State.VALUE && (reported < 0 || reported > (if (trim == Trim.LEADING) 8191 else 1023))) {
            observed = State.UNSUPPORTED
        }
        val hasValue = observed == State.VALUE || observed == State.UNSUPPORTED
        if (target.state != State.UNSEEN) {
            if (target.api != api || target.state != observed || target.hasValue != hasValue || hasValue && target.value != reported) {
                target.state = State.CONFLICT; trimConflict = true
            }
            return
        }
        target.api = api; target.state = observed; target.hasValue = hasValue; target.value = reported
        val other = slot(if (phase == Phase.FORMAT) Phase.OUTPUT_EOS else Phase.FORMAT, trim)
        if (target.state == State.VALUE && other.state == State.VALUE && target.value != other.value) {
            target.state = State.CONFLICT; other.state = State.CONFLICT; trimConflict = true
        }
    }

    /** Called only after the existing queueInputBuffer actually returned.
     * Counts copied source components, not AudioRecord read totals. */
    @Synchronized fun inputQueued(value: Long, raw: Long, zero: Long) {
        if (!active(value)) return
        if (producersJoined || raw < 0 || zero < 0 || raw == 0L && zero == 0L) { invalid = true; return }
        submittedBuffers = add(submittedBuffers, 1)
        rawFrames = add(rawFrames, raw); zeroFrames = add(zeroFrames, zero)
        submittedFrames = add(submittedFrames, add(raw, zero))
    }
    @Synchronized fun inputEosQueued(value: Long) {
        if (!active(value)) return
        inputEosQueued = add(inputEosQueued, 1)
    }

    /** Original callback observations precede legacy priming/timestamp policy.
     * A buffer count is not an AU count, decoded frames or encoded extent. */
    @Synchronized fun output(value: Long, bytes: Int, ptsUs: Long, flags: Int, config: Boolean, eos: Boolean, started: Boolean, offset: Int = 0) {
        if (!active(value)) return
        if (callbackBarrier) { invalid = true; return }
        // This scalar classification precedes the legacy priming/stale filter.
        // No classification result controls capture or recording admission.
        val disposition = AudioAacOutputDisposition.classify(offset, bytes, flags, started)
        if (AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.CARDINALITY_UNKNOWN)) {
            cardinalityUnknownBuffers = add(cardinalityUnknownBuffers, 1)
            if (!started) earlyDataBuffers = add(earlyDataBuffers, 1)
            if (AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.EOS)) nonemptyOutputEosBuffers = add(nonemptyOutputEosBuffers, 1)
        }
        if (AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.PARTIAL_FLAG)) partialOutputBuffers = add(partialOutputBuffers, 1)
        if (AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.OTHER_FLAGS)) otherOutputFlagsBuffers = add(otherOutputFlagsBuffers, 1)
        if (AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.RANGE_INVALID)) {
            invalidOutputRanges = add(invalidOutputRanges, 1); invalid = true
        }
        outputBuffers = add(outputBuffers, 1); flagsOr = flagsOr or flags
        if (!started) preStartBuffers = add(preStartBuffers, 1)
        if (eos) outputEosBuffers = add(outputEosBuffers, 1)
        if (config) configBuffers = add(configBuffers, 1)
        if (bytes < 0) invalid = true
        if (bytes == 0) emptyBuffers = add(emptyBuffers, 1)
        if (bytes > 0 && !config) {
            dataBuffers = add(dataBuffers, 1); dataBytes = add(dataBytes, bytes.toLong())
            if (!hasPts) { firstPts = ptsUs; hasPts = true }
            else if (ptsUs < lastPts) ptsWentBackwards = true
            lastPts = ptsUs
        }
    }
    /** Only an already borrowed native buffer supplies capacity. No extra
     * native lookup, throw, admission decision or compressed-byte validation. */
    @Synchronized fun borrowedOutputRange(value: Long, offset: Int, bytes: Int, capacity: Int) {
        if (!active(value)) return
        if (callbackBarrier) { invalid = true; return }
        checkedBorrowedOutputRanges = add(checkedBorrowedOutputRanges, 1)
        val disposition = AudioAacOutputDisposition.classify(offset, bytes, 0, true, capacity)
        if (capacity < 0 || AudioAacOutputDisposition.has(disposition, AudioAacOutputDisposition.RANGE_INVALID)) {
            invalidBorrowedOutputRanges = add(invalidBorrowedOutputRanges, 1); invalid = true
        }
    }
    @Synchronized fun constructed(value: Long) {
        if (active(value)) constructedSamples = add(constructedSamples, 1)
    }
    @Synchronized fun primingFiltered(value: Long) {
        if (active(value)) primingFiltered = add(primingFiltered, 1)
    }
    @Synchronized fun staleFiltered(value: Long) {
        if (active(value)) staleFiltered = add(staleFiltered, 1)
    }
    @Synchronized fun handoffAttempt(value: Long) {
        if (active(value)) handoffAttempts = add(handoffAttempts, 1)
    }
    @Synchronized fun handoffOfferReturned(value: Long) {
        if (active(value)) handoffReturns = add(handoffReturns, 1)
    }
    @Synchronized fun handoffEntered(value: Long) {
        if (active(value)) callbackEntries = add(callbackEntries, 1)
    }
    @Synchronized fun handoffReturned(value: Long) {
        if (active(value)) callbackReturns = add(callbackReturns, 1)
    }
    @Synchronized fun producersJoined(value: Long) {
        if (active(value)) producersJoined = true
    }
    @Synchronized fun callbackBarrier(value: Long, nativeStopReturned: Boolean) {
        if (!active(value)) return
        callbackBarrier = true; stopReturned = nativeStopReturned
    }
    @Synchronized fun handoffIdle(value: Long) {
        if (active(value)) handoffIdle = true
    }

    /** Only the existing RELEASE action calls this after real producer,
     * callback and closed-handoff barriers, before native release. Snapshot
     * contains no release/recording-consumed/complete-AU/EOF certificate. */
    @Synchronized fun freeze(value: Long): Snapshot? {
        if (value != generation) return null
        frozen?.let { return it }
        if (!producersJoined || !callbackBarrier || !handoffIdle) return null
        val result = Snapshot(generation, formatLeading.snapshot(), formatPadding.snapshot(), eosLeading.snapshot(), eosPadding.snapshot(),
            submittedBuffers, submittedFrames, rawFrames, zeroFrames, inputEosQueued,
            outputBuffers, dataBuffers, configBuffers, emptyBuffers, preStartBuffers, dataBytes,
            flagsOr, outputEosBuffers, if (hasPts) firstPts else null, if (hasPts) lastPts else null, ptsWentBackwards,
            constructedSamples, primingFiltered, staleFiltered, handoffAttempts, handoffReturns, callbackEntries, callbackReturns,
            overflow, invalid, trimConflict, producersJoined, callbackBarrier, stopReturned, handoffIdle,
            cardinalityUnknownBuffers, partialOutputBuffers, otherOutputFlagsBuffers, invalidOutputRanges,
            earlyDataBuffers, nonemptyOutputEosBuffers, checkedBorrowedOutputRanges, invalidBorrowedOutputRanges, profile.freeze(this, value))
        frozen = result
        return result
    }
}
