package dev.forgesworn.sapflow.broadcaster

import java.net.URI
import java.nio.ByteBuffer
import java.nio.CharBuffer
import java.nio.charset.CodingErrorAction
import kotlinx.serialization.json.*

internal class ManualProfileFailure : IllegalArgumentException("Manual destination action could not be confirmed. Check saved profiles before retrying.")

/** A manual destination is data only. Parsing/review never constructs a sink. */
internal class ManualDestinationProfile private constructor(
    val name: String, val transport: String, val endpoint: String, val streamKey: String,
    val streamID: String, val passphrase: String, val allowPlaintext: Boolean,
    val review: Review,
) {
    class Review(val transport: String, val host: String, val port: Int, val plaintext: Boolean) {
        val security: String get() = if (plaintext) "Unencrypted" else if (transport == "srt") "AES-256" else "TLS"
        override fun toString() = "Manual destination review (redacted)"
    }
    override fun toString() = "Manual destination (redacted)"
    fun document(): JsonObject = buildJsonObject {
        put("version", 1); put("kind", "sapflow-manual-destination"); put("name", name); put("transport", transport)
        put("endpoint", endpoint); put("streamKey", streamKey); put("streamID", streamID); put("passphrase", passphrase); put("allowPlaintext", allowPlaintext)
    }
    companion object {
        const val MAX_BYTES = 16_384
        private val fields = setOf("version", "kind", "name", "transport", "endpoint", "streamKey", "streamID", "passphrase", "allowPlaintext")
        private fun controls(value: String) = value.codePoints().anyMatch { Character.isISOControl(it) || Character.getType(it) == Character.FORMAT.toInt() }
        private fun text(o: JsonObject, key: String, maximum: Int): String {
            val p = o.getValue(key).jsonPrimitive
            require(p.isString)
            return p.content.also {
                val encoded = Charsets.UTF_8.newEncoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).encode(CharBuffer.wrap(it))
                require(encoded.remaining() <= maximum && !controls(it))
            }
        }
        fun parse(bytes: ByteArray): ManualDestinationProfile = try {
            require(bytes.isNotEmpty() && bytes.size <= MAX_BYTES)
            val decoded = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
            val o = V4vJson.parse(decoded, MAX_BYTES)
            require(o.keys == fields)
            val version = o.getValue("version").jsonPrimitive
            require(!version.isString && version.content == "1")
            require(text(o, "kind", 64) == "sapflow-manual-destination")
            val name = o.getValue("name").jsonPrimitive.let { require(it.isString); it.content.trim(' ') }
            require(name.length in 1..48 && name.all { it.code in 32..126 })
            val transport = text(o, "transport", 4)
            val endpoint = text(o, "endpoint", 4096)
            require(endpoint == endpoint.trim())
            val key = text(o, "streamKey", 4096)
            val streamID = text(o, "streamID", 512)
            val passphrase = text(o, "passphrase", 79)
            require(passphrase.all { it.code in 32..126 })
            val flag = o.getValue("allowPlaintext").jsonPrimitive
            require(!flag.isString && flag.content in setOf("true", "false"))
            val plaintext = flag.boolean
            val review = if (transport == "rtmp") {
                require(streamID.isEmpty() && passphrase.isEmpty() && key.isNotEmpty())
                val uri = URI(endpoint)
                val authority = requireNotNull(uri.rawAuthority)
                // An explicitly empty/overflowing port must never choose a default.
                val portText = if (authority.startsWith('[')) authority.substringAfter(']', "").let { if (it.isEmpty()) null else { require(it.startsWith(':')); it.substring(1) } }
                    else if (authority.contains(':')) authority.substringAfterLast(':') else null
                if (portText != null) require(portText.matches(Regex("[0-9]+")) && portText.toIntOrNull() in 1..65535)
                require(uri.path.split('/').none { it == "." || it == ".." } && !controls(uri.path))
                val destination = RtmpDestination.parse(endpoint, plaintext)
                require(destination.host.toByteArray().size in 1..253)
                Review(transport, destination.host, destination.port, !destination.tls)
            } else {
                require(transport == "srt" && key.isEmpty())
                val destination = SrtDestination.parse(endpoint)
                require(destination.address.address.size != 4 || (destination.address.address[0].toInt() and 255) in 1..223)
                SrtDestination.validateCredentials(streamID, passphrase, plaintext)
                Review(transport, destination.address.hostAddress!!, destination.port, plaintext)
            }
            ManualDestinationProfile(name, transport, endpoint, key, streamID, passphrase, plaintext, review)
        } catch (_: Exception) { throw ManualProfileFailure() }
        fun fields(name: String, transport: String, endpoint: String, key: String, streamID: String, passphrase: String, plaintext: Boolean): ManualDestinationProfile =
            parse(buildJsonObject {
                put("version", 1); put("kind", "sapflow-manual-destination"); put("name", name); put("transport", transport)
                put("endpoint", endpoint); put("streamKey", key); put("streamID", streamID); put("passphrase", passphrase); put("allowPlaintext", plaintext)
            }.toString().toByteArray(Charsets.UTF_8))
    }
}
