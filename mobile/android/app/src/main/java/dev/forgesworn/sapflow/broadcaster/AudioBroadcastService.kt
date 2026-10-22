package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Handler
import android.os.Build
import android.os.IBinder
import android.os.Looper

/** No binding, exported entry point, boot receiver, restart, alarm or worker.
 * A delivered Intent never creates a capture controller or reconstructs a show. */
class AudioBroadcastService : Service() {
    private val main = Handler(Looper.getMainLooper())
    // The existing coalesced cold check also renews only the exact eligible
    // owner's timed partial wake lease. No media/UI callback drives renewal.
    private val check: Runnable = Runnable { lifecycle.check() }
    private val lifecycle: AudioBroadcastLifecycle<BackgroundAudioPolicy.Owner> = AudioBroadcastLifecycle(
        object : AudioBroadcastLifecycle.Owners<BackgroundAudioPolicy.Owner> {
            override fun match(id: Long, capability: String?) = AudioBroadcastOwner.existing()?.match(id, capability)
            override fun owns(value: BackgroundAudioPolicy.Owner) = AudioBroadcastOwner.existing()?.owns(value) == true
            override fun revalidateAuthority() { AudioBroadcastOwner.existing()?.revalidateAuthority() }
            override fun promoted(value: BackgroundAudioPolicy.Owner) = AudioBroadcastOwner.existing()?.promoted(value) == true
            override fun check(value: BackgroundAudioPolicy.Owner) = AudioBroadcastOwner.existing()?.check(value) == true
            override fun stop(value: BackgroundAudioPolicy.Owner, reason: String) { AudioBroadcastOwner.existing()?.stop(value, reason) }
            override fun retired(value: BackgroundAudioPolicy.Owner) { AudioBroadcastOwner.existing()?.retired(value) }
            override fun serviceDestroyed(value: BackgroundAudioPolicy.Owner) { AudioBroadcastOwner.existing()?.serviceDestroyed(value) }
        },
        object : AudioBroadcastLifecycle.Platform<BackgroundAudioPolicy.Owner> {
            override fun promote(value: BackgroundAudioPolicy.Owner) {
                val owner = AudioBroadcastOwner.existing() ?: throw SecurityException("Background owner unavailable")
                if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED || !owner.notificationsGranted()) {
                    throw SecurityException("Background microphone permission unavailable")
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    startForeground(NOTIFICATION, notification(value), ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
                } else {
                    // API29 has no microphone foreground-service type constant.
                    startForeground(NOTIFICATION, notification(value))
                }
            }
            override fun removeNotification() { stopForeground(STOP_FOREGROUND_REMOVE) }
            override fun stopService(startId: Int) { stopSelfResult(startId) }
            override fun scheduleCheck(delayMs: Long) {
                main.removeCallbacks(check)
                main.postDelayed(check, delayMs)
            }
            override fun cancelCheck() { main.removeCallbacks(check) }
        },
    )
    override fun onBind(intent: Intent?): IBinder? = null
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val command = when (intent?.action) {
            ARM -> AudioBroadcastLifecycle.Command.ARM
            STOP -> AudioBroadcastLifecycle.Command.STOP
            else -> null
        }
        lifecycle.command(command, intent?.getLongExtra(ID, -1) ?: -1, intent?.getStringExtra(CAPABILITY), startId)
        return START_NOT_STICKY
    }
    private fun notification(value: BackgroundAudioPolicy.Owner): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Microphone capture", NotificationManager.IMPORTANCE_LOW))
        // PendingIntent equivalence ignores extras: the data URI includes the
        // unique random capability, and the explicit non-exported service checks both fields.
        val identity = Uri.Builder().scheme("sapflow-audio").authority("stop")
            .appendPath(value.preparation.id.toString()).appendPath(value.capability).build()
        val stop = command(this, STOP, value).setData(identity)
        val action = PendingIntent.getService(this, 0, stop, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        return Notification.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("Microphone capture active").setContentText("Microphone audio may be broadcast and recorded.")
            .setVisibility(Notification.VISIBILITY_PRIVATE).setOngoing(true).setOnlyAlertOnce(true)
            .setContentIntent(open).addAction(Notification.Action.Builder(null, "Stop and save", action).build()).build()
    }
    override fun onTaskRemoved(rootIntent: Intent?) {
        lifecycle.taskRemoved()
        super.onTaskRemoved(rootIntent)
    }
    override fun onDestroy() {
        lifecycle.destroy()
        super.onDestroy()
    }
    companion object {
        internal const val CHANNEL = "sapflow-microphone"
        private const val NOTIFICATION = 71
        private const val ARM = "dev.forgesworn.sapflow.broadcaster.ARM_AUDIO"
        private const val STOP = "dev.forgesworn.sapflow.broadcaster.STOP_AUDIO"
        private const val ID = "preparation"
        private const val CAPABILITY = "owner"
        private fun command(context: Context, action: String, value: BackgroundAudioPolicy.Owner) =
            Intent(context, AudioBroadcastService::class.java).setAction(action)
                .putExtra(ID, value.preparation.id).putExtra(CAPABILITY, value.capability)
        internal fun arm(context: Context, value: BackgroundAudioPolicy.Owner) {
            context.startForegroundService(command(context, ARM, value))
        }
    }
}
