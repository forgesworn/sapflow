package dev.forgesworn.sapflow.broadcaster

/** Production native-owner driver. Events are actual receipts, not close()
 * requests. No callbacks or platform operations execute under this lock. */
internal class AudioGenerationLifecycle(initialSerial: Long = 0) {
    enum class Action { NONE, STOP_PRODUCERS, SEND_EOS, QUIESCE, RELEASE, CLOSED, FAIL }
    enum class Stage { ACTIVE, PRODUCERS, EOS, CALLBACKS, RELEASE, CLOSED, RETAINED }
    class Generation internal constructor(val serial: Long)
    private var serial = initialSerial
    private var current: Generation? = null
    private var stage = Stage.CLOSED
    private var deadlineNs = Long.MAX_VALUE
    private var startedNs = 0L
    private var hadInput = false
    private var callbackClosed = false
    private var handoffClosed = false
    private var failed = false
    init { require(initialSerial >= 0) }
    @Synchronized fun begin(): Generation? {
        if (stage != Stage.CLOSED || failed || serial == Long.MAX_VALUE) return null
        stage = Stage.ACTIVE; callbackClosed = false; handoffClosed = false
        return Generation(++serial).also { current = it }
    }
    @Synchronized fun owns(value: Generation) = current === value && stage != Stage.CLOSED
    @Synchronized fun retire(value: Generation, acceptedInput: Boolean, nowNs: Long, originalDeadlineNs: Long): Action {
        if (current !== value || stage != Stage.ACTIVE) return Action.NONE
        if (nowNs < 0 || originalDeadlineNs <= nowNs) { failed = true; stage = Stage.PRODUCERS; return Action.STOP_PRODUCERS }
        startedNs = nowNs; deadlineNs = originalDeadlineNs; hadInput = acceptedInput
        stage = Stage.PRODUCERS; return Action.STOP_PRODUCERS
    }
    /** Preserve the scalar-driver entry point. The concrete native owner uses
     * the explicit overload with input observed AFTER its actual joins. */
    @Synchronized fun producersClosed(value: Generation): Action = producersClosed(value, hadInput)
    @Synchronized fun producersClosed(value: Generation, joinedAcceptedInput: Boolean): Action {
        if (current !== value || stage != Stage.PRODUCERS) return Action.NONE
        // Retirement may precede the first successful graceful-tail queue.
        // A positive earlier observation can never be erased by a later one.
        hadInput = hadInput || joinedAcceptedInput
        stage = if (hadInput) Stage.EOS else Stage.CALLBACKS
        return if (hadInput) Action.SEND_EOS else Action.QUIESCE
    }
    @Synchronized fun eos(value: Generation): Action {
        if (current !== value || stage != Stage.EOS) return Action.NONE
        stage = Stage.CALLBACKS; return Action.QUIESCE
    }
    /** EOS timeout is a failed prefix, but still closes callbacks before release. */
    @Synchronized fun abortEos(value: Generation): Action {
        if (current !== value || stage != Stage.EOS) return Action.NONE
        failed = true; stage = Stage.CALLBACKS; return Action.QUIESCE
    }
    @Synchronized fun callbacksClosed(value: Generation): Action {
        if (current !== value || stage != Stage.CALLBACKS) return Action.NONE
        callbackClosed = true; return releaseIfReady()
    }
    @Synchronized fun handoffIdle(value: Generation): Action {
        if (current !== value || stage != Stage.CALLBACKS) return Action.NONE
        handoffClosed = true; return releaseIfReady()
    }
    private fun releaseIfReady(): Action {
        if (!callbackClosed || !handoffClosed) return Action.NONE
        stage = Stage.RELEASE; return Action.RELEASE
    }
    @Synchronized fun released(value: Generation, actualReleaseSucceeded: Boolean, nowNs:Long): Action {
        if (current !== value || stage != Stage.RELEASE) return Action.NONE
        if (!actualReleaseSucceeded) { failed = true; stage = Stage.RETAINED; return Action.FAIL }
        if(nowNs<startedNs || nowNs>=deadlineNs)failed=true
        stage = Stage.CLOSED; return Action.CLOSED
    }
    @Synchronized fun tick(value: Generation, nowNs: Long): Action {
        if (current !== value || stage == Stage.ACTIVE || stage == Stage.CLOSED || failed) return Action.NONE
        if (nowNs < startedNs || nowNs >= deadlineNs) { failed = true; return Action.FAIL }
        return Action.NONE
    }
    @Synchronized fun fail(value: Generation) { if (current === value) failed = true }
    @Synchronized fun actualClosed(value: Generation) = current === value && stage == Stage.CLOSED
    @Synchronized fun isFailed() = failed
    @Synchronized fun currentStage() = stage
}

