import dev.forgesworn.sapflow.*
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.Base64

fun main() {
    check(sdkAbiVersion() == 1u)
    checkPackagedStatusLeaseCache()
    checkIntentionalTransportReplacement()
    checkNetworkRecoveryAxes()
    checkNativeRecordingTimingBinding()
    checkSharedAacTimingViews()
    checkProgrammeWindowCheckpoints()
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

/** Real generated SDK container calls over synthetic compressed bytes only.
 * Supplied source/end scalars certify no native codec, source or file closure. */
private fun checkProgrammeWindowCheckpoints() {
    val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).apply { flip() }
    val start = NativeProgrammeWindowStart(7uL, 48_000uL, 8192uL, 2048uL, 8192uL, 0uL)
    val end = NativeProgrammeGenerationEnd(7uL, NativeProgrammeQueuedFrames(8192uL, 1517uL, 1248uL, 307uL, 4096uL), 15_360uL)
    val recording = ByteArray(32) { 7 }; val file = ByteArray(32) { 9 }
    MediaCodec.aac(asc).use { codec ->
        NativeRecordingMuxer.withProgrammeAacTiming(0uL, 4096uL, 100u).use { mux ->
            mux.beginProgrammeWindow(start)
            var first: NativeProgrammeCheckpoint? = null
            var firstSlot: ByteArray? = null
            var firstRegion: ByteArray? = null
            var previous: DecodedProgrammeCheckpoint? = null
            var initialSize = 0
            var initialHash = byteArrayOf()
            var regionBytes = 0uL
            var terminal = false
            val hash = MessageDigest.getInstance("SHA-256")
            fun drain() {
                while (true) {
                    val chunk = mux.takeProgrammeOutput() ?: break
                    check(chunk.bytes.isNotEmpty())
                    if (chunk.kind == NativeRecordingChunkKind.INITIAL) {
                        check(initialSize == 0 && chunk.checkpoint == null)
                        initialSize = chunk.bytes.size
                        initialHash = MessageDigest.getInstance("SHA-256").digest(chunk.bytes)
                    }
                    if (chunk.kind == NativeRecordingChunkKind.FINAL_HEADER) check(chunk.bytes.size == initialSize)
                    val token = chunk.checkpoint
                    if (first == null && token != null) first = token
                    try {
                        if (chunk.kind == NativeRecordingChunkKind.FRAGMENT) {
                            val issued = checkNotNull(token)
                            regionBytes += chunk.bytes.size.toULong(); hash.update(chunk.bytes)
                            val regionHash = (hash.clone() as MessageDigest).digest()
                            val encoded = mux.encodeProgrammeCheckpoint(issued, recording, file, regionHash)
                            check(encoded.bytes.copyOfRange(0, 8).contentEquals("SFCP0002".toByteArray()))
                            check(encoded.info.facts == issued.facts() && encoded.info.start == start)
                            check(encoded.info.initialHeaderLen == initialSize.toUInt())
                            check(encoded.info.initialHeaderSha256.contentEquals(initialHash))
                            check(encoded.info.fragmentRegionBytes == regionBytes)
                            check(encoded.info.committedOffset == initialSize.toULong() + regionBytes)
                            check(encoded.info.fragmentRegionSha256.contentEquals(regionHash))
                            val decoded = decodeProgrammeCheckpoint(encoded.bytes, recording, file)
                            try {
                                check(decoded.info().facts == encoded.info.facts && decoded.info().start == start)
                                check(decoded.header().contentEquals(mux.programmeCheckpointHeader(issued)))
                                check(decoded.presentationExtent() == issued.presentationExtent())
                                previous?.let { validateProgrammeCheckpointSuccessor(it, decoded) }
                                previous?.close(); previous = decoded
                            } catch (failure: Throwable) { decoded.close(); throw failure }
                            if (firstSlot == null) { firstSlot = encoded.bytes; firstRegion = regionHash }
                            terminal = issued.facts().windowGeometryComplete
                            if (terminal) {
                                check(issued.facts().audioUnits == 5uL && issued.facts().audioStart == 56_192uL)
                                check(issued.facts().audioEnd == 61_312uL && issued.facts().audioContainerEnd == 61_005uL)
                                check(issued.facts().trailingFrames == 307u)
                                check(issued.windowEnd() == end)
                                check(issued.presentationExtent().durationTicks == 41_475uL)
                            }
                        }
                    } finally { if (token !== first) token?.close() }
                }
            }
            try {
                repeat(5) { n ->
                    val tick = 56_192L + n * 1024L
                    val bytes = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x21, n.toByte())).apply { flip() }
                    codec.sample(bytes, tick, tick, 1024u, 48_000u, false).use { mux.push(it) }; drain()
                }
                val issued = checkNotNull(first)
                check(issued.facts().audioUnits == 4uL && !issued.facts().windowGeometryComplete)
                check(issued.facts().audioStart == 56_192uL && issued.presentationExtent().durationTicks == 30_720uL)
                try { mux.takeNativeOutput(); error("programme output used converter API") }
                catch (_: NativeRecordingException.InvalidConfig) {}
                NativeRecordingMuxer.withProgrammeAacTiming(0uL, 4096uL, 100u).use { other ->
                    other.beginProgrammeWindow(start)
                    repeat(5) { n ->
                        val tick = 56_192L + n * 1024L
                        codec.sample(ByteBuffer.allocateDirect(1).put(1).apply { flip() }, tick, tick, 1024u, 48_000u, false).use { other.push(it) }
                        while (true) { val output = other.takeProgrammeOutput() ?: break; output.checkpoint?.close() }
                    }
                    try { other.encodeProgrammeCheckpoint(issued, recording, file, checkNotNull(firstRegion)); error("foreign programme token") }
                    catch (_: RecordingCheckpointException.ForeignCheckpoint) {}
                }
                try { decodeRecordingCheckpoint(checkNotNull(firstSlot), recording, file).close(); error("V2 reinterpreted as V1") }
                catch (_: RecordingCheckpointException.Unsupported) {}
                val prefixExtent = issued.presentationExtent()
                mux.closeProgrammeWindow(end)
                mux.finishProgramme(NativeProgrammeCompletion.COMPLETE_SUBMITTED_WINDOW)
                drain(); check(terminal)
                check(mux.stats().durationMs == 58uL && mux.stats().acceptedSamples == 5uL)
                mux.cancel()
                val old = mux.encodeProgrammeCheckpoint(issued, recording, file, checkNotNull(firstRegion))
                check(old.bytes.contentEquals(checkNotNull(firstSlot)) && issued.presentationExtent() == prefixExtent)
            } finally { previous?.close(); first?.close() }
        }
        NativeRecordingMuxer.withProgrammeAacTiming(0uL, 4096uL, 100u).use { incomplete ->
            incomplete.beginProgrammeWindow(start)
            repeat(5) { n ->
                val tick = 56_192L + n * 1024L
                codec.sample(ByteBuffer.allocateDirect(1).put(1).apply { flip() }, tick, tick, 1024u, 48_000u, false).use { incomplete.push(it) }
                while (true) { val chunk = incomplete.takeProgrammeOutput() ?: break; chunk.checkpoint?.close() }
            }
            incomplete.closeProgrammeWindow(end)
            incomplete.finishProgramme(NativeProgrammeCompletion.INCOMPLETE_PREFIX)
            var checked = false
            while (true) {
                val chunk = incomplete.takeProgrammeOutput() ?: break
                chunk.checkpoint?.use { token -> check(!token.facts().windowGeometryComplete && token.facts().trailingFrames == 0u); checked = true }
            }
            check(checked)
        }
    }
    println("Sapflow Kotlin programme window checkpoints passed")
}

