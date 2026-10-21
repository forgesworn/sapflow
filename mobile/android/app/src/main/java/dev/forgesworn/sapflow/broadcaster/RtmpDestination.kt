package dev.forgesworn.sapflow.broadcaster

import java.net.URI

internal data class RtmpDestination(val host: String, val port: Int, val app: String, val tcUrl: String, val tls: Boolean) {
    companion object {
        fun parse(applicationUrl: String, allowPlaintextDevelopment: Boolean): RtmpDestination {
            require(applicationUrl.length in 1..4096) { "Enter a bounded RTMPS application URL" }
            val uri = URI(applicationUrl)
            val tls = when (uri.scheme?.lowercase()) {
                "rtmps" -> true
                "rtmp" -> { require(allowPlaintextDevelopment) { "Plain RTMP requires the explicit development option" }; false }
                else -> error("Destination must use rtmps://")
            }
            require(uri.rawUserInfo == null && uri.rawFragment == null && uri.rawQuery == null) { "Put authorised credentials in the stream key, not URL userinfo or query" }
            val host = uri.host?.removePrefix("[")?.removeSuffix("]") ?: error("Destination hostname is missing")
            require(host.isNotEmpty()) { "Destination hostname is missing" }
            val port = if (uri.port == -1) { if (tls) 443 else 1935 } else uri.port
            require(port in 1..65535) { "Destination port is invalid" }
            val app = uri.rawPath?.removePrefix("/")?.trimEnd('/') ?: ""
            require(app.isNotEmpty() && !app.contains('\u0000')) { "Application path is missing" }
            return RtmpDestination(host, port, app, uri.toASCIIString(), tls)
        }
    }
}
