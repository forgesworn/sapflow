package dev.forgesworn.sapflow.broadcaster

/** Captured once per exact scoped destination and shared by all its attempts.
 * A valid check performs only the supplied atomic-state/BOOTTIME read. */
internal class PublishingAuthority(private val onDenied: () -> Unit = {}, private val permitted: () -> Boolean) {
    fun requireValid() {
        if (!permitted()) {
            onDenied() // Cold scalar destination fence only; no socket close/join here.
            throw IllegalStateException("Scoped publishing authority expired or retired")
        }
    }
}

/** Used at the actual native-push and user-space wire-write boundaries. Inlining
 * creates no per-packet lambda or copy. A null manual authority adds no clock read. */
internal inline fun <T> withPublishingAuthority(authority: PublishingAuthority?, operation: () -> T): T {
    authority?.requireValid()
    return operation()
}
