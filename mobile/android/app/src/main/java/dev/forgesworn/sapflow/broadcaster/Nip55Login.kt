package dev.forgesworn.sapflow.broadcaster

// Signing-only adaptation of KithMoot (MIT, Copyright 2026 TheCryptoDonkey).
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Handler
import android.os.Looper
import kotlinx.serialization.json.*

/** Application-owned one-launch bridge. Timeout/cancelled launches keep a bounded tombstone until their result arrives. */
internal class Nip55LoginRelay {
    private data class Request(val id: String, val answer: (Intent?) -> Unit, var abandoned: Boolean = false)
    private val ui = Handler(Looper.getMainLooper())
    private val launches = ArrayDeque<Request>()
    private var launcher: ((Intent) -> Unit)? = null
    private var active: Request? = null
    fun attach(value: (Intent) -> Unit) { check(Looper.myLooper() == Looper.getMainLooper()); launcher = value }
    fun detach(value: (Intent) -> Unit) { if (launcher === value) launcher = null }
    fun request(intent: Intent, answer: (Intent?) -> Unit) {
        check(Looper.myLooper() == Looper.getMainLooper())
        check(active == null && launches.size < 8) { "Signer request still pending" }
        val launch = checkNotNull(launcher) { "Signer screen unavailable" }
        val item = Request(V4vDestinationSession.nonce(), answer)
        intent.putExtra("id", item.id); launches.addLast(item); active = item
        try { launch(intent) } catch (error: Exception) { launches.remove(item); active = null; throw error }
        ui.postDelayed({ if (active === item) { active = null; item.abandoned = true; item.answer(null) } }, 60_000)
    }
    fun deliver(ok: Boolean, data: Intent?) {
        val echoed = data?.getStringExtra("id")
        val at = if (echoed == null) 0 else launches.indexOfFirst { it.id == echoed }
        if (at !in launches.indices) return
        val item = launches.removeAt(at)
        if (item.abandoned) return
        if (active === item) active = null
        item.answer(if (ok) data else null)
    }
}

internal class Nip55Login(private val context: Context, private val relay: Nip55LoginRelay) {
    fun installed(): List<Pair<String, String>> {
        val probe = Intent(Intent.ACTION_VIEW, Uri.parse("nostrsigner:"))
        @Suppress("DEPRECATION")
        return context.packageManager.queryIntentActivities(probe, PackageManager.MATCH_DEFAULT_ONLY)
            .take(32).map { it.activityInfo.packageName to it.loadLabel(context.packageManager).toString().take(80) }.distinctBy { it.first }
    }
    fun sign(packageName: String, challenge: V4vLoginChallenge, completed: (JsonObject?, Exception?) -> Unit) {
        require(installed().any { it.first == packageName })
        fun failed() = completed(null, IllegalStateException("Signer cancelled or timed out"))
        val permissions = "[{\"type\":\"sign_event\",\"kind\":21236}]"
        val connect = Intent(Intent.ACTION_VIEW, Uri.parse("nostrsigner:")).setPackage(packageName).putExtra("type", "get_public_key").putExtra("permissions", permissions)
        relay.request(connect) first@{ account ->
            if (account == null || account.getBooleanExtra("rejected", false)) { failed(); return@first }
            try {
                val key = (account.getStringExtra("result") ?: account.getStringExtra("signature"))?.lowercase()
                require(key != null && key.matches(Regex("[0-9a-f]{64}"))) { "Signer must return a hexadecimal public key" }
                account.getStringExtra("package")?.let { require(it == packageName) }
                val expected = challenge.unsigned(key)
                val payload = buildJsonObject { for ((k, v) in expected) put(k, v); put("id", SignedLoginEvent.id(expected)) }
                val sign = Intent(Intent.ACTION_VIEW, Uri.Builder().scheme("nostrsigner").opaquePart(payload.toString()).build()).setPackage(packageName)
                    .putExtra("type", "sign_event").putExtra("current_user", key)
                relay.request(sign) second@{ response ->
                    if (response == null || response.getBooleanExtra("rejected", false)) { failed(); return@second }
                    try { completed(SignedLoginEvent.validate(expected, response.getStringExtra("event"), response.getStringExtra("result") ?: response.getStringExtra("signature")), null) }
                    catch (_: Exception) { completed(null, IllegalStateException("Signer returned an invalid login event")) }
                }
            } catch (_: Exception) { completed(null, IllegalStateException("Signer account or request is invalid")) }
        }
    }
}
