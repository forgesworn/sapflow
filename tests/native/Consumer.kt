import dev.forgesworn.sapflow.*
import java.nio.ByteBuffer
import java.util.Base64

fun main() {
    check(sdkAbiVersion() == 1u)
    checkPackagedStatusLeaseCache()
    checkIntentionalTransportReplacement()
    checkNetworkRecoveryAxes()
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
                rtmp.declareTracks(false, true)
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
                var transferredWire = rtmp.takeOutput().size - 1536 // C2 excluded from ACK accounting
                // Independent fmt-0 AMF0 connect/createStream/Publish.Start replies.
                val replies = listOf(
                    "AwAAAAAAPhQAAAAAAgAHX3Jlc3VsdAA/8AAAAAAAAAUDAARjb2RlAgAdTmV0Q29ubmVjdGlvbi5Db25uZWN0LlN1Y2Nlc3MAAAk=",
                    "AwAAAAAAHRQAAAAAAgAHX3Jlc3VsdABAAAAAAAAAAAUAP/AAAAAAAAA=",
                    "AwAAAAAAORQBAAAAAgAIb25TdGF0dXMAAAAAAAAAAAAFAwAEY29kZQIAF05ldFN0cmVhbS5QdWJsaXNoLlN0YXJ0AAAJ",
                )
                replies.forEachIndexed { index, reply ->
                    val bytes = Base64.getDecoder().decode(reply)
                    rtmp.feed(ByteBuffer.allocateDirect(bytes.size).put(bytes).flip(), (index + 3).toULong())
                    transferredWire += rtmp.takeOutput().size
                }
                val sent: NativeRtmpSendOutput = rtmp.sendAndTakeOutput(sample, 6uL)
                check(sent.output.isNotEmpty() && sent.output.size <= 8 * 1024 * 1024)
                check(sent.queuedOutputBytes == 0uL && sent.nextDeadlineMs == null && sent.state == NativeRtmpState.PUBLISHING)
                check(sent.startupSubmitted && !sent.peerMediaProgress)
                check(rtmp.pollEvent() == NativeRtmpEvent.SENDING)
                transferredWire += sent.output.size
                val acknowledgement = ByteBuffer.allocateDirect(16)
                    .put(byteArrayOf(2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0)).putInt(transferredWire).flip()
                rtmp.feed(acknowledgement, 6uL)
                check(rtmp.pollEvent() == null)
                codec.sample(ByteBuffer.allocateDirect(4096).put(ByteArray(4096) { 0x21 }).flip(), 1024, 1024, 1024u, 48_000u, false).use { second ->
                    rtmp.send(second)
                    val queued = rtmp.queuedOutputBytes()
                    try { rtmp.sendAndTakeOutput(second, 7uL); error("duplicate fused sample was accepted") }
                    catch (_: NativeRtmpException.InvalidMedia) {}
                    check(queued > 0uL && rtmp.queuedOutputBytes() == queued)
                    val retainedOutput = rtmp.takeOutput()
                    check(retainedOutput.isNotEmpty())
                    transferredWire += retainedOutput.size
                    val guardedAck = ByteBuffer.allocateDirect(16)
                        .put(byteArrayOf(2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0)).putInt(transferredWire).flip()
                    rtmp.feed(guardedAck, 7uL)
                    check(rtmp.pollEvent() == NativeRtmpEvent.PEER_MEDIA_PROGRESS)
                }
                val window = byteArrayOf(2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0)
                rtmp.feed(ByteBuffer.allocateDirect(window.size).put(window).flip(), 8uL)
                codec.sample(payload, 2048, 2048, 1024u, 48_000u, false).use { third ->
                    val blocked: NativeRtmpSendOutput = rtmp.sendAndTakeOutput(third, 9uL)
                    check(blocked.output.size == 1 && blocked.queuedOutputBytes > 0uL)
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
                queues.closePublishingAdmission()
                queues.closePublishingAdmission()
                codec.sample(payload, 1024, 1024, 1024u, 48_000u, false).use { next ->
                    val recordingOnly = queues.push(next, 1uL)
                    check(!recordingOnly.publishingQueued && recordingOnly.recordingQueued && !recordingOnly.recordingFailed)
                }
                queues.popPublishing(1uL)!!.use { check(it.len() == 3uL) }
                check(queues.popPublishing(1uL) == null)
                queues.popRecording(1uL)!!.use { check(it.timescale() == 48_000u) }
                queues.popRecording(1uL)!!.use { check(it.len() == 3uL) }
                queues.closeRecordingAdmission()
                codec.sample(payload, 2048, 2048, 1024u, 48_000u, false).use { next ->
                    val neither = queues.push(next, 2uL)
                    check(!neither.publishingQueued && !neither.recordingQueued && !neither.recordingFailed)
                }
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

/** Real generated control bindings; no sockets, capture or timers are started. */
private fun checkIntentionalTransportReplacement() {
    NativeSession(90_000uL).use { session ->
        val generation = session.start(0uL)
        check(session.prepared(generation) == NativeSessionAction.Connect)
        session.connected(generation, 1uL)
        session.mediaConfirmed(generation, 2uL)
        check(session.replaceTransport(generation, 200_000_000_000uL) == NativeSessionAction.Connect)
        val healthy = session.snapshot()
        check(healthy.generation == generation && healthy.state == NativeSessionState.CONNECTING)
        check(!healthy.transportConnected && healthy.reconnectDeadlineNs == null && healthy.transportFailures == 0uL)
        session.transportFailed(generation, 200_000_000_010uL)
        val deadline = session.snapshot().reconnectDeadlineNs!!
        check(deadline == 290_000_000_010uL)
        check(session.replaceTransport(generation, 200_000_000_020uL) == NativeSessionAction.Connect)
        val outage = session.snapshot()
        check(outage.state == NativeSessionState.RECONNECTING && outage.reconnectDeadlineNs == deadline)
        check(!outage.transportConnected && outage.transportFailures == 1uL)
        check(session.replaceTransport(generation, deadline) == NativeSessionAction.GraceExpired)
        val cancelled = session.cancel(generation)
        try { session.replaceTransport(generation, deadline); error("stale replacement accepted") }
        catch (_: BindingException.StaleSession) {}
        check(session.snapshot().generation == cancelled && session.snapshot().state == NativeSessionState.STOPPED)
    }
}

private fun checkNetworkRecoveryAxes() {
    NativeSession(1uL).use { session ->
        val generation = session.start(0uL)
        session.prepared(generation)
        session.transportFailed(generation, 1uL)
        session.replaceTransport(generation, 2uL)
        val attempt = session.snapshot().attemptEpoch
        session.connected(generation, 3uL)
        session.sending(generation, attempt, 4uL)
        session.transportRecovered(generation, attempt, 5uL, NativeTransportRecoverySource.SRT_PEER_ACKNOWLEDGED_WATERMARK)
        val networkOnly = session.snapshot()
        check(networkOnly.state == NativeSessionState.SENDING && networkOnly.mediaReadiness == NativeMediaReadiness.UNKNOWN)
        check(networkOnly.retryBudget == NativeRetryBudgetState.AVAILABLE && networkOnly.networkRecoveries == 1uL && networkOnly.nextDeadlineNs == null)
        session.transportFailed(generation, 10uL)
        session.replaceTransport(generation, 11uL)
        val uncertainAttempt = session.snapshot().attemptEpoch
        session.connected(generation, 12uL)
        session.sending(generation, uncertainAttempt, 13uL)
        check(session.tick(generation, 1_000_010uL) == NativeSessionAction.RetryBudgetExhausted)
        val uncertain = session.snapshot()
        check(uncertain.transportConnected && uncertain.retryBudget == NativeRetryBudgetState.EXHAUSTED)
        check(uncertain.nextDeadlineNs == null && uncertain.outageDeadlineNs == 1_000_010uL)
        try { session.transportRecovered(generation, attempt, 1_000_011uL, NativeTransportRecoverySource.SRT_PEER_ACKNOWLEDGED_WATERMARK); error("stale attempt recovered") }
        catch (_: BindingException.StaleSession) {}
        try { session.mediaConfirmedForAttempt(generation, attempt, ULong.MAX_VALUE); error("old decoded observation confirmed successor") }
        catch (_: BindingException.StaleSession) {}
        check(session.snapshot().mediaReadiness == NativeMediaReadiness.UNKNOWN)
        // This fixture exercises the control input, not an actual media decoder.
        session.mediaConfirmedForAttempt(generation, uncertainAttempt, 1_000_012uL)
        val decoded = session.snapshot()
        check(decoded.state == NativeSessionState.LIVE && decoded.mediaReadiness == NativeMediaReadiness.CONFIRMED)
        check(decoded.retryBudget == NativeRetryBudgetState.EXHAUSTED && decoded.outageDeadlineNs == 1_000_010uL)
        session.cancel(generation)
        try { session.mediaConfirmedForAttempt(generation, uncertainAttempt, 1_000_013uL); error("decoded observation resurrected cancelled session") }
        catch (_: BindingException.StaleSession) {}
    }
}
