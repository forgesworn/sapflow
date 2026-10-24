package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.Callable
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

internal class V4vEndConfirmationTest {
    @Test fun cancelHasNoClaimAndCannotBeConfirmedLater() {
        val gate = V4vEndAdmission(true)
        val ticket = checkNotNull(gate.invitation())
        gate.cancel(ticket)
        assertNull(gate.confirm(ticket)); assertFalse(gate.ending())
        assertNotNull(gate.invitation())
    }
    @Test fun latestPromptAndExactGateAreRequired() {
        val gate = V4vEndAdmission(true)
        val old = checkNotNull(gate.invitation()); val fresh = checkNotNull(gate.invitation())
        gate.cancel(old)
        assertNull(gate.confirm(old))
        assertNull(V4vEndAdmission(true).confirm(fresh))
        assertNull(gate.confirm(V4vEndAdmission.Ticket()))
        assertNotNull(gate.confirm(fresh))
    }
    @Test fun claimIsOneShotAndBlocksSuccessorUntilActorCompletion() {
        val gate = V4vEndAdmission(true)
        val ticket = checkNotNull(gate.invitation()); val claim = checkNotNull(gate.confirm(ticket))
        assertTrue(gate.current(claim)); assertNull(gate.confirm(ticket))
        assertNull(gate.beginReplacement()); assertNull(gate.invitation())
        gate.complete(claim)
        assertFalse(gate.current(claim)); assertNull(gate.confirm(ticket))
        assertNotNull(gate.beginReplacement())
    }
    @Test fun replacementRetiresPromptBeforeAnyNativeOrNetworkWork() {
        val gate = V4vEndAdmission(true)
        val old = checkNotNull(gate.invitation()); val transition = checkNotNull(gate.beginReplacement())
        assertNull(gate.invitation()); assertNull(gate.confirm(old))
        gate.settle(transition, true)
        assertNull(gate.confirm(old)); assertNotNull(gate.invitation())
    }
    @Test fun failedPrepareRestoresFreshAuthorityWithoutResurrectingOldPrompt() {
        val gate = V4vEndAdmission(true)
        val old = checkNotNull(gate.invitation()); val transition = checkNotNull(gate.beginReplacement())
        gate.settle(transition, true)
        val fresh = checkNotNull(gate.invitation())
        assertNull(gate.confirm(old)); assertNotNull(gate.confirm(fresh))
    }
    @Test fun lateOldReplacementSettlementCannotOverwriteConfirmedSuccessor() {
        val gate = V4vEndAdmission(false)
        val first = checkNotNull(gate.beginReplacement()); gate.settle(first, true)
        val next = checkNotNull(gate.beginReplacement()); gate.settle(next, true)
        val claim = checkNotNull(gate.confirm(checkNotNull(gate.invitation())))
        gate.settle(first, false)
        assertTrue(gate.current(claim))
    }
    @Test fun closedGateRejectsEveryLateOperation() {
        val gate = V4vEndAdmission(true)
        val ticket = checkNotNull(gate.invitation()); val claim = checkNotNull(gate.confirm(ticket))
        gate.close(); gate.complete(claim)
        assertFalse(gate.current(claim)); assertNull(gate.invitation()); assertNull(gate.confirm(ticket))
        assertNull(gate.beginReplacement())
    }

