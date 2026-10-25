package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.NativeAudioFileReceipt
import dev.forgesworn.sapflow.NativeAudioGenerationReceipt
import dev.forgesworn.sapflow.NativeRecordingSourceMapping
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicBoolean

/** The same object whose consume() returns after an actual SDK push. */
internal interface AudioNativeAacWorker {
    fun beginAudio(firstLeadingFrames: UInt)
    fun generationEnded(receipt: NativeAudioGenerationReceipt)
    fun completeAudio(receipt: NativeAudioFileReceipt)
}

internal enum class AudioNativeAacFinish { EMPTY, FILE_RECEIPT }

internal class NativeCloseOnce {
    private val attempted = AtomicBoolean(false)
    private val returned = AtomicBoolean(false)
    private val unknown = AtomicBoolean(false)
    val closeUnknown get() = unknown.get()
    fun close(block: () -> Unit) {
        if (unknown.get()) throw IllegalStateException("Native sink close is unconfirmed")
        if (returned.get()) return
        if (!attempted.compareAndSet(false, true)) {
            if (returned.get()) return
            if (unknown.get()) throw IllegalStateException("Native sink close is unconfirmed")
            throw IllegalStateException("Native sink close is already in flight")
        }
        try {
            block()
            returned.set(true)
        } catch (error: Throwable) {
            unknown.set(true)
            throw error
        }
    }
}

/** Scalar native-generation and accepted-AU FIFO accounting for one capture run.
 * No media, payload, diagnostic snapshot or application-admission capability is
 * retained. All worker calls occur outside guard and only on one bound thread. */
internal class AudioNativeAacRun(private val runOwner: Any, private val runEpoch: Long) {
    private class Completion(
        val issuer: AudioNativeAacGeneration,
        val receipt: AudioNativeAacGeneration.Receipt,
        val fileGeneration: Long,
        val acceptedEnd: Long,
    ) {
        var calling = false
    }

    private val guard = Any()
    private val completions = ArrayDeque<Completion>()
    private var active: Any? = null
    private var activeIssuer: AudioNativeAacGeneration? = null
    private var activeSerial = 0L
    private var activeAccepted = 0L
    private var activeLeading: Int? = null
    private var lastSerial = 0L
    private var accepted = 0L
    private var acceptedMedia = 0L
    private var consumed = 0L
    private var consuming = false
    private var registered = 0L
    private var returned = 0L
    private var aggregateInput = 0L
    private var aggregateStandby = 0L
    private var aggregateEncoded = 0L
    private var firstLeading: Int? = null
    private var lastTrailing: Int? = null
    private var beginLeading: Int? = null
    private var beginAttempted = false
    private var beginReturned = false
    private var fileAttempted = false
    private var fileReturned = false
    private var emptyClosed = false
    private var sealed = false
    private var failed = false
    private var worker: AudioNativeAacWorker? = null
    private var workerThread: Thread? = null

    init { require(runEpoch >= 0) }

    private fun failLocked(): Boolean {
        failed = true
        return false
    }

    private fun ownsActive(who: Any, generation: Any): Boolean =
        who === runOwner && active === generation

    fun started(who: Any, generation: Any, serial: Long, issuer: AudioNativeAacGeneration): Boolean = synchronized(guard) {
        if (who !== runOwner) return@synchronized false
        if (failed || sealed || active != null || serial <= lastSerial || !issuer.belongsTo(generation, serial))
            return@synchronized failLocked()
        active = generation
        activeIssuer = issuer
        activeSerial = serial
        activeAccepted = 0L
        activeLeading = null
        lastSerial = serial
        true
    }

    fun leading(who: Any, generation: Any, frames: Int): Boolean = synchronized(guard) {
        if (!ownsActive(who, generation)) return@synchronized false
        if (failed || sealed || frames !in 0..8191 || activeLeading != null && activeLeading != frames)
            return@synchronized failLocked()
        activeLeading = frames
        true
    }

    /** Called only for actual recordingQueued audio, never for a handoff offer. */
    fun accepted(who: Any, generation: Any): Boolean = synchronized(guard) {
        if (!ownsActive(who, generation)) return@synchronized false
        if (failed || sealed || accepted == Long.MAX_VALUE || acceptedMedia == Long.MAX_VALUE || activeAccepted == Long.MAX_VALUE)
            return@synchronized failLocked()
        if (accepted == 0L) {
            beginLeading = activeLeading ?: return@synchronized failLocked()
        }
        accepted++
        acceptedMedia++
        activeAccepted++
        true
    }

