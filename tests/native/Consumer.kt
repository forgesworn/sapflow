import dev.forgesworn.sapflow.*
import java.nio.ByteBuffer
import java.util.Base64

fun main() {
    check(sdkAbiVersion() == 1u)
    checkPackagedStatusLeaseCache()
    val input = ByteBuffer.allocateDirect(4)
    input.put(byteArrayOf(1, 2, 3, 4)).flip()
    EncodedBuffer(input).use { sample ->
        input.put(0, 0)
        check(sample.len() == 4uL)
        check(sample.byteAt(0uL) == 1.toUByte())
        try {
            sample.byteAt(ULong.MAX_VALUE)
            error("invalid index accepted")
        } catch (_: BindingException.IndexOutOfBounds) {}
    }
    val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x90.toByte())).flip()
    val payload = ByteBuffer.allocateDirect(3).put(byteArrayOf(1, 2, 3)).flip()
    MediaCodec.aac(asc).use { codec ->
        codec.sample(payload, 0, 0, 1024u, 48_000u, false).use { sample ->
            // The mock peer is entirely in memory: no socket or TLS connection.
            NativeRtmpPublisher("live", "synthetic", "rtmp://origin.invalid/live", 0uL, 0uL).use { rtmp ->
                val pendingHandshake = rtmp.queuedOutputBytes()
                check(pendingHandshake == 1537uL)
                try { rtmp.sendAndTakeOutput(sample, 1uL); error("prepublication fused send accepted media") }
                catch (_: NativeRtmpException.NotPublishing) {}
                check(rtmp.queuedOutputBytes() == pendingHandshake && rtmp.nextDeadlineMs() == 15_000uL)
                val c0c1 = rtmp.takeOutput()
                check(c0c1.size == 1537 && c0c1[0] == 3.toByte())
                val response = ByteBuffer.allocateDirect(3073)
                    .put(3.toByte()).put(ByteArray(1536)).put(c0c1, 1, 1536).flip()
                rtmp.feed(response, 2uL)
                rtmp.takeOutput() // C2/connect, never written to a socket
                // Independent fmt-0 AMF0 connect/createStream/Publish.Start replies.
                val replies = listOf(
                    "AwAAAAAAPhQAAAAAAgAHX3Jlc3VsdAA/8AAAAAAAAAUDAARjb2RlAgAdTmV0Q29ubmVjdGlvbi5Db25uZWN0LlN1Y2Nlc3MAAAk=",
                    "AwAAAAAAHRQAAAAAAgAHX3Jlc3VsdABAAAAAAAAAAAUAP/AAAAAAAAA=",
                    "AwAAAAAAORQBAAAAAgAIb25TdGF0dXMAAAAAAAAAAAAFAwAEY29kZQIAF05ldFN0cmVhbS5QdWJsaXNoLlN0YXJ0AAAJ",
                )
                replies.forEachIndexed { index, reply ->
                    val bytes = Base64.getDecoder().decode(reply)
                    rtmp.feed(ByteBuffer.allocateDirect(bytes.size).put(bytes).flip(), (index + 3).toULong())
                    rtmp.takeOutput()
                }
                val sent: NativeRtmpSendOutput = rtmp.sendAndTakeOutput(sample, 6uL)
                check(sent.output.isNotEmpty() && sent.output.size <= 8 * 1024 * 1024)
                check(sent.queuedOutputBytes == 0uL && sent.nextDeadlineMs == null && sent.state == NativeRtmpState.PUBLISHING)
                codec.sample(payload, 1024, 1024, 1024u, 48_000u, false).use { second ->
                    rtmp.send(second)
                    val queued = rtmp.queuedOutputBytes()
                    try { rtmp.sendAndTakeOutput(second, 7uL); error("duplicate fused sample was accepted") }
                    catch (_: NativeRtmpException.InvalidMedia) {}
                    check(queued > 0uL && rtmp.queuedOutputBytes() == queued && rtmp.takeOutput().isNotEmpty())
                }
                val window = byteArrayOf(2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0)
                rtmp.feed(ByteBuffer.allocateDirect(window.size).put(window).flip(), 8uL)
                codec.sample(payload, 2048, 2048, 1024u, 48_000u, false).use { third ->
                    val blocked: NativeRtmpSendOutput = rtmp.sendAndTakeOutput(third, 9uL)
                    check(blocked.output.isEmpty() && blocked.queuedOutputBytes > 0uL)
                    check(blocked.nextDeadlineMs == 1009uL && blocked.state == NativeRtmpState.PUBLISHING)
                }
                codec.sample(payload, 3072, 3072, 1024u, 48_000u, false).use { fourth ->
                    try { rtmp.sendAndTakeOutput(fourth, 1009uL); error("stale wire media was accepted") }
                    catch (_: NativeRtmpException.Timeout) {}
                    rtmp.cancel()
                    try { rtmp.sendAndTakeOutput(fourth, 1010uL); error("cancelled fused publisher accepted media") }
                    catch (_: NativeRtmpException.Closed) {}
                }
            }
            NativeSampleFanout(1024uL, 1024uL, 1000u, false).use { queues ->
                val result = queues.push(sample, 0uL)
                check(result.publishingQueued && result.recordingQueued)
                queues.popPublishing(1uL)!!.use { check(it.len() == 3uL) }
                queues.popRecording(1uL)!!.use { check(it.timescale() == 48_000u) }
                queues.cancel()
                try { queues.push(sample, 2uL); error("cancelled fanout accepted media") }
                catch (_: BindingException.Closed) {}
            }
            NativeRecordingMuxer(false, true, 0uL, 4096uL, 1000u).use { recorder ->
                recorder.push(sample)
                val header = recorder.takeOutput()!!
                check(header.bytes.isNotEmpty() && !header.rewriteHeader)
                recorder.finish()
                var finalHeader = false
                while (true) {
                    val write = recorder.takeOutput() ?: break
                    check(write.bytes.isNotEmpty())
                    finalHeader = finalHeader || write.rewriteHeader
                }
                check(finalHeader)
            }
            val config = NativeSrtConfig("127.0.0.1:9000", "synthetic", null,
                120u, 64u, 84_224u, 3000u, 1000u, 1_250_000uL)
            NativeSrtPublisher(config, null, codec, null, 0uL).use { srt ->
                val handshake = srt.pollDatagram(0uL)!!
                check(handshake.bytes.isNotEmpty() && handshake.bytes.size <= 1500)
                check(srt.nextDeadlineUs() != null)
                srt.stop(0uL)
                check(srt.state() == NativeSrtState.CLOSED)
                check(srt.stats().bufferedBytes == 0uL)
            }
        }
    }
    println("Sapflow Kotlin direct-buffer ownership consumer passed")
}

/** Reflection checks the actual compiled binding classes, including when this
 * consumer is compiled separately against the extracted AAR classes.jar. */
private fun checkPackagedStatusLeaseCache() {
    val pool = Class.forName("dev.forgesworn.sapflow.SapflowRustCallStatusPool")
    val instance = pool.getField("INSTANCE").get(null)
    val current = pool.declaredMethods.single { it.name.startsWith("current") && it.parameterCount == 0 }
    current.isAccessible = true
    val stack = current.invoke(instance)
    val cached = stack.javaClass.getDeclaredField("cached").apply { isAccessible = true }
    val depth = stack.javaClass.getDeclaredField("depth").apply { isAccessible = true }
    val slots = cached.get(stack) as Array<*>
    check(slots.size == 4 && depth.getInt(stack) == 0)
    val first = slots[0] ?: error("SDK call did not populate the generated status cache")
    repeat(100) { check(sdkAbiVersion() == 1u) }
    check((cached.get(stack) as Array<*>)[0] === first && depth.getInt(stack) == 0)
}
