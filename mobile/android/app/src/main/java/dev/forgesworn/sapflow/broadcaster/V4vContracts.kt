package dev.forgesworn.sapflow.broadcaster

import java.net.URI
import kotlinx.serialization.json.*

/** All connector endpoints originate in reviewed build configuration, never an Intent or UI URL. */
internal class V4vProfile private constructor(val apiOrigin: String, val playbackOrigin: String, val playbackPrefix: String) {
    companion object {
        fun approved(api: String, playback: String, prefix: String): V4vProfile? {
            if (api.isEmpty() && playback.isEmpty() && prefix.isEmpty()) return null
            fun origin(value: String): String {
                val u = URI(value)
                require(u.scheme == "https" && u.host != null && u.rawUserInfo == null && u.rawQuery == null && u.rawFragment == null && u.rawPath in listOf("", "/")) { "Invalid approved connector origin" }
                require(u.port == -1 || u.port in 1..65535)
                return value.trimEnd('/')
            }
            require(prefix.startsWith('/') && prefix.endsWith('/') && !prefix.contains("..") && !prefix.contains("//") && !prefix.contains('%') && prefix.matches(Regex("/[A-Za-z0-9/_-]*")))
            return V4vProfile(origin(api), origin(playback), prefix)
        }
    }
    override fun toString() = "Approved V4V connector"
}

internal object V4vJson {
    const val MAX_BODY = 64 * 1024
    const val MAX_SAFE = 9_007_199_254_740_991L
    const val MAX_EPOCH = 253_402_300_799_999L
    val json = Json { isLenient = false; allowSpecialFloatingPointValues = false }
    fun parse(text: String, max: Int = MAX_BODY): JsonObject {
        require(text.toByteArray(Charsets.UTF_8).size <= max) { "Connector response exceeds bound" }
        // The wire is shallow. Bound nesting before recursive parsing, honouring JSON strings.
        data class Frame(val objectKeys: MutableSet<String>?, var keyExpected: Boolean)
        val stack = ArrayDeque<Frame>()
        var quoted = false; var escaped = false; var stringStart = 0
        for ((index, c) in text.withIndex()) {
            if (quoted) {
                if (escaped) escaped = false
                else if (c == '\\') escaped = true
                else if (c == '"') {
                    quoted = false
                    stack.lastOrNull()?.takeIf { it.keyExpected && it.objectKeys != null }?.let { frame ->
                        val key = json.parseToJsonElement(text.substring(stringStart, index + 1)).jsonPrimitive.content
                        require(checkNotNull(frame.objectKeys).add(key)) { "Duplicate connector JSON key" }
                        frame.keyExpected = false
                    }
                }
            } else when (c) {
                '"' -> { quoted = true; stringStart = index }
                '{' -> { stack.addLast(Frame(mutableSetOf(), true)); require(stack.size <= 16) }
                '[' -> { stack.addLast(Frame(null, false)); require(stack.size <= 16) }
                '}', ']' -> { require(stack.isNotEmpty()); stack.removeLast() }
                ',' -> stack.lastOrNull()?.let { it.keyExpected = it.objectKeys != null }
            }
        }
        require(!quoted && stack.isEmpty())
        return json.parseToJsonElement(text).jsonObject
    }
    fun JsonObject.text(key: String, max: Int = 4096): String = getValue(key).jsonPrimitive.let {
        require(it.isString); it.content.also { s -> require(s.length <= max && !s.contains('\u0000')) }
    }
    fun JsonObject.number(key: String, min: Long = 0, max: Long = MAX_SAFE): Long = getValue(key).jsonPrimitive.let {
        require(!it.isString && it.content.matches(Regex("0|[1-9][0-9]*"))); it.long.also { n -> require(n in min..max) }
    }
    fun JsonObject.flag(key: String) = getValue(key).jsonPrimitive.let { require(!it.isString); it.boolean }
    fun event(value: String) = value.also { require(it.matches(Regex("event-[a-z0-9-]{1,122}"))) }
    fun id(value: String) = value.also { require(it.matches(Regex("[0-9a-f]{32}"))) }
}

