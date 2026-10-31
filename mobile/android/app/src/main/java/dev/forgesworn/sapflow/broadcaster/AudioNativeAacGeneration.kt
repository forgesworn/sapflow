package dev.forgesworn.sapflow.broadcaster

/** Actual event collector for ONE source-preserving codec generation. No media,
 * codec calls or downstream queue live here. The real adapter supplies events
 * only after the named operations returned. Refusal affects qualification only.
 * This is separate from the pre-release diagnostic observation. */
internal class AudioNativeAacGeneration(
    private val owner: Any,
    private val codec: Any,
    private val generation: Long,
    preserveSource: Boolean,
) {
    /** Constructor is capability checked: another caller cannot manufacture a
     * receipt from scalars or from an AudioGenerationObservation.Snapshot. */
    class Receipt internal constructor(issuer: AudioNativeAacGeneration, key: Any,
        val generation: Long, val inputFrames: Long, val rawFrames: Long,
        val zeroFrames: Long, val accessUnits: Long, val encodedFrames: Long,
        val leadingFrames: Int, val trailingFrames: Int,
    ) {
        init { require(issuer.receiptKey === key) }
        val inputRate = 48000
        val recorderConsumed = false
        val applicationAdmission = false
    }
    private val receiptKey = Any()
    private val profile = AudioAacProfile.Observation(this, generation)
    private var eligibility = AudioAacProfile.Eligibility.UNKNOWN
    private var setup: AudioAacProfile.Setup? = null
    private var callbackOwner: Any? = null
    private var formatSeen = false
    private var terminalSeen = false
    private var head: AudioAacProfile.Fact<Int>? = null
    private var formatTail: AudioAacProfile.Fact<Int>? = null
    private var terminalHead: AudioAacProfile.Fact<Int>? = null
    private var tail: AudioAacProfile.Fact<Int>? = null
    private var failed = generation <= 0 || !preserveSource
    private var finished = false
    private var issued: Receipt? = null
    private var raw = 0L
    private var zero = 0L
    private var input = 0L
    private var joined = false
    private var inputEos = false
    private var outputEos = false
    private var stopped = false
    private var nextOutput = 0L
    private var activeOutput = 0L
    private var activeSlot = -1
    private var activeAu = false
    private var activeEligible = false
    private var activeEos = false
    private var units = 0L
    private var headBeforeFirstAu = false
    // Existing transfer-slot index + scalar serial, no token per output. A slot
    // cannot be recycled until actual checked Transfer.close returned.
    private val serials = LongArray(AudioHandoffBounds.SLOTS)
    private val states = IntArray(AudioHandoffBounds.SLOTS)
    private fun owns(who: Any, native: Any) = who === owner && native === codec && !finished
    private fun callback(who: Any, native: Any, actualCallback: Any) = owns(who, native) && actualCallback === callbackOwner
    private fun refuse(): Boolean { failed = true; return false }
    private fun value(fact: AudioAacProfile.Fact<Int>?, maximum: Int): Int? =
        if (fact?.state == AudioAacProfile.State.VALUE && fact.value != null && fact.value in 0..maximum) fact.value else null
    private fun sameValue(a: AudioAacProfile.Fact<Int>?, b: AudioAacProfile.Fact<Int>?): Boolean {
        if (a == null || b == null) return true
        return a.state != AudioAacProfile.State.VALUE || b.state != AudioAacProfile.State.VALUE || a.value == b.value
    }
    private fun malformedTrim(fact: AudioAacProfile.Fact<Int>, maximum: Int): Boolean = when (fact.state) {
        AudioAacProfile.State.VALUE -> fact.value == null || fact.value !in 0..maximum
        AudioAacProfile.State.WRONG_TYPE, AudioAacProfile.State.UNSUPPORTED -> true
        else -> fact.value != null
    }

    /** Bind the exact real EncodedAudioEpoch once, before native start. */
    @Synchronized fun bindCallback(who: Any, native: Any, actualCallback: Any): Boolean {
        if (!owns(who, native)) return false
        if (callbackOwner != null || stopped || nextOutput != 0L || formatSeen || terminalSeen) return refuse()
        callbackOwner = actualCallback; return true
    }

    @Synchronized fun configured(who: Any, native: Any, facts: AudioAacProfile.Setup): Boolean {
        if (!owns(who, native)) return false
        if (stopped || nextOutput != 0L || formatSeen || terminalSeen) return refuse()
        if (setup != null && setup != facts) return refuse()
        setup = facts
        if (!profile.configured(this, generation, facts)) return refuse()
        eligibility = profile.eligibility(this, generation)
        return true
    }

    /** Cold actual format getters only. FORMAT head must exist before the
     * first original AU. EOS head cannot fill in a missing initial head. */
    @Synchronized fun format(who: Any, native: Any, actualCallback: Any, phase: AudioAacProfile.Phase,
        facts: AudioAacProfile.Format, leading: AudioAacProfile.Fact<Int>,
        padding: AudioAacProfile.Fact<Int>,
    ): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (setup == null || stopped || outputEos) return refuse()
        if (malformedTrim(leading, 8191) || malformedTrim(padding, 1023)) return refuse()
        if (phase == AudioAacProfile.Phase.OUTPUT_FORMAT) {
            if (units != 0L || terminalSeen || formatSeen && (head != leading || formatTail != padding)) return refuse()
            formatSeen = true; head = leading; formatTail = padding
        } else {
            if (!activeEos || activeOutput == 0L || terminalSeen && (terminalHead != leading || tail != padding)) return refuse()
            terminalSeen = true; terminalHead = leading; tail = padding
        }
        if (!sameValue(head, terminalHead) || !sameValue(formatTail, tail)) return refuse()
        val accepted = profile.format(this, generation, phase, facts)
        eligibility = profile.eligibility(this, generation)
        if (!accepted) return refuse()
        return true
    }

    /** Actual successful queueInputBuffer; raw/zero are copied PCM components,
     * not AudioRecord read totals. No attempted queue may call this. */
    @Synchronized fun inputQueued(who: Any, native: Any, rawFrames: Long, zeroFrames: Long): Boolean {
        if (!owns(who, native)) return false
        if (joined || stopped || rawFrames < 0 || zeroFrames < 0 || rawFrames == 0L && zeroFrames == 0L) return refuse()
        return try {
            val amount = Math.addExact(rawFrames, zeroFrames)
            val newRaw = Math.addExact(raw, rawFrames); val newZero = Math.addExact(zero, zeroFrames)
            val newInput = Math.addExact(input, amount)
            raw = newRaw; zero = newZero; input = newInput; true
        } catch (_: ArithmeticException) { refuse() }
    }
    @Synchronized fun producersJoined(who: Any, native: Any, actualAcceptedFrames: Long): Boolean {
        if (!owns(who, native)) return false
        if (joined || stopped || actualAcceptedFrames < 0 || actualAcceptedFrames != input) return refuse()
        joined = true; return true
    }
    @Synchronized fun inputEosReturned(who: Any, native: Any): Boolean {
        if (!owns(who, native)) return false
        if (!joined || input == 0L || inputEos || stopped) return refuse()
        inputEos = true; return true
    }

    /** Invoke once per native output callback, with the ALREADY borrowed
     * capacity for data. Config/empty outputs need no extra native lookup.
     * Returns a scalar callback serial; zero is refusal, never an AU token. */
    @Synchronized fun output(who: Any, native: Any, actualCallback: Any, offset: Int, size: Int,
        flags: Int, started: Boolean, capacity: Int,
    ): Long {
        if (!callback(who, native, actualCallback)) return 0
        if (stopped || activeOutput != 0L || outputEos || nextOutput == Long.MAX_VALUE) { refuse(); return 0 }
        nextOutput++; activeOutput = nextOutput; activeSlot = -1
        val d = AudioAacOutputDisposition
        val disposition = d.classify(offset, size, flags, started, capacity)
        activeEos = d.has(disposition, d.EOS)
        activeAu = size > 0 && !d.has(disposition, d.CONFIG_FLAG)
        activeEligible = activeAu && AudioAacProfile.output(eligibility, offset, size, flags, started, capacity) == AudioAacProfile.Output.SINGLE_AU_ELIGIBLE
        if (d.has(disposition, d.RANGE_INVALID) || d.has(disposition, d.PARTIAL_FLAG) ||
            d.has(disposition, d.OTHER_FLAGS) || d.has(disposition, d.EARLY) || activeAu && !activeEligible) refuse()
        return activeOutput
    }

    /** Only AFTER the existing original sample AND timing-views acquisition
     * returned. No sample/metadata is passed or queried here. */
    @Synchronized fun acquired(who: Any, native: Any, actualCallback: Any, serial: Long, slot: Int): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (serial == 0L || serial != activeOutput || !activeEligible || activeSlot != -1 ||
            slot !in states.indices || states[slot] != 0 || eligibility == AudioAacProfile.Eligibility.UNKNOWN) return refuse()
        if (units == 0L) headBeforeFirstAu = value(head, 8191) != null && formatSeen
        if (units == Long.MAX_VALUE) return refuse()
        units++; activeSlot = slot; serials[slot] = serial; states[slot] = ACQUIRED
        return true
    }
    /** Exactly the actual releaseOutputBuffer return; call fault on a throw. */
    @Synchronized fun outputReleased(who: Any, native: Any, actualCallback: Any, serial: Long): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (serial == 0L || serial != activeOutput) return refuse()
        if (activeAu && activeSlot == -1) refuse()
        if (activeSlot >= 0) states[activeSlot] = states[activeSlot] or RELEASED
        if (activeEos) outputEos = true
        activeOutput = 0L; activeSlot = -1; activeAu = false; activeEligible = false; activeEos = false
        return true
    }
    private fun slot(who: Any, native: Any, actualCallback: Any, serial: Long, index: Int, required: Int, absent: Int): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (index !in states.indices || serial == 0L || serials[index] != serial ||
            states[index] and required != required || states[index] and absent != 0) return refuse()
        return true
    }
    @Synchronized fun handoffEntered(who: Any, native: Any, actualCallback: Any, serial: Long, index: Int): Boolean {
        if (!slot(who, native, actualCallback, serial, index, ACQUIRED or RELEASED, ENTERED)) return false
        states[index] = states[index] or ENTERED; return true
    }
    /** Callback return is NOT recordingQueued or recorder-consumed authority. */
    @Synchronized fun handoffReturned(who: Any, native: Any, actualCallback: Any, serial: Long, index: Int): Boolean {
        if (!slot(who, native, actualCallback, serial, index, ENTERED, RETURNED)) return false
        states[index] = states[index] or RETURNED; return true
    }
    /** Actual successful checked Transfer.close, after views/native wrapper
     * close. Unknown close leaves this slot occupied and calls fault. */
    @Synchronized fun transferClosed(who: Any, native: Any, actualCallback: Any, serial: Long, index: Int): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (index !in states.indices || serial == 0L || serials[index] != serial || states[index] == 0) return refuse()
        if (states[index] != (ACQUIRED or RELEASED or ENTERED or RETURNED)) refuse()
        states[index] = 0; serials[index] = 0; return true
    }
    /** Catch paths call this synchronously; no publishing exception is raised. */
    @Synchronized fun fault(who: Any, native: Any): Boolean {
        if (!owns(who, native)) return false
        failed = true; return true
    }
    /** Existing callback barrier AFTER actual stop returned. Does not assert
     * transfer closure: pending checked-close reports may still arrive. */
    @Synchronized fun callbacksStopped(who: Any, native: Any, actualCallback: Any, nativeStopReturned: Boolean): Boolean {
        if (!callback(who, native, actualCallback)) return false
        if (stopped || activeOutput != 0L || !nativeStopReturned) return refuse()
        stopped = true; return true
    }
    /** Only the real AudioCaptureOwner CLOSED path calls this, AFTER actual
     * codec/microphone release. This is native-source scope, never sink proof.
     * One issue attempt seals the collector; no late repair or replay. */
    @Synchronized fun closed(who: Any, native: Any, codecReleaseReturned: Boolean,
        microphoneReleaseReturned: Boolean,
    ): Receipt? {
        if (!owns(who, native)) return null
        finished = true
        if (failed || callbackOwner == null || !joined || !stopped || !inputEos || !outputEos || !terminalSeen ||
            !codecReleaseReturned || !microphoneReleaseReturned || activeOutput != 0L ||
            states.any { it != 0 } || units == 0L || !headBeforeFirstAu ||
            eligibility == AudioAacProfile.Eligibility.UNKNOWN) return null
        val h = value(head, 8191) ?: return null
        val t = value(tail, 1023) ?: return null
        val encoded = try { Math.multiplyExact(units, 1024L) } catch (_: ArithmeticException) { return null }
        val accounted = try { Math.addExact(Math.addExact(input, h.toLong()), t.toLong()) }
            catch (_: ArithmeticException) { return null }
        if (input <= 0L || accounted != encoded) return null
        return Receipt(this, receiptKey, generation, input, raw, zero, units, encoded, h, t).also { issued = it }
    }
    /** Cold bridge check against the actual retained generation collector,
     * rather than accepting equal scalar values from another run/issuer. */
    @Synchronized fun ownsReceipt(who: Any, native: Any, receipt: Receipt): Boolean =
        who === owner && native === codec && receipt === issued
    internal fun ownsIssued(receipt: Receipt): Boolean = ownsReceipt(owner, codec, receipt)
    internal fun belongsTo(who: Any, serial: Long): Boolean = who === owner && generation == serial && generation > 0
    private companion object {
        const val ACQUIRED = 1
        const val RELEASED = 2
        const val ENTERED = 4
        const val RETURNED = 8
    }
}
