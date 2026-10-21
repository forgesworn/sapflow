package dev.forgesworn.sapflow.broadcaster

import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.URI

/** Numeric unicast only: this path never performs destination DNS. */
internal data class SrtDestination(val address: InetAddress, val port: Int) {
    val socketAddress: InetSocketAddress get() = InetSocketAddress(address, port)
    val nativeAddress: String get() = if (address is Inet4Address) "${address.hostAddress}:$port" else "[${address.hostAddress}]:$port"

    companion object {
        fun parse(value: String): SrtDestination {
            require(value.length in 1..256) { "Enter a bounded numeric SRT URL" }
            val uri = URI(value)
            require(uri.scheme?.lowercase() == "srt" && uri.rawUserInfo == null && uri.rawQuery == null && uri.rawFragment == null) {
                "Use srt://numeric-address:port with separate stream ID and passphrase"
            }
            require(uri.rawPath.isNullOrEmpty() || uri.rawPath == "/") { "Put the stream ID in its separate field" }
            require(uri.port in 1..65535) { "An explicit SRT port is required" }
            val host = uri.host?.removePrefix("[")?.removeSuffix("]") ?: error("Numeric SRT address is missing")
            val literal = if (host.contains(':')) {
                require(host.matches(Regex("[0-9a-fA-F:]+"))) { "IPv6 zone IDs and hostnames are not supported" }
                host
            } else {
                val parts = host.split('.')
                require(parts.size == 4 && parts.all { it.isNotEmpty() && it.all(Char::isDigit) && (it.toIntOrNull() ?: -1) in 0..255 }) {
                    "SRT currently requires a numeric unicast address"
                }
                parts.joinToString(".") { it.toInt().toString() }
            }
            val address = InetAddress.getByName(literal) // validated numeric grammar only
            require(!address.isAnyLocalAddress && !address.isMulticastAddress && address.hostAddress != "255.255.255.255") { "A unicast destination is required" }
            return SrtDestination(address, uri.port)
        }

        fun validateCredentials(streamId: String, passphrase: String, plaintext: Boolean) {
            require(streamId.toByteArray(Charsets.UTF_8).size <= 512 && !streamId.contains('\u0000')) { "SRT stream ID exceeds its wire bound" }
            if (plaintext) require(passphrase.isEmpty()) { "Clear the passphrase before explicitly selecting plaintext" }
            else require(passphrase.toByteArray(Charsets.UTF_8).size in 10..79) { "AES-256 requires a 10 to 79 byte passphrase" }
        }
    }
}
