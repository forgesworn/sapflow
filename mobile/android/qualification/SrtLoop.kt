package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaCodec
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path

/** Host qualification of the identical production JVM UDP loop, using actual
 * generated bindings/native SDK and independent libsrt receiver. Not Android
 * hardware proof. Fixture allocations below are not the capture pipeline. */
private data class Tag(val video: Boolean, val timestampMs: Long, val bytes: ByteArray)

private fun direct(bytes: ByteArray): ByteBuffer = ByteBuffer.allocateDirect(bytes.size).apply { put(bytes); flip() }

fun main(arguments: Array<String>) {
    require(arguments.size == 3) { "SrtLoop <numeric SRT URL> <fixture.flv> <public lab passphrase>" }
    val path = Path.of(arguments[1])
    require(Files.size(path) <= 8 * 1024 * 1024)
    val file = Files.readAllBytes(path)
    require(file.size >= 13 && file.copyOfRange(0, 3).contentEquals("FLV".toByteArray()))
    val tags = mutableListOf<Tag>()
    var position = 13
    var videoCodec: MediaCodec? = null
    var audioCodec: MediaCodec? = null
    while (position < file.size) {
        require(position + 15 <= file.size && tags.size < 4096)
        fun byte(offset: Int) = file[position + offset].toInt() and 255
        val type = byte(0)
        val count = (byte(1) shl 16) or (byte(2) shl 8) or byte(3)
        val timestamp = (byte(7).toLong() shl 24) or (byte(4).toLong() shl 16) or (byte(5).toLong() shl 8) or byte(6).toLong()
        require(count in 1..4 * 1024 * 1024 && position + 15L + count <= file.size)
        val payload = file.copyOfRange(position + 11, position + 11 + count)
        if (type == 9 && payload.size >= 5 && payload[1].toInt() == 0) {
            val config = payload.copyOfRange(5, payload.size)
            require(config.size >= 11 && config[0].toInt() == 1 && (config[4].toInt() and 3) == 3)
            val spsLength = ((config[6].toInt() and 255) shl 8) or (config[7].toInt() and 255)
            require(8 + spsLength + 3 <= config.size)
            val ppsOffset = 8 + spsLength + 1
            val ppsLength = ((config[ppsOffset].toInt() and 255) shl 8) or (config[ppsOffset + 1].toInt() and 255)
            require(ppsOffset + 2 + ppsLength <= config.size)
            videoCodec = MediaCodec.h264(direct(config.copyOfRange(8, 8 + spsLength)), direct(config.copyOfRange(ppsOffset + 2, ppsOffset + 2 + ppsLength)))
        } else if (type == 8 && payload.size >= 2 && payload[1].toInt() == 0) {
            // ffmpeg appends an optional sync extension. This fixture is
            // explicitly AAC-LC/48k/mono, matching the hardware adapter's
            // canonical two-byte indexed ASC descriptor.
            require(payload.size >= 4 && (payload[2].toInt() ushr 3) == 2)
            audioCodec = MediaCodec.aac(direct(payload.copyOfRange(2, 4)))
        } else if ((type == 8 || type == 9) && payload.size >= 2 && payload[1].toInt() == 1) {
            tags.add(Tag(type == 9, timestamp, payload))
        }
        position += 15 + count
    }
    val video = checkNotNull(videoCodec)
    val audio = checkNotNull(audioCodec)
    val sink = SrtPublishingSink(SrtDestination.parse(arguments[0]), "publish:lab", arguments[2], false)
    var videoUnits = 0L
    var audioUnits = 0L
    val firstVideoMs = tags.first { it.video }.timestampMs
    val firstAudioMs = tags.first { !it.video }.timestampMs
    try {
        sink.configure(video, audio)
        sink.start()
        val connectDeadline = System.nanoTime() + 12_000_000_000L
        while (!sink.status.contains("handshake complete")) {
            check(System.nanoTime() < connectDeadline && !sink.status.contains("stopped", true)) { sink.status }
            Thread.sleep(5)
        }
        val started = System.nanoTime()
        for (tag in tags) {
            val ticks = if (tag.video) firstVideoMs * 90 + videoUnits * 3000 else firstAudioMs * 48 + audioUnits * 1024
            val scale = if (tag.video) 90_000L else 48_000L
            val dueNs = ticks * 1_000_000_000 / scale
            while (System.nanoTime() - started < dueNs) Thread.sleep(1)
            val payload = tag.bytes.copyOfRange(if (tag.video) 5 else 2, tag.bytes.size)
            val codec = if (tag.video) video else audio
            val keyframe = tag.video && (tag.bytes[0].toInt() ushr 4) == 1
            val offset = if (tag.video) {
                val raw = ((tag.bytes[2].toInt() and 255) shl 16) or ((tag.bytes[3].toInt() and 255) shl 8) or (tag.bytes[4].toInt() and 255)
                ((raw shl 8) shr 8).toLong() * 90
            } else 0
            codec.sample(direct(payload), ticks + offset, ticks, if (tag.video) 3000u else 1024u, scale.toUInt(), keyframe).use { sink.consume(it) }
            check(!sink.status.contains("stopped", true)) { sink.status }
            if (tag.video) videoUnits++ else audioUnits++
        }
        Thread.sleep(500)
        check(sink.status.contains("handshake complete")) { sink.status }
        println("UDP_LOOP_HANDSHAKE_ENCRYPTED=true offered_video=$videoUnits offered_audio=$audioUnits observed_media=false")
    } finally {
        sink.close(); video.close(); audio.close()
    }
}
