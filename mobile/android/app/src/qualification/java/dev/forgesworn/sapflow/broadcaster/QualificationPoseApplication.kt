package dev.forgesworn.sapflow.broadcaster

import android.Manifest
import android.app.Activity
import android.app.Application
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.util.Log
import androidx.core.content.ContextCompat

/** No manifest receiver: an intent cannot start this app or unlock a device.
 * DUMP restricts this finite qualification control to shell/privileged senders. */
class QualificationPoseApplication : Application(), Application.ActivityLifecycleCallbacks {
    private var owner: MainActivity? = null
    private var receiver: BroadcastReceiver? = null

    override fun onCreate() {
        super.onCreate()
        if (applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0 && applicationInfo.isProfileableByShell) {
            registerActivityLifecycleCallbacks(this)
        }
    }

    override fun onActivityStarted(activity: Activity) {
        if (activity !is MainActivity) return
        detach()
        owner = activity
        activity.enableQualificationPoseControl(true)
        val attached = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (owner !== activity || receiver !== this || intent.action != ACTION) return
                val result = try {
                    if (intent.`package` != packageName || intent.data != null || intent.clipData != null || intent.extras?.keySet() != setOf("pose")) {
                        PoseInputPolicy.Result.INVALID
                    } else activity.requestQualificationPose(intent.getStringExtra("pose")?.takeIf { it.length <= 24 })
                } catch (_: RuntimeException) { PoseInputPolicy.Result.INVALID }
                resultCode = result.ordinal
                resultData = result.name
                Log.i("SapflowPoseQualification", "result=${result.name}")
            }
        }
        receiver = attached
        try {
            ContextCompat.registerReceiver(this, attached, IntentFilter(ACTION), Manifest.permission.DUMP, null, ContextCompat.RECEIVER_EXPORTED)
        } catch (_: RuntimeException) { detach() }
    }

    private fun detach() {
        owner?.enableQualificationPoseControl(false)
        owner = null
        receiver?.let { runCatching { unregisterReceiver(it) } }
        receiver = null
    }

    override fun onActivityStopped(activity: Activity) { if (owner === activity) detach() }
    override fun onActivityDestroyed(activity: Activity) { if (owner === activity) detach() }
    override fun onActivityCreated(activity: Activity, state: Bundle?) = Unit
    override fun onActivityResumed(activity: Activity) = Unit
    override fun onActivityPaused(activity: Activity) = Unit
    override fun onActivitySaveInstanceState(activity: Activity, state: Bundle) = Unit

    companion object {
        private const val ACTION = "dev.forgesworn.sapflow.broadcaster.QUALIFICATION_POSE"
    }
}
