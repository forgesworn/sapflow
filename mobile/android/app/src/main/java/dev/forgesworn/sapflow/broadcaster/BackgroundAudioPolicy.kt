package dev.forgesworn.sapflow.broadcaster

/** Cold service admission only. No media, callbacks or platform calls under this monitor.
 * A fresh random capability accompanies the exact preparation; serials alone are not authority. */
internal class BackgroundAudioPolicy {
    class Owner internal constructor(val preparation: CaptureStartAdmission.Preparation, val capability: String,
        val usesV4v: Boolean, val recordsLocally: Boolean)
    private var owner: Owner? = null
    private var promoted = false
    @Synchronized fun reserve(preparation: CaptureStartAdmission.Preparation, capability: String,
        usesV4v: Boolean = false, recordsLocally: Boolean = false): Owner? {
        if (owner != null || !preparation.backgroundAudio || preparation.intent.hasVideo ||
            !capability.matches(Regex("[0-9a-f]{32}"))) return null
        return Owner(preparation, capability, usesV4v, recordsLocally).also { owner = it; promoted = false }
    }
    @Synchronized fun current(): Owner? = owner
    @Synchronized fun matches(id: Long, capability: String?): Owner? = owner?.takeIf {
        it.preparation.id == id && it.capability == capability
    }
    @Synchronized fun promote(value: Owner): Boolean {
        if (owner !== value) return false
        promoted = true; return true
    }
    @Synchronized fun ready(preparation: CaptureStartAdmission.Preparation): Boolean = owner?.preparation === preparation && promoted
    @Synchronized fun retire(value: Owner): Boolean {
        if (owner !== value) return false
        owner = null; promoted = false; return true
    }
}

/** Only the exact last foreground client may decide what its departure does.
 * After B leaves, delayed A remains STALE even though there is no foreground client. */
internal class CaptureForegroundClients {
    enum class Departure { STALE, CONTINUE, STOP }
    private var current: Any? = null
    fun enter(client: Any) { current = client }
    fun present(): Boolean = current != null
    fun leave(client: Any, mayContinue: Boolean): Departure {
        if (current !== client) return Departure.STALE
        current = null
        return if (mayContinue) Departure.CONTINUE else Departure.STOP
    }
}