    private val profile = checkNotNull(V4vProfile.approved("https://creator.invalid", "https://play.invalid", "/hls/"))
    private val event = "event-fixture"
    private class Store : V4vJournalStore {
        @Volatile var value: JsonObject? = null
        @Volatile var fail = false
        val writes = AtomicInteger()
        override fun load() = value
        override fun save(document: JsonObject) { check(!fail); value = document; writes.incrementAndGet() }
    }
    private class Hold(val method: String, val suffix: String) {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
    }
    private data class Sent(val method: String, val path: String, val body: JsonObject?)
    private inner class Harness {
        val store = Store(); val sent = CopyOnWriteArrayList<Sent>()
        val publisherFences = AtomicInteger(); val grants = AtomicInteger()
        @Volatile var hold: Hold? = null
        @Volatile var loseReply = false
        var sid = "1".repeat(32); var generation = 1L; private var revision = 0
        private var nonce = 0
        private fun receipt(phase: String = "active", complete: Boolean = false) = buildJsonObject {
            put("contractVersion", 1); put("eventId", event); put("eventGeneration", generation); put("sessionId", sid); put("revision", revision)
            put("expiresAtUnixMs", 601_000); put("phase", phase); put("endComplete", complete)
            put("origin", buildJsonObject {
                put("contractVersion", 1); put("eventId", event); put("eventGeneration", generation); put("sessionId", sid); put("revision", revision)
                put("admission", if (complete) "denied" else "open"); put("protocol", if (complete) "stopped" else "active")
                put("recording", if (complete) "finalised" else "active"); put("callbacks", if (complete) "delivered" else "pending")
            })
        }
        private fun grant() = buildJsonObject {
            for ((key, value) in receipt()) if (key in setOf("contractVersion", "eventId", "eventGeneration", "sessionId", "revision", "expiresAtUnixMs")) put(key, value)
            put("destination", buildJsonObject { put("kind", "v4v"); put("connector", "sapflow"); put("unlimited", true); put("watermark", false); put("subscriptionRequired", false) })
            put("transport", "srt"); put("endpoint", "srt://192.0.2.1:8890"); put("streamId", "publish:live/$event:$sid:${"a".repeat(43)}")
            put("encryption", buildJsonObject { put("cipher", "aes256-ctr"); put("passphrase", "b".repeat(43)) })
            put("refreshAfterUnixMs", 301_000); put("reconnectGraceMs", 90_000)
            put("limits", buildJsonObject { put("maxPublishBps", 4_000_000); put("maxQueueBytes", 524288) })
        }
        private val api = object : V4vApi {
            override fun request(method: String, path: String, bearer: String?, body: JsonObject?): JsonObject {
                sent += Sent(method, path, body)
                hold?.takeIf { it.method == method && path.endsWith(it.suffix) }?.let {
                    it.entered.countDown(); check(it.release.await(5, TimeUnit.SECONDS))
                }
                if (method == "GET" && path.endsWith("native-session")) return buildJsonObject {
                    put("session", buildJsonObject { put("eventId", event); put("eventGeneration", generation) })
                }
                if (method == "POST") {
                    check(store.value?.get("pending") is JsonObject) { "Mutation requires the actual durable request" }
                    if (loseReply) throw V4vHttpFailure()
                    val end = path.endsWith("/end")
                    revision = if (path.endsWith("native-session")) 1 else revision + 1
                    return buildJsonObject { put("ok", true); put("session", receipt(if (end) "ended" else "active", end)); if (!end) put("grant", grant()) }
                }
                return buildJsonObject { put("ok", true); put("session", receipt()) }
            }
        }
        fun signed() = V4vSessionEngine(profile, api, store, { 1000L }, { (++nonce).toString(16).padStart(32, '0') },
            { publisherFences.incrementAndGet() }, { grants.incrementAndGet() }).also { it.installLogin("a".repeat(64), "synthetic-token", 86400) }
    }
    private fun waitFor(latch: CountDownLatch) = assertTrue("Bounded actor completion", latch.await(5, TimeUnit.SECONDS))
    private fun actor(engine: V4vSessionEngine): V4vDestinationSession {
        val actor = V4vDestinationSession { engine }
        val ready = CountDownLatch(1); val failure = AtomicReference<Exception?>()
        actor.submit({}) { failure.set(it); ready.countDown() }
        waitFor(ready); assertNull(failure.get())
        return actor
    }
    private fun close(actor: V4vDestinationSession) { actor.close(); assertTrue(actor.awaitClosed(5000)) }
    private fun grantSession(actor: V4vDestinationSession): String {
        val done = CountDownLatch(1); val result = AtomicReference<String?>(); val error = AtomicReference<Exception?>()
        actor.submit({ result.set(captureGrant().identity.sessionId) }) { error.set(it); done.countDown() }
        waitFor(done); assertNull(error.get()); return checkNotNull(result.get())
    }

