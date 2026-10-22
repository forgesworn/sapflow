package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test

internal class V4vOriginObserverTest {
    private val profile = checkNotNull(V4vProfile.approved("https://api.example.invalid", "https://play.example.invalid", "/live/"))
    private val identity = V4vIdentity("event-fixture", 1, "a".repeat(32), 1)
    private fun scope(attempt: Long = 1) = V4vCapturedAttempt(PublishingAttemptContext(1, 1, attempt, SdkAttemptIdentity(1uL, attempt.toULong())), identity, attempt.toString().padStart(32, '0'))
    private fun receipt() = V4vSessionReceipt.read(buildJsonObject {
        put("contractVersion", 1); put("eventId", identity.eventId); put("eventGeneration", 1); put("sessionId", identity.sessionId)
        put("revision", 1); put("phase", "active"); put("expiresAtUnixMs", 1000); put("endComplete", false)
    })
    private fun boundary(scope: V4vCapturedAttempt, part: Long = 1, init: Long = 0, epoch: Long = 1, audio: Boolean = true) =
        V4vMatchedMedia(scope, epoch, part, init, "/live/event-fixture/abc_init${if (init == 0L) "" else init}.mp4", "/live/event-fixture/abc_part$part.mp4", audio)
    private fun proof() = NativeOriginEvidence(NativeTrackEvidence("hardware-avc", false, 2, 0, 33333, 1024), NativeTrackEvidence("native-aac", true, 2, 0, 21333, 4096))
    private class Io(private val answer: () -> V4vSessionReceipt) : OriginObservationResources {
        val closed = CountDownLatch(1); val reads = AtomicInteger()
        override fun status() = answer()
        override fun get(path: String, max: Int, cancel: OriginProbeCancellation): ByteArray { reads.incrementAndGet(); cancel.check(); return byteArrayOf(1) }
        override fun close() { closed.countDown() }
    }
    @Test fun onlyDecodedBothTracksAndFreshSameInitializationCanConfirm() {
        val scope = scope(); val decoded = AtomicInteger(); val confirmed = CountDownLatch(1)
        val io = Io(::receipt); var matches = 0
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
            { _, _ -> boundary(scope, part = (++matches).toLong()) }, { actual, evidence, _ -> assertSame(scope, actual); assertTrue(evidence.audio.software); confirmed.countDown() },
            OriginDecoder { _, _, _, _ -> decoded.incrementAndGet(); proof() }, { io })
        try { observer.accepted(scope, CaptureProfile.HD_720); assertTrue(confirmed.await(2, TimeUnit.SECONDS)); assertEquals(1, decoded.get()); assertEquals(2, io.reads.get()) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun changedCodecOrServerConnectionAfterDecodeCannotConfirm() {
        for (changeInit in listOf(true, false)) {
            val scope = scope(); val calls = AtomicInteger(); val confirmations = AtomicInteger(); val io = Io(::receipt)
            val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
                { _, _ -> if (calls.incrementAndGet() == 1) boundary(scope) else boundary(scope, init = if (changeInit) 1 else 0, epoch = if (changeInit) 1 else 2) },
                { _, _, _ -> confirmations.incrementAndGet() }, OriginDecoder { _, _, _, _ -> proof() }, { io })
            try { observer.accepted(scope, CaptureProfile.HD_720); assertTrue(io.closed.await(2, TimeUnit.SECONDS)); assertEquals(0, confirmations.get()) }
            finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
        }
    }
    @Test fun cancelledNativeOwnerRetainsCapacityUntilRealCleanupAndDropsOldReply() {
        val a = scope(); val b = scope(2); val current = AtomicReference(a)
        val entered = CountDownLatch(1); val release = CountDownLatch(1); val finished = CountDownLatch(1)
        val creations = AtomicInteger(); val confirmed = CountDownLatch(1); val observed = AtomicReference<V4vCapturedAttempt?>(); val io = Io(::receipt)
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { current.get() === it },
            { value, _ -> boundary(value) }, { value, _, _ -> observed.set(value); confirmed.countDown() },
            OriginDecoder { _, _, _, _ -> entered.countDown(); try { check(release.await(2, TimeUnit.SECONDS)); proof() } finally { finished.countDown() } },
            { if (creations.incrementAndGet() == 1) io else Io(::receipt) })
        try {
            observer.accepted(a, CaptureProfile.HD_720); assertTrue(entered.await(2, TimeUnit.SECONDS))
            current.set(b); observer.cancel(a.context); assertTrue(io.closed.await(1, TimeUnit.SECONDS))
            observer.accepted(b, CaptureProfile.HD_720); assertEquals(1, creations.get()); assertTrue(observer.diagnostics.contains("previous cleanup"))
            release.countDown(); assertTrue(finished.await(2, TimeUnit.SECONDS))
            assertTrue(confirmed.await(2, TimeUnit.SECONDS)); assertSame(b, observed.get()); assertEquals(2, creations.get())
        } finally { release.countDown(); observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun endOrExpiryWhileWaitingForControlPermitDoesNotWaitForHttpActor() {
        val pending = AtomicReference<((V4vObservationPermit?) -> Unit)?>(); val requested = CountDownLatch(1)
        val scope = scope(); val confirms = AtomicInteger(); val reads = AtomicInteger()
        val observer = V4vOriginObserver(profile, { _, answer -> pending.set(answer); requested.countDown() }, { true }, { _, _ -> boundary(scope) },
            { _, _, _ -> confirms.incrementAndGet() }, OriginDecoder { _, _, _, _ -> error("not reached") }, { reads.incrementAndGet(); Io(::receipt) })
        observer.accepted(scope, CaptureProfile.HD_720); assertTrue(requested.await(2, TimeUnit.SECONDS))
        observer.cancel(); observer.close(); assertTrue(observer.awaitStopped(1000))
        pending.get()?.invoke(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE))
        assertEquals(0, confirms.get()); assertEquals(0, reads.get())
    }
    @Test fun metadataWithoutAacNeverInvokesDecoderAndFailureDoesNotFailTransport() {
        val scope = scope(); val io = Io(::receipt); val confirms = AtomicInteger()
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
            { _, _ -> boundary(scope, audio = false) }, { _, _, _ -> confirms.incrementAndGet() }, OriginDecoder { _, _, _, _ -> error("must not decode") }, { io })
        try { observer.accepted(scope, CaptureProfile.HD_720); assertTrue(io.closed.await(2, TimeUnit.SECONDS)); assertEquals(0, io.reads.get()); assertEquals(0, confirms.get()) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun actualDecoderOrCleanupFailureCannotReachConfirmation() {
        val scope = scope(); val io = Io(::receipt); val confirms = AtomicInteger()
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
            { _, _ -> boundary(scope) }, { _, _, _ -> confirms.incrementAndGet() }, OriginDecoder { _, _, _, _ -> throw IllegalStateException("synthetic native cleanup failed") }, { io })
        try { observer.accepted(scope, CaptureProfile.HD_720); assertTrue(io.closed.await(2, TimeUnit.SECONDS)); assertEquals(0, confirms.get()) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun failedNativeCleanupDisablesNewProbeRatherThanRelyingOnGarbageCollection() {
        val scope = scope(); val io = Io(::receipt); val creations = AtomicInteger(); val confirms = AtomicInteger()
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
            { value, _ -> boundary(value) }, { _, _, _ -> confirms.incrementAndGet() },
            OriginDecoder { _, _, _, _ -> throw OriginProbeCleanupFailure(IllegalStateException("synthetic release failure"), listOf(Any())) }, { creations.incrementAndGet(); io })
        try {
            observer.accepted(scope, CaptureProfile.HD_720); assertTrue(io.closed.await(2, TimeUnit.SECONDS))
            observer.accepted(scope(2), CaptureProfile.HD_720)
            assertEquals(1, creations.get()); assertEquals(0, confirms.get()); assertTrue(observer.diagnostics.contains("previous cleanup"))
        } finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun slowHttpDisconnectCannotBlockCancellationOrStartTheNextNativeOwner() {
        val a = scope(); val b = scope(2); val current = AtomicReference(a)
        val decoding = CountDownLatch(1); val releaseDecode = CountDownLatch(1)
        val closing = CountDownLatch(1); val releaseClose = CountDownLatch(1); val confirms = CountDownLatch(1)
        val creations = AtomicInteger(); val decodes = AtomicInteger()
        val slow = object : OriginObservationResources {
            override fun status() = receipt()
            override fun get(path: String, max: Int, cancel: OriginProbeCancellation) = byteArrayOf(1)
            override fun close() { closing.countDown(); check(releaseClose.await(2, TimeUnit.SECONDS)) }
        }
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { current.get() === it },
            { value, _ -> boundary(value) }, { value, _, _ -> assertSame(b, value); confirms.countDown() },
            OriginDecoder { _, _, _, _ -> if (decodes.incrementAndGet() == 1) { decoding.countDown(); check(releaseDecode.await(2, TimeUnit.SECONDS)) }; proof() },
            { if (creations.incrementAndGet() == 1) slow else Io(::receipt) })
        try {
            observer.accepted(a, CaptureProfile.HD_720); assertTrue(decoding.await(2, TimeUnit.SECONDS))
            current.set(b); observer.cancel(a.context) // Returns even while the cancellation owner blocks.
            assertTrue(closing.await(1, TimeUnit.SECONDS)); assertEquals(1L, releaseClose.count)
            observer.accepted(b, CaptureProfile.HD_720); releaseDecode.countDown()
            assertEquals(1, creations.get()); assertEquals(1, decodes.get())
            releaseClose.countDown(); assertTrue(confirms.await(2, TimeUnit.SECONDS)); assertEquals(2, creations.get())
        } finally { releaseClose.countDown(); releaseDecode.countDown(); observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun busyControlAdmissionRetriesWithinTheSameAcceptedAttempt() {
        val scope = scope(); val requests = AtomicInteger(); val confirmed = CountDownLatch(1)
        val observer = V4vOriginObserver(profile, { _, answer -> answer(if (requests.incrementAndGet() == 1) null else V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) },
            { true }, { _, _ -> boundary(scope) }, { _, _, _ -> confirmed.countDown() }, OriginDecoder { _, _, _, _ -> proof() }, { Io(::receipt) })
        try { observer.accepted(scope, CaptureProfile.HD_720); assertTrue(confirmed.await(2, TimeUnit.SECONDS)); assertEquals(2, requests.get()) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun statusAuthorityExpiryAfterHttpOrInvalidEnvelopeCannotSupplyFreshProof() {
        var now = 1L
        val permit = V4vObservationPermit(identity, "synthetic", 10) { now }
        var ok = true; var late = true
        val api = object : V4vApi {
            override fun request(method: String, path: String, bearer: String?, body: kotlinx.serialization.json.JsonObject?): kotlinx.serialization.json.JsonObject {
                if (late) now = 10
                return buildJsonObject { put("ok", ok); put("session", receipt().raw) }
            }
            override fun close() {}
        }
        assertThrows(IllegalStateException::class.java) { permit.status(api) }
        now = 1; late = false; ok = false
        assertThrows(IllegalArgumentException::class.java) { permit.status(api) }
        ok = true; assertEquals(identity, permit.status(api).identity)
    }
    @Test fun uncertainHttpCloseRetainsCapacityAndNeverConfirmsDecodedEvidence() {
        val scope = scope(); val closing = CountDownLatch(1); val creates = AtomicInteger(); val confirmations = AtomicInteger()
        val io = object : OriginObservationResources {
            override fun status() = receipt()
            override fun get(path: String, max: Int, cancel: OriginProbeCancellation) = byteArrayOf(1)
            override fun close() { closing.countDown(); throw IllegalStateException("synthetic disconnect failure") }
        }
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) }, { true },
            { value, _ -> boundary(value) }, { _, _, _ -> confirmations.incrementAndGet() }, OriginDecoder { _, _, _, _ -> proof() }, { creates.incrementAndGet(); io })
        try {
            observer.accepted(scope, CaptureProfile.HD_720); assertTrue(closing.await(2, TimeUnit.SECONDS))
            observer.accepted(scope(2), CaptureProfile.HD_720)
            assertEquals(1, creates.get()); assertEquals(0, confirmations.get())
        } finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun audioOnlyConfirmsNativePcmAfterActualCleanupWithoutVideoEvidence() {
        val original = scope(); val scope = V4vCapturedAttempt(original.context, identity, original.nonce, CaptureIntent.AudioOnly48kMono)
        val confirmed = CountDownLatch(1); val io = Io(::receipt)
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) },
            { true }, { _, _ -> V4vMatchedMedia(scope, 1, 1, 0, "/live/event-fixture/abc_init.mp4", "/live/event-fixture/abc_part1.mp4", true, false) },
            { _, evidence, _ -> assertNull(evidence.video); assertEquals(0L, io.closed.count); confirmed.countDown() },
            OriginDecoder { _, _, expected, _ ->
                assertSame(CaptureIntent.AudioOnly48kMono, expected)
                val pcm = DecodedProbeProgress(false, 0); pcm.output(4096, 0)
                NativeOriginEvidence(null, pcm.evidence("native-aac", true))
            }, { io })
        try { observer.accepted(scope, scope.intent); assertTrue(confirmed.await(2, TimeUnit.SECONDS)) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
    @Test fun audioOnlyRefusesAnUnexpectedDecodedVideoTrack() {
        val original = scope(); val scope = V4vCapturedAttempt(original.context, identity, original.nonce, CaptureIntent.AudioOnly48kMono)
        val confirms = AtomicInteger(); val io = Io(::receipt)
        val observer = V4vOriginObserver(profile, { _, answer -> answer(V4vObservationPermit(identity, "synthetic", Long.MAX_VALUE)) },
            { true }, { _, _ -> V4vMatchedMedia(scope, 1, 1, 0, "/live/event-fixture/abc_init.mp4", "/live/event-fixture/abc_part1.mp4", true, false) },
            { _, _, _ -> confirms.incrementAndGet() }, OriginDecoder { _, _, _, _ -> proof() }, { io })
        try { observer.accepted(scope, scope.intent); assertTrue(io.closed.await(2, TimeUnit.SECONDS)); assertEquals(0, confirms.get()) }
        finally { observer.close(); assertTrue(observer.awaitStopped(2000)) }
    }
}
