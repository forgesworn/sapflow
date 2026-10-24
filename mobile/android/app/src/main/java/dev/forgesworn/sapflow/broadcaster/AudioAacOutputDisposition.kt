package dev.forgesworn.sapflow.broadcaster

/** Fixed scalar observations of a codec output buffer, never an AU count,
 * native timing/EOF receipt or admission decision. Non-partial audio buffers
 * can still contain multiple AUs. No payload is inspected or retained. */
internal object AudioAacOutputDisposition {
    const val KIND_MASK = 7
    const val CONFIG = 1
    const val EMPTY = 2
    const val DATA_CARDINALITY_UNKNOWN = 3
    const val PARTIAL = 4
    const val INVALID_RANGE = 5
    const val EOS = 1 shl 3
    const val EARLY = 1 shl 4
    const val OTHER_FLAGS = 1 shl 5
    const val CARDINALITY_UNKNOWN = 1 shl 6
    const val CONFIG_FLAG = 1 shl 7
    const val PARTIAL_FLAG = 1 shl 8
    const val EMPTY_BUFFER = 1 shl 9
    const val RANGE_INVALID = 1 shl 10
    const val CAPACITY_UNKNOWN = 1 shl 11
    // Public MediaCodec buffer flag values, available below minSdk29. Other
    // bits remain observations without assigning them AAC semantics here.
    private const val CODEC_CONFIG = 2
    private const val END_OF_STREAM = 4
    private const val PARTIAL_FRAME = 8
    private const val DESCRIBED_FLAGS = CODEC_CONFIG or END_OF_STREAM or PARTIAL_FRAME

    fun kind(value: Int) = value and KIND_MASK
    fun has(value: Int, flag: Int) = value and flag != 0

    /** capacity=-1 means unavailable. Even a valid range and no partial flag
     * leave AU cardinality unknown. All arithmetic stays in fixed scalars. */
    fun classify(offset: Int, size: Int, flags: Int, started: Boolean, capacity: Int = -1): Int {
        val config = flags and CODEC_CONFIG != 0
        val partial = flags and PARTIAL_FRAME != 0
        val end = offset.toLong() + size.toLong()
        val invalid = offset < 0 || size < 0 || end > Int.MAX_VALUE || capacity < -1 || capacity >= 0 && end > capacity
        var result = when {
            invalid -> INVALID_RANGE
            partial -> PARTIAL
            config -> CONFIG
            size == 0 -> EMPTY
            else -> DATA_CARDINALITY_UNKNOWN
        }
        if (flags and END_OF_STREAM != 0) result = result or EOS
        if (!started) result = result or EARLY
        if (flags and DESCRIBED_FLAGS.inv() != 0) result = result or OTHER_FLAGS
        if (size > 0 && !config) result = result or CARDINALITY_UNKNOWN
        if (config) result = result or CONFIG_FLAG
        if (partial) result = result or PARTIAL_FLAG
        if (size == 0) result = result or EMPTY_BUFFER
        if (invalid) result = result or RANGE_INVALID
        if (capacity == -1) result = result or CAPACITY_UNKNOWN
        return result
    }
}
