package dev.forgesworn.sapflow.broadcaster

import java.util.Base64

/** A bounded data envelope only. No URI resolution, network or profile authority. */
internal object ManualDestinationEnvelope {
    const val PREFIX = "sapflow-manual://import/v1#"
    const val MAX_URL_BYTES = 24_576
    const val MAX_DOCUMENT_BYTES = 16_384

    fun decode(text: String): ByteArray {
        var decoded: ByteArray? = null
        try {
            require(text.length in (PREFIX.length + 1)..MAX_URL_BYTES && text.all { it.code in 0..127 })
            require(text.startsWith(PREFIX))
            val payload = text.substring(PREFIX.length)
            require(payload.length % 4 != 1 && payload.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '-' || it == '_' })
            // Check decoded size before the decoder allocates its result.
            require(payload.length.toLong() * 3 / 4 <= MAX_DOCUMENT_BYTES)
            decoded = Base64.getUrlDecoder().decode(payload)
            require(decoded.size in 1..MAX_DOCUMENT_BYTES)
            require(Base64.getUrlEncoder().withoutPadding().encodeToString(decoded) == payload)
            return decoded
        } catch (_: Exception) {
            decoded?.fill(0)
            throw ManualProfileFailure()
        }
    }
}

/** Process capacity one, with no Activity, URI, credentials or approval in saved state.
 * All production callers use the main thread; synchronisation also makes exact
 * invitation ownership explicit to tests. An invitation never grants Apply. */
internal class ManualLinkInbox {
    class Invitation internal constructor(internal val client: Any, internal val text: String) {
        internal var ready = false
        override fun toString() = "Manual destination invitation (redacted)"
    }
    private var pending: Invitation? = null
    private var blockedReturnClient: Any? = null

    @Synchronized fun offer(client: Any, text: String, allowed: Boolean, newDelivery: Boolean = true): Invitation? {
        if (!newDelivery || !allowed || pending != null || blockedReturnClient === client) return null
        // Reject invalid envelope syntax before retaining secret-bearing text.
        val bytes = try { ManualDestinationEnvelope.decode(text) } catch (_: ManualProfileFailure) { return null }
        bytes.fill(0)
        return Invitation(client, text).also { pending = it }
    }
    @Synchronized fun foreground(client: Any, allowed: Boolean): Invitation? {
        if (blockedReturnClient === client) blockedReturnClient = null
        val selected = pending ?: return null
        if (selected.client !== client) return null
        if (!allowed) { pending = null; return null }
        selected.ready = true
        return selected
    }
    @Synchronized fun consume(client: Any, selected: Invitation, allowed: Boolean): String? {
        if (pending !== selected || selected.client !== client) return null
        pending = null
        return if (selected.ready && allowed) selected.text else null
    }
    // Android pauses the resumed Activity before delivering onNewIntent. A
    // second delivery must not replace a review that this very pause invalidated.
    @Synchronized fun pause(client: Any, conflictingReview: Boolean) {
        if (pending?.client === client) pending = null
        if (conflictingReview) blockedReturnClient = client
    }
    @Synchronized fun stopped(client: Any) {
        if (pending?.client === client) pending = null
        if (blockedReturnClient === client) blockedReturnClient = null
    }
    @Synchronized fun invalidate(client: Any) {
        if (pending?.client === client) pending = null
    }
    companion object { val application = ManualLinkInbox() }
}
