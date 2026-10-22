package dev.forgesworn.sapflow.broadcaster

/** Local bounded metadata; names do not select or authorise input. */
internal class AudioInputOption private constructor(val id: String, val name: String, val kind: Kind) {
    enum class Kind { BUILT_IN, WIRED, USB }
    companion object {
        private fun control(codePoint: Int): Boolean = Character.isISOControl(codePoint) || Character.getType(codePoint) == Character.FORMAT.toInt()
        fun validId(id: String): Boolean = id.isNotEmpty() && id.length <= 256 &&
            id.toByteArray(Charsets.UTF_8).size <= 256 && id.codePoints().noneMatch(::control)
        fun create(id: String, name: String, kind: Kind): AudioInputOption? =
            if (!validId(id) || name.length > 1024 || name.toByteArray(Charsets.UTF_8).size > 1024) null
            else AudioInputOption(id, buildString {
                val points = name.codePoints().iterator()
                var count = 0
                while (points.hasNext() && count < 80) {
                    val point = points.nextInt()
                    if (!control(point)) { appendCodePoint(point); count++ }
                }
            }, kind)
        fun inventory(values: List<AudioInputOption>): List<AudioInputOption> =
            values.asSequence().take(64).distinctBy { it.id }.take(32).toList()
    }
}

/** One immutable choice. Callbacks supply a captured preparation + epoch/native-owner
 * token: the preparation alone may survive multiple rotation epochs. */
internal class AudioInputPolicy<Owner> private constructor(
    val owner: Owner, val requestedId: String?, private val available: Set<String>,
) {
    enum class Observation { ACCEPTED, FAILED, STALE, IGNORED }
    private var actualId: String? = null
    private var failed = false
    private var closed = false
    @Synchronized fun observe(owner: Owner, id: String?): Observation {
        if (owner != this.owner) return Observation.STALE
        if (closed || failed) return Observation.IGNORED
        if (id == null || id !in available || requestedId != null && requestedId != id || actualId != null && actualId != id) {
            failed = true; return Observation.FAILED
        }
        actualId = id; return Observation.ACCEPTED
    }
    @Synchronized fun permitsMedia(owner: Owner): Boolean = owner == this.owner && !closed && !failed && actualId != null
    @Synchronized fun observedId(owner: Owner): String? = if (owner == this.owner && !closed && !failed) actualId else null
    @Synchronized fun close(owner: Owner) { if (owner == this.owner) closed = true }
    companion object {
        fun <Owner> create(owner: Owner, requestedId: String? = null, available: List<AudioInputOption>): AudioInputPolicy<Owner>? {
            val ids = AudioInputOption.inventory(available).map { it.id }.toSet()
            if (requestedId != null && (!AudioInputOption.validId(requestedId) || requestedId !in ids)) return null
            return AudioInputPolicy(owner, requestedId, ids)
        }
    }
}