internal data class V4vIdentity(val eventId: String, val generation: Long, val sessionId: String, val revision: Long) {
    init { V4vJson.event(eventId); V4vJson.id(sessionId); require(generation in 1..V4vJson.MAX_SAFE && revision in 1..V4vJson.MAX_SAFE) }
    fun sameSession(other: V4vIdentity) = eventId == other.eventId && generation == other.generation && sessionId == other.sessionId
    companion object {
        fun read(o: JsonObject): V4vIdentity = with(V4vJson) {
            require(o.number("contractVersion", 1, 1) == 1L)
            V4vIdentity(o.text("eventId"), o.number("eventGeneration", 1), o.text("sessionId"), o.number("revision", 1))
        }
    }
}
internal class V4vGrant(val identity: V4vIdentity, val endpoint: String, val streamId: String, val passphrase: String,
    val expiresMs: Long, val refreshMs: Long, val graceMs: Long, val maxPublishBps: Long, val maxQueueBytes: Long, val expiresAtMonotonicNs: Long) {
    override fun toString() = "Scoped V4V grant (redacted)"
    fun permitsStartOf(prepared: V4vGrant, nowNs: Long): Boolean =
        identity.sameSession(prepared.identity) && identity.revision >= prepared.identity.revision && expiresAtMonotonicNs > nowNs &&
        endpoint == prepared.endpoint && streamId == prepared.streamId && passphrase == prepared.passphrase &&
        graceMs == prepared.graceMs && maxPublishBps == prepared.maxPublishBps && maxQueueBytes == prepared.maxQueueBytes
    companion object {
        // Parse the negotiated ceiling independently of local quality selection.
        fun read(o: JsonObject, nowMs: Long, authorityNowNs: Long = System.nanoTime()): V4vGrant = with(V4vJson) {
            val identity = V4vIdentity.read(o)
            val policy = o.getValue("destination").jsonObject
            require(policy.text("kind") == "v4v" && policy.text("connector") == "sapflow" && policy.flag("unlimited") && !policy.flag("watermark") && !policy.flag("subscriptionRequired"))
            require(o.text("transport") == "srt")
            val endpoint = o.text("endpoint", 256); SrtDestination.parse(endpoint)
            val streamId = o.text("streamId", 512)
            require(streamId.matches(Regex("publish:live/${identity.eventId}:${identity.sessionId}:[A-Za-z0-9_-]{43}")))
            val encryption = o.getValue("encryption").jsonObject
            require(encryption.text("cipher") == "aes256-ctr")
            val passphrase = encryption.text("passphrase", 79)
            require(passphrase.matches(Regex("[A-Za-z0-9_-]{43}")))
            require(streamId.substringAfterLast(':') != passphrase)
            val expires = o.number("expiresAtUnixMs", 1, MAX_EPOCH)
            val refresh = o.number("refreshAfterUnixMs", 1, MAX_EPOCH)
            require(expires - nowMs <= 900_000 && refresh < expires)
            val limits = o.getValue("limits").jsonObject
            val bps = limits.number("maxPublishBps", 1)
            V4vGrant(identity, endpoint, streamId, passphrase, expires, refresh, o.number("reconnectGraceMs", 0, 3_600_000), bps, limits.number("maxQueueBytes", 1500, 4_194_304), Math.addExact(authorityNowNs, Math.multiplyExact((expires - nowMs).coerceAtLeast(0), 1_000_000)))
        }
    }
}

internal data class V4vSessionReceipt(val identity: V4vIdentity, val phase: String, val expiresMs: Long,
    val endComplete: Boolean, val recoveryState: String?, val acknowledgementKey: String?, val raw: JsonObject) {
    companion object {
        fun read(o: JsonObject): V4vSessionReceipt = with(V4vJson) {
            val identity = V4vIdentity.read(o)
            val phase = o.text("phase", 24)
            require(phase in setOf("preparing", "active", "refreshing", "unconfirmed", "ending", "ended", "expired", "revoked"))
            val recovery = (o["recovery"] as? JsonObject)
            val state = recovery?.text("state", 24)?.also { require(it in setOf("acknowledging", "acknowledged", "consumed")) }
            val key = recovery?.text("acknowledgementKey", 32)?.let(::id)
            recovery?.let { require(it.number("eventGeneration", 1) == identity.generation && it.number("revision", 1) == identity.revision && it.flag("explicitPrepareRequired")) }
            val complete = o.flag("endComplete")
            if (complete) {
                require(recovery == null)
                val origin = o.getValue("origin").jsonObject
                require(V4vIdentity.read(origin) == identity)
                require(origin.text("admission") == "denied" && origin.text("protocol") in setOf("none", "stopped") && origin.text("recording") in setOf("never-started", "finalised") && origin.text("callbacks") in setOf("disabled", "none", "delivered"))
            }
            V4vSessionReceipt(identity, phase, o.number("expiresAtUnixMs", 1, MAX_EPOCH), complete, state, key, o)
        }
    }
}

internal enum class V4vOperationKind { PREPARE, REFRESH, END, ACKNOWLEDGE }
internal data class V4vOperation(val kind: V4vOperationKind, val eventId: String, val sessionId: String?, val body: JsonObject) {
    init {
        V4vJson.event(eventId); sessionId?.let(V4vJson::id)
        with(V4vJson) {
            id(body.text("idempotencyKey", 32)); require(body.toString().toByteArray().size <= 4096)
            val fields = when (kind) {
                V4vOperationKind.PREPARE -> {
                    require(sessionId == null); body.number("expectedEventGeneration")
                    (body["replaces"] as? JsonObject)?.let { old ->
                        require(old.keys == setOf("sessionId", "revision", "acknowledgementKey")); id(old.text("sessionId")); old.number("revision", 1); id(old.text("acknowledgementKey"))
                    }
                    setOf("expectedEventGeneration", "idempotencyKey", "replaces")
                }
                V4vOperationKind.ACKNOWLEDGE -> {
                    require(sessionId != null); body.number("eventGeneration", 1); body.number("revision", 1)
                    require(body.text("acknowledgement") == "abandon-automatic-recording")
                    setOf("eventGeneration", "revision", "idempotencyKey", "acknowledgement")
                }
                else -> { require(sessionId != null); body.number("revision", 1); setOf("revision", "idempotencyKey") }
            }
            require(body.keys.all { it in fields })
        }
    }
    val path: String get() = "/api/live-events/$eventId/native-session" + when (kind) {
        V4vOperationKind.PREPARE -> ""
        V4vOperationKind.REFRESH -> "/${requireNotNull(sessionId)}/refresh"
        V4vOperationKind.END -> "/${requireNotNull(sessionId)}/end"
        V4vOperationKind.ACKNOWLEDGE -> "/${requireNotNull(sessionId)}/recovery-ack"
    }
    override fun toString() = "V4V ${kind.name} operation (redacted)"
}

internal class V4vHttpFailure(val status: Int = 0) : Exception(when (status) {
    401 -> "V4V authentication expired; sign in before preparing capture"
    403 -> "V4V refused this account or event permission"
    409 -> "V4V session changed; reconcile the saved operation"
    429 -> "V4V request limit reached"
    else -> "V4V request was not confirmed"
})
internal interface V4vApi : AutoCloseable {
    fun request(method: String, path: String, bearer: String?, body: JsonObject? = null): JsonObject
    override fun close() {}
}

