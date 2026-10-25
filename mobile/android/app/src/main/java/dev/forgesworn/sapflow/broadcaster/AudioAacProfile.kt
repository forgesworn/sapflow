package dev.forgesworn.sapflow.broadcaster

/** Cold public-API facts only. No MediaCodec, payload or native owner is stored
 * in a snapshot. Eligible packing never grants H/T, source EOF or admission. */
internal object AudioAacProfile {
    enum class State { VALUE, ABSENT, API_UNAVAILABLE, READ_FAILED, WRONG_TYPE, UNSUPPORTED }
    data class Fact<T>(val state: State, val value: T? = null)
    fun <T> known(value: T) = Fact(State.VALUE, value)
    fun <T> missing(state: State) = Fact<T>(state)
    data class Request(val bitrate: Int, val rate: Int = 48000, val channels: Int = 1,
        val aacProfile: Int = 2, val maxInputBytes: Int = 2048,
        val batchMaximum: Fact<Int> = missing(State.ABSENT),
        val batchThreshold: Fact<Int> = missing(State.ABSENT))
    data class Selection(val api: Int, val name: Fact<String>, val canonicalName: Fact<String>,
        val encoder: Fact<Boolean>, val multipleSupported: Fact<Boolean>,
        val multipleRequired: Fact<Boolean>, val request: Request)
    data class Format(val mime: Fact<String>, val rate: Fact<Int>, val channels: Fact<Int>,
        val csd0: Fact<String>, val batchMaximum: Fact<Int>, val batchThreshold: Fact<Int>)
    data class Setup(val selection: Selection, val input: Format, val output: Format)
    enum class Phase { OUTPUT_FORMAT, OUTPUT_EOS }
    enum class Eligibility { UNKNOWN, SINGLE_AU_CAPABILITY, SINGLE_AU_API35_DEFAULT }
    enum class Output { EXCLUDED, UNKNOWN, SINGLE_AU_ELIGIBLE }
    data class Snapshot(val generation: Long, val setup: Setup?, val output: Format?,
        val terminalOutput: Format?, val conflict: Boolean, val eligibility: Eligibility) {
        val nativeSourceQualified get() = false
        val sdkReceiptIssued get() = false
        val appAdmission get() = false
    }
    private fun <T> Fact<T>.isValue(value: T) = state == State.VALUE && this.value == value
    private fun Fact<String>.identity() = state == State.VALUE && value != null && value.length in 1..256
    private fun Fact<Int>.unset() = state == State.ABSENT && value == null
    private fun Format.aac() = mime.isValue("audio/mp4a-latm") && rate.isValue(48000) &&
        channels.isValue(1) && csd0.isValue("1188")
    private fun Format.noBatchContradiction(api: Int) =
        (batchMaximum.unset() || api < 35 && batchMaximum.state == State.API_UNAVAILABLE && batchMaximum.value == null) &&
        (batchThreshold.unset() || api < 35 && batchThreshold.state == State.API_UNAVAILABLE && batchThreshold.value == null)

    private fun <T> contradicts(first: Fact<T>, last: Fact<T>): Boolean = when (last.state) {
        State.VALUE -> first.state != State.VALUE || first.value != last.value
        State.WRONG_TYPE, State.UNSUPPORTED -> true
        else -> false // An absent/failed terminal field is retained as unknown, not a new value.
    }
    private fun changed(first: Format, last: Format) = contradicts(first.mime,last.mime) ||
        contradicts(first.rate,last.rate) || contradicts(first.channels,last.channels) ||
        contradicts(first.csd0,last.csd0) || contradicts(first.batchMaximum,last.batchMaximum) ||
        contradicts(first.batchThreshold,last.batchThreshold)

    /** Both capability fields must have actually returned; read failure is not
     * false. A returned nondefault batch size is not guessed into an AU limit. */
    fun evaluate(selection: Selection?, output: Format?): Eligibility {
        if (selection == null || output == null || selection.api < 29 || !selection.name.identity() ||
            !selection.canonicalName.identity() || !selection.encoder.isValue(true) || !output.aac()) return Eligibility.UNKNOWN
        val request = selection.request
        if (request.bitrate <= 0 || request.rate != 48000 || request.channels != 1 || request.aacProfile != 2 ||
            request.maxInputBytes != 2048 || !request.batchMaximum.unset() || !request.batchThreshold.unset()) return Eligibility.UNKNOWN
        if (!selection.multipleRequired.isValue(false) || selection.multipleSupported.state != State.VALUE ||
            selection.multipleSupported.value == null || !output.noBatchContradiction(selection.api)) return Eligibility.UNKNOWN
        if (selection.multipleSupported.isValue(false)) return Eligibility.SINGLE_AU_CAPABILITY
        if (selection.api >= 35 && output.batchMaximum.unset() && output.batchThreshold.unset())
            return Eligibility.SINGLE_AU_API35_DEFAULT
        return Eligibility.UNKNOWN
    }

