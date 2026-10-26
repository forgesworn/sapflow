import dev.forgesworn.sapflow.*
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.lang.management.ManagementFactory

private val process = ManagementFactory.getOperatingSystemMXBean() as com.sun.management.OperatingSystemMXBean
private const val ROUNDS = 7
private var checksum = 0UL
private var rtmpOutputEvidence = 0UL
private var fmp4OutputEvidence = 0UL

private inline fun measure(iterations: Int, block: (Int) -> Unit): Double {
    val start = System.nanoTime()
    repeat(iterations) { block(it) }
    return (System.nanoTime() - start).toDouble() / iterations
}

private fun direct(bytes: ByteArray): ByteBuffer = ByteBuffer.allocateDirect(bytes.size).put(bytes).flip()
private fun direct(size: Int, value: Byte): ByteBuffer = ByteBuffer.allocateDirect(size).also { repeat(size) { index -> it.put(index, value) } }

private fun amfString(out: ByteArrayOutputStream, value: String) {
    val bytes = value.toByteArray(Charsets.UTF_8)
    out.write(2)
    out.write(bytes.size ushr 8)
    out.write(bytes.size)
    out.writeBytes(bytes)
}

private fun amfNumber(out: ByteArrayOutputStream, value: Double) {
    out.write(0)
    val bits = value.toRawBits()
    for (index in 7 downTo 0) out.write((bits ushr (index * 8)).toInt())
}

private fun rtmpResponse(name: String, transaction: Double, code: String? = null, streamId: Double? = null): ByteArray {
    val out = ByteArrayOutputStream()
    amfString(out, name)
    amfNumber(out, transaction)
    out.write(5)
    if (streamId != null) {
        amfNumber(out, streamId)
    } else if (code != null) {
        out.write(3)
        for ((key, value) in arrayOf("level" to "status", "code" to code)) {
            val keyBytes = key.toByteArray(Charsets.UTF_8)
            out.write(keyBytes.size ushr 8)
            out.write(keyBytes.size)
            out.writeBytes(keyBytes)
            amfString(out, value)
        }
        out.write(byteArrayOf(0, 0, 9))
    }
    return out.toByteArray()
}

private fun rtmpWire(body: ByteArray, streamId: Int): ByteArray {
    val out = ByteArrayOutputStream()
    out.write(byteArrayOf(3, 0, 0, 0))
    out.write(body.size ushr 16)
    out.write(body.size ushr 8)
    out.write(body.size)
    out.write(20)
    out.write(streamId and 255)
    out.write(streamId ushr 8)
    out.write(streamId ushr 16)
    out.write(streamId ushr 24)
    out.writeBytes(body)
    return out.toByteArray()
}

private fun readyRtmp(): NativeRtmpPublisher {
    val publisher = NativeRtmpPublisher("live", "fixture", "rtmp://origin.invalid/live", 0u, 0u)
    val client = publisher.takeOutput()
    check(client.size == 1537)
    val handshake = ByteArray(3073) { 0x53 }
    handshake[0] = 3
    handshake[1] = 0; handshake[2] = 0; handshake[3] = 0; handshake[4] = 123
    for (index in 5..8) handshake[index] = 0
    client.copyInto(handshake, 1537, 1)
    val receipt = 456
    for (index in 0..3) handshake[1537 + 4 + index] = (receipt ushr ((3 - index) * 8)).toByte()
    publisher.feed(direct(handshake), 1u)
    publisher.takeOutput()
    val responses = arrayOf(
        Triple(rtmpResponse("_result", 1.0, "NetConnection.Connect.Success"), 0, NativeRtmpState.CREATING_STREAM),
        Triple(rtmpResponse("_result", 2.0, streamId = 7.0), 0, NativeRtmpState.AWAITING_PUBLISH),
        Triple(rtmpResponse("onStatus", 0.0, "NetStream.Publish.Start"), 7, NativeRtmpState.PUBLISHING),
    )
    for ((body, stream, expected) in responses) {
        publisher.feed(direct(rtmpWire(body, stream)), 2u)
        check(publisher.state() == expected)
        publisher.takeOutput()
    }
    return publisher
}

