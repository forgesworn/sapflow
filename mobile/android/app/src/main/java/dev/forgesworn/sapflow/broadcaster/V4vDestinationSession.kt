package dev.forgesworn.sapflow.broadcaster

import java.security.SecureRandom
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

internal data class V4vView(val message: String, val eventId: String?, val phase: String?, val signedIn: Boolean,
    val pending: Boolean, val endComplete: Boolean, val recoveryState: String?, val prepared: Boolean, val recoveryOffered: Boolean = false)

/** Single-owner transaction engine. Tests inject clock, API and durable store.
 * A save failure permanently fences this instance; an uncertain HTTP result retains exactly its saved operation. */
internal class V4vSessionEngine(private val profile: V4vProfile, private val api: V4vApi,
    private val store: V4vJournalStore, private val clock: () -> Long, private val nonce: () -> String,
    private val fencePublisher: () -> Unit, private val committedGrant: (V4vGrant) -> Unit = {}) {
    private var document: JsonObject = store.load()?.also(::validateDocument) ?: buildJsonObject {
        put("version", 1); put("apiOrigin", profile.apiOrigin); put("clockFloorMs", 0)
    }
    private val clockOffset = (with(V4vJson) { document.number("clockFloorMs", 0, MAX_EPOCH) } - clock()).coerceAtLeast(0)
    private var restoredRequiresRefresh = document["session"] is JsonObject
    private var unusable = false
    private var stopped = false
    private var failures = 0
    private var retryAt = Long.MAX_VALUE
    private var statusAt = Long.MAX_VALUE
    var message = "Sign in to the approved V4V connector"; private set
    private val now: Long get() = maxOf(clock() + clockOffset, with(V4vJson) { document.number("clockFloorMs", 0, MAX_EPOCH) })
    private val bearer: String? get() = (document["bearer"] as? JsonPrimitive)?.content
    private val bearerExpiry: Long get() = (document["bearerExpiresMs"] as? JsonPrimitive)?.long ?: 0
    private val session: V4vSessionReceipt? get() = (document["session"] as? JsonObject)?.let(V4vSessionReceipt::read)
    private val terminalIntent: Boolean get() = (document["terminalIntent"] as? JsonPrimitive)?.booleanOrNull == true
    private fun operation(): V4vOperation? = (document["pending"] as? JsonObject)?.let { with(V4vJson) {
        V4vOperation(V4vOperationKind.valueOf(it.text("kind")), it.text("eventId"), (it["sessionId"] as? JsonPrimitive)?.content, it.getValue("body").jsonObject)
    } }
    private fun validateDocument(d: JsonObject) = with(V4vJson) {
        require(d.number("version", 1, 1) == 1L && d.text("apiOrigin") == profile.apiOrigin)
        d.number("clockFloorMs", 0, MAX_EPOCH)
        (d["session"] as? JsonObject)?.let(V4vSessionReceipt::read)
        (d["bearer"] as? JsonPrimitive)?.let { require(it.isString && it.content.length in 1..8192); d.number("bearerExpiresMs", 1, MAX_EPOCH); require(d.text("pubkey", 64).matches(Regex("[0-9a-f]{64}"))) }
        (d["pending"] as? JsonObject)?.let { V4vOperation(V4vOperationKind.valueOf(it.text("kind")), it.text("eventId"), (it["sessionId"] as? JsonPrimitive)?.content, it.getValue("body").jsonObject) }
    }
    private fun save(vararg fields: Pair<String, JsonElement?>) {
        check(!unusable && !stopped)
        val entries = document.toMutableMap()
        entries["clockFloorMs"] = JsonPrimitive(now)
        for ((key, value) in fields) if (value == null) entries.remove(key) else entries[key] = value
        val next = JsonObject(entries)
        try { store.save(next); document = next }
        catch (error: Exception) { unusable = true; fencePublisher(); message = "V4V journal unavailable; publishing fenced"; throw error }
    }
    private fun authenticated(): String {
        check(!unusable && !stopped)
        check(bearer != null && now < bearerExpiry) { "V4V sign-in required before capture" }
        return checkNotNull(bearer)
    }
    fun installLogin(pubkey: String, token: String, maxAgeSeconds: Long) {
        require(pubkey.matches(Regex("[0-9a-f]{64}")) && token.length in 1..8192 && !token.contains('\r') && !token.contains('\n'))
        require(maxAgeSeconds in 1..86_400)
        val old = (document["pubkey"] as? JsonPrimitive)?.content
        check(old == null || old == pubkey || (session == null && operation() == null)) { "Reconcile the saved session before changing account" }
        save("pubkey" to JsonPrimitive(pubkey), "bearer" to JsonPrimitive(token), "bearerExpiresMs" to JsonPrimitive(now + maxAgeSeconds * 1000))
        message = "Signed in · explicitly prepare an event"
    }
    fun signedIn() = !unusable && !stopped && bearer != null && now < bearerExpiry
    fun listEvents(): List<Pair<String, String>> {
        val result = api.request("GET", "/api/live-events", authenticated())
        val events = result.getValue("events").jsonArray; require(events.size <= 128) { "V4V event list exceeds supported bound" }
        return events.map { with(V4vJson) { val o = it.jsonObject; event(o.text("id")) to o.text("title", 160) } }
    }
    fun prepare(eventId: String, replaceAcknowledged: Boolean = false) {
        authenticated(); V4vJson.event(eventId); check(operation() == null)
        val old = session
        if (old != null) check(old.endComplete || (replaceAcknowledged && old.recoveryState == "acknowledged")) { "End or explicitly acknowledge the previous failure first" }
        val current = api.request("GET", "/api/live-events/$eventId/native-session", authenticated()).getValue("session").jsonObject
        val generation = with(V4vJson) { require(current.text("eventId") == eventId); current.number("eventGeneration") }
        val body = buildJsonObject {
            put("expectedEventGeneration", generation); put("idempotencyKey", nonce())
            if (replaceAcknowledged) {
                require(old != null && old.identity.eventId == eventId && old.identity.generation == generation && old.recoveryState == "acknowledged")
                put("replaces", buildJsonObject { put("sessionId", old.identity.sessionId); put("revision", old.identity.revision); put("acknowledgementKey", requireNotNull(old.acknowledgementKey)) })
            }
        }
        begin(V4vOperation(V4vOperationKind.PREPARE, eventId, null, body), false)
    }
    fun refresh() {
        authenticated(); check(!terminalIntent && operation() == null)
        val old = checkNotNull(session); check(old.phase == "active" && old.expiresMs > now)
        begin(V4vOperation(V4vOperationKind.REFRESH, old.identity.eventId, old.identity.sessionId,
            buildJsonObject { put("revision", old.identity.revision); put("idempotencyKey", nonce()) }), false)
    }
    fun end() {
        // Fence first even when the network/auth/journal is unavailable.
        fencePublisher()
        check(!unusable && !stopped)
        save("terminalIntent" to JsonPrimitive(true))
        // Resolve an uncertain prior mutation by its exact saved request before making End at its acknowledged revision.
        operation()?.let { retry(); if (operation() != null) return }
        val old = session ?: return
        if (old.endComplete || old.phase in setOf("ending", "ended", "revoked")) { statusAt = now; return }
        begin(V4vOperation(V4vOperationKind.END, old.identity.eventId, old.identity.sessionId,
            buildJsonObject { put("revision", old.identity.revision); put("idempotencyKey", nonce()) }), true)
    }
    fun acknowledge() {
        authenticated(); check(operation() == null)
        val old = checkNotNull(session)
        check(terminalIntent && recoveryOffered(old)) { "Origin has not confirmed a safe recording-failure acknowledgement" }
        begin(V4vOperation(V4vOperationKind.ACKNOWLEDGE, old.identity.eventId, old.identity.sessionId, buildJsonObject {
            put("eventGeneration", old.identity.generation); put("revision", old.identity.revision); put("idempotencyKey", nonce()); put("acknowledgement", "abandon-automatic-recording")
        }), true)
    }
    private fun begin(op: V4vOperation, ending: Boolean) {
        save("pending" to buildJsonObject { put("kind", op.kind.name); put("eventId", op.eventId); op.sessionId?.let { put("sessionId", it) }; put("body", op.body) },
            "terminalIntent" to JsonPrimitive(ending || (terminalIntent && op.kind != V4vOperationKind.PREPARE)))
        failures = 0; retryAt = now; retry()
    }
    fun retry() {
        val op = operation() ?: return
        try {
            val answer = api.request("POST", op.path, authenticated(), op.body)
            require(with(V4vJson) { answer.flag("ok") })
            val receipt = V4vSessionReceipt.read(answer.getValue("session").jsonObject)
            require(receipt.identity.eventId == op.eventId && (op.sessionId == null || receipt.identity.sessionId == op.sessionId))
            val prior = session
            if (prior != null && op.kind != V4vOperationKind.PREPARE) require(receipt.identity.sameSession(prior.identity) && receipt.identity.revision >= prior.identity.revision)
            val grantJson = answer["grant"] as? JsonObject
            if (grantJson != null) {
                val grant = V4vGrant.read(grantJson, now); require(grant.identity == receipt.identity)
                (document["grant"] as? JsonObject)?.takeIf { op.kind == V4vOperationKind.REFRESH }?.let { old ->
                    require(with(V4vJson) { grantJson.number("expiresAtUnixMs") >= old.number("expiresAtUnixMs") })
                    for (key in listOf("endpoint", "streamId", "encryption", "limits", "reconnectGraceMs", "destination", "transport")) require(old[key] == grantJson[key])
                }
            }
            save("session" to receipt.raw, "pending" to null, "grant" to if (terminalIntent || op.kind in setOf(V4vOperationKind.END, V4vOperationKind.ACKNOWLEDGE)) null else grantJson)
            if (grantJson != null && !terminalIntent && op.kind in setOf(V4vOperationKind.PREPARE, V4vOperationKind.REFRESH)) {
                restoredRequiresRefresh = false
                committedGrant(V4vGrant.read(grantJson, now))
            }
            retryAt = Long.MAX_VALUE; failures = 0; statusAt = now + 5000
            message = when { grantJson != null && !terminalIntent && with(V4vJson) { grantJson.getValue("limits").jsonObject.number("maxPublishBps") } < V4vGrant.REQUIRED_PUBLISH_BPS -> "V4V session reserved but cap is insufficient for fixed 720p (requires 4 Mbps)"; receipt.endComplete -> "V4V End completed"; receipt.recoveryState != null -> "Recording failure acknowledged · explicit Prepare required"; terminalIntent -> "V4V End pending confirmation"; grantJson != null -> "V4V grant prepared · explicit capture Start required"; else -> "V4V operation confirmed without a publishing grant" }
            if (terminalIntent && op.kind !in setOf(V4vOperationKind.END, V4vOperationKind.ACKNOWLEDGE) && !receipt.endComplete) end()
        } catch (error: Exception) {
            if (unusable) throw error
            failures = (failures + 1).coerceAtMost(6)
            retryAt = if (error is V4vHttpFailure && error.status in setOf(401, 403, 409)) Long.MAX_VALUE else now + minOf(30_000L, 1000L shl failures)
            message = if (error is V4vHttpFailure) error.message.orEmpty() else "V4V operation unconfirmed; saved request retained"
        }
    }
    fun status() {
        val old = session ?: return
        val answer = api.request("GET", "/api/live-events/${old.identity.eventId}/native-session/${old.identity.sessionId}", authenticated())
        val receipt = V4vSessionReceipt.read(answer.getValue("session").jsonObject)
        require(receipt.identity.sameSession(old.identity) && receipt.identity.revision >= old.identity.revision)
        // Status cannot acknowledge an uncertain mutation or recover lost grant credentials.
        if (operation() == null) save("session" to receipt.raw)
        if (receipt.phase in setOf("ending", "ended", "expired", "revoked")) { fencePublisher(); save("terminalIntent" to JsonPrimitive(true), "grant" to null) }
        message = when { receipt.endComplete -> "V4V End completed"; receipt.recoveryState != null -> "Recording failure acknowledged · explicit Prepare required"; terminalIntent -> "V4V End/finalisation not complete"; else -> "V4V origin status received · decoded media unconfirmed" }
        statusAt = now + 5000
    }
    fun captureGrant(): V4vGrant {
        authenticated(); check(!restoredRequiresRefresh) { "Refresh the restored V4V grant before capture" }; check(!terminalIntent && operation() == null)
        val grant = V4vGrant.read(checkNotNull(document["grant"] as? JsonObject), now)
        require(session?.identity == grant.identity && session?.phase == "active" && grant.expiresMs > now) { "V4V grant expired" }
        require(grant.maxPublishBps >= V4vGrant.REQUIRED_PUBLISH_BPS) { "V4V cap is insufficient for fixed 720p capture (requires 4 Mbps)" }
        return grant
    }
    fun tick() {
        check(!unusable && !stopped)
        val old = session
        if (old?.endComplete == true && operation() == null) { statusAt = Long.MAX_VALUE; return }
        if (old != null && now >= old.expiresMs && document["grant"] is JsonObject) { fencePublisher(); message = "V4V grant expired · capture/recording may continue"; save("grant" to null) }
        if (!signedIn()) { message = "V4V sign-in required · no signer will open during capture"; return }
        if (operation() != null && now >= retryAt) retry()
        else if (restoredRequiresRefresh && operation() == null && !terminalIntent && old?.phase == "active" && now < old.expiresMs) refresh()
        else if (operation() == null && !terminalIntent && (document["grant"] as? JsonObject)?.let { with(V4vJson) { it.number("refreshAfterUnixMs") <= now } } == true && old != null && now < old.expiresMs) refresh()
        else if (old != null && now >= statusAt) try { status() } catch (_: Exception) { statusAt = now + 5000; message = "V4V status unavailable · media unconfirmed" }
    }
    private fun recoveryOffered(value: V4vSessionReceipt?): Boolean {
        if (value == null || value.phase !in setOf("ended", "expired", "revoked") || value.endComplete || value.recoveryState != null) return false
        val origin = value.raw["origin"] as? JsonObject ?: return false
        return with(V4vJson) { origin.text("admission") == "denied" && origin.text("protocol") in setOf("none", "stopped") && (origin.text("recording") in setOf("incomplete", "failed") || origin.text("callbacks") == "failed") }
    }
    fun view() = V4vView(message, session?.identity?.eventId, session?.phase, signedIn(), operation() != null, session?.endComplete == true, session?.recoveryState,
        !terminalIntent && !unusable && !restoredRequiresRefresh && operation() == null && signedIn() && session?.phase == "active" && (session?.expiresMs ?: 0) > now && (document["grant"] as? JsonObject)?.let { with(V4vJson) { it.getValue("limits").jsonObject.number("maxPublishBps") >= V4vGrant.REQUIRED_PUBLISH_BPS } } == true,
        recoveryOffered(session))
    fun loginChallenge(): V4vLoginChallenge = with(V4vJson) {
        val answer = api.request("GET", "/api/auth/nostr/challenge", null)
        require(answer.flag("enabled")) { "V4V login is not enabled for this account" }
        V4vLoginChallenge(answer.text("challenge", 64), profile.apiOrigin, answer.text("appName", 128), now / 1000)
    }
    fun login(event: JsonObject) = with(V4vJson) {
        val answer = api.request("POST", "/api/auth/nostr/login", null, buildJsonObject { put("authEvent", event) })
        val expected = event.text("pubkey", 64)
        require(answer.flag("ok") && answer.text("pubkey", 64) == expected)
        installLogin(expected, answer.text("sessionToken", 8192), answer.number("sessionTokenMaxAgeSec", 1, 86_400))
    }
    fun nextDelayMs(): Long? {
        if (unusable || stopped) return null
        val old = session ?: return if (operation() != null && signedIn()) (retryAt - now).coerceAtLeast(1) else null
        if (old.endComplete && operation() == null) return null
        val times = mutableListOf<Long>()
        if (document["grant"] is JsonObject) times += old.expiresMs
        if (signedIn()) {
            times += bearerExpiry
            if (operation() != null) times += retryAt
            else {
                if (restoredRequiresRefresh && !terminalIntent && old.phase == "active") times += now
                times += statusAt
                if (!terminalIntent) (document["grant"] as? JsonObject)?.let { times += with(V4vJson) { it.number("refreshAfterUnixMs") } }
            }
        }
        return times.filter { it != Long.MAX_VALUE }.minOrNull()?.let { (it - now).coerceAtLeast(100) }
    }
    fun stop() { stopped = true; fencePublisher(); api.close() }
}

