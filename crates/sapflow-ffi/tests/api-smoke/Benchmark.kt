import dev.forgesworn.sapflow.*
import java.nio.ByteBuffer
import kotlin.math.roundToLong

private const val ROUNDS = 7
private var checksum = 0UL

private inline fun measure(iterations: Int, block: (Int) -> Unit): Double {
    val start = System.nanoTime()
    repeat(iterations) { block(it) }
    return (System.nanoTime() - start).toDouble() / iterations
}

fun main() {
    val small = ByteBuffer.allocateDirect(4096)
    repeat(4096) { small.put(it, 0x21) }
    val large = ByteBuffer.allocateDirect(1024 * 1024)
    repeat(large.capacity()) { large.put(it, 0x42) }
    val codec = MediaCodec.aac(ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).flip())
    val first = codec.sample(small, 0, 0, 1024u, 48_000u, false)
    val fanout = NativeSampleFanout(64uL * 1024u * 1024u, 64uL * 1024u * 1024u, 1000u, false)
    val encoded = EncodedBuffer(small)
    val names = arrayOf("sample_4kib", "sample_1mib", "fanout_push_pop", "scalar_len", "encoded_buffer_4kib")
    val iterations = intArrayOf(1000, 50, 2000, 20000, 5000)
    val fanoutSamples = Array(iterations[2]) { codec.sample(small, (it * 1024).toLong(), (it * 1024).toLong(), 1024u, 48_000u, false) }
    for (metric in names.indices) {
        val rounds = DoubleArray(ROUNDS)
        repeat(ROUNDS) { round ->
            val roundFanout = if (metric == 2) NativeSampleFanout(64uL * 1024u * 1024u, 64uL * 1024u * 1024u, 1000u, false) else fanout
            rounds[round] = measure(iterations[metric]) { index ->
                when (metric) {
                    0, 1 -> codec.sample(if (metric == 0) small else large, 0, 0, 1024u, 48_000u, false).use { checksum += if (it.len() != 0uL) 1uL else 0uL }
                    2 -> {
                        val outcome = roundFanout.push(fanoutSamples[index], round.toULong())
                        check(outcome.publishingQueued)
                        roundFanout.popPublishing(round.toULong())!!.use { checksum += it.len() }
                    }
                    3 -> checksum += first.len()
                    else -> EncodedBuffer(small).use { checksum += it.len() }
                }
            }
            if (metric == 2) roundFanout.close()
        }
        rounds.sort()
        print("\"${names[metric]}\":${"%.3f".format(rounds[ROUNDS / 2])}${if (metric == names.lastIndex) "" else ","}")
    }
    encoded.close(); fanout.close(); fanoutSamples.forEach { it.close() }; first.close(); codec.close()
    println(",\"checksum\":$checksum")
}