    /** Missing initial geometry may be supplied by the later required callback
     * format. A known contrary value or malformed field may not be forgotten. */
    private fun <T> reportedMatches(fact: Fact<T>, expected: T): Boolean = when (fact.state) {
        State.VALUE -> fact.value == expected
        State.WRONG_TYPE, State.UNSUPPORTED -> false
        else -> fact.value == null
    }

    /** Production qualification consumes ALL retained setup facts. getOutputFormat
     * after configure reports effective optional parameters; callback omission
     * is not authority to cancel a previously returned batch/geometry setting.
     * Input mime/CSD describe the raw input side, not the AAC output side. */
    fun evaluateSetup(setup: Setup?, output: Format?): Eligibility {
        if (setup == null) return Eligibility.UNKNOWN
        val api = setup.selection.api
        val input = setup.input; val configured = setup.output
        if (!input.noBatchContradiction(api) || !configured.noBatchContradiction(api) ||
            !reportedMatches(input.rate,48000) || !reportedMatches(input.channels,1) ||
            !reportedMatches(configured.rate,48000) || !reportedMatches(configured.channels,1) ||
            !reportedMatches(configured.mime,"audio/mp4a-latm") || !reportedMatches(configured.csd0,"1188"))
            return Eligibility.UNKNOWN
        return evaluate(setup.selection,output)
    }

    /** Uses already owned range scalars; allocates nothing and reads no media.
     * Nonempty EOS stays an eligible AU. Empty EOS is a terminal event, not AU. */
    fun output(eligibility: Eligibility, offset: Int, size: Int, flags: Int,
        started: Boolean, capacity: Int): Output {
        val d = AudioAacOutputDisposition
        val value = d.classify(offset, size, flags, started, capacity)
        if (d.has(value, d.RANGE_INVALID) || d.has(value, d.PARTIAL_FLAG) ||
            d.has(value, d.CONFIG_FLAG) || d.has(value, d.EMPTY_BUFFER) ||
            d.has(value, d.OTHER_FLAGS) || d.has(value, d.EARLY)) return Output.EXCLUDED
        if (capacity < 0 || eligibility == Eligibility.UNKNOWN) return Output.UNKNOWN
        return Output.SINGLE_AU_ELIGIBLE
    }

    /** Exact owner/generation, one setup and bounded two-phase observations.
     * No callback acquires or closes a native resource through this recorder. */
    class Observation(private val owner: Any, private val generation: Long) {
        private var setup: Setup? = null
        private var output: Format? = null
        private var terminal: Format? = null
        private var conflict = false
        private var frozen: Snapshot? = null
        private fun owns(value: Any, serial: Long) = value === owner && serial == generation && generation > 0
        @Synchronized fun configured(value: Any, serial: Long, facts: Setup): Boolean {
            if (!owns(value, serial) || frozen != null) return false
            if (setup != null && setup != facts) conflict = true else setup = facts
            return !conflict
        }
        @Synchronized fun format(value: Any, serial: Long, phase: Phase, facts: Format): Boolean {
            if (!owns(value, serial) || frozen != null) return false
            val previous = if (phase == Phase.OUTPUT_FORMAT) output else terminal
            if (previous != null && previous != facts) conflict = true
            else if (phase == Phase.OUTPUT_FORMAT) output = facts else terminal = facts
            // A terminal observation never silently replaces a different format.
            val first = output; val last = terminal
            if (first != null && last != null && changed(first,last)) conflict = true
            return !conflict
        }
        @Synchronized fun eligibility(value: Any, serial: Long): Eligibility =
            if (!owns(value, serial) || conflict) Eligibility.UNKNOWN else evaluateSetup(setup, output)
        @Synchronized fun freeze(value: Any, serial: Long): Snapshot? {
            if (!owns(value, serial)) return null
            return frozen ?: Snapshot(generation, setup, output, terminal, conflict,
                eligibility(value, serial)).also { frozen = it }
        }
    }
}
