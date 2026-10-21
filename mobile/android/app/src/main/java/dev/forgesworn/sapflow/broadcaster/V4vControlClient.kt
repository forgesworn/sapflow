package dev.forgesworn.sapflow.broadcaster

import java.io.ByteArrayOutputStream
import java.net.URL
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import javax.net.ssl.HttpsURLConnection
import kotlinx.serialization.json.JsonObject

/** Called only on the control owner. One bounded request; no trust overrides, redirects or global cookies. */
internal class V4vControlClient(private val profile: V4vProfile) : V4vApi {
    private val stopped = AtomicBoolean()
    private val current = AtomicReference<HttpsURLConnection?>()
    private val watchdog = Executors.newSingleThreadScheduledExecutor { Thread(it, "sapflow-v4v-http-deadline").apply { isDaemon = true } }
    override fun request(method: String, path: String, bearer: String?, body: JsonObject?): JsonObject {
        check(!stopped.get())
        require(method in setOf("GET", "POST"))
        require(path.matches(Regex("/api/auth/nostr/(challenge|login)|/api/live-events(?:/event-[a-z0-9-]{1,122}/native-session(?:/[0-9a-f]{32}(?:/(?:refresh|end|recovery-ack))?)?)?")))
        require((method == "POST") == (body != null))
        val bytes = body?.toString()?.toByteArray(Charsets.UTF_8)
        require(bytes == null || bytes.size <= if (path.endsWith("/login")) 16_384 else 4096)
        val connection = URL(profile.apiOrigin + path).openConnection() as HttpsURLConnection
        check(current.compareAndSet(null, connection))
        val expired = AtomicBoolean()
        val timeout = watchdog.schedule({ expired.set(true); connection.disconnect() }, 8, TimeUnit.SECONDS)
        try {
            connection.instanceFollowRedirects = false
            connection.connectTimeout = 3000; connection.readTimeout = 3000
            connection.useCaches = false; connection.requestMethod = method
            connection.setRequestProperty("Accept", "application/json")
            connection.setRequestProperty("Cache-Control", "no-store")
            bearer?.let { require(it.length in 1..8192 && !it.contains('\r') && !it.contains('\n')); connection.setRequestProperty("Authorization", "Bearer $it") }
            if (bytes != null) {
                connection.doOutput = true; connection.setFixedLengthStreamingMode(bytes.size)
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(bytes) }
            }
            val code = connection.responseCode
            if (code != 200) throw V4vHttpFailure(code)
            require(connection.contentType?.substringBefore(';')?.trim()?.lowercase() == "application/json")
            require(connection.contentLengthLong <= V4vJson.MAX_BODY)
            val data = connection.inputStream.use { input ->
                val result = ByteArrayOutputStream(); val buffer = ByteArray(4096)
                while (true) {
                    check(!expired.get() && !stopped.get())
                    val n = input.read(buffer); if (n < 0) break
                    require(result.size() + n <= V4vJson.MAX_BODY); result.write(buffer, 0, n)
                }
                result.toByteArray()
            }
            check(!expired.get() && !stopped.get())
            return V4vJson.parse(Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(data)).toString())
        } catch (error: V4vHttpFailure) { throw error }
        catch (_: Exception) { throw V4vHttpFailure() }
        finally { timeout.cancel(false); current.compareAndSet(connection, null); connection.disconnect() }
    }
    override fun close() { stopped.set(true); current.get()?.disconnect(); watchdog.shutdownNow() }
}
