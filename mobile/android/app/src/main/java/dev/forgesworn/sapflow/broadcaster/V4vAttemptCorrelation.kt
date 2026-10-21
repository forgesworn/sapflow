package dev.forgesworn.sapflow.broadcaster

import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

/** Correlation metadata only: no admission/encryption authority or decode proof.
 * The SDK identity is captured before this attempt's transport is constructed. */
internal class V4vCapturedAttempt internal constructor(val context: PublishingAttemptContext,
    val identity: V4vIdentity, val nonce: String) {
    override fun toString() = "Captured V4V attempt (redacted)"
}
internal class V4vWireAttempt(val scope: V4vCapturedAttempt, val streamId: String) {
    override fun toString() = "V4V attempt wire credentials (redacted)"
}
internal class V4vMatchedMedia(val scope: V4vCapturedAttempt, val connectionEpoch: Long,
    val partId: Long, val initializationId: Long, val initPath: String, val partPath: String, val hasAudio: Boolean) {
    override fun toString() = "Correlated encoded-media boundary (not decoded)"
}

/** One current scope, plus one previous nonce for fail-closed immediate collision
 * detection. A restored receipt never reconstructs this local SDK scope;
 * the existing journal may retain the origin's visible echo as metadata.
 * Randomness is cold per attempt. */
internal class V4vAttemptCorrelation(private val nonce: () -> String = ::randomNonce) {
    private val current = AtomicReference<V4vCapturedAttempt?>()
    private var previousNonce: String? = null // Serial reconnect owner calls begin.

    fun begin(context: PublishingAttemptContext, grant: V4vGrant): V4vWireAttempt {
        val value = nonce()
        V4vJson.id(value)
        check(value != previousNonce) { "Attempt randomness repeated" }
        val wire = canonicalStreamId(grant, value) // Revalidate before emitting credentials.
        val scope = V4vCapturedAttempt(context, grant.identity, value)
        previousNonce = value
        current.set(scope)
        return V4vWireAttempt(scope, wire)
    }
    fun clear() { current.set(null) }
    fun currentFor(context: PublishingAttemptContext): V4vCapturedAttempt? = current.get()?.takeIf { it.context == context }

    /** A future decoder may use this only with fresh authenticated GET status,
     * before AND after decode, and while the coordinator still exposes scope.
     * A match alone never invokes NativeSession or marks playback confirmed. */
    fun matchFreshMedia(scope: V4vCapturedAttempt, receipt: V4vSessionReceipt): V4vMatchedMedia? = with(V4vJson) {
        if (current.get() !== scope || !scope.identity.sameSession(receipt.identity) || receipt.identity.revision < scope.identity.revision || receipt.phase != "active") return@with null
        val origin = receipt.raw["origin"] as? JsonObject ?: return@with null
        if (V4vIdentity.read(origin) != receipt.identity) return@with null
        val echoed = origin["connectionAttemptId"] ?: return@with null
        if (echoed == JsonNull) return@with null
        val id = origin.text("connectionAttemptId", 32); V4vJson.id(id)
        if (id != scope.nonce) return@with null
        if (origin.text("state", 24) != "live" || origin.text("admission", 24) != "open" || origin.text("protocol", 24) != "active") return@with null
        val epoch = origin.number("connectionEpoch", 1)
        val mediaValue = origin["freshMedia"] ?: return@with null
        if (mediaValue == JsonNull) return@with null
        val media = mediaValue.jsonObject
        require(media.keys == setOf("connectionEpoch", "partId", "initializationId", "initPath", "partPath", "hasVideo", "hasAudio"))
        require(media.number("connectionEpoch", 1) == epoch && media.flag("hasVideo"))
        val part = media.number("partId"); val initialization = media.number("initializationId")
        val initPath = media.text("initPath", 256); val partPath = media.text("partPath", 256)
        val suffix = if (initialization == 0L) "" else initialization.toString()
        val pattern = Regex("/live/${Regex.escape(scope.identity.eventId)}/([0-9a-f]{1,12})_init$suffix\\.mp4")
        val prefix = requireNotNull(pattern.matchEntire(initPath)).groupValues[1]
        require(partPath == "/live/${scope.identity.eventId}/${prefix}_part$part.mp4")
        val result = V4vMatchedMedia(scope, epoch, part, initialization, initPath, partPath, media.flag("hasAudio"))
        if (current.get() === scope) result else null
    }

    companion object {
        private val random = SecureRandom()
        private val hex = "0123456789abcdef".toCharArray()
        private fun randomNonce(): String {
            val bytes = ByteArray(16); random.nextBytes(bytes)
            return CharArray(32) { index ->
                val byte = bytes[index / 2].toInt() and 255
                hex[if (index % 2 == 0) byte ushr 4 else byte and 15]
            }.concatToString()
        }
        /** Original durable colon credentials remain immutable. Only the
         * transport attempt gets this canonical SRT access-control ID. */
        fun canonicalStreamId(grant: V4vGrant, attemptId: String): String {
            V4vJson.id(attemptId)
            val prefix = "publish:live/${grant.identity.eventId}:${grant.identity.sessionId}:"
            require(grant.streamId.startsWith(prefix))
            val password = grant.streamId.removePrefix(prefix)
            require(password.matches(Regex("[A-Za-z0-9_-]{43}")))
            require(grant.passphrase.matches(Regex("[A-Za-z0-9_-]{43}")) && password != grant.passphrase)
            return "#!::m=publish,r=live/${grant.identity.eventId},u=${grant.identity.sessionId},s=$password,forgesworn_attempt=$attemptId"
        }
    }
}