private fun srtPublisher(codec: MediaCodec) = NativeSrtPublisher(
    NativeSrtConfig("127.0.0.1:9000", "publish:test", null, 120u, 4096u, 4u * 1024u * 1024u, 3000u, 1000u, 1_250_000u),
    null, codec, null, 1_000_000u,
)

private fun correctnessCheck() {
    val large = direct(1024 * 1024, 0x42)
    val codec = MediaCodec.aac(direct(byteArrayOf(0x11, 0x88.toByte())))
    val rtmp = readyRtmp()
    val sample = codec.sample(large, 0, 0, 1024u, 48_000u, false)
    val rtmpOutput = rtmp.sendAndTakeOutput(sample, 10u)
    check(rtmpOutput.output.size > 1024 * 1024 && rtmpOutput.state == NativeRtmpState.PUBLISHING)
    val next = codec.sample(large, 1024, 1024, 1024u, 48_000u, false)
    val muxer = NativeRecordingMuxer(false, true, 0u, 1024uL * 1024uL, 100u)
    var outputBytes = 0UL
    var largest = 0
    for (media in listOf(sample, next)) {
        muxer.push(media)
        while (true) {
            val output = muxer.takeOutput() ?: break
            outputBytes += output.bytes.size.toULong()
            largest = maxOf(largest, output.bytes.size)
        }
    }
    check(largest > 1024 * 1024)
    val srt = srtPublisher(codec)
    srt.feed("127.0.0.1:9001", direct(1316, 0), 1_000_000u)
    srt.feed("127.0.0.1:9001", direct(1500, 0), 1_000_000u)
    check(srt.stats().strayDatagrams == 2uL)
    srt.close(); muxer.close(); next.close(); sample.close(); rtmp.close(); codec.close()
    println("correctness_checksum=${rtmpOutput.output.size.toULong() + outputBytes + 2816uL}")
}