/** Synthetic compressed bytes check generated ownership/API, not AAC decoding. */
private fun checkNativeRecordingTimingBinding() {
    val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).flip()
    MediaCodec.aac(asc).use { codec ->
        NativeRecordingMuxer.withNativeAacTiming(false, true, 0uL, 4096uL, 100u).use { mux ->
            mux.beginNativeAudio(2112u)
            var firstSize = 0
            var finalSize = 0
            var sealed: NativeRecordingCheckpoint? = null
            var terminalFacts: NativeRecordingCheckpointFacts? = null
            var terminalExtent: NativeRecordingPresentationExtent? = null
            val recordingBinding = ByteArray(32) { 7 }
            val sourceFileBinding = ByteArray(32) { 9 }
            var initialHash = byteArrayOf()
            val fragmentHash = MessageDigest.getInstance("SHA-256")
            var regionBytes = 0uL
            var serializedEnd = 0uL
            var firstSlot: ByteArray? = null
            var firstRegionHash: ByteArray? = null
            var firstSealedEnd: ULong? = null
            var previousDecoded: DecodedRecordingCheckpoint? = null
            var fragmentCount = 0
            fun drain() {
                while (true) {
                    val chunk = mux.takeNativeOutput() ?: break
                    check(chunk.bytes.isNotEmpty())
                    val token = chunk.checkpoint
                    if (chunk.kind == NativeRecordingChunkKind.INITIAL) {
                        check(firstSize == 0 && serializedEnd == 0uL && token == null)
                        firstSize = chunk.bytes.size
                        serializedEnd = firstSize.toULong()
                        initialHash = MessageDigest.getInstance("SHA-256").digest(chunk.bytes)
                    }
                    if (chunk.kind == NativeRecordingChunkKind.FINAL_HEADER) {
                        // Offset-zero replacement contributes no region bytes/hash.
                        finalSize = chunk.bytes.size
                        check(previousDecoded?.info()?.committedOffset == serializedEnd)
                    }
                    token?.let { actual ->
                        val extent = actual.presentationExtent()
                        check(extent.movieTimescale == 720_000u)
                        actual.facts().let { if (it.completeAudioEof) { terminalFacts = it; terminalExtent = extent } }
                    }
                    if (sealed == null) sealed = token
                    try {
                        if (chunk.kind == NativeRecordingChunkKind.FRAGMENT) {
                            val exactToken = checkNotNull(token)
                            check(firstSize > 0)
                            fragmentCount += 1
                            regionBytes += chunk.bytes.size.toULong()
                            serializedEnd += chunk.bytes.size.toULong()
                            fragmentHash.update(chunk.bytes)
                            // The standard host SHA-256 provider's cloned state
                            // finalises this prefix without resetting the owner.
                            val regionHash = (fragmentHash.clone() as MessageDigest).digest()
                            val encoded = mux.encodeCheckpoint(exactToken, recordingBinding, sourceFileBinding, regionHash)
                            check(encoded.bytes.size in 545..66080)
                            val info = encoded.info
                            check(info.facts == exactToken.facts())
                            check(info.initialHeaderLen == firstSize.toUInt() && info.mediaEpochNs == 0uL)
                            check(info.initialHeaderSha256.contentEquals(initialHash))
                            check(info.recordingBinding.contentEquals(recordingBinding) && info.sourceFileBinding.contentEquals(sourceFileBinding))
                            check(info.committedOffset == serializedEnd && info.fragmentRegionBytes == regionBytes)
                            check(info.fragmentRegionSha256.contentEquals(regionHash))
                            val decoded = decodeRecordingCheckpoint(encoded.bytes, recordingBinding, sourceFileBinding)
                            try {
                                val parsed = decoded.info()
                                check(parsed.facts == info.facts && parsed.mediaEpochNs == info.mediaEpochNs)
                                check(parsed.initialHeaderLen == info.initialHeaderLen && parsed.initialHeaderSha256.contentEquals(initialHash))
                                check(parsed.recordingBinding.contentEquals(recordingBinding) && parsed.sourceFileBinding.contentEquals(sourceFileBinding))
                                check(parsed.committedOffset == serializedEnd && parsed.fragmentRegionBytes == regionBytes)
                                check(parsed.fragmentRegionSha256.contentEquals(regionHash) && decoded.fragmentRegionSha256().contentEquals(regionHash))
                                check(decoded.header().contentEquals(mux.checkpointHeader(exactToken)))
                                previousDecoded?.let { prior ->
                                    validateRecordingCheckpointSuccessor(prior, decoded)
                                    prior.close()
                                }
                                previousDecoded = decoded
                            } catch (failure: Throwable) {
                                decoded.close()
                                throw failure
                            }
                            if (firstSlot == null) {
                                firstSlot = encoded.bytes
                                firstRegionHash = regionHash
                                firstSealedEnd = info.committedOffset
                            }
                        }
                    } finally {
                        if (token !== sealed) token?.close()
                    }
                }
            }
            try {
                repeat(9) { n ->
                    val pts = (n * 1024).toLong()
                    val payload = ByteBuffer.allocateDirect(3).put(byteArrayOf(1, 2, 3)).flip()
                    codec.sample(payload, pts, pts, 1024u, 48_000u, false).use { mux.push(it) }
                    drain()
                }
                val checkpoint = checkNotNull(sealed)
                val facts = checkpoint.facts()
                val prefixExtent = checkpoint.presentationExtent()
                check(prefixExtent.movieTimescale == 720_000u && prefixExtent.durationTicks == 29_760uL)
                check(prefixExtent.durationMsCeil == 42uL && !prefixExtent.completeAudioEof)
                check(facts.audioUnits == 4uL && facts.audioEnd == 4096uL)
                check(facts.completedGenerationCount == 0uL)
                check(facts.audioContainerEnd == facts.audioEnd && facts.finalTrailingFrames == 0u && !facts.completeAudioEof)
                check(firstSize > 0 && mux.checkpointHeader(checkpoint).size == firstSize)
                val beforeRejection = mux.stats()
                NativeRecordingMuxer.withNativeAacTiming(false, true, 0uL, 4096uL, 100u).use { other ->
                    try { other.checkpointHeader(checkpoint); error("foreign recording checkpoint accepted") }
                    catch (_: NativeRecordingException.InvalidMedia) {}
                    val savedHash = checkNotNull(firstRegionHash)
                    try { other.encodeCheckpoint(checkpoint, recordingBinding, sourceFileBinding, savedHash); error("foreign live checkpoint encoded") }
                    catch (_: RecordingCheckpointException.ForeignCheckpoint) {}
                }
                val savedSlot = checkNotNull(firstSlot)
                val savedRegionHash = checkNotNull(firstRegionHash)
                val savedEnd = checkNotNull(firstSealedEnd)
                check(fragmentCount >= 2 && serializedEnd > savedEnd)
                val oldAfterAdvance = mux.encodeCheckpoint(checkpoint, recordingBinding, sourceFileBinding, savedRegionHash)
                check(oldAfterAdvance.bytes.contentEquals(savedSlot) && oldAfterAdvance.info.committedOffset == savedEnd)
                check(checkpoint.presentationExtent() == prefixExtent)
                try {
                    decodeRecordingCheckpoint(savedSlot, ByteArray(32) { 8 }, sourceFileBinding).use { error("foreign persisted recording binding accepted") }
                } catch (_: RecordingCheckpointException.Binding) {}
                try {
                    decodeRecordingCheckpoint(savedSlot, recordingBinding, ByteArray(32) { 8 }).use { error("foreign persisted file binding accepted") }
                } catch (_: RecordingCheckpointException.Binding) {}
                val afterRejection = mux.stats()
                check(beforeRejection.acceptedSamples == afterRejection.acceptedSamples)
                check(beforeRejection.queuedOutputBytes == afterRejection.queuedOutputBytes)
                mux.endNativeAudioGeneration(NativeAudioGenerationReceipt(1uL, 6400uL, 6400uL,
                    0uL, 48_000u, 9216uL, 2112u, 704u, NativeRecordingSourceMapping.NEAREST))
                mux.completeNativeAudio(NativeAudioFileReceipt(1uL, 6400uL, 0uL, 9216uL, 2112u, 704u))
                mux.finishNative(NativeRecordingCompletion.COMPLETE)
                drain()
                check(finalSize == firstSize)
                val terminal = checkNotNull(terminalFacts)
                val measured = checkNotNull(terminalExtent)
                check(measured.movieTimescale == 720_000u && measured.durationTicks == 96_000uL)
                check(measured.durationMsCeil == 134uL && measured.completeAudioEof)
                check(terminal.audioUnits == 9uL && terminal.audioEnd == 9216uL)
                check(terminal.audioContainerEnd == 8512uL && terminal.finalTrailingFrames == 704u)
                check(terminal.completeAudioEof && terminal.completedGenerationCount == 1uL)
                val stats = mux.stats()
                check(stats.finished && stats.acceptedSamples == 9uL)
                check(fragmentCount == 3 && serializedEnd == firstSize.toULong() + regionBytes)
                check(previousDecoded?.info()?.committedOffset == serializedEnd)
                val oldAfterFinish = mux.encodeCheckpoint(checkpoint, recordingBinding, sourceFileBinding, savedRegionHash)
                check(oldAfterFinish.bytes.contentEquals(savedSlot) && oldAfterFinish.info.committedOffset == savedEnd)
                check(checkpoint.presentationExtent() == prefixExtent)
                try { mux.takeOutput(); error("native recording used legacy output API") }
                catch (_: NativeRecordingException.InvalidConfig) {}
                mux.cancel()
                check(checkpoint.presentationExtent() == prefixExtent)
            } finally { previousDecoded?.close(); sealed?.close() }
        }
    }
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

