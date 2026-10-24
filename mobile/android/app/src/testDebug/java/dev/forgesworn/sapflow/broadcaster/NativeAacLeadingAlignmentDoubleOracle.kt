package dev.forgesworn.sapflow.broadcaster

import kotlin.math.sqrt

/** Preregistered generated-fixture estimator, never a production timing receipt.
 * Lossy-input correlation thresholds below do not replace presentation oracles. */
internal object NativeAacLeadingAlignmentDoubleOracle {
    const val PREAMBLE_FRAMES = 8192
    const val MIN_DECODED_FRAMES = 6144
    const val MAX_DECODED_FRAMES = 8192
    const val ANCHOR_FRAMES = 1024
    const val MAX_LAG = 4095
    const val MIN_CORRELATION = 0.92
    const val MIN_RUNNER_UP_MARGIN = 0.02
    const val MIN_GAIN = 0.50
    const val MAX_GAIN = 1.50

    data class Anchor(val sourceStart: Int, val lag: Int, val correlation: Double,
                      val runnerUp: Double, val gain: Double)
    data class Candidate(val lag: Int, val first: Anchor, val second: Anchor)

    /** Independent deterministic nonperiodic anchors; low-pass shaping is part
     * of the exported source fixture, never a transformation of decoded PCM. */
    fun sample(frame: Int, seed: Int): Short {
        var sum = 0
        repeat(4) { tap ->
            var n = (frame - tap) xor seed
            n = (n xor (n ushr 16)) * 0x45d9f3b
            n = (n xor (n ushr 16)) * 0x45d9f3b
            sum += ((n xor (n ushr 16)) ushr 16) - 32768
        }
        return (sum / 8).toShort()
    }

    fun measure(source: ShortArray, decoded: ShortArray, decodedFrames: Int): Candidate {
        require(source.size == PREAMBLE_FRAMES)
        require(decodedFrames in MIN_DECODED_FRAMES..MAX_DECODED_FRAMES && decodedFrames <= decoded.size)
        fun anchor(start: Int): Anchor {
            var sourceEnergy = 0.0
            repeat(ANCHOR_FRAMES) { i -> val x = source[start + i].toDouble(); sourceEnergy += x * x }
            require(sourceEnergy > ANCHOR_FRAMES * 256.0 * 256.0) { "Unusable source anchor" }
            var best = -2.0; var next = -2.0; var bestLag = -1; var bestGain = 0.0
            for (lag in 0..MAX_LAG) {
                var dot = 0.0; var energy = 0.0
                repeat(ANCHOR_FRAMES) { i ->
                    val x = source[start + i].toDouble()
                    val y = decoded[start + lag + i].toDouble()
                    dot += x * y; energy += y * y
                }
                val score = if (energy > 0.0) dot / sqrt(sourceEnergy * energy) else -1.0
                if (score > best) {
                    next = best; best = score; bestLag = lag; bestGain = dot / sourceEnergy
                } else if (score > next) next = score
            }
            require(best.isFinite() && next.isFinite() && bestGain.isFinite())
            require(best >= MIN_CORRELATION && best - next >= MIN_RUNNER_UP_MARGIN && bestGain in MIN_GAIN..MAX_GAIN) {
                "Nonunique/weak/reversed/clipped anchor: start=$start lag=$bestLag score=$best runner=$next gain=$bestGain"
            }
            return Anchor(start, bestLag, best, next, bestGain)
        }
        val first = anchor(0); val second = anchor(ANCHOR_FRAMES)
        require(first.lag == second.lag) { "Disjoint anchor integer lags disagree" }
        return Candidate(first.lag, first, second)
    }
}
