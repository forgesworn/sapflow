package dev.forgesworn.sapflow.broadcaster

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

internal class V4vSessionTest {
    private val event = "event-fixture"
    private val sid = "1".repeat(32)
    private val profile = V4vProfile.approved("https://creator.invalid", "https://play.invalid", "/hls/")!!
    private fun receipt(revision: Int = 1, phase: String = "active", complete: Boolean = false): JsonObject = buildJsonObject {
        put("contractVersion", 1); put("eventId", event); put("eventGeneration", 1); put("sessionId", sid); put("revision", revision)
        put("expiresAtUnixMs", 601_000); put("phase", phase); put("endComplete", complete)
        put("origin", buildJsonObject {
            put("contractVersion", 1); put("eventId", event); put("eventGeneration", 1); put("sessionId", sid); put("revision", revision)
            put("admission", if (complete) "denied" else "open"); put("protocol", if (complete) "stopped" else "active")
            put("recording", if (complete) "finalised" else "active"); put("callbacks", if (complete) "delivered" else "pending")
        })
    }
    private fun grant(revision: Int = 1, bps: Int = 4_000_000) = buildJsonObject {
        for ((key, value) in receipt(revision)) if (key in setOf("contractVersion", "eventId", "eventGeneration", "sessionId", "revision", "expiresAtUnixMs")) put(key, value)
        put("destination", buildJsonObject { put("kind", "v4v"); put("connector", "sapflow"); put("unlimited", true); put("watermark", false); put("subscriptionRequired", false) })
        put("transport", "srt"); put("endpoint", "srt://192.0.2.1:8890"); put("streamId", "publish:live/$event:$sid:${"a".repeat(43)}")
        put("encryption", buildJsonObject { put("cipher", "aes256-ctr"); put("passphrase", "b".repeat(43)) })
        put("refreshAfterUnixMs", 301_000); put("reconnectGraceMs", 90_000)
        put("limits", buildJsonObject { put("maxPublishBps", bps); put("maxQueueBytes", 524288) })
    }
    private class Store : V4vJournalStore {
        var value: JsonObject? = null; var fail = false; var silentRenameFailure = false; var writes = 0
        override fun load() = value
        override fun save(document: JsonObject) {
            check(!fail)
            if (silentRenameFailure) V4vDurableCommit({}, { value?.toString()?.toByteArray() ?: ByteArray(0) }, {}).write(document.toString().toByteArray())
            value = document; writes++
        }
    }
    private inner class Harness {
        var now = 1000L; var fences = 0; var nonceCount = 0; var loseReply = false; var incompleteEnd = false; var generation = 0; var bps = 4_000_000
        val store = Store(); var blockStatus: CountDownLatch? = null; val entered = CountDownLatch(1); val sent = mutableListOf<Pair<String, JsonObject?>>()
        val api = object : V4vApi {
            override fun request(method: String, path: String, bearer: String?, body: JsonObject?): JsonObject {
                if (method == "GET" && path.endsWith("native-session")) return buildJsonObject { put("session", buildJsonObject { put("eventId", event); put("eventGeneration", generation) }) }
                sent += path to body
                if (method == "POST") {
                    assertNotNull("durable operation must precede HTTP", store.value?.get("pending"))
                    if (loseReply) throw V4vHttpFailure()
                    val end = path.endsWith("/end"); val ack = path.endsWith("/recovery-ack")
                    if (path.endsWith("native-session")) generation = 1
                    val revision = if (end || ack || path.endsWith("/refresh")) 2 else 1
                    var r = receipt(revision, if (end || ack) "ended" else "active", end && !incompleteEnd)
                    if (end && incompleteEnd) r = JsonObject(r.toMutableMap().also { it["origin"] = buildJsonObject {
                        for ((key, value) in r.getValue("origin").jsonObject) put(key, value)
                        put("admission", "denied"); put("protocol", "stopped"); put("recording", "incomplete")
                    } })
                    if (ack) r = JsonObject(r.toMutableMap().also { it["recovery"] = buildJsonObject { put("state", "acknowledged"); put("eventGeneration", 1); put("revision", revision); put("acknowledgementKey", body!!.getValue("idempotencyKey")); put("explicitPrepareRequired", true) } })
                    return buildJsonObject { put("ok", true); put("session", r); if (!end && !ack) put("grant", grant(revision, bps)) }
                }
                blockStatus?.let { entered.countDown(); check(it.await(2, TimeUnit.SECONDS)) }
                return buildJsonObject { put("ok", true); put("session", receipt()) }
            }
        }
        fun engine() = V4vSessionEngine(profile, api, store, { now }, { (++nonceCount).toString(16).padStart(32, '0') }, { fences++ })
        fun signed(): V4vSessionEngine = engine().also { it.installLogin("a".repeat(64), "synthetic-token", 86400) }
    }
    @Test fun lostPrepareRestartsWithTheExactPersistedNonceAndBody() {
        val h = Harness(); val first = h.signed(); h.loseReply = true; first.prepare(event)
        val original = h.sent.single(); assertTrue(first.view().pending)
        h.loseReply = false; val restored = h.engine(); restored.retry()
        assertEquals(original, h.sent.last()); assertEquals(1, h.nonceCount); assertTrue(restored.view().prepared)
    }
    @Test fun journalFailureFencesBeforeAnyMutationIsSentAndCannotRetry() {
        val h = Harness(); val e = h.signed(); h.store.fail = true
        assertThrows(Exception::class.java) { e.prepare(event) }
        assertEquals(1, h.fences); assertTrue(h.sent.isEmpty())
        assertThrows(Exception::class.java) { e.prepare(event) }
    }
    @Test fun grantControlRemainsAvailableIndependentOfSelectedCaptureQuality() {
        val h = Harness(); h.bps = 1_000_000; val e = h.signed(); e.prepare(event)
        assertFalse(e.view().pending)
        val grant = e.captureGrant()
        assertEquals(1_000_000L, e.view().maxPublishBps)
        assertTrue(e.view().prepared)
        assertThrows(IllegalArgumentException::class.java) { CaptureProfile.HD_720.requireGrant(grant.maxPublishBps) }
        CaptureProfile.SD_480.requireGrant(grant.maxPublishBps)
        e.refresh(); assertEquals(2L, e.captureGrant().identity.revision)
        e.end(); assertTrue(e.view().endComplete); assertTrue(h.fences > 0)
    }
    @Test fun endRetainsTerminalIntentAcrossUncertainRefreshAndDoesNotReprepare() {
        val h = Harness(); val e = h.signed(); e.prepare(event); h.loseReply = true; e.refresh(); e.end()
        assertTrue(h.store.value!!.getValue("terminalIntent").jsonPrimitive.boolean)
        assertFalse(e.view().prepared)
        h.loseReply = false; e.retry()
        assertTrue(e.view().endComplete)
        assertEquals(1, h.sent.count { it.first.endsWith("native-session") })
        assertEquals(1, h.sent.count { it.first.endsWith("/end") })
    }
    @Test fun expiredBearerBlocksCaptureAndNeverOpensASigner() {
        val h = Harness(); val e = h.signed(); e.prepare(event); h.now += 86_400_001
        assertThrows(IllegalStateException::class.java) { e.captureGrant() }
        e.tick(); assertFalse(e.view().signedIn); assertTrue(h.fences > 0)
    }
    @Test fun mismatchedOrCorruptExistingJournalIsNeverReset() {
        val h = Harness(); h.store.value = buildJsonObject { put("version", 2); put("apiOrigin", "https://creator.invalid"); put("clockFloorMs", 1) }
        assertThrows(IllegalArgumentException::class.java) { h.engine() }; assertEquals(0, h.store.writes)
    }
    @Test fun acknowledgementNeverCompletesEndOrAutomaticallyPreparesReplacement() {
        val h = Harness(); val e = h.signed(); e.prepare(event); h.incompleteEnd = true; e.end()
        assertFalse(e.view().endComplete)
        val before = h.sent.size; e.acknowledge()
        assertEquals(before + 1, h.sent.size); assertEquals("acknowledged", e.view().recoveryState)
        assertFalse(e.view().endComplete); assertFalse(e.view().prepared)
        assertThrows(IllegalStateException::class.java) { e.prepare(event) }
        h.loseReply = true; e.prepare(event, true)
        val saved = h.store.value!!.getValue("pending").jsonObject.getValue("body").jsonObject
        assertEquals(1L, saved.getValue("expectedEventGeneration").jsonPrimitive.long)
        assertEquals(sid, saved.getValue("replaces").jsonObject.getValue("sessionId").jsonPrimitive.content)
        assertTrue(e.view().pending)
    }
    @Test fun restoredGrantNeedsAnAcknowledgedFreshRevisionBeforeCapture() {
        val h = Harness(); h.signed().prepare(event)
        val restored = h.engine()
        assertFalse(restored.view().prepared)
        assertThrows(IllegalStateException::class.java) { restored.captureGrant() }
        restored.refresh()
        assertEquals(2L, restored.captureGrant().identity.revision)
    }
    @Test fun reservedEndIsNotDroppedBehindABusyStatusRequest() {
        val h = Harness(); val e = h.signed(); e.prepare(event)
        h.blockStatus = CountDownLatch(1)
        val actor = V4vDestinationSession { e }
        val statusDone = CountDownLatch(1); val endDone = CountDownLatch(1)
        try {
            actor.submit({ status() }) { assertNull(it); statusDone.countDown() }
            assertTrue(h.entered.await(2, TimeUnit.SECONDS))
            val ticket = checkNotNull(actor.endInvitation())
            assertTrue(actor.endDestination(ticket, completed = { assertNull(it); endDone.countDown() }))
            h.blockStatus!!.countDown()
            assertTrue(statusDone.await(2, TimeUnit.SECONDS)); assertTrue(endDone.await(2, TimeUnit.SECONDS))
            assertTrue(actor.view.endComplete)
            assertEquals(1, h.sent.count { it.first.endsWith("/end") })
        } finally { h.blockStatus!!.countDown(); actor.close(); assertTrue(actor.awaitClosed(2000)) }
    }
    @Test fun completedEndHasNoIdleNetworkOrAlarmUntilAnExplicitOperation() {
        val h = Harness(); val e = h.signed(); e.prepare(event); e.end()
        assertTrue(e.view().endComplete); assertNull(e.nextDelayMs())
        val requests = h.sent.size
        h.now += 60_000; e.tick()
        assertEquals(requests, h.sent.size); assertNull(e.nextDelayMs())
    }
    @Test fun unconfirmedAtomicRenameCannotSendMutationAndReopenKeepsOldJournal() {
        val h = Harness(); val e = h.signed(); val old = h.store.value
        h.store.silentRenameFailure = true
        assertThrows(IllegalStateException::class.java) { e.prepare(event) }
        assertTrue(h.sent.isEmpty()); assertEquals(old, h.store.value); assertEquals(1, h.fences)
        h.store.silentRenameFailure = false
        val reopened = h.engine()
        assertFalse(reopened.view().pending); assertFalse(reopened.view().prepared)
    }
    @Test fun profileDefaultIsDisabledAndRejectsInjectedOrigins() {
        assertNull(V4vProfile.approved("", "", ""))
        for (u in listOf("http://creator.invalid", "https://creator.invalid/private", "https://x@creator.invalid", "https://creator.invalid?x=1")) {
            assertThrows(IllegalArgumentException::class.java) { V4vProfile.approved(u, "https://play.invalid", "/hls/") }
        }
    }
    @Test fun receiptCannotClaimEndWithActiveOrFailedRecording() {
        val o = receipt().toMutableMap(); o["endComplete"] = JsonPrimitive(true)
        assertThrows(IllegalArgumentException::class.java) { V4vSessionReceipt.read(JsonObject(o)) }
    }
    @Test fun hostileNestedAndOversizedJsonRejectedBeforeDto() {
        assertThrows(IllegalArgumentException::class.java) { V4vJson.parse("{" + " ".repeat(65536) + "}") }
        assertThrows(IllegalArgumentException::class.java) { V4vJson.parse("{\"x\":" + "[".repeat(20) + "0" + "]".repeat(20) + "}") }
    }
    @Test fun grantNeverPrintsCredentials() {
        val g = V4vGrant.read(grant(), 1000)
        assertFalse(g.toString().contains("a".repeat(43))); assertFalse(g.toString().contains("b".repeat(43)))
    }
}