/** Generated API/ownership controls with synthetic bytes, never codec/AU proof. */
private fun checkSharedAacTimingViews() {
    val asc = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x88.toByte())).flip()
    MediaCodec.aac(asc).use { codec ->
        val payload = ByteBuffer.allocateDirect(3).put(byteArrayOf(1, 2, 3)).flip()
        val original = codec.sample(payload, 4096, 4096, 1024u, 48_000u, false)
        check(original.aacSourceInfo() == null)
        original.aacTimingViews(17uL, -2112L, 0L).use { views ->
            val info = views.sourceInfo()
            check(info.producerGeneration == 17uL && info.nativePtsUs == -2112L)
            check(info.originalPts == 4096L && info.originalDts == 4096L)
            check(info.originalDuration == 1024u && info.originalTimescale == 48_000u)
            original.close() // Views retain their original ownership independently.
            NativeSampleFanout.withAacTimingViews(1024uL, 1024uL, 1000u, false).use { queues ->
                val first = queues.pushAacTimingViews(views, 0uL)
                check(first.publishingQueued && first.recordingQueued)
                codec.sample(payload, 5120, 5120, 1024u, 48_000u, false).use { next ->
                    next.aacTimingViews(17uL, -1L, null).use { withheld ->
                        check(withheld.publishingSample() == null)
                        val result = queues.pushAacTimingViews(withheld, 1uL)
                        check(!result.publishingQueued && result.recordingQueued)
                    }
                }
                check(queues.stats().publishingDrops == 0uL)
                queues.popPublishing(2uL)!!.use { publishing ->
                    check(publishing.pts() == 0L && publishing.aacSourceInfo() == info)
                }
                queues.popRecording(2uL)!!.use { recording ->
                    check(recording.pts() == 4096L && recording.aacSourceInfo() == info)
                    try { recording.aacTimingViews(17uL, 0L, 0L).close(); error("view-of-view accepted") }
                    catch (_: BindingException.InvalidMedia) {}
                }
                queues.popRecording(2uL)!!.use { check(it.pts() == 5120L && it.aacSourceInfo()!!.originalPts == 5120L) }
                check(queues.popPublishing(2uL) == null && queues.popRecording(2uL) == null)
            }
        }
        val stereo = ByteBuffer.allocateDirect(2).put(byteArrayOf(0x11, 0x90.toByte())).flip()
        MediaCodec.aac(stereo).use { unsupported ->
            unsupported.sample(payload, 0, 0, 1024u, 48_000u, false).use { sample ->
                try { sample.aacTimingViews(1uL, 0L, 0L).close(); error("stereo view accepted") }
                catch (_: BindingException.InvalidMedia) {}
            }
        }
    }
    println("Sapflow Kotlin shared AAC timing views passed")
}
