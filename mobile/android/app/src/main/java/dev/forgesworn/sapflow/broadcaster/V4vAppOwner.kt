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
    private val expiryOwner = ScheduledThreadPoolExecutor(1) { Thread(it, "sapflow-v4v-grant-expiry").apply { isDaemon = true } }.apply { removeOnCancelPolicy = true }
    private var expiryAlarm: ScheduledFuture<*>? = null
    private fun arm(ticket: V4vGrantExpiry.Ticket) {
        synchronized(expiry) {
            expiryAlarm?.cancel(false)
            expiryAlarm = expiryOwner.schedule({
                synchronized(expiry) { if (expiry.expired(ticket) != null) { admission.invalidate(); preparedGrant = null; destination.getAndSet(null)?.stopDestination() } }
            }, (ticket.deadlineNs - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
        }
    }
    /** Called under the same lock as expiry/admission/destination adoption. */
    private fun fenceExpiredOwner() {
        if (expiry.expiredCurrent() != null) {
            admission.invalidate(); preparedGrant = null
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
    fun stopPublisher() {
        synchronized(expiry) { admission.invalidate(); preparedGrant = null; expiry.clear(); expiryAlarm?.cancel(false); expiryAlarm = null; destination.getAndSet(null)?.stopDestination() }
    }
    fun current(permit: V4vCapturePermit): Boolean = synchronized(expiry) {
        fenceExpiredOwner(); admission.current(permit.ticket)
    }
    fun canStart(binding: V4vCaptureAdmission.Prepared, permit: V4vCapturePermit): Boolean = synchronized(expiry) {
        fenceExpiredOwner()
        val old = preparedGrant
        old != null && expiry.admits(binding.owner) && destination.get() != null && admission.canStart(binding, permit.ticket, permit.grant.identity) &&
            permit.grant.permitsStartOf(old, System.nanoTime())
    }
    fun captureFactory(permit: V4vCapturePermit): V4vPreparedDestination {
        val result = synchronized(expiry) {
            admission.consume(permit.ticket)
            expiry.clear(); expiryAlarm?.cancel(false); preparedGrant = null; destination.getAndSet(null)?.stopDestination()
            val grant = permit.grant
            val ticket = expiry.begin(++captureSerial, grant.identity, grant.expiresAtMonotonicNs)
            val numeric = SrtDestination.parse(grant.endpoint)
            val coordinator = ReconnectingPublishingSink(graceMs = grant.graceMs) { events ->
                SrtPublishingSink(numeric, grant.streamId, grant.passphrase, false, events)
            }
            destination.set(coordinator); preparedGrant = grant; arm(ticket)
            V4vPreparedDestination(admission.bind(captureSerial, grant.identity)) { coordinator.newEpoch() }
        }
        // Private bounded scalar lifecycle correlation only; no destination/event/credential/media data.
        Log.i("SapflowV4v", "explicitCapturePrepare serial=$captureSerial graceMs=${permit.grant.graceMs}")
        return result
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
    fun grant(complete: (V4vCapturePermit?, Exception?) -> Unit) {
        val ticket = synchronized(expiry) { admission.snapshot() }
        var value: V4vGrant? = null
        control.submit({ value = captureGrant() }) { error -> ui.post {
            complete(if (error == null) V4vCapturePermit(ticket, checkNotNull(value)) else null, error)
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
