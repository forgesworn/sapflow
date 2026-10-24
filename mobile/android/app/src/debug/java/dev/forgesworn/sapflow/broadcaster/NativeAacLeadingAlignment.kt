package dev.forgesworn.sapflow.broadcaster

import kotlin.math.sqrt

/** Preregistered generated-fixture estimator, never a production timing receipt.
 * Lossy-input correlation thresholds below do not replace presentation oracles. */
internal object NativeAacLeadingAlignment {
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
            // PCM16 products fit Int; each 1024-term sum is at most 2^40.
            // Thus Long accumulation and the original Double sums are exact.
            var sourceEnergySum = 0L
            for (i in 0 until ANCHOR_FRAMES) {
                val x = source[start + i].toInt()
                sourceEnergySum += (x * x).toLong()
            }
            val sourceEnergy = sourceEnergySum.toDouble()
            require(sourceEnergy > ANCHOR_FRAMES * 256.0 * 256.0) { "Unusable source anchor" }
            var windowEnergy = 0L
            for (i in 0 until ANCHOR_FRAMES) {
                val y = decoded[start + i].toInt()
                windowEnergy += (y * y).toLong()
            }
            var best = -2.0; var next = -2.0; var bestLag = -1; var bestGain = 0.0
            for (lag in 0..MAX_LAG) {
                if (lag != 0) {
                    val leaving = decoded[start + lag - 1].toInt()
                    val entering = decoded[start + lag + ANCHOR_FRAMES - 1].toInt()
                    windowEnergy -= (leaving * leaving).toLong()
                    windowEnergy += (entering * entering).toLong()
                }
                // Eight scalar Long lanes, with no per-lag scratch allocation.
                var d0 = 0L; var d1 = 0L; var d2 = 0L; var d3 = 0L
                var d4 = 0L; var d5 = 0L; var d6 = 0L; var d7 = 0L
                var i = 0
                while (i < ANCHOR_FRAMES) {
                    val p = start + i
                    val q = p + lag
                    d0 += (source[p].toInt() * decoded[q].toInt()).toLong()
                    d1 += (source[p + 1].toInt() * decoded[q + 1].toInt()).toLong()
                    d2 += (source[p + 2].toInt() * decoded[q + 2].toInt()).toLong()
                    d3 += (source[p + 3].toInt() * decoded[q + 3].toInt()).toLong()
                    d4 += (source[p + 4].toInt() * decoded[q + 4].toInt()).toLong()
                    d5 += (source[p + 5].toInt() * decoded[q + 5].toInt()).toLong()
                    d6 += (source[p + 6].toInt() * decoded[q + 6].toInt()).toLong()
                    d7 += (source[p + 7].toInt() * decoded[q + 7].toInt()).toLong()
                    i += 8
                }
                val dot = (d0 + d1 + d2 + d3 + d4 + d5 + d6 + d7).toDouble()
                val energy = windowEnergy.toDouble()
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
