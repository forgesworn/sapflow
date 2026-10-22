package dev.forgesworn.sapflow.broadcaster

/** Owner policy. Native adapters supply actual receipts. No codec/pixel/PCM proof is
 * manufactured here: native adapters must call receipt methods only after the
 * named actual event. Old accepted compressed material remains old prefix.
 * Preparation and Run use identity, not a caller-selected numeric epoch alone.
 * Cold transitions allocate; raw/replacement gates are one volatile read. */
internal class PrivacyStandbyPolicy(
    private val preparation: Any,
    private val hasVideo: Boolean,
    initialStandby: Boolean = false,
    private val sinks: Sinks = Sinks.BOTH,
    private val budgetNs: Long = 6_000_000_000L,
    initialRevision: Long = 0,
) {
    enum class Desired { LIVE, STANDBY }
    enum class Phase { LIVE, ARMED, ENTERING_STANDBY, RESUMING, STANDBY, FAILED, STOPPED }
    enum class Sink(val bit: Int) { PUBLISHING(1), RECORDING(2) }
    enum class Sinks(val mask: Int) { PUBLISHING_ONLY(1), RECORDING_ONLY(2), BOTH(3) }
    enum class Fault { DEADLINE, CLOCK, EXHAUSTED, NATIVE }
    enum class NativeAdmission { SUBMIT, SUPERSEDED, RETIRING, STALE, WAIT_START, FAULT }
    class Run internal constructor(val epoch: Long, internal val descriptor: Any)
    class Command internal constructor(val run: Run, val revision: Long, val desired: Desired,
        val startedNs: Long, val deadlineNs: Long)
    class SourceMutation internal constructor(val run: Run, val revision: Long)
    class MediaProof internal constructor(internal val command: Command, internal val video: Boolean,
        val pts: Long, internal val audioGeneration: Long?)
    class Snapshot(val desired: Desired, val phase: Phase, val revision: Long,
        val run: Run?, val pending: Command?, val fault: Fault?, val requiredSinks: Int)
    private class Published(val run: Run?, val revision: Long, val raw: Boolean, val pending: Command?, val replacement: Command?, val mediaStarted: Boolean, val preview: Boolean, val sinks: Int, val tailRun: Run?, val outputBoundary:Command?, val ownerRun:Run?, val faulted:Boolean)
    private var desired = if (initialStandby) Desired.STANDBY else Desired.LIVE
    private var phase = if (initialStandby) Phase.ARMED else Phase.LIVE
    private var revision = initialRevision
    private var run: Run? = null
    private var closing = false
    private var lastEpoch = -1L
    private var mediaStarted = false // Logical show, preserved through orientation children.
    private var command: Command? = null
    private var stopBoundary:Command?=null
    private var activeReplacement: Command? = null
    private var mutation: SourceMutation? = null
    private var fault: Fault? = null
    private var stopped = false
    private var requiredSinks = sinks.mask // Terminal detachment persists through orientation children.
    private var audioGeneration: Long? = null
    private var largestAudioGeneration = 0L
    private var oldAudioClosed = false
    private var retiringAudioGeneration: Long? = null
    private var zeroAudioGeneration: Long? = null
    // At 30 fps this covers the entire maximum six-second operation plus its
    // initial frame; it adds no earlier half-second keyframe deadline.
    private val videoInputs = LongArray(181)
    private var videoInputCount = 0
    private var videoProof: MediaProof? = null
    private var audioProof: MediaProof? = null
    private var videoAccepted = 0
    private var audioAccepted = 0
    @Volatile private var published = Published(null, revision, false, null, null, false,false,requiredSinks,null,null,null,false)

    init { require(budgetNs in 1..6_000_000_000L && initialRevision >= 0) }

    /** Native raw admission reservation point. An already reserved native call
     * belongs to the old prefix; Standby never claims to retract it. */
    fun permitsRaw(value: Run, expectedRevision: Long): Boolean {
        val view = published
        return view.run === value && view.revision == expectedRevision && view.raw
    }
    fun permitsRaw(value: Run): Boolean { val view=published; return view.run === value && view.raw }
    fun permitsPreview(value: Run): Boolean { val view=published;return view.run === value && view.preview }
    fun branchesAccepted(publishing:Boolean,recording:Boolean):Boolean { val mask=published.sinks;return (mask and 1==0 || publishing) && (mask and 2==0 || recording) }
    /** Final output admission, including queued native output when owner
     * timers were withheld. Explicit Stop/Auto permits only its old tail; an
     * expired privacy transaction never acquires that allowance. */
    fun permitsOutput(value:Run,nowNs:Long):Boolean {
        val view=published
        if(!view.mediaStarted || nowNs<0)return false
        val pending=view.outputBoundary
        if(pending!=null && (nowNs<pending.startedNs || nowNs>=pending.deadlineNs))return false
        return view.tailRun===value || view.run===value
    }
    val cleanupDeadlineNs:Long? get()=published.outputBoundary?.deadlineNs
    fun permitsResume(value:Command,nowNs:Long):Boolean { val view=published;return view.mediaStarted && view.run===value.run && view.pending===value && value.desired==Desired.LIVE && nowNs>=value.startedNs && nowNs<value.deadlineNs }
    fun needsOutputProof(value: Command): Boolean = published.pending === value
    /** One immutable state read distinguishes normal native work cancellation
     * from an actual expiry/fault. Resume/Stop/Auto must not fault a held zero
     * input or slate draw merely because its old replacement gate was closed. */
    fun replacementAdmission(value:Command,nowNs:Long):NativeAdmission {
        val view=published
        return nativeAdmission(view,value,nowNs,false)
    }
    fun resumeAdmission(value:Command,nowNs:Long):NativeAdmission {
        val view=published
        return nativeAdmission(view,value,nowNs,true)
    }
    private fun nativeAdmission(view:Published,value:Command,nowNs:Long,resume:Boolean):NativeAdmission {
        if(view.ownerRun!==value.run)return NativeAdmission.STALE
        if(view.faulted || nowNs<value.startedNs)return NativeAdmission.FAULT
        val boundary=view.outputBoundary
        if(boundary!=null && (nowNs<boundary.startedNs || nowNs>=boundary.deadlineNs))return NativeAdmission.FAULT
        if(!view.mediaStarted)return NativeAdmission.WAIT_START
        if(view.tailRun===value.run)return NativeAdmission.RETIRING
        if(view.run!==value.run)return NativeAdmission.STALE
        if(resume)return if(view.pending===value && value.desired==Desired.LIVE) NativeAdmission.SUBMIT else NativeAdmission.SUPERSEDED
        if(view.raw || value.desired!=Desired.STANDBY)return NativeAdmission.SUPERSEDED
        return if(view.replacement===value || view.pending===value) NativeAdmission.SUBMIT else NativeAdmission.SUPERSEDED
    }
    /** Withheld timers cannot extend native input admission. */
    fun permitsReplacement(value:Command,nowNs:Long):Boolean = replacementAdmission(value,nowNs)==NativeAdmission.SUBMIT
    @Synchronized fun snapshot() = Snapshot(desired, phase, revision, run, command, fault, requiredSinks)

    /** Call only once previous runClosed proved actual closure. Descriptor is
     * the retained Rust AAC descriptor, never a newly equal configuration. */
    @Synchronized fun attach(owner: Any, epoch: Long, descriptor: Any, existingAudioGeneration: Long?, nowNs: Long): Run? {
        if (owner !== preparation || stopped || fault != null || run != null || epoch < 0 || epoch <= lastEpoch) return null
        if (existingAudioGeneration != null && existingAudioGeneration <= largestAudioGeneration) return null
        val inherited = command
        if (inherited != null && !within(inherited, nowNs)) return null
        if (nowNs < 0) { fail(Fault.CLOCK); return null }
        val value = Run(epoch, descriptor)
        run = value; lastEpoch = epoch; closing = false; activeReplacement = null
        audioGeneration = existingAudioGeneration
        existingAudioGeneration?.let { largestAudioGeneration = it }
        resetEvidence()
        if (inherited != null) {
            command = null
            makeCommand(value, inherited.desired, inherited.startedNs, inherited.deadlineNs)
        } else if (mediaStarted && desired == Desired.STANDBY) {
            newCommand(value, Desired.STANDBY, nowNs)
        } else phase = if (desired == Desired.STANDBY) Phase.ARMED else Phase.LIVE
        publish(); return value.takeIf { fault == null }
    }
    /** Explicit committed Start, never Prepare, arms sink-output confirmation. */
    @Synchronized fun beganMedia(value: Run, nowNs: Long): Command? {
        if (!current(value) || mediaStarted) return null
        if (nowNs < 0) { fail(Fault.CLOCK); return null }
        mediaStarted = true
        command?.let { if (!within(it,nowNs)) return null; publish(); return it }
        return if (desired == Desired.STANDBY) newCommand(value, Desired.STANDBY, nowNs) else { publish(); null }
    }
    /** Highest-priority privacy request. Source mutation and pending Resume do
     * not prevent its synchronous raw fence. Repeats keep the same deadline. */
    @Synchronized fun standby(value: Run, nowNs: Long): Command? {
        if (run !== value || stopped || fault != null) return null
        if (nowNs < 0) { fail(Fault.CLOCK); return null }
        val pending = command
        if (pending != null && !within(pending, nowNs)) return null
        if (desired == Desired.STANDBY) return pending
        desired = Desired.STANDBY
        return if (pending != null) makeCommand(value, Desired.STANDBY, pending.startedNs, pending.deadlineNs)
        else if (mediaStarted || mutation != null) newCommand(value, Desired.STANDBY, nowNs)
        else { if (advanceRevision()) { phase = Phase.ARMED; publish() }; null }
    }
    /** Call only from an explicit creator action, with that displayed revision.
     * No reconnect, authority or native callback is authorised to request Live. */
    @Synchronized fun userResume(value: Run, expectedRevision: Long, nowNs: Long): Command? {
        if (!current(value) || expectedRevision != revision || desired != Desired.STANDBY || mutation != null) return null
        val pending = command
        if (pending != null && !within(pending, nowNs)) return null
        // Retirement may be irreversible. Resume is Busy until the actual old
        // AAC close, fresh zero-only owner and attached sink receipts all join.
        if (pending?.desired == Desired.STANDBY) return null
        desired = Desired.LIVE
        return if (pending != null) makeCommand(value, Desired.LIVE, pending.startedNs, pending.deadlineNs)
        else newCommand(value, Desired.LIVE, nowNs)
    }
    /** Actual verified microphone owner and (for AV) camera ready; this is not
     * granted by synthetic output or a delayed old Resume command. */
    @Synchronized fun resumed(value: Command, generation: Long, microphoneReady: Boolean, cameraReady: Boolean, nowNs: Long): Boolean {
        if (!owns(value, nowNs) || value.desired != Desired.LIVE || generation != audioGeneration ||
            !microphoneReady || hasVideo && !cameraReady || mutation != null) return false
        command = null; activeReplacement = null; phase = Phase.LIVE; publish(); return true
    }
    /** Readiness callbacks share the same cold commit decision as production
     * controller code. Urgent Standby/Stop winning this lock is a normal stale
     * result; an exact current invalid readiness/expiry remains a fault. */
    @Synchronized fun commitReadyResume(value:Command,generation:Long,microphoneReady:Boolean,cameraReady:Boolean,nowNs:Long):NativeAdmission {
        val admission=resumeAdmission(value,nowNs)
        if(admission!=NativeAdmission.SUBMIT)return admission
        return if(resumed(value,generation,microphoneReady,cameraReady,nowNs))NativeAdmission.SUBMIT else NativeAdmission.FAULT
    }
    /** Camera and privacy mutations share this reservation before owner posts. */
    @Synchronized fun reserveSource(value: Run, expectedRevision: Long): SourceMutation? {
        if (!hasVideo || !current(value) || revision != expectedRevision || desired != Desired.LIVE || command != null || mutation != null) return null
        return SourceMutation(value, revision).also { mutation = it }
    }
    @Synchronized fun sourceClosed(value: SourceMutation, actualCleanupComplete: Boolean, nowNs: Long): Boolean {
        if (mutation !== value || run !== value.run || stopped || fault != null) return false
        if (nowNs < 0) { fail(Fault.CLOCK); return false }
        if (!actualCleanupComplete) { fail(Fault.NATIVE); return false }
        mutation = null
        val pending = command
        if (pending != null && !within(pending, nowNs)) return false
        if (!mediaStarted && desired == Desired.STANDBY) {
            command = null; phase = Phase.ARMED; publish(); return true
        }
        confirmIfReady(); return true
    }
    /** Receipt includes old native output release, quiescence and actual close. */
    @Synchronized fun audioClosed(value: Command, generation: Long, nowNs: Long): Boolean {
        if (!mediaStarted || !owns(value, nowNs) || value.desired != Desired.STANDBY || generation != retiringAudioGeneration) return false
        oldAudioClosed = true; return true
    }
    /** Adapter guarantees this new platform generation has received ONLY zeros.
     * One active AAC at a time; same Rust descriptor identity is mandatory. */
    @Synchronized fun zeroAudioReady(value: Command, generation: Long, descriptor: Any, nowNs: Long): Boolean {
        if (!mediaStarted || !owns(value, nowNs) || value.desired != Desired.STANDBY || descriptor !== value.run.descriptor || generation <= 0) return false
        if (zeroAudioGeneration == generation) return true
        if (zeroAudioGeneration != null || audioGeneration != null && !oldAudioClosed || generation <= largestAudioGeneration) return false
        audioGeneration = generation; largestAudioGeneration = generation; zeroAudioGeneration = generation
        return true
    }
    /** Scalar marker from an actual generated EGL submission, not a request. */
    @Synchronized fun pictureSubmitted(value: Command, pts90k: Long, nowNs: Long): Boolean {
        if (!mediaStarted || !owns(value, nowNs) || value.desired != Desired.STANDBY || !hasVideo || pts90k < 0) return false
        if (videoProof != null) return true
        if (videoInputCount > 0 && pts90k <= videoInputs[videoInputCount - 1]) return pts90k == videoInputs[videoInputCount - 1]
        if (videoInputCount == videoInputs.size) { fail(Fault.EXHAUSTED); return false }
        videoInputs[videoInputCount++] = pts90k; return true
    }
    /** Actual coded H264 IDR (NAL type 5) and PTS must match the submitted
     * picture. Adapter validates the coded output, e.g. existing Rust sample
     * validation; a keyframe request, I-frame or platform keyframe flag alone
     * is insufficient. This pure receipt boundary does not parse NAL bytes. */
    @Synchronized fun pictureEncoded(value: Command, pts90k: Long, independentIdr: Boolean, nowNs: Long): MediaProof? {
        if (!mediaStarted || !owns(value, nowNs) || value.desired != Desired.STANDBY || !hasVideo || !independentIdr) return null
        videoProof?.let { return it.takeIf { proof -> proof.pts == pts90k } }
        if ((0 until videoInputCount).none { videoInputs[it] == pts90k }) return null
        return MediaProof(value, true, pts90k, null).also { videoProof = it; confirmIfReady() }
    }
    /** PTS/cadence/priming validation belongs to the persistent show audio
     * cursor. This marker is only for its actually validated fresh AAC output. */
    @Synchronized fun zeroAudioEncoded(value: Command, generation: Long, pts48k: Long, frames: Int, nowNs: Long): MediaProof? {
        if (!mediaStarted || !owns(value, nowNs) || value.desired != Desired.STANDBY || generation != zeroAudioGeneration || pts48k < 0 || frames != 1024) return null
        audioProof?.let { return it.takeIf { proof -> proof.pts == pts48k } }
        return MediaProof(value, false, pts48k, generation).also { audioProof = it; confirmIfReady() }
    }
    /** Validated bounded local branch acceptance, such as NativeSampleFanout's
     * publishingQueued/recordingQueued for this exact encoded marker. No remote
     * handshake is required; void consume, socket ACK and health are not proof.
     * Both sinks must accept the SAME proof; old prefix cannot confirm it. */
    @Synchronized fun accepted(value: MediaProof, sink: Sink, nowNs: Long): Boolean {
        if (!mediaStarted || !owns(value.command, nowNs) || requiredSinks and sink.bit == 0) return false
        if (value.video) {
            if (videoProof !== value) return false
            videoAccepted = videoAccepted or sink.bit
        } else {
            if (audioProof !== value || value.audioGeneration != zeroAudioGeneration) return false
            audioAccepted = audioAccepted or sink.bit
        }
        confirmIfReady(); return true
    }
    /** Adapter receipt: this exact run's branch irreversibly admits no further
     * samples (End or terminal publisher failure). Old prefix may still drain.
     * Never call for transient transport loss. There is no within-show reattach. */
    @Synchronized fun sinkTerminated(value: Run, sink: Sink, nowNs: Long): Boolean {
        if (run !== value || stopped || fault != null) return false
        if (nowNs < 0) { fail(Fault.CLOCK); return false }
        command?.let { if (!within(it,nowNs)) return false }
        requiredSinks = requiredSinks and sink.bit.inv()
        confirmIfReady(); return true
    }
    @Synchronized fun tick(value: Run, nowNs: Long): Boolean {
        if (!current(value)) return false
        if (nowNs < 0) { fail(Fault.CLOCK); return false }
        return command?.let { within(it, nowNs) } ?: true
    }
    @Synchronized fun nativeFailed(value: Run) { if (run === value && !stopped) fail(Fault.NATIVE) }
    /** Synchronous epoch close fence; actual closure is a separate receipt. */
    @Synchronized fun fenceRun(value: Run): Boolean {
        if (run !== value) return false
        closing = true; publish(); return true
    }
    @Synchronized fun runClosed(value: Run): Boolean {
        if (run !== value || !closing) return false
        run = null; mutation = null; activeReplacement = null; publish(); return true
    }
    @Synchronized fun stop(owner: Any): Boolean {
        if (owner !== preparation || stopped) return false
        stopBoundary=command
        stopped = true; closing = true; command = null; activeReplacement = null; phase = Phase.STOPPED; publish(); return true
    }
    private fun current(value: Run) = run === value && !closing && !stopped && fault == null
    private fun owns(value: Command, nowNs: Long) = current(value.run) && command === value && within(value, nowNs)
    private fun within(value: Command, nowNs: Long): Boolean {
        if (nowNs < value.startedNs) { fail(Fault.CLOCK); return false }
        if (nowNs >= value.deadlineNs) { fail(Fault.DEADLINE); return false }
        return true
    }
    private fun advanceRevision(): Boolean {
        if (revision == Long.MAX_VALUE) { fail(Fault.EXHAUSTED); return false }
        revision++; return true
    }
    private fun newCommand(value: Run, target: Desired, nowNs: Long): Command? {
        if (nowNs < 0) { fail(Fault.CLOCK); return null }
        val deadline = try { Math.addExact(nowNs, budgetNs) } catch (_: ArithmeticException) { fail(Fault.CLOCK); return null }
        return makeCommand(value, target, nowNs, deadline)
    }
    private fun makeCommand(value: Run, target: Desired, start: Long, deadline: Long): Command? {
        if (!advanceRevision()) return null
        resetEvidence(); desired = target
        return Command(value, revision, target, start, deadline).also {
            command = it; phase = if (target == Desired.STANDBY) Phase.ENTERING_STANDBY else Phase.RESUMING; publish()
        }
    }
    private fun resetEvidence() {
        retiringAudioGeneration = audioGeneration
        oldAudioClosed = retiringAudioGeneration == null; zeroAudioGeneration = null
        videoInputCount = 0; videoProof = null; audioProof = null; videoAccepted = 0; audioAccepted = 0
    }
    private fun confirmIfReady() {
        if (command?.desired == Desired.STANDBY && mutation == null && audioProof != null &&
            audioAccepted and requiredSinks == requiredSinks &&
            (!hasVideo || videoProof != null && videoAccepted and requiredSinks == requiredSinks)) {
            activeReplacement = command; command = null; phase = Phase.STANDBY; publish()
        }
    }
    private fun fail(reason: Fault) { if (stopped || fault != null) return; fault = reason; phase = Phase.FAILED; publish() }
    private fun publish() {
        val live = !closing && !stopped && fault == null
        published = Published(if (live) run else null, revision, live && mediaStarted && phase == Phase.LIVE && desired == Desired.LIVE,
            command.takeIf { live }, activeReplacement.takeIf { live }, mediaStarted,live && desired==Desired.LIVE && phase==Phase.LIVE,requiredSinks,run.takeIf { fault==null && (closing || stopped) },command ?: stopBoundary,run,fault!=null)
    }
}

/** One posted transaction per immutable command. Urgent Standby may replace a
 * queued Resume; that old continuation still has to pass the policy identity.
 * No ordinary repeat extends time or queues another native retirement. */
internal class PrivacyStandbyDispatch {
    private val last=java.util.concurrent.atomic.AtomicReference<PrivacyStandbyPolicy.Command?>()
    fun admit(value:PrivacyStandbyPolicy.Command):Boolean = last.getAndSet(value) !== value
}