    fun accepted(generation: Any): Boolean = accepted(runOwner, generation)

    fun acceptedVideo(): Boolean = synchronized(guard) {
        if (failed || sealed || fileAttempted || emptyClosed || acceptedMedia == Long.MAX_VALUE)
            return@synchronized failLocked()
        acceptedMedia++
        true
    }

    /** The existing bounded fanout owns early video until an actual accepted
     * original AU has bound first H. A failed or sealed run must instead drain
     * into its existing refusal/cleanup path, never wait indefinitely. */
    fun awaitingFirstAudio(): Boolean = synchronized(guard) { !failed && !sealed && accepted == 0L }

    /** Native closure remains independent of recording acceptance. A mismatch
     * fences only this checked recording bridge, not publishing or the receipt. */
    fun nativeClosed(who: Any, generation: Any, inputFrames: Long,
        receipt: AudioNativeAacGeneration.Receipt?,
    ): Boolean = synchronized(guard) {
        if (!ownsActive(who, generation)) return@synchronized false
        val issuer = checkNotNull(activeIssuer)
        val serial = activeSerial
        val generationAccepted = activeAccepted
        val generationLeading = activeLeading
        active = null
        activeIssuer = null
        activeSerial = 0L
        activeAccepted = 0L
        activeLeading = null
        if (failed) return@synchronized false
        if (inputFrames == 0L) {
            if (receipt == null && generationAccepted == 0L) return@synchronized true
            return@synchronized failLocked()
        }
        if (inputFrames < 0L || receipt == null || generationAccepted == 0L ||
            receipt.generation != serial || receipt.inputFrames != inputFrames ||
            receipt.accessUnits != generationAccepted || !issuer.ownsIssued(receipt) ||
            generationLeading != receipt.leadingFrames || registered == Long.MAX_VALUE ||
            completions.size >= MAX_PENDING_GENERATIONS) return@synchronized failLocked()
        val fileGeneration = registered + 1L
        completions.addLast(Completion(issuer, receipt, fileGeneration, acceptedMedia))
        registered = fileGeneration
        true
    }

    fun seal(who: Any): Boolean = synchronized(guard) {
        if (who !== runOwner) return@synchronized false
        if (failed || sealed || active != null || lastSerial == 0L) return@synchronized failLocked()
        sealed = true
        true
    }

    fun fail() = synchronized(guard) { failed = true }

    private fun bindWorker(value: AudioNativeAacWorker) {
        val bound = synchronized(guard) {
            val current = worker
            if (current == null) {
                worker = value
                workerThread = Thread.currentThread()
                true
            } else current === value && workerThread === Thread.currentThread()
        }
        check(bound) { "Native AAC receipt worker changed" }
        synchronized(guard) { check(!failed) { "Native AAC receipt bridge is failed" } }
    }

    private fun ensureBegin(value: AudioNativeAacWorker) {
        val head = synchronized(guard) {
            if (beginReturned) return
            if (beginAttempted || failed) {
                failLocked()
                throw IllegalStateException("Native AAC begin is unknown or replayed")
            }
            val actual = beginLeading
            if (actual == null) {
                failLocked()
                throw IllegalStateException("Native AAC first leading is unknown")
            }
            beginAttempted = true
            actual
        }
        try {
            value.beginAudio(head.toUInt())
        } catch (error: Throwable) {
            fail()
            throw error
        }
        synchronized(guard) {
            check(beginAttempted && !beginReturned) { "Native AAC begin return changed" }
            beginReturned = true
            check(!failed) { "Native AAC recording failed during begin" }
        }
    }