/** App-owned executor. No media work, no UI thread HTTP/FS; queue admission is one command. */
internal class V4vDestinationSession(engineFactory: () -> V4vSessionEngine) : AutoCloseable {
    private val executor = ScheduledThreadPoolExecutor(1) { Thread(it, "sapflow-v4v-control") }.apply { removeOnCancelPolicy = true }
    private val closed = AtomicBoolean()
    private val busy = AtomicBoolean()
    private val endRequested = AtomicBoolean()
    private val snapshot = AtomicReference(V4vView("Loading encrypted V4V journal", null, null, false, false, false, null, false))
    private var engine: V4vSessionEngine? = null
    private var alarm: ScheduledFuture<*>? = null
    init { executor.execute { try { engine = engineFactory(); snapshot.set(checkNotNull(engine).view()); arm() } catch (_: Exception) { snapshot.set(V4vView("V4V journal unavailable · connector blocked", null, null, false, false, false, null, false)) } } }
    val view: V4vView get() = snapshot.get()
    fun submit(action: V4vSessionEngine.() -> Unit, completed: (Exception?) -> Unit = {}) {
        if (closed.get() || !busy.compareAndSet(false, true)) { completed(IllegalStateException("V4V control busy")); return }
        executor.execute {
            var error: Exception? = null
            try { if (!closed.get()) checkNotNull(engine) { "V4V control unavailable" }.action() } catch (caught: Exception) { error = caught }
            finally { engine?.let { snapshot.set(it.view()) }; busy.set(false); completed(error); arm() }
        }
    }
    /** One reserved End admission survives an in-flight refresh/status request. */
    fun endDestination(completed: (Exception?) -> Unit = {}) {
        if (closed.get() || !endRequested.compareAndSet(false, true)) return
        executor.execute {
            var error: Exception? = null
            try { checkNotNull(engine).end() } catch (caught: Exception) { error = caught }
            finally { engine?.let { snapshot.set(it.view()) }; endRequested.set(false); completed(error); arm() }
        }
    }
    private fun arm() {
        alarm?.cancel(false)
        if (!closed.get()) engine?.nextDelayMs()?.let { delay -> alarm = executor.schedule({ submit({ tick() }) }, delay, TimeUnit.MILLISECONDS) }
    }
    fun awaitClosed(timeoutMs: Long): Boolean = executor.awaitTermination(timeoutMs, TimeUnit.MILLISECONDS)
    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        alarm?.cancel(false)
        executor.execute { try { engine?.stop() } finally { executor.shutdown() } }
    }
    companion object { fun nonce(): String = ByteArray(16).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it.toInt() and 255) } }
}
