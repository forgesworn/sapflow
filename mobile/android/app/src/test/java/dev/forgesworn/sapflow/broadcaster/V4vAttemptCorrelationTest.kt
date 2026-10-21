package dev.forgesworn.sapflow.broadcaster

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

internal class V4vAttemptCorrelationTest {
    private val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
    private val password = "b".repeat(43)
    private val passphrase = "c".repeat(43)
    private fun grant(id: V4vIdentity = identity, stream: String = "publish:live/${id.eventId}:${id.sessionId}:$password", pass: String = passphrase) =
        V4vGrant(id, "srt://192.0.2.1:8890", stream, pass, 1000, 500, 90000, 4_000_000, 524288, 1000000)
    private fun context(epoch: Long = 1, attempt: Long = 1) = PublishingAttemptContext(1, epoch, attempt, SdkAttemptIdentity(1uL, attempt.toULong()))
    private fun receipt(nonce: String, epoch: Long = 1, id: V4vIdentity = identity, phase: String = "active",
        mutateOrigin: (MutableMap<String, JsonElement>) -> Unit = {}, mutateMedia: (MutableMap<String, JsonElement>) -> Unit = {}): V4vSessionReceipt {
        fun fields() = buildJsonObject {
            put("contractVersion", 1); put("eventId", id.eventId); put("eventGeneration", id.generation)
            put("sessionId", id.sessionId); put("revision", id.revision)
        }
        val media = buildJsonObject {
            put("connectionEpoch", epoch); put("partId", 4); put("initializationId", 2)
            put("initPath", "/live/${id.eventId}/abc123_init2.mp4"); put("partPath", "/live/${id.eventId}/abc123_part4.mp4")
            put("hasVideo", true); put("hasAudio", true)
        }.toMutableMap().also(mutateMedia)
        val origin = fields().toMutableMap().also {
            it["state"] = JsonPrimitive("live"); it["admission"] = JsonPrimitive("open"); it["protocol"] = JsonPrimitive("active")
            it["connectionEpoch"] = JsonPrimitive(epoch); it["connectionAttemptId"] = JsonPrimitive(nonce)
            it["freshMedia"] = JsonObject(media)
        }.also(mutateOrigin)
        return V4vSessionReceipt.read(buildJsonObject {
            for ((key, value) in fields()) put(key, value)
            put("phase", phase); put("expiresAtUnixMs", 1000); put("endComplete", false); put("origin", JsonObject(origin))
        })
    }
    @Test fun canonicalAttemptWireDoesNotMutateDurableGrantOrRefreshImmutables() {
        val original = grant(); val nonce = "1".repeat(32)
        val wire = V4vAttemptCorrelation { nonce }.begin(context(), original)
        assertEquals("#!::m=publish,r=live/event-fixture,u=${identity.sessionId},s=$password,forgesworn_attempt=$nonce", wire.streamId)
        assertEquals("publish:live/event-fixture:${identity.sessionId}:$password", original.streamId)
        assertEquals(passphrase, original.passphrase)
        assertTrue(grant(identity.copy(revision = 2)).permitsStartOf(original, 999999))
        assertFalse(wire.toString().contains(password)); assertFalse(wire.scope.toString().contains(identity.sessionId))
    }
    @Test fun malformedGrantAndNonceCannotBecomeAdditionalSrtAttributes() {
        for (bad in listOf("A".repeat(32), "0".repeat(31), "0".repeat(33), "0".repeat(30) + ",s")) {
            assertThrows(IllegalArgumentException::class.java) { V4vAttemptCorrelation.canonicalStreamId(grant(), bad) }
        }
        for (bad in listOf("#!::m=publish,r=live/event-fixture", "publish:live/event-other:${identity.sessionId}:$password",
            "publish:live/event-fixture:${"d".repeat(32)}:$password", "publish:live/event-fixture:${identity.sessionId}:$password,forgesworn_attempt=${"0".repeat(32)}",
            "publish:live/event-fixture:${identity.sessionId}:${"b".repeat(42)}")) {
            assertThrows(IllegalArgumentException::class.java) { V4vAttemptCorrelation.canonicalStreamId(grant(stream = bad), "0".repeat(32)) }
        }
        assertThrows(IllegalArgumentException::class.java) { V4vAttemptCorrelation.canonicalStreamId(grant(pass = password), "0".repeat(32)) }
    }
    @Test fun freshCryptographicNoncePerAttemptAndImmediateCollisionFailsClosed() {
        val correlation = V4vAttemptCorrelation(); val seen = mutableSetOf<String>()
        repeat(256) { index ->
            val scope = correlation.begin(context(attempt = index + 1L), grant()).scope
            assertTrue(scope.nonce.matches(Regex("[0-9a-f]{32}"))); assertTrue(seen.add(scope.nonce))
        }
        val fixed = V4vAttemptCorrelation { "0".repeat(32) }
        val first = fixed.begin(context(), grant()).scope
        assertThrows(IllegalStateException::class.java) { fixed.begin(context(attempt = 2), grant()) }
        assertSame(first, fixed.currentFor(context()))
    }
    @Test fun freshStatusMatchIsEncodedAvailabilityOnlyAndKeepsCapturedSdkContext() {
        val correlation = V4vAttemptCorrelation { "1".repeat(32) }
        val scope = correlation.begin(context(), grant()).scope
        val matched = checkNotNull(correlation.matchFreshMedia(scope, receipt(scope.nonce, epoch = 41)))
        assertSame(scope, matched.scope); assertEquals(SdkAttemptIdentity(1uL, 1uL), matched.scope.context.sdk)
        assertEquals(41L, matched.connectionEpoch); assertEquals(4L, matched.partId); assertTrue(matched.hasAudio)
        assertTrue(matched.toString().contains("not decoded"))
        assertNotNull(correlation.matchFreshMedia(scope, receipt(scope.nonce, id = identity.copy(revision = 2))))
    }
    @Test fun oldReplyCannotMatchRetryRotationStopEndOrAnotherSession() {
        var sequence = 0
        val correlation = V4vAttemptCorrelation { (++sequence).toString(16).padStart(32, '0') }
        val a = correlation.begin(context(), grant()).scope
        val replyA = receipt(a.nonce)
        val b = correlation.begin(context(attempt = 2), grant()).scope
        assertEquals(a.context.sdk.generation, b.context.sdk.generation)
        assertNull(correlation.matchFreshMedia(a, replyA)); assertNull(correlation.matchFreshMedia(b, replyA))
        val rotated = correlation.begin(context(epoch = 2, attempt = 3), grant()).scope
        assertNull(correlation.matchFreshMedia(b, receipt(b.nonce)))
        assertNull(correlation.matchFreshMedia(rotated, receipt(rotated.nonce, id = identity.copy(generation = 2))))
        assertNull(correlation.matchFreshMedia(rotated, receipt(rotated.nonce, id = identity.copy(sessionId = "d".repeat(32)))))
        correlation.clear() // Stop, End and grant-expiry clear the same bounded scope.
        assertNull(correlation.currentFor(rotated.context)); assertNull(correlation.matchFreshMedia(rotated, receipt(rotated.nonce)))
    }
    @Test fun missingNullOrUnacceptedOriginMetadataCannotConfirmAnAttempt() {
        val correlation = V4vAttemptCorrelation { "1".repeat(32) }; val scope = correlation.begin(context(), grant()).scope
        val changes: List<(MutableMap<String, JsonElement>) -> Unit> = listOf(
            { it.remove("connectionAttemptId"); it.remove("state") }, { it["connectionAttemptId"] = JsonNull },
            { it["freshMedia"] = JsonNull }, { it.remove("freshMedia") }, { it["protocol"] = JsonPrimitive("stopped") },
            { it["admission"] = JsonPrimitive("denied") }, { it["state"] = JsonPrimitive("connected") })
        for (change in changes) assertNull(correlation.matchFreshMedia(scope, receipt(scope.nonce, mutateOrigin = change)))
        assertNull(correlation.matchFreshMedia(scope, receipt(scope.nonce, phase = "ended")))
    }
    @Test fun strictDescriptorRejectsUnsafeEpochPathsPrefixIdsAndExtraFields() {
        val correlation = V4vAttemptCorrelation { "1".repeat(32) }; val scope = correlation.begin(context(), grant()).scope
        val changes: List<(MutableMap<String, JsonElement>) -> Unit> = listOf(
            { it["connectionEpoch"] = JsonPrimitive(2) }, { it["partId"] = JsonPrimitive(-1) },
            { it["partId"] = JsonPrimitive(V4vJson.MAX_SAFE + 1) }, { it["hasVideo"] = JsonPrimitive(false) },
            { it["initPath"] = JsonPrimitive("https://other.invalid/live/event-fixture/abc123_init2.mp4") },
            { it["initPath"] = JsonPrimitive("/live/event-other/abc123_init2.mp4") },
            { it["initPath"] = JsonPrimitive("/live/event-fixture/abc123_init.mp4") },
            { it["partPath"] = JsonPrimitive("/live/event-fixture/def456_part4.mp4") },
            { it["partPath"] = JsonPrimitive("/live/event-fixture/abc123_part5.mp4") },
            { it["partPath"] = JsonPrimitive("/live/event-fixture/abc123_part4.mp4?token=synthetic") },
            { it["partPath"] = JsonPrimitive("/live/event-fixture/%2e%2e/abc123_part4.mp4") }, { it["extra"] = JsonPrimitive(true) })
        for (change in changes) assertThrows(IllegalArgumentException::class.java) { correlation.matchFreshMedia(scope, receipt(scope.nonce, mutateMedia = change)) }
        assertThrows(IllegalArgumentException::class.java) { correlation.matchFreshMedia(scope, receipt(scope.nonce, epoch = 0)) }
        assertThrows(IllegalArgumentException::class.java) { correlation.matchFreshMedia(scope, receipt(scope.nonce, mutateOrigin = { it["connectionAttemptId"] = JsonPrimitive("F".repeat(32)) })) }
    }
    @Test fun initializationZeroAndNoAudioAreMetadataRatherThanAnInventedAudioProof() {
        val correlation = V4vAttemptCorrelation { "1".repeat(32) }; val scope = correlation.begin(context(), grant()).scope
        val matched = checkNotNull(correlation.matchFreshMedia(scope, receipt(scope.nonce, mutateMedia = {
            it["initializationId"] = JsonPrimitive(0); it["initPath"] = JsonPrimitive("/live/event-fixture/f_init.mp4");
            it["partPath"] = JsonPrimitive("/live/event-fixture/f_part4.mp4"); it["hasAudio"] = JsonPrimitive(false)
        })))
        assertFalse(matched.hasAudio); assertEquals(0L, matched.initializationId)
    }
    @Test fun retiredDestinationFactoryCannotReplaceTheNewDestinationsScope() {
        var oldNonce = 0
        val old = V4vAttemptCorrelation { (++oldNonce).toString(16).padStart(32, '0') }; old.begin(context(), grant()); old.clear()
        val fresh = V4vAttemptCorrelation { "2".repeat(32) }
        val current = fresh.begin(context().copy(destination = 2), grant()).scope
        old.begin(context(attempt = 2), grant()) // Late already-fenced old factory, isolated owner.
        assertSame(current, fresh.currentFor(current.context))
    }
}
