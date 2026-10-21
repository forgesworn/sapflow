package dev.forgesworn.sapflow.broadcaster

/** Bounded outstanding platform buffer ownership; closing and admission are
 * atomic so codec release cannot overtake a callback that is about to post. */
internal class CodecOutputAdmission(private val limit: Int = 8) {
    enum class Result { ACCEPTED, CLOSED, SATURATED }
    private var open = true
    private var outstanding = 0
    init { require(limit > 0) }
    @Synchronized fun acquire(): Result = when {
        !open -> Result.CLOSED
        outstanding >= limit -> Result.SATURATED
        else -> { outstanding++; Result.ACCEPTED }
    }
    @Synchronized fun release() { check(outstanding > 0); outstanding-- }
    @Synchronized fun pending(): Int = outstanding
    @Synchronized fun close(): Boolean { open = false; return outstanding == 0 }
}