/** A synchronous privacy/Stop fence can precede the cold retirement message.
 * Hold that bounded raw borrow without native submission; only actual normal
 * retirement may admit its existing tail. Denial is never an implicit fault. */
internal object AudioRawAdmission {
    enum class Action { RAW, TAIL, WAIT, CANCEL, FAULT }
    fun action(rawPermitted:Boolean,retiring:Boolean,gracefulTail:Boolean,failed:Boolean):Action = when {
        failed -> Action.FAULT
        retiring -> if(gracefulTail)Action.TAIL else Action.CANCEL
        rawPermitted -> Action.RAW
        else -> Action.WAIT
    }
}

/** Shared by actual audio input and GL submission adapters. A native operation
 * held across a cold transition retries/cancels; only an exact real fault or
 * original deadline failure is fatal. All values are scalar, no hot allocation. */
internal object PrivacyNativeWork {
    enum class Action { SUBMIT, RETRY, CANCEL, FAULT }
    fun action(admission:PrivacyStandbyPolicy.NativeAdmission,exactOwner:Boolean,workCurrent:Boolean,failed:Boolean):Action = when {
        !exactOwner -> Action.CANCEL
        failed || admission==PrivacyStandbyPolicy.NativeAdmission.FAULT -> Action.FAULT
        admission==PrivacyStandbyPolicy.NativeAdmission.STALE || admission==PrivacyStandbyPolicy.NativeAdmission.RETIRING -> Action.CANCEL
        admission==PrivacyStandbyPolicy.NativeAdmission.SUBMIT && workCurrent -> Action.SUBMIT
        else -> Action.RETRY
    }
}

/** Exact native-generation failure linearisation, called synchronously from
 * real codec callbacks as well as PCM workers. Delivery/cleanup remains cold.
 * An old callback racing a fresh bind cannot fault the new generation. */
internal class AudioNativeFailure(private val run:CameraRunAdmission,private val beforeFence:(()->Unit)?=null) {
    private class State(val owner:Any,val reason:String?=null)
    private val state=java.util.concurrent.atomic.AtomicReference<State?>()
    val reason:String? get()=state.get()?.reason
    fun bind(owner:Any):Boolean {
        while(true){val old=state.get();if(old?.reason!=null)return false;if(state.compareAndSet(old,State(owner)))return true}
    }
    fun fail(owner:Any,reason:String):Boolean {
        while(true){
            val old=state.get() ?:return false
            if(old.owner!==owner || old.reason!=null)return false
            beforeFence?.invoke()
            if(state.compareAndSet(old,State(owner,reason))){run.fail();return true}
        }
    }
}

/** Failure classification for the actual PCM pump. This grants no native
 * completion authority. Its pending flag spans queue attempt through returned
 * call bookkeeping; intentional interruption is safe only before that span. */
internal object AudioInputFailure {
    fun requiresFailure(submissionPending:Boolean,pumping:Boolean,retiring:Boolean,interrupted:Boolean):Boolean =
        submissionPending || pumping && (!retiring || !interrupted)
    fun mayReturnBorrowedIndex(nativeQueueUnconfirmed:Boolean):Boolean = !nativeQueueUnconfirmed
}
