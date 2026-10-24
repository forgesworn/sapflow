// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.AudioFormat
import java.io.FileOutputStream
import java.security.MessageDigest

/** Concrete output and terminal owner AFTER successful decoder start. The
 * caller still owns configuration, input acquisition/preparation and format checking.
 * Internal geometry is an observation, not source or presentation authority.
 * No PCM is stored, copied, queued or passed through FFI by this owner. */
internal class NativeRecordingReader(
    private val codec: MediaCodec,
    private val extractor: MediaExtractor,
    private val stream: FileOutputStream,
    private val generation: Long,
    geometry: ReturnedPcmClock.Geometry,
    private val event: (String, Boolean, Exception?) -> Unit,
    private val label: String,
) {
    data class Output(
        val capacity: Int?, val sha256: String, val eos: Boolean, val borrowSerial: Long,
        val position: ReturnedPcmClock.Position?, val sinkBytesReturned: Long,
        val sinkWriteReturns: Long,
    )
    private val thread = Thread.currentThread()
    private val clock = ReturnedPcmClock(codec, generation, geometry)
    private val sink = RecordingPcmSink(stream.channel)
    private var serial = 0L
    private var formatAccepted = false
    var sinkBytesReturned = 0L; private set
    var sinkWriteReturns = 0L; private set
    private var closing = false
    private var inputEosAttempted = false
    var inputEosReturned = false; private set
    var inputQueueUncertain = false; private set
    var clockBookkeepingFailure: Exception? = null; private set
    var observationFailure: Exception? = null; private set
    private fun report(name: String, returned: Boolean, error: Exception? = null) {
        try { event(name, returned, error) } catch (failure: Exception) {
            observationFailure = observationFailure ?: failure
            clock.knownLoss(codec, generation)
        }
    }
    var releaseAttempts = 0; private set
    var releaseReturned = 0; private set
    var stopReturned = false; private set
    var decoderReleased = false; private set
    var extractorReleased = false; private set
    var streamClosed = false; private set
    var streamSynced = false; private set
    var nativeIndexUncertain = false; private set
    val cleanupConfirmed get() = decoderReleased && extractorReleased && streamClosed && !nativeIndexUncertain
    val actualNativeOwnershipUnknown get() = nativeIndexUncertain || (closing && (!decoderReleased || !extractorReleased))
    private fun onThread() { check(Thread.currentThread() === thread) }

    var inputQueueAttempts = 0L; private set
    var inputQueueReturns = 0L; private set
    var inputMediaQueueReturns = 0L; private set
    /** Every checked-reader input queue call is made here. A throwing call may
     * have consumed the borrowed input index, so it quarantines this codec. */
    fun queueInput(index: Int, offset: Int, size: Int, ptsUs: Long, flags: Int) {
        onThread(); check(!closing && !nativeIndexUncertain)
        if (inputEosAttempted) { clock.knownLoss(codec, generation); error("Input after EOS attempt refused") }
        require(index >= 0 && offset == 0 && size >= 0 && (flags == 0 || flags == MediaCodec.BUFFER_FLAG_END_OF_STREAM))
        val eos = flags == MediaCodec.BUFFER_FLAG_END_OF_STREAM
        require(if (eos) size == 0 else size > 0)
        if (eos) inputEosAttempted = true
        inputQueueAttempts = Math.addExact(inputQueueAttempts, 1)
        // A non-return, including an abrupt Throwable, must reach cleanup with
        // this actual input index quarantined. Only native return clears it.
        inputQueueUncertain = true; nativeIndexUncertain = true
        try { codec.queueInputBuffer(index, offset, size, ptsUs, flags) }
        catch (error: Exception) {
            inputQueueUncertain = true; nativeIndexUncertain = true
            clock.knownLoss(codec, generation)
            report("decoder.queueInput:$label", false, error)
            throw error
        }
        inputQueueUncertain = false; nativeIndexUncertain = false
        // Actual return facts precede journal/JSON work and EOS bookkeeping.
        inputQueueReturns = Math.addExact(inputQueueReturns, 1)
        if (!eos) inputMediaQueueReturns = Math.addExact(inputMediaQueueReturns, 1)
        else {
            inputEosReturned = true
            try { clock.inputEosQueued(codec, generation) } catch (error: Exception) {
                clockBookkeepingFailure = error; clock.knownLoss(codec, generation)
            }
        }
        report("decoder.queueInput:$label", true, null)
    }

    fun acceptOutputFormat(actual: MediaFormat) {
        onThread(); check(!closing && !formatAccepted)
        check(actual.getInteger(MediaFormat.KEY_SAMPLE_RATE) == 48000 && actual.getInteger(MediaFormat.KEY_CHANNEL_COUNT) == 1)
        check(!actual.containsKey(MediaFormat.KEY_PCM_ENCODING) || actual.getInteger(MediaFormat.KEY_PCM_ENCODING) == AudioFormat.ENCODING_PCM_16BIT)
        formatAccepted = true
    }

    /** index/info come from this codec's actual dequeue on the same worker.
     * A repeated native index is distinguished by the monotonic borrow serial. */
    fun consumeOutput(index: Int, info: MediaCodec.BufferInfo): Output {
        onThread(); check(!closing && !nativeIndexUncertain)
        var ticket: ReturnedPcmClock.Ticket? = null
        try {
            check(formatAccepted)
            val actual = codec.getOutputBuffer(index)?.duplicate()
            check(info.size == 0 || actual != null)
            val capacity = actual?.capacity()
            serial = Math.addExact(serial, 1)
            val range = clock.observe(codec, generation, serial, index, info.offset, info.size,
                capacity ?: 0, info.presentationTimeUs, info.flags)
            ticket = range
            val digest = MessageDigest.getInstance("SHA-256")
            var bytesReturned = 0L; var writes = 0L
            clock.sinkAttempt(codec, generation, range)
            try {
                if (info.size > 0) {
                    val borrowed = checkNotNull(actual)
                    borrowed.position(info.offset); borrowed.limit(info.offset + info.size)
                    digest.update(borrowed.duplicate())
                    val transfer = sink.write(borrowed)
                    bytesReturned = transfer.bytesReturned; writes = transfer.writeReturns
                    sinkBytesReturned = Math.addExact(sinkBytesReturned, bytesReturned)
                    sinkWriteReturns = Math.addExact(sinkWriteReturns, writes)
                    if (!transfer.complete) {
                        clock.sinkFailed(codec, generation, range, transfer.acceptanceUnknown)
                        throw checkNotNull(transfer.failure)
                    }
                }
                clock.sinkAccepted(codec, generation, range)
            } catch (error: Exception) {
                if (range.sinkDisposition == 0) clock.sinkFailed(codec, generation, range, true)
                throw error
            }
            val hex = "0123456789abcdef"
            val hash = buildString { for (b in digest.digest()) { val v = b.toInt() and 255; append(hex[v ushr 4]); append(hex[v and 15]) } }
            return Output(capacity, hash, info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0, range.serial,
                range.position, bytesReturned, writes)
        } finally {
            // Even a failed sink return leaves a known native borrow releasable.
            // A thrown native release retains this owner and is never retried.
            val range = ticket
            if (range != null) clock.releaseAttempt(codec, generation, range) else clock.knownLoss(codec, generation)
            releaseAttempts++
            nativeIndexUncertain = true
            try { codec.releaseOutputBuffer(index, false) } catch (error: Exception) {
                nativeIndexUncertain = true
                if (range != null && range.releaseOutcome == 0) clock.releaseFailed(codec, generation, range, true)
                report("decoder.releaseOutput:$label:$index", false, error)
                throw error
            }
            nativeIndexUncertain = false
            releaseReturned++
            if (range != null) clock.releaseReturned(codec, generation, range)
            report("decoder.releaseOutput:$label:$index", true, null)
        }
    }

    /** No caller Boolean can declare closure. Every fact follows the actual
     * native/file call in this method; the final snapshot is taken afterwards. */
    fun closeOwned() {
        onThread(); check(!closing); closing = true
        var ownerAttempted = false
        if (!nativeIndexUncertain) {
            clock.ownerCloseAttempt(codec, generation); ownerAttempted = true
            try { codec.stop(); stopReturned = true; report("decoder.stop:$label", true, null) }
            catch (error: Exception) { report("decoder.stop:$label", false, error) }
            if (stopReturned) {
                try { codec.release(); decoderReleased = true; report("decoder.release:$label", true, null) }
                catch (error: Exception) { report("decoder.release:$label", false, error) }
            }
        }
        try { extractor.release(); extractorReleased = true; report("extractor.release:$label", true, null) }
        catch (error: Exception) { report("extractor.release:$label", false, error) }
        if (ownerAttempted) {
            if (decoderReleased && extractorReleased) clock.ownerCloseReturned(codec, generation)
            else clock.ownerCloseUnknown(codec, generation)
        }
        clock.sinkCloseAttempt(codec, generation)
        try { stream.fd.sync(); streamSynced = true; report("pcm.sync:$label", true, null) }
        catch (error: Exception) { report("pcm.sync:$label", false, error) }
        try { stream.close(); streamClosed = true; report("pcm.close:$label", true, null) }
        catch (error: Exception) { report("pcm.close:$label", false, error) }
        if (streamSynced && streamClosed) clock.sinkCloseReturned(codec, generation)
        else clock.sinkCloseUnknown(codec, generation)
    }
    fun snapshot(): ReturnedPcmClock.Snapshot { onThread(); check(closing); return clock.snapshot() }
}
