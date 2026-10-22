package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import java.security.SecureRandom

/** Authority deadlines include deep sleep; codec PTS, hardware timestamps and
 * PCM residence/age remain on their existing MONOTONIC clock. */
internal object AndroidAuthorityClock {
    fun nowNs(): Long = SystemClock.elapsedRealtimeNanos()
}

/** Application-context runtime ownership, never an Activity, Surface or mutable
 * destination form. Service delivery can only look up an already admitted owner. */
internal class AudioBroadcastOwner private constructor(private val context: Context) {
    private val policy = BackgroundAudioPolicy()
    val capture = CaptureController(context) { preparation ->
        notificationsGranted() && microphoneGranted() && policy.ready(preparation)
    }
    private val random = SecureRandom()
    private val clients = CaptureForegroundClients() // Main-thread opaque tokens, never Activities.
    fun notificationsGranted(): Boolean {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false
        val manager = context.getSystemService(NotificationManager::class.java)
        return manager.areNotificationsEnabled() && manager.getNotificationChannel(AudioBroadcastService.CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE
    }
    fun microphoneGranted(): Boolean = context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
    private fun stopPreparation(preparation: CaptureStartAdmission.Preparation, reason: String) {
        if (capture.stop(preparation, reason)) V4vAppOwner.existing()?.stopPublisher()
    }
    fun continuingShow(): BackgroundAudioPolicy.Owner? = policy.current()?.takeIf {
        policy.ready(it.preparation) && capture.continuesInBackground(it.preparation)
    }
    fun arm(preparation: CaptureStartAdmission.Preparation, usesV4v: Boolean, recordsLocally: Boolean): Boolean {
        if (!preparation.backgroundAudio || preparation.intent.hasVideo || !notificationsGranted() || !microphoneGranted()) {
            stopPreparation(preparation, "Background audio permissions are required. Prepare again.")
            return false
        }
        policy.current()?.takeIf { !capture.owns(it.preparation) }?.let(policy::retire)
        val bytes = ByteArray(16).also(random::nextBytes)
        val capability = bytes.joinToString("") { "%02x".format(it.toInt() and 255) }
        val value = policy.reserve(preparation, capability, usesV4v, recordsLocally) ?: run {
            stopPreparation(preparation, "The previous background audio owner is still finishing."); return false
        }
        return try { AudioBroadcastService.arm(context, value); true }
        catch (_: Exception) { stop(value, "Background audio service could not start. Prepare again."); policy.retire(value); false }
    }
    fun match(id: Long, capability: String?): BackgroundAudioPolicy.Owner? = policy.matches(id, capability)
    fun owns(value: BackgroundAudioPolicy.Owner): Boolean = policy.current() === value && capture.owns(value.preparation)
    fun promoted(value: BackgroundAudioPolicy.Owner): Boolean = owns(value) && notificationsGranted() && microphoneGranted() &&
        capture.servicePromoted(value.preparation) && policy.promote(value)
    fun canStart(preparation: CaptureStartAdmission.Preparation): Boolean = !preparation.backgroundAudio ||
        notificationsGranted() && microphoneGranted() && policy.ready(preparation) && capture.backgroundReady(preparation)
    fun awaitingPromotion(preparation: CaptureStartAdmission.Preparation): Boolean = preparation.backgroundAudio &&
        policy.current()?.preparation === preparation && capture.owns(preparation) && !policy.ready(preparation) &&
        notificationsGranted() && microphoneGranted()
    fun enterForeground(client: Any) { clients.enter(client); revalidateAuthority() }
    fun leaveForeground(client: Any): CaptureForegroundClients.Departure {
        val value = policy.current()
        val allowed = value != null && notificationsGranted() && microphoneGranted() && policy.ready(value.preparation) && capture.continuesInBackground(value.preparation)
        return clients.leave(client, allowed)
    }
    fun revalidateAuthority() {
        V4vAppOwner.existing()?.revalidateAuthority()
        capture.revalidatePublishingAuthority()
    }
    /** Service calls on entry and its one coalesced cold tick, including first wake after sleep. */
    fun check(value: BackgroundAudioPolicy.Owner): Boolean {
        revalidateAuthority()
        if (!owns(value)) return false
        if (!notificationsGranted() || !microphoneGranted() ||
            !clients.present() && !capture.continuesInBackground(value.preparation)) {
            stop(value, "Background capture stopped; permissions or active ownership changed.")
        }
        return owns(value) // Retain the service through actual recorder/publisher cleanup.
    }
    fun stop(value: BackgroundAudioPolicy.Owner, reason: String) {
        if (policy.current() !== value) return
        stopPreparation(value.preparation, reason)
    }
    fun serviceDestroyed(value: BackgroundAudioPolicy.Owner) {
        if (policy.current() === value && capture.serviceInterrupted(value.preparation,
                "Background service ended unexpectedly; recording may be incomplete. Prepare again.")) V4vAppOwner.existing()?.stopPublisher()
        policy.retire(value)
    }
    fun retired(value: BackgroundAudioPolicy.Owner) { policy.retire(value) }
    companion object {
        // Private construction receives context.applicationContext below, so
        // this singleton's Context field cannot retain an Activity.
        @field:SuppressLint("StaticFieldLeak")
        @Volatile private var instance: AudioBroadcastOwner? = null
        fun application(context: Context): AudioBroadcastOwner = instance ?: synchronized(this) {
            instance ?: AudioBroadcastOwner(context.applicationContext).also { instance = it }
        }
        fun existing(): AudioBroadcastOwner? = instance
    }
}
