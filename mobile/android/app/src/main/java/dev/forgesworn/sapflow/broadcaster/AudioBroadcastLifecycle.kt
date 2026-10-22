package dev.forgesworn.sapflow.broadcaster

/** Cold, service-main-thread orchestration. No owner is created from a command.
 * Ownership includes STOPPING: only actual cleanup, never a failed promotion or
 * duplicate command, releases the service lease. Platform destruction is an
 * interruption rather than proof that recorder/publisher cleanup completed. */
internal class AudioBroadcastLifecycle<O : Any>(private val owners: Owners<O>, private val platform: Platform<O>) {
    enum class Command { ARM, STOP }
    interface Owners<O> {
        fun match(id: Long, capability: String?): O?
        fun owns(value: O): Boolean
        fun revalidateAuthority()
        fun promoted(value: O): Boolean
        fun check(value: O): Boolean
        fun stop(value: O, reason: String)
        fun retired(value: O)
        fun serviceDestroyed(value: O)
    }
    interface Platform<O> {
        /** Returns only after actual foreground promotion; permission/promotion
         * failures throw. A failed call cannot extend Android's FGS deadline. */
        fun promote(value: O)
        fun removeNotification()
        fun stopService(startId: Int)
        /** Replaces the single outstanding check; never accumulates timers. */
        fun scheduleCheck(delayMs: Long)
        fun cancelCheck()
    }
    private var lease: O? = null
    private var lastStart = 0
    private var stopRequested = false
    private var foregroundPromoted = false
    private var closingUnpromoted = false
    private var destroyed = false

    fun command(command: Command?, id: Long, capability: String?, startId: Int) {
        if (destroyed) return
        lastStart = startId
        if (closingUnpromoted) { platform.stopService(startId); return }
        val value = if (command != null) owners.match(id, capability) else null
        if (value == null) {
            if (lease == null) platform.stopService(startId)
            return
        }
        owners.revalidateAuthority()
        val previous = lease
        if (previous != null) {
            if (previous === value) {
                // A repeated ARM cannot re-promote a show already stopping.
                if (command == Command.STOP) requestStop(value)
                check()
                return
            }
            if (owners.owns(previous)) return // Never replace a retained cleanup owner.
            release(previous, stopService = false)
        }
        if (!owners.owns(value)) { platform.stopService(startId); return }
        lease = value
        stopRequested = false
        when (command) {
            Command.STOP -> requestStop(value)
            Command.ARM -> {
                try {
                    platform.promote(value)
                    foregroundPromoted = true
                    if (!owners.promoted(value)) requestStop(value, "Background service admission expired. Prepare again.")
                } catch (_: Exception) {
                    requestStop(value, "Background audio service permission or promotion failed. Prepare again.")
                    if (!foregroundPromoted) {
                        // Stop the unpromoted platform service promptly. The
                        // application still owns the exact native cleanup;
                        // stopping this Service neither retires it nor proves
                        // it complete. No timer may extend the FGS deadline.
                        closingUnpromoted = true
                        platform.cancelCheck()
                        platform.stopService(lastStart)
                        return
                    }
                }
            }
            null -> error("Unmatched command")
        }
        check()
    }

    private fun requestStop(value: O, reason: String = "Stopped from the background audio notification; saving recording") {
        if (lease !== value || stopRequested) return
        stopRequested = true
        owners.stop(value, reason)
    }

    fun check() {
        if (destroyed || closingUnpromoted) return
        val value = lease ?: return
        if (!owners.check(value)) release(value, stopService = true)
        else platform.scheduleCheck(1_000)
    }

    fun taskRemoved() { check() }

    fun destroy() {
        if (destroyed) return
        destroyed = true
        platform.cancelCheck()
        val value = lease
        lease = null
        if (value != null && foregroundPromoted) owners.serviceDestroyed(value)
    }

    private fun release(value: O, stopService: Boolean) {
        if (lease !== value) return
        platform.cancelCheck()
        lease = null
        stopRequested = false
        foregroundPromoted = false
        owners.retired(value)
        platform.removeNotification()
        if (stopService) platform.stopService(lastStart)
    }
}
