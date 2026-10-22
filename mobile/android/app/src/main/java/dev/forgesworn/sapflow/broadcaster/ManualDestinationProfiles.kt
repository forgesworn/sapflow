package dev.forgesworn.sapflow.broadcaster

import java.io.InputStream
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.serialization.json.*

internal interface ManualProfileBytes {
    fun read(): ByteArray?
    fun write(bytes: ByteArray)
}

/** Serial cold repository. An unconfirmed mutation poisons this owner; reopening
 * validates the actual encrypted document and never resets a broken store. */
internal class ManualDestinationProfiles(private val bytes: ManualProfileBytes, private val seal: (ByteArray) -> ByteArray, private val open: (ByteArray) -> ByteArray) {
    class Saved(val id: String, val name: String) { override fun toString() = "Saved manual destination" }
    private var poisoned = false
    private fun id(value: String): String = value.also { require(it.matches(Regex("[0-9a-f]{32}"))) }
    private fun rows(): LinkedHashMap<String, ManualDestinationProfile> = try { readRows() } catch (_: Exception) { poisoned = true; throw ManualProfileFailure() }
    private fun readRows(): LinkedHashMap<String, ManualDestinationProfile> {
        check(!poisoned)
        val sealed = bytes.read() ?: return linkedMapOf()
        require(sealed.size <= MAX_STORE + 64)
        val plain = open(sealed)
        try {
            require(plain.size <= MAX_STORE)
            val text = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(plain)).toString()
            val doc = V4vJson.parse(text, MAX_STORE)
            require(doc.keys == setOf("version", "profiles") && doc["version"] == JsonPrimitive(1))
            val list = doc.getValue("profiles").jsonArray; require(list.size <= 16)
            val result = linkedMapOf<String, ManualDestinationProfile>()
            for (element in list) {
                val row = element.jsonObject; require(row.keys == setOf("id", "profile"))
                val key = row.getValue("id").jsonPrimitive.let { require(it.isString); id(it.content) }
                require(!result.containsKey(key))
                result[key] = ManualDestinationProfile.parse(row.getValue("profile").toString().toByteArray())
            }
            return result
        } finally { plain.fill(0) }
    }
    private fun persist(rows: LinkedHashMap<String, ManualDestinationProfile>) {
        val plain = buildJsonObject {
            put("version", 1)
            putJsonArray("profiles") {
                rows.forEach { (id, profile) -> add(buildJsonObject { put("id", id); put("profile", profile.document()) }) }
            }
        }.toString().toByteArray()
        try { require(plain.size <= MAX_STORE); bytes.write(seal(plain)) }
        catch (_: Exception) { poisoned = true; throw ManualProfileFailure() }
        finally { plain.fill(0) }
    }
    private fun <T> checked(action: () -> T): T = try { check(!poisoned); action() } catch (_: Exception) { throw ManualProfileFailure() }
    fun list(): List<Saved> = checked { rows().map { Saved(it.key, it.value.name) } }
    fun load(id: String): ManualDestinationProfile = checked { requireNotNull(rows()[id(id)]) }
    fun save(existing: String?, profile: ManualDestinationProfile): String = checked {
        val current = rows()
        val selected = existing?.let { id(it).also { key -> require(current.containsKey(key)) } } ?: UUID.randomUUID().toString().replace("-", "")
        require(current.containsKey(selected) || current.size < 16)
        current[selected] = profile; persist(current); selected
    }
    fun delete(selected: String) = checked { val current = rows(); require(current.remove(id(selected)) != null); persist(current) }
    companion object { const val MAX_STORE = 272 * 1024 }
}

/** One UI generation, not an execution queue. Invalidated picker/IO ownership
 * remains occupied until its real reply, so a late callback cannot bind B. */
internal class ManualProfileActions {
    class Ticket internal constructor(internal val generation: Long)
    class Review internal constructor(internal val generation: Long, val profile: ManualDestinationProfile)
    private var generation = 0L
    private var pending: Ticket? = null
    private var consentedReview: Review? = null
    @get:Synchronized var review: Review? = null; private set
    val busy: Boolean @Synchronized get() = pending != null
    @Synchronized fun invalidate() { if (generation != Long.MAX_VALUE) generation++; review = null; consentedReview = null }
    @Synchronized fun begin(allowed: Boolean): Ticket? {
        if (!allowed || pending != null || generation == Long.MAX_VALUE) return null
        invalidate(); return Ticket(generation).also { pending = it }
    }
    @Synchronized fun current(ticket: Ticket, allowed: Boolean): Boolean = allowed && pending === ticket && ticket.generation == generation
    @Synchronized fun finish(ticket: Ticket, allowed: Boolean): Boolean {
        if (pending !== ticket) return false
        val valid = current(ticket, allowed); pending = null; return valid
    }
    @Synchronized fun reviewed(ticket: Ticket, profile: ManualDestinationProfile, allowed: Boolean): Boolean {
        if (!finish(ticket, allowed)) return false
        review = Review(generation, profile); return true
    }
    @Synchronized fun cancel(selected: Review): Boolean {
        if (review !== selected) return false
        invalidate(); return true
    }
    @Synchronized fun setConsent(selected: Review, value: Boolean): Boolean {
        if (review !== selected || selected.generation != generation) return false
        consentedReview = if (value) selected else null; return true
    }
    @Synchronized fun apply(selected: Review, allowed: Boolean, fields: (ManualDestinationProfile) -> Unit): Boolean {
        if (!allowed || pending != null || review !== selected || selected.generation != generation || (selected.profile.review.plaintext && consentedReview !== selected)) return false
        val profile = selected.profile; invalidate(); fields(profile); return true
    }
}