fun main() {
    if (System.getenv("SAPFLOW_BENCH_CHECK") == "1") {
        correctnessCheck()
        return
    }
    val small = direct(4096, 0x21)
    val large = direct(1024 * 1024, 0x42)
    val codec = MediaCodec.aac(direct(byteArrayOf(0x11, 0x88.toByte())))
    val first = codec.sample(small, 0, 0, 1024u, 48_000u, false)
    val fanout = NativeSampleFanout(64uL * 1024uL * 1024uL, 64uL * 1024uL * 1024uL, 1000u, false)
    val encoded = EncodedBuffer(small)
    val names = arrayOf(
        "sample_4kib", "sample_1mib", "fanout_push_pop", "scalar_len", "encoded_buffer_4kib",
        "rtmp_feed", "rtmp_send_1mib", "fmp4_output_1mib", "srt_feed_1316", "srt_feed_1500",
    )
    val iterations = intArrayOf(1000, 50, 2000, 20000, 5000, 10000, 16, 6, 10000, 10000)
    val fanoutSamples = Array(iterations[2]) { codec.sample(small, (it * 1024).toLong(), (it * 1024).toLong(), 1024u, 48_000u, false) }
    val rtmpAck = direct(byteArrayOf(2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0, 0, 0, 0, 0))
    val datagram1316 = direct(1316, 0)
    val datagram1500 = direct(1500, 0)
    var evidenceStats: NativeSrtStats? = null
    for (metric in names.indices) {
        val rounds = DoubleArray(ROUNDS)
        val cpuRounds = DoubleArray(ROUNDS)
        repeat(ROUNDS) { round ->
            val roundFanout = if (metric == 2) NativeSampleFanout(64uL * 1024uL * 1024uL, 64uL * 1024uL * 1024uL, 1000u, false) else fanout
            val rtmp = if (metric == 5 || metric == 6) readyRtmp() else null
            val srt = if (metric >= 8) srtPublisher(codec) else null
            val samples = when (metric) {
                6 -> Array(iterations[metric]) { codec.sample(large, (it * 1024).toLong(), (it * 1024).toLong(), 1024u, 48_000u, false) }
                7 -> Array(iterations[metric] * 2) { codec.sample(large, (it * 1024).toLong(), (it * 1024).toLong(), 1024u, 48_000u, false) }
                else -> null
            }
            val cpuStart = process.processCpuTime
            rounds[round] = measure(iterations[metric]) { index ->
                when (metric) {
                    0, 1 -> codec.sample(if (metric == 0) small else large, 0, 0, 1024u, 48_000u, false).use { checksum += if (it.len() != 0uL) 1uL else 0uL }
                    2 -> {
                        val outcome = roundFanout.push(fanoutSamples[index], round.toULong())
                        check(outcome.publishingQueued)
                        roundFanout.popPublishing(round.toULong())!!.use { checksum += it.len() }
                    }
                    3 -> checksum += first.len()
                    4 -> EncodedBuffer(small).use { checksum += it.len() }
                    5 -> {
                        rtmp!!.feed(rtmpAck, (index + 10).toULong())
                        checksum += rtmp.state().ordinal.toULong()
                    }
                    6 -> {
                        val output = rtmp!!.sendAndTakeOutput(samples!![index], (index + 10).toULong())
                        check(output.output.isNotEmpty() && output.state == NativeRtmpState.PUBLISHING)
                        checksum += output.output.size.toULong() + output.queuedOutputBytes
                        rtmpOutputEvidence += output.output.size.toULong()
                    }
                    7 -> {
                        val muxer = NativeRecordingMuxer(false, true, 0u, 1024uL * 1024uL, 100u)
                        repeat(2) { part ->
                            muxer.push(samples!![index * 2 + part])
                            while (true) {
                                val output = muxer.takeOutput() ?: break
                                checksum += output.bytes.size.toULong()
                                fmp4OutputEvidence += output.bytes.size.toULong()
                            }
                        }
                        muxer.close()
                    }
                    8, 9 -> {
                        val size = if (metric == 8) 1316 else 1500
                        srt!!.feed("127.0.0.1:9001", if (size == 1316) datagram1316 else datagram1500, 1_000_000u)
                        checksum += size.toULong()
                    }
                }
            }
            cpuRounds[round] = (process.processCpuTime - cpuStart).toDouble() / iterations[metric]
            samples?.forEach { it.close() }
            rtmp?.close()
            if (srt != null) evidenceStats = srt.stats()
            srt?.close()
            if (metric == 2) roundFanout.close()
        }
        rounds.sort()
        cpuRounds.sort()
        print("\"${names[metric]}\":${"%.3f".format(rounds[ROUNDS / 2])},\"${names[metric]}_cpu\":${"%.3f".format(cpuRounds[ROUNDS / 2])},")
    }
    check(rtmpOutputEvidence > 0uL && fmp4OutputEvidence > 0uL)
    val stats = evidenceStats!!
    check(stats.strayDatagrams == iterations[9].toULong())
    encoded.close(); fanout.close(); fanoutSamples.forEach { it.close() }; first.close(); codec.close()
    print("\"rtmp_output_bytes\":$rtmpOutputEvidence,\"fmp4_output_bytes\":$fmp4OutputEvidence")
    print(",\"srt_stray_datagrams\":${stats.strayDatagrams},\"srt_returned_datagram_bytes\":${stats.returnedDatagramBytes}")
    print(",\"srt_ts_output_allocations\":${stats.tsOutputAllocations},\"srt_writer_payload_copy_bytes\":${stats.writerPayloadCopyBytes}")
    println(",\"srt_ts_serialised_bytes\":${stats.tsSerialisedBytes},\"checksum\":$checksum")
}