    @Test fun cancelledConfirmationHasNoFenceSaveOrHttpOnActualEngine() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        try {
            val ticket = checkNotNull(actor.endInvitation()); actor.cancelEndInvitation(ticket)
            val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
            assertFalse(actor.endDestination(ticket, { fail("Cancelled action must not reach capture or publishing") }))
            assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
            assertTrue(actor.view.prepared)
        } finally { close(actor) }
    }
    @Test fun staleSameEventSessionCannotFenceSaveOrSendAgainstSuccessor() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event)
        val old = checkNotNull(engine.endAdmission.invitation())
        engine.end(); h.sid = "2".repeat(32); h.generation = 2; engine.prepare(event)
        val actor = actor(engine)
        try {
            val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
            assertFalse(actor.endDestination(old, { fail("Old Activity action must not fence successor") }))
            assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
            assertEquals(h.sid, grantSession(actor))
        } finally { close(actor) }
    }
    @Test fun replacementFirstGetAlreadyRetiresOldPromptWithoutWaitingForSave() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event)
        val old = checkNotNull(engine.endAdmission.invitation()); engine.end()
        h.sid = "2".repeat(32); h.generation = 2
        val actor = actor(engine); val hold = Hold("GET", "native-session"); h.hold = hold
        val done = CountDownLatch(1)
        try {
            actor.submit({ prepare(event) }) { assertNull(it); done.countDown() }
            waitFor(hold.entered)
            val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
            assertFalse(actor.endDestination(old, { fail("Successor GET has retired old authority") }))
            assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
            hold.release.countDown(); waitFor(done)
            assertEquals(h.sid, grantSession(actor))
        } finally { hold.release.countDown(); close(actor) }
    }
    @Test fun confirmBehindRefreshUsesSameOwnerAndLatestAcknowledgedRevision() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        val hold = Hold("POST", "/refresh"); h.hold = hold; val endDone = CountDownLatch(1); val refreshDone = CountDownLatch(1)
        val pendingCaptureFences = AtomicInteger()
        try {
            val ticket = checkNotNull(actor.endInvitation())
            actor.submit({ refresh() }) { assertNull(it); refreshDone.countDown() }; waitFor(hold.entered)
            assertTrue(actor.endDestination(ticket, { pendingCaptureFences.incrementAndGet() }, { assertNull(it); endDone.countDown() }))
            assertEquals(1, pendingCaptureFences.get())
            assertTrue(engine.endAdmission.ending())
            hold.release.countDown(); waitFor(refreshDone); waitFor(endDone)
            val end = h.sent.single { it.path.endsWith("/end") }
            assertEquals(2L, end.body!!.getValue("revision").jsonPrimitive.long)
            assertTrue(actor.view.endComplete)
            assertFalse(actor.endDestination(ticket, { fail("Duplicate cannot fence") }))
            assertEquals(1, h.sent.count { it.path.endsWith("/end") })
        } finally { hold.release.countDown(); close(actor) }
    }
    @Test fun confirmationDuringUncertainPrepareEndsOnlyItsSavedRequestAndNeverAdoptsGrant() {
        val h = Harness(); val engine = h.signed(); val actor = actor(engine)
        val hold = Hold("POST", "native-session"); h.hold = hold; val prepareDone = CountDownLatch(1); val endDone = CountDownLatch(1)
        try {
            actor.submit({ prepare(event) }) { assertNull(it); prepareDone.countDown() }; waitFor(hold.entered)
            val prepare = h.sent.single { it.method == "POST" }
            val saved = h.store.value!!.getValue("pending").jsonObject.getValue("body").jsonObject
            assertEquals(prepare.body, saved)
            val ticket = checkNotNull(actor.endInvitation())
            assertTrue(actor.endDestination(ticket, completed = { assertNull(it); endDone.countDown() }))
            hold.release.countDown(); waitFor(prepareDone); waitFor(endDone)
            assertEquals(0, h.grants.get())
            assertEquals(1, h.sent.count { it.method == "POST" && it.path.endsWith("native-session") })
            assertTrue(h.sent.single { it.path.endsWith("/end") }.path.contains(h.sid))
            assertTrue(actor.view.endComplete)
        } finally { hold.release.countDown(); close(actor) }
    }
    @Test fun uncertainRefreshAndEndRetainOriginalNonceBodyAcrossConfirmedEnd() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); h.loseReply = true; engine.refresh()
        val original = h.sent.last(); val actor = actor(engine); val done = CountDownLatch(1)
        try {
            assertTrue(actor.endDestination(checkNotNull(actor.endInvitation()), completed = { assertNull(it); done.countDown() })); waitFor(done)
            assertTrue(h.store.value!!.getValue("terminalIntent").jsonPrimitive.boolean)
            assertEquals(original, h.sent.last()); assertTrue(actor.view.pending)
            h.loseReply = false; val retryDone = CountDownLatch(1)
            actor.submit({ retry() }) { assertNull(it); retryDone.countDown() }; waitFor(retryDone)
            val refreshes = h.sent.filter { it.path.endsWith("/refresh") }
            assertEquals(listOf(original, original, original), refreshes)
            assertEquals(1, h.sent.count { it.path.endsWith("/end") }); assertTrue(actor.view.endComplete)
        } finally { close(actor) }
    }
    @Test fun closedActorRejectsRetainedPromptWithoutCallerFence() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        val ticket = checkNotNull(actor.endInvitation()); close(actor)
        val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
        assertFalse(actor.endDestination(ticket, { fail("Closed Activity owner must not fence") }))
        assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
    }
    @Test fun manualSuccessorInSameActivityCannotReachEndOrAnyLocalFence() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        try {
            val ticket = checkNotNull(actor.endInvitation())
            val activity = Any(); val v4vPending = Any(); val v4vCapture = Any()
            val scope = V4vEndUiScope(activity, 7, v4vPending, v4vCapture)
            val manualPending = Any(); val manualCapture = Any()
            val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
            // Switching to manual and then back does not revive the old request.
            for (selected in listOf(false, true)) {
                assertFalse(scope.confirmIfCurrent(activity, 8, manualPending, manualCapture, selected, true, true) {
                    actor.endDestination(ticket, { fail("Manual successor must never be cancelled or stopped") })
                })
            }
            assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
            assertTrue(actor.view.prepared)
        } finally { close(actor) }
    }
    @Test fun staleActivityOrForegroundDepartureCannotReachExactSessionClaim() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        try {
            val ticket = checkNotNull(actor.endInvitation()); val activity = Any(); val capture = Any()
            val scope = V4vEndUiScope(activity, 7, null, capture)
            val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
            for ((client, foreground, alive) in listOf(Triple(Any(), true, true), Triple(activity, false, true), Triple(activity, true, false))) {
                assertFalse(scope.confirmIfCurrent(client, 7, null, capture, true, foreground, alive) {
                    actor.endDestination(ticket, { fail("Stale Activity action must have no fence") })
                })
            }
            assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
        } finally { close(actor) }
    }
    @Test fun actorClaimRevalidationPrecedesEveryFenceAndDurableMutation() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event)
        val claim = checkNotNull(engine.endAdmission.confirm(checkNotNull(engine.endAdmission.invitation())))
        assertThrows(IllegalStateException::class.java) { engine.captureGrant() }
        engine.endAdmission.complete(claim)
        val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
        assertThrows(IllegalStateException::class.java) { engine.endConfirmed(claim) }
        assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
    }

    @Test fun confirmedEndJournalFailureCannotSendHttpOrReuseItsOwner() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); val actor = actor(engine)
        try {
            val ticket = checkNotNull(actor.endInvitation()); val done = CountDownLatch(1)
            val localFence = AtomicInteger(); val error = AtomicReference<Exception?>()
            val requests = h.sent.size; val writes = h.store.writes.get()
            h.store.fail = true
            assertTrue(actor.endDestination(ticket, { localFence.incrementAndGet() }, { error.set(it); done.countDown() }))
            waitFor(done)
            assertNotNull(error.get()); assertEquals(1, localFence.get())
            assertEquals(requests, h.sent.size); assertEquals(writes, h.store.writes.get())
            assertNull(actor.endInvitation()); assertFalse(actor.view.prepared)
            assertFalse(actor.endDestination(ticket, { fail("Failed journal owner cannot be reused") }))
        } finally { close(actor) }
    }

    @Test fun confirmedEndPreventsAnOtherwiseLegalSuccessorBeforeFirstGet() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event); engine.end()
        val claim = checkNotNull(engine.endAdmission.confirm(checkNotNull(engine.endAdmission.invitation())))
        val writes = h.store.writes.get(); val requests = h.sent.size; val fences = h.publisherFences.get()
        assertThrows(IllegalStateException::class.java) { engine.prepare(event) }
        assertEquals(writes, h.store.writes.get()); assertEquals(requests, h.sent.size); assertEquals(fences, h.publisherFences.get())
        assertTrue(engine.endAdmission.current(claim)); engine.endAdmission.complete(claim)
    }
    @Test fun lostPrepareHasNoGuessedSidAndConfirmedEndResolvesOnlyOriginalNonce() {
        val h = Harness(); val engine = h.signed(); h.loseReply = true; engine.prepare(event)
        val original = h.sent.last(); val actor = actor(engine); val done = CountDownLatch(1)
        try {
            assertNull(actor.view.eventId); assertTrue(actor.view.pending)
            assertTrue(actor.endDestination(checkNotNull(actor.endInvitation()), completed = { assertNull(it); done.countDown() })); waitFor(done)
            assertTrue(h.sent.none { it.path.endsWith("/end") }); assertEquals(original, h.sent.last())
            assertTrue(h.store.value!!.getValue("terminalIntent").jsonPrimitive.boolean)
            h.loseReply = false; val retryDone = CountDownLatch(1)
            actor.submit({ retry() }) { assertNull(it); retryDone.countDown() }; waitFor(retryDone)
            val prepareRequests = h.sent.filter { it.method == "POST" && it.path.endsWith("native-session") }
            assertEquals(listOf(original, original, original), prepareRequests)
            assertEquals(0, h.grants.get()); assertTrue(actor.view.endComplete)
            assertTrue(h.sent.single { it.path.endsWith("/end") }.path.endsWith("/" + h.sid + "/end"))
        } finally { close(actor) }
    }

    /** Both Java scheduling overloads keep their actual task and delay. Barriers
     * expose the real arm()/close() gap; no fake shutdown or timer is substituted. */
    private class HeldAlarmExecutor : ScheduledThreadPoolExecutor(1) {
        val firstScheduled = CountDownLatch(1)
        val pauseNext = AtomicBoolean()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val shutdownReturned = CountDownLatch(1)
        val heldFuture = AtomicReference<ScheduledFuture<*>?>()
        private fun beforeSchedule(delay: Long): Boolean {
            if (delay <= 0 || !pauseNext.compareAndSet(true, false)) return false
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); return true
        }
        override fun schedule(command: Runnable, delay: Long, unit: TimeUnit): ScheduledFuture<*> {
            val held = beforeSchedule(delay)
            val future = super.schedule(command, delay, unit)
            if (held) heldFuture.set(future)
            if (delay > 0) firstScheduled.countDown(); return future
        }
        override fun <V> schedule(command: Callable<V>, delay: Long, unit: TimeUnit): ScheduledFuture<V> {
            val held = beforeSchedule(delay)
            val future = super.schedule(command, delay, unit)
            if (held) heldFuture.set(future)
            if (delay > 0) firstScheduled.countDown(); return future
        }
        override fun shutdown() {
            super.shutdown()
            shutdownReturned.countDown()
        }
    }
    @Test fun closeCancelsActualAlarmInsertedAfterItsEarlierCancellation() {
        val h = Harness(); val engine = h.signed(); engine.prepare(event)
        val executor = HeldAlarmExecutor()
        val actor = V4vDestinationSession(controlExecutor = executor) { engine }
        try {
            waitFor(executor.firstScheduled)
            val requests = h.sent.size
            executor.pauseNext.set(true)
            actor.submit({}) { assertNull(it) }
            // arm() has passed !closed and is about to call the real scheduler.
            waitFor(executor.entered)
            actor.close()
            executor.release.countDown()
            // This is actual super.shutdown(), reached by the queued owner cleanup.
            waitFor(executor.shutdownReturned)
            val raced = checkNotNull(executor.heldFuture.get())
            assertTrue("Shutdown must cancel the new delayed alarm, not retain it until its deadline", raced.isCancelled)
            assertTrue(actor.awaitClosed(5000))
            assertEquals(requests, h.sent.size)
        } finally {
            executor.release.countDown(); actor.close()
            // Failure-only test cleanup cannot make the preceding cancellation assertion pass.
            executor.heldFuture.get()?.cancel(false)
            executor.shutdownNow(); assertTrue(executor.awaitTermination(5000, TimeUnit.MILLISECONDS))
        }
    }

    @Test fun multiThreadTransferredExecutorIsRejectedAndClosedBeforeEngineCreation() {
        val executor = ScheduledThreadPoolExecutor(2)
        val factories = AtomicInteger()
        try {
            assertThrows(IllegalArgumentException::class.java) {
                V4vDestinationSession(controlExecutor = executor) {
                    factories.incrementAndGet(); error("A rejected executor must not create an engine")
                }
            }
            assertEquals(0, factories.get())
            assertTrue(executor.isShutdown)
            assertTrue(executor.awaitTermination(5000, TimeUnit.MILLISECONDS))
        } finally {
            executor.shutdownNow(); assertTrue(executor.awaitTermination(5000, TimeUnit.MILLISECONDS))
        }
    }

}