internal object ManualProfileImport {
    fun read(input: InputStream, nowNs: () -> Long, deadlineNs: Long, beforeRead: () -> Unit = {}): ByteArray {
        val output = ByteArrayOutputStream(); val buffer = ByteArray(4096)
        while (true) {
            check(nowNs() < deadlineNs)
            beforeRead()
            val count = input.read(buffer, 0, minOf(buffer.size, ManualDestinationProfile.MAX_BYTES + 1 - output.size()))
            check(nowNs() < deadlineNs)
            if (count < 0) break
            require(count > 0); output.write(buffer, 0, count); require(output.size() <= ManualDestinationProfile.MAX_BYTES)
        }
        return output.toByteArray()
    }
}

/** Android delivers an OpenDocument reply independently of onStart/onResume.
 * Keep its exact launch until the real reply; an invalidated A cannot lend its
 * untagged platform result to a new B. Used only on the Activity main thread. */
internal class ManualProfilePicker<T> {
    class Delivery<T>(val ticket: ManualProfileActions.Ticket?, val value: T?, val valid: Boolean)
    private class Launch<T>(val client: Any, val ticket: ManualProfileActions.Ticket?) {
        var valid = true; var returned = false; var value: T? = null
    }
    private var launch: Launch<T>? = null
    fun expectedDeparture(client: Any): Boolean = launch?.let { it.client === client && it.valid } == true
    val hasOutstanding: Boolean get() = launch != null
    fun outstandingFor(client: Any): Boolean = launch?.client === client
    fun restoreDiscard(client: Any) {
        // A saved marker never restores approval or a secret-bearing draft.
        // Rebind only the discard obligation so another recreation preserves it.
        if (launch == null || launch?.valid != true) launch = Launch<T>(client, null).also { it.valid = false }
    }
    fun begin(client: Any, ticket: ManualProfileActions.Ticket): Boolean {
        if (launch != null) return false
        launch = Launch(client, ticket); return true
    }
    fun invalidate(client: Any) { launch?.takeIf { it.client === client }?.valid = false }
    fun returned(client: Any, value: T?) {
        val active = launch ?: return
        if (active.returned) return
        if (active.client !== client) active.valid = false
        active.returned = true; active.value = value
    }
    fun resume(client: Any, allowed: Boolean): Delivery<T>? {
        val active = launch ?: return null
        if (!active.returned) return null
        launch = null
        return Delivery(active.ticket, active.value, active.valid && active.client === client && allowed)
    }
}

internal class ManualProfileIOAdmission {
    private val occupied = AtomicBoolean(false)
    private val retained = AtomicBoolean(false)
    val busy: Boolean get() = occupied.get() || retained.get()
    fun reserve() = !retained.get() && occupied.compareAndSet(false, true)
    fun finish(cleanupComplete: Boolean) {
        if (!cleanupComplete) retained.set(true)
        else if (!retained.get()) occupied.set(false)
    }
}

/** A logical timeout reports once, independently of a hostile provider's
 * cancellation/read/close. Real capacity remains occupied until both the
 * actual worker and any begun cancellation call have returned. */
internal class ManualProfileIOJob<T>(private val admission: ManualProfileIOAdmission, private val deadlineNs: Long?) {
    private var reported = false
    private var finished = false
    private var cleanupComplete = true
    private var cancelling = false
    private var cancellationFinished = false
    private var released = false
    @Synchronized fun timeout(nowNs: Long): Result<T>? {
        if (finished || cancelling || deadlineNs == null || nowNs < deadlineNs) return null
        cancelling = true
        if (reported) return null
        reported = true
        return Result.failure(ManualProfileFailure())
    }
    @Synchronized fun complete(result: Result<T>, nowNs: Long, clean: Boolean): Result<T>? {
        if (finished) return null
        finished = true; cleanupComplete = clean
        releaseIfClosed()
        if (reported) return null
        reported = true
        return if (deadlineNs != null && nowNs >= deadlineNs) Result.failure(ManualProfileFailure()) else result
    }
    @Synchronized fun cancellationReturned() { cancellationFinished = true; releaseIfClosed() }
    private fun releaseIfClosed() {
        if (!released && finished && (!cancelling || cancellationFinished)) {
            released = true; admission.finish(cleanupComplete)
        }
    }
}