    /** Deliver only at the exact consumed audio frontier. The accepted count is
     * per physical generation, so a later generation cannot fill an earlier gap. */
    private fun deliverDue(value: AudioNativeAacWorker) {
        while (true) {
            val next = synchronized(guard) {
                if (failed) throw IllegalStateException("Native AAC receipt bridge is failed")
                val candidate = completions.peekFirst() ?: return
                if (candidate.calling || consumed > candidate.acceptedEnd) {
                    failLocked()
                    throw IllegalStateException("Native AAC generation frontier was overtaken")
                }
                if (consumed < candidate.acceptedEnd) return
                if (!beginReturned || returned + 1L != candidate.fileGeneration ||
                    !candidate.issuer.ownsIssued(candidate.receipt)) {
                    failLocked()
                    throw IllegalStateException("Native AAC generation issuer or order is invalid")
                }
                candidate.calling = true
                candidate
            }
            val source = next.receipt
            val sdkReceipt = NativeAudioGenerationReceipt(next.fileGeneration.toULong(),
                source.inputFrames.toULong(), source.rawFrames.toULong(), source.zeroFrames.toULong(),
                source.inputRate.toUInt(), source.encodedFrames.toULong(), source.leadingFrames.toUInt(),
                source.trailingFrames.toUInt(), NativeRecordingSourceMapping.NEAREST)
            try {
                value.generationEnded(sdkReceipt)
            } catch (error: Throwable) {
                fail()
                throw error
            }
            synchronized(guard) {
                check(next.calling && completions.peekFirst() === next && returned + 1L == next.fileGeneration) {
                    "Native AAC generation return was replayed"
                }
                check(!failed) { "Native AAC recording failed during generation completion" }
                try {
                    val nextInput = Math.addExact(aggregateInput, source.inputFrames)
                    val nextEncoded = Math.addExact(aggregateEncoded, source.encodedFrames)
                    val nextStandby = if (next.fileGeneration == 1L) 0L else
                        Math.addExact(aggregateStandby,
                            Math.addExact(checkNotNull(lastTrailing).toLong(), source.leadingFrames.toLong()))
                    aggregateInput = nextInput
                    aggregateEncoded = nextEncoded
                    aggregateStandby = nextStandby
                } catch (_: ArithmeticException) {
                    failLocked()
                    throw IllegalStateException("Native AAC file aggregate overflow")
                }
                if (next.fileGeneration == 1L) firstLeading = source.leadingFrames
                lastTrailing = source.trailingFrames
                returned = next.fileGeneration
                next.calling = false
                completions.removeFirst()
            }
        }
    }

    fun beforeSample(value: AudioNativeAacWorker) {
        bindWorker(value)
        synchronized(guard) {
            if (fileAttempted || emptyClosed) {
                failLocked()
                throw IllegalStateException("Native AAC media arrived after closure")
            }
        }
        ensureBegin(value)
        deliverDue(value)
        synchronized(guard) {
            if (failed || consuming || consumed >= acceptedMedia) {
                failLocked()
                throw IllegalStateException("Native AAC sample has no unique accepted frontier")
            }
            consuming = true
        }
    }

    /** Called only after the same worker's consume/push actually returned. */
    fun afterSample(value: AudioNativeAacWorker) {
        bindWorker(value)
        synchronized(guard) {
            if (fileAttempted || emptyClosed || !consuming || consumed == Long.MAX_VALUE || consumed >= acceptedMedia) {
                failLocked()
                throw IllegalStateException("Native AAC consumption has no pending accepted sample")
            }
            consumed++
            consuming = false
        }
        deliverDue(value)
    }

    /** Fold only returned generation calls. This is native SDK timing completion,
     * not durable storage, recorder-consumption or application admission. */
    fun finish(value: AudioNativeAacWorker): AudioNativeAacFinish {
        bindWorker(value)
        deliverDue(value)
        val receipt = synchronized(guard) {
            if (fileReturned) return AudioNativeAacFinish.FILE_RECEIPT
            if (emptyClosed) return AudioNativeAacFinish.EMPTY
            if (!fileAttempted && !failed && sealed && active == null &&
                accepted == 0L && acceptedMedia == 0L && consumed == 0L && !consuming &&
                registered == 0L && returned == 0L && completions.isEmpty() && !beginAttempted) {
                emptyClosed = true
                return AudioNativeAacFinish.EMPTY
            }
            if (fileAttempted || failed || !sealed || active != null || !beginReturned ||
                acceptedMedia != consumed || consuming ||
                completions.isNotEmpty() || returned == 0L || returned != registered) {
                failLocked()
                throw IllegalStateException("Native AAC file closure is incomplete")
            }
            fileAttempted = true
            NativeAudioFileReceipt(returned.toULong(), aggregateInput.toULong(), aggregateStandby.toULong(),
                aggregateEncoded.toULong(), checkNotNull(firstLeading).toUInt(), checkNotNull(lastTrailing).toUInt())
        }
        try {
            value.completeAudio(receipt)
        } catch (error: Throwable) {
            fail()
            throw error
        }
        synchronized(guard) {
            check(fileAttempted && !fileReturned) { "Native AAC file return was replayed" }
            fileReturned = true
            check(!failed) { "Native AAC recording failed during file completion" }
        }
        return AudioNativeAacFinish.FILE_RECEIPT
    }

    private companion object {
        const val MAX_PENDING_GENERATIONS = 16
    }
}
