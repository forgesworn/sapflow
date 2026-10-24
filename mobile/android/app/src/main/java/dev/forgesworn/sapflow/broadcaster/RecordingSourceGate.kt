// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

/** Real-source EOF/resource and delivered-handle scope only. This is not a
 * native programme, H/T, coded clock, playback or application-admission receipt.
 * Actual AudioCaptureOwner retains the only issued SourceKey. Test gates have
 * their own keys and can never authorise a separately bound production owner. */
internal class RecordingSourceGate(private val epoch: Long) {
    internal val guard = Any()
    class SourceKey internal constructor()
    enum class Kind { NO_INPUT_CLOSED, INPUT_EOS_CLOSED }
    class Terminal internal constructor(internal val issuer: RecordingSourceGate,
        val kind: Kind, val generations: Long, val acceptedNativeInputFrames: Long)
    class FinishPermit internal constructor(internal val issuer: RecordingSourceGate,
        internal val recorder: Recorder, internal val terminal: Terminal,
        internal val frontier: Long)
    inner class Recorder internal constructor() {
        internal val issuer: RecordingSourceGate get() = this@RecordingSourceGate
        fun owns(owner: Any) = synchronized(guard) { recorder === this && recorderOwner === owner }
        fun accepted() = add(this, true)
        fun consumed() = add(this, false)
        fun failed() = fail()
        fun writerOpened(writer: Any) = synchronized(guard) {
            check(recorder === this && boundWriter == null && !committed)
            boundWriter = writer
        }
        fun drained(source: Terminal?): FinishPermit? = synchronized(guard) {
            if (recorder !== this || failedState || committed || source == null || source !== issuedTerminal ||
                source.issuer !== this@RecordingSourceGate || activeGeneration != null ||
                acceptedCount != consumedCount || issuedPermit != null) return@synchronized null
            FinishPermit(this@RecordingSourceGate, this, source, acceptedCount).also { issuedPermit = it }
        }
        fun commitEmpty(permit: FinishPermit) = synchronized(guard) {
            check(boundWriter == null && acceptedCount == 0L && consumedCount == 0L)
            commitLocked(permit, null)
        }
    }
    private var sourceKey: SourceKey? = null
    private var producer: Any? = null
    private var preparation: Any? = null
    private var recorder: Recorder? = null
    private var recorderOwner: Any? = null
    private var boundWriter: Any? = null
    private var activeGeneration: Any? = null
    private var started = 0L
    private var closed = 0L
    private var nativeFrames = 0L
    private var acceptedCount = 0L
    private var consumedCount = 0L
    private var failedState = false
    private var committed = false
    private var issuedTerminal: Terminal? = null
    private var issuedPermit: FinishPermit? = null
    fun bindProducer(owner: Any, prepared: Any, sourceEpoch: Long): SourceKey = synchronized(guard) {
        check(epoch >= 0 && sourceEpoch == epoch && producer == null && !committed)
        producer = owner; preparation = prepared
        SourceKey().also { sourceKey = it }
    }
    fun bindRecorder(owner: Any): Recorder = synchronized(guard) {
        check(recorder == null && !committed)
        recorderOwner = owner
        Recorder().also { recorder = it }
    }
    fun generationStarted(key: SourceKey, generation: Any): Boolean = synchronized(guard) {
        if (key !== sourceKey || failedState || committed || issuedTerminal != null || activeGeneration != null) return@synchronized false
        if (started == Long.MAX_VALUE) { failedState = true; return@synchronized false }
        started++; activeGeneration = generation; true
    }
    /** A recording-only failure cannot stop publishing replacements. A sealed
     * or committed source never grants permission to construct another codec. */
    fun publishingOnlyAfterLoss(key: SourceKey): Boolean = synchronized(guard) {
        key === sourceKey && failedState && !committed && issuedTerminal == null
    }
    /** Called only AFTER real lifecycle CLOSED and actual release returns. The
     * values come from joined producers and actual native queue/output events,
     * never AudioGenerationObservation, source JSON or a caller completion bit. */
    fun generationClosed(key: SourceKey, generation: Any, inputFrames: Long,
        inputEosReturned: Boolean, outputEosObserved: Boolean): Boolean = synchronized(guard) {
        if (key !== sourceKey || generation !== activeGeneration || failedState || committed) return@synchronized false
        if (inputFrames < 0 || inputFrames > 0 && (!inputEosReturned || !outputEosObserved) ||
            closed == Long.MAX_VALUE || inputFrames > Long.MAX_VALUE - nativeFrames) {
            failedState = true; return@synchronized false
        }
        nativeFrames += inputFrames; closed++; activeGeneration = null; true
    }
    fun terminal(key: SourceKey, owner: Any, prepared: Any, sourceEpoch: Long): Terminal? = synchronized(guard) {
        if (key !== sourceKey || owner !== producer || prepared !== preparation || sourceEpoch != epoch ||
            failedState || committed || activeGeneration != null || started == 0L || started != closed) return@synchronized null
        issuedTerminal ?: Terminal(this, if (nativeFrames == 0L) Kind.NO_INPUT_CLOSED else Kind.INPUT_EOS_CLOSED,
            closed, nativeFrames).also { issuedTerminal = it }
    }
    fun fail() = synchronized(guard) { failLocked() }
    internal fun failLocked() {
        check(Thread.holdsLock(guard))
        if (!committed) failedState = true
    }
    private fun add(value: Recorder, offered: Boolean) = synchronized(guard) {
        check(value === recorder)
        if (committed || issuedPermit != null || if (offered) acceptedCount == Long.MAX_VALUE else consumedCount == Long.MAX_VALUE) {
            failLocked(); return@synchronized
        }
        if (offered) acceptedCount++ else consumedCount++
        if (consumedCount > acceptedCount) failLocked()
    }
    /** Called by the actual catalogue CAS while holding guard, then its own
     * compoundGuard. All file/native IO and callbacks stay outside both. */
    internal fun requireCommitLocked(permit: FinishPermit, writer: Any?) {
        check(Thread.holdsLock(guard))
        check(!failedState && !committed && permit === issuedPermit && permit.issuer === this &&
            permit.recorder === recorder && permit.terminal === issuedTerminal &&
            activeGeneration == null && closed == started && acceptedCount == consumedCount && acceptedCount == permit.frontier &&
            boundWriter === writer) { "Source/recorder/frontier is incomplete, foreign, failed or already consumed" }
    }
    internal fun commitLocked(permit: FinishPermit, writer: Any?) {
        requireCommitLocked(permit, writer); committed = true
    }
}

/** Includes release/close failures after a successful consume return. Inline
 * so the existing worker/control task gains no extra payload or task owner. */
internal inline fun <T> RecordingSourceGate.Recorder?.fenceFailure(block: () -> T): T =
    try { block() } catch (error: Throwable) { this?.failed(); throw error }
