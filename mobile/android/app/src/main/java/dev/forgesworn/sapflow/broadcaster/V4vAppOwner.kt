package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

internal class V4vPreparedDestination(val binding: V4vCaptureAdmission.Prepared, val factory: () -> EncodedSink)

/** Process owner survives signer Activity transitions; it never resumes capture automatically. */
internal class V4vAppOwner private constructor(context: Context, val profile: V4vProfile) {
    val signerRelay = Nip55LoginRelay()
    private val ui = Handler(Looper.getMainLooper())
    private val destination = AtomicReference<ReconnectingPublishingSink?>()
    private var captureSerial = 0L
    private var preparedGrant: V4vGrant? = null
    private val expiry = V4vGrantExpiry(System::nanoTime)
    private val admission = V4vCaptureAdmission()
    private val correlation = AtomicReference<V4vAttemptCorrelation?>()
    private data class ObservationAdmission(val owner: Long, val deadlineNs: Long)
    // Read without taking expiry lock inside the coordinator's cold gate.
    private val observationAdmission = AtomicReference<ObservationAdmission?>()
    private val expiryOwner = ScheduledThreadPoolExecutor(1) { Thread(it, "sapflow-v4v-grant-expiry").apply { isDaemon = true } }.apply { removeOnCancelPolicy = true }
    private var expiryAlarm: ScheduledFuture<*>? = null
    private fun arm(ticket: V4vGrantExpiry.Ticket) {
        synchronized(expiry) {
            observationAdmission.set(ObservationAdmission(ticket.owner, ticket.deadlineNs))
            expiryAlarm?.cancel(false)
            expiryAlarm = expiryOwner.schedule({
                synchronized(expiry) { if (expiry.expired(ticket) != null) { observationAdmission.set(null); observer.cancel(); admission.invalidate(); correlation.getAndSet(null)?.clear(); preparedGrant = null; destination.getAndSet(null)?.stopDestination() } }
            }, (ticket.deadlineNs - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
        }
    }
    /** Called under the same lock as expiry/admission/destination adoption. */
    private fun fenceExpiredOwner() {
        if (expiry.expiredCurrent() != null) {
            observationAdmission.set(null); observer.cancel(); admission.invalidate(); correlation.getAndSet(null)?.clear(); preparedGrant = null
            expiryAlarm?.cancel(false); expiryAlarm = null
            destination.getAndSet(null)?.stopDestination()
        }
    }
    private fun grantCommitted(grant: V4vGrant) {
        synchronized(expiry) {
            fenceExpiredOwner() // A delayed alarm cannot let a late refresh revive its owner.
            expiry.refresh(grant.identity, grant.expiresAtMonotonicNs)?.let(::arm)
        }
    }
    private val wallAnchor = System.currentTimeMillis()
    private val monoAnchor = System.nanoTime()
    val control = V4vDestinationSession {
        V4vSessionEngine(profile, V4vControlClient(profile), V4vSessionStore(context.applicationContext),
            { wallAnchor + (System.nanoTime() - monoAnchor) / 1_000_000 }, { V4vDestinationSession.nonce() }, ::stopPublisher, ::grantCommitted)
    }
    private val observer by lazy { V4vOriginObserver(profile,
        permit = { scope, complete ->
            var value: V4vObservationPermit? = null
            control.submit({ value = observationPermit(scope.identity) }) { error -> complete(if (error == null) value else null) }
        }, current = { observationScope() === it }, match = ::matchFreshMedia,
        confirmed = { scope, _, probeDeadlineNs ->
            synchronized(expiry) {
                if (observationScope() === scope) {
                    val owner = observationAdmission.get()?.owner
                    val coordinator = destination.get()
                    coordinator?.originMediaConfirmed(scope.context) {
                        // No locks, HTTP or decoder work under final SDK gate.
                        val ticket = observationAdmission.get()
                        ticket != null && ticket.owner == owner && System.nanoTime() < minOf(ticket.deadlineNs, probeDeadlineNs) &&
                            destination.get() === coordinator && correlation.get()?.currentFor(scope.context) === scope
                    }
                }
            }
        }) }
    val playbackStatus: String get() = observer.diagnostics
    fun stopPublisher() {
        synchronized(expiry) { observationAdmission.set(null); observer.cancel(); admission.invalidate(); correlation.getAndSet(null)?.clear(); preparedGrant = null; expiry.clear(); expiryAlarm?.cancel(false); expiryAlarm = null; destination.getAndSet(null)?.stopDestination() }
    }
    fun current(permit: V4vCapturePermit): Boolean = synchronized(expiry) {
        fenceExpiredOwner(); admission.current(permit.ticket)
    }
    fun canStart(binding: V4vCaptureAdmission.Prepared, permit: V4vCapturePermit, profile: CaptureProfile): Boolean = synchronized(expiry) {
        fenceExpiredOwner()
        val old = preparedGrant
        old != null && expiry.admits(binding.owner) && destination.get() != null && permit.profile == profile && admission.canStart(binding, permit.ticket, permit.grant.identity, profile) &&
            permit.grant.permitsStartOf(old, System.nanoTime())
    }
    fun captureFactory(permit: V4vCapturePermit): V4vPreparedDestination {
        val result = synchronized(expiry) {
            // Reject insufficient quality BEFORE retiring an owner or consuming
            // approval. End/reconcile remain available on the control actor.
            admission.consume(permit.ticket, permit.profile, permit.grant.maxPublishBps)
            observationAdmission.set(null); observer.cancel(); correlation.getAndSet(null)?.clear(); expiry.clear(); expiryAlarm?.cancel(false); preparedGrant = null; destination.getAndSet(null)?.stopDestination()
            val grant = permit.grant
            val ticket = expiry.begin(++captureSerial, grant.identity, grant.expiresAtMonotonicNs)
            val numeric = SrtDestination.parse(grant.endpoint)
            // A retired factory cannot overwrite the next destination's scope.
            val attempts = V4vAttemptCorrelation()
            val coordinator = ReconnectingPublishingSink(graceMs = grant.graceMs,
                observationChanged = { context, active ->
                    if (active) attempts.currentFor(context)?.let { observer.accepted(it, permit.profile) }
                    else observer.cancel(context)
                }) { events ->
                val wire = attempts.begin(this, grant)
                SrtPublishingSink(numeric, wire.streamId, grant.passphrase, false, events, permit.profile.srtPacingBytesPerSecond(grant.maxPublishBps))
            }
            destination.set(coordinator); correlation.set(attempts); preparedGrant = grant; arm(ticket)
            V4vPreparedDestination(admission.bind(captureSerial, grant.identity, permit.profile)) { coordinator.newEpoch() }
        }
        // Private bounded scalar lifecycle correlation only; no destination/event/credential/media data.
        Log.i("SapflowV4v", "explicitCapturePrepare serial=$captureSerial graceMs=${permit.grant.graceMs}")
        return result
    }
    /** Exact accepted local scope; cached server receipts never create it. */
    fun observationScope(): V4vCapturedAttempt? = synchronized(expiry) {
        fenceExpiredOwner()
        val context = destination.get()?.observationScope ?: return@synchronized null
        correlation.get()?.currentFor(context)
    }
    fun matchFreshMedia(scope: V4vCapturedAttempt, receipt: V4vSessionReceipt): V4vMatchedMedia? = synchronized(expiry) {
        if (observationScope() !== scope) return@synchronized null
        val matched = correlation.get()?.matchFreshMedia(scope, receipt)
        if (observationScope() === scope) matched else null
    }
    fun beginLogin(packageName: String, context: Context, complete: (Exception?) -> Unit) {
        var challenge: V4vLoginChallenge? = null
        control.submit({ challenge = loginChallenge() }) { failure -> ui.post {
            if (failure != null) complete(failure)
            else try { Nip55Login(context, signerRelay).sign(packageName, checkNotNull(challenge)) { event, error ->
                if (error != null) complete(error)
                else control.submit({ login(checkNotNull(event)) }) { loginError -> ui.post { complete(loginError) } }
            } } catch (_: Exception) { complete(IllegalStateException("Signer unavailable")) }
        } }
    }
    fun grant(profile: CaptureProfile, complete: (V4vCapturePermit?, Exception?) -> Unit) {
        val ticket = synchronized(expiry) { admission.snapshot() }
        var value: V4vGrant? = null
        control.submit({ value = captureGrant() }) { error -> ui.post {
            complete(if (error == null) V4vCapturePermit(ticket, checkNotNull(value), profile) else null, error)
        } }
    }
    companion object {
        @Volatile private var instance: V4vAppOwner? = null
        @Synchronized fun configured(context: Context): V4vAppOwner? {
            instance?.let { return it }
            val profile = V4vProfile.approved(BuildConfig.V4V_API_ORIGIN, BuildConfig.V4V_PLAYBACK_ORIGIN, BuildConfig.V4V_PLAYBACK_PREFIX) ?: return null
            return V4vAppOwner(context.applicationContext, profile).also { instance = it }
        }
    }
}
