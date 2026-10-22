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
    private var lease: BackgroundAudioPolicy.Owner? = null
    private var lastStart = 0
    private val check = object : Runnable {
        override fun run() {
            val value = lease ?: return
            val owner = AudioBroadcastOwner.existing()
            if (owner == null || !owner.check(value)) finish(value)
            else main.postDelayed(this, 1_000)
        }
    }
    override fun onBind(intent: Intent?): IBinder? = null
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        lastStart = startId
        val owner = AudioBroadcastOwner.existing()
        val command = intent
        if (owner == null || command == null) {
            if (lease == null) stopSelfResult(startId)
            return START_NOT_STICKY
        }
        val value = owner.match(command.getLongExtra(ID, -1), command.getStringExtra(CAPABILITY))
        if (value == null) {
            // A mismatched old notification must not stop the active successor.
            if (lease == null) stopSelfResult(startId)
            return START_NOT_STICKY
        }
        owner.revalidateAuthority()
        when (command.action) {
            STOP -> owner.stop(value, "Stopped from the background audio notification; saving recording")
            ARM -> {
                if (!owner.owns(value)) { if (lease == null) stopSelfResult(startId); return START_NOT_STICKY }
                try {
                    if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED || !owner.notificationsGranted()) {
                        throw SecurityException("Background microphone permission unavailable")
                    }
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                        startForeground(NOTIFICATION, notification(value), ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
                    } else {
                        // API29 has no microphone foreground-service type constant.
                        // The notification and pre-granted microphone permission still apply.
                        startForeground(NOTIFICATION, notification(value))
                    }
                    if (!owner.promoted(value)) {
                        owner.stop(value, "Background service admission expired. Prepare again."); owner.retired(value)
                        if (lease == null) { stopForeground(STOP_FOREGROUND_REMOVE); stopSelfResult(startId) }
                        return START_NOT_STICKY
                    }
                    lease = value
                } catch (_: Exception) {
                    owner.stop(value, "Background audio service permission or promotion failed. Prepare again.")
                    owner.retired(value)
                    if (lease == null) { stopForeground(STOP_FOREGROUND_REMOVE); stopSelfResult(startId) }
                    return START_NOT_STICKY
                }
            }
            else -> return START_NOT_STICKY
        }
        main.removeCallbacks(check)
        if (lease != null) main.post(check)
        else stopSelfResult(startId)
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
    private fun finish(value: BackgroundAudioPolicy.Owner) {
        if (lease !== value) return
        main.removeCallbacks(check); lease = null
        AudioBroadcastOwner.existing()?.retired(value)
        stopForeground(STOP_FOREGROUND_REMOVE); stopSelfResult(lastStart)
    }
    override fun onTaskRemoved(rootIntent: Intent?) {
        // Eligible committed owner remains service-owned; all other states are stopped.
        lease?.let { AudioBroadcastOwner.existing()?.check(it) }
        super.onTaskRemoved(rootIntent)
    }
    override fun onDestroy() {
        main.removeCallbacks(check)
        lease?.let { AudioBroadcastOwner.existing()?.serviceDestroyed(it) }; lease = null
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
