package dev.forgesworn.sapflow.broadcaster

import java.io.ByteArrayOutputStream
import java.net.URL
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import javax.net.ssl.HttpsURLConnection

/** No bearer, cookies, redirect, supplied host or TLS override exists in this
 * interface. Only the approved route can construct resource URLs. */
internal class OriginMediaFetch(private val route: ApprovedPlaybackRoute) : AutoCloseable {
    private val current = AtomicReference<HttpsURLConnection?>()
    private val timer = Executors.newSingleThreadScheduledExecutor { Thread(it, "sapflow-probe-http-deadline").apply { isDaemon = true } }
    private val stopped = java.util.concurrent.atomic.AtomicBoolean()
    fun get(path: String, maxBytes: Int, cancel: OriginProbeCancellation): ByteArray {
        require(maxBytes in 1..OriginProbeLimits.PART_BYTES)
        cancel.check(); check(!stopped.get())
        check(java.net.CookieHandler.getDefault() == null) { "Ambient cookies are not permitted for origin checks" }
        val connection = URL(route.resource(path)).openConnection() as HttpsURLConnection
        check(current.compareAndSet(null, connection))
        val requestDeadline = System.nanoTime() + minOf(cancel.remainingNs(), 5_000_000_000L)
        val expired = java.util.concurrent.atomic.AtomicBoolean()
        val alarm = timer.schedule({ expired.set(true); connection.disconnect() }, (requestDeadline - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
        fun checkRequest() { cancel.check(); check(!stopped.get() && !expired.get() && System.nanoTime() < requestDeadline) }
        try {
            connection.instanceFollowRedirects = false
            connection.connectTimeout = 2000; connection.readTimeout = 2000
            connection.useCaches = false
            connection.setRequestProperty("Cache-Control", "no-store")
            connection.setRequestProperty("Accept", "video/mp4, audio/mp4, application/octet-stream")
            // Suppress ambient CookieHandler credentials; API bearer never enters this owner.
            connection.setRequestProperty("Cookie", "")
            connection.setRequestProperty("Authorization", "")
            connection.allowUserInteraction = false
            require(connection.responseCode == 200) { "Origin media unavailable" }
            require(connection.contentEncoding == null || connection.contentEncoding.equals("identity", true))
            require(connection.contentLengthLong <= maxBytes)
            val result = connection.inputStream.use { input ->
                val bytes = ByteArrayOutputStream(minOf(maxBytes, 16 * 1024)); val scratch = ByteArray(8192)
                while (true) {
                    checkRequest()
                    val read = input.read(scratch)
                    if (read < 0) break
                    require(read <= maxBytes - bytes.size()) { "Origin media exceeds bound" }
                    bytes.write(scratch, 0, read)
                }
                bytes.toByteArray()
            }
            checkRequest(); require(result.isNotEmpty())
            return result
        } finally { alarm.cancel(false); current.compareAndSet(connection, null); connection.disconnect() }
    }
    override fun close() { stopped.set(true); current.get()?.disconnect(); timer.shutdownNow() }
}
