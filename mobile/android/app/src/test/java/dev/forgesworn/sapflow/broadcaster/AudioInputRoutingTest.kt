package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

class AudioInputRoutingTest {
    private val a = requireNotNull(AudioInputOption.create("a", "Built-in", AudioInputOption.Kind.BUILT_IN))
    private val b = requireNotNull(AudioInputOption.create("b", "USB", AudioInputOption.Kind.USB))
    private fun owner(epoch: Long = 1): AudioInputOwner = AudioInputOwner(requireNotNull(CaptureStartAdmission().reserve()), epoch)
    @Test fun preferenceIsNotActualAndMismatchFailsOnce() {
        var actual: String? = "b"; var failures = 0
        val route = AudioInputRouting(owner(), AudioInputPin("a"), listOf(a,b), { actual }, { failures++ }, {})
        assertFalse(route.permitsPcm()); assertFalse(route.verify())
        actual = "a"; assertFalse(route.verify()); assertEquals(1, failures)
    }
    @Test fun defaultPinSurvivesRotationAndOldCallbacksCannotRepin() {
        val first = owner(); val pin = AudioInputPin(null); var actual: String? = "a"
        val old = AudioInputRouting(first, pin, listOf(a,b), { actual }, {}, {})
        assertTrue(old.verify()); old.close(); actual = "b"
        val next = AudioInputRouting(AudioInputOwner(first.preparation, 2), pin, listOf(a,b), { actual }, {}, {})
        assertFalse(next.verify()); assertFalse(old.verify()); assertEquals("a", pin.resolvedId())
    }
    @Test fun notificationsBeforeStartAreNotRoutedProofThenDisconnectClosesGate() {
        var actual: String? = null; var failures = 0
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { actual }, { failures++ }, {})
        route.routeChanged(); assertEquals(0, failures); assertFalse(route.permitsPcm())
        actual = "a"; assertTrue(route.verify()); route.routeChanged(); assertTrue(route.permitsPcm())
        actual = null; route.routeChanged(); assertFalse(route.permitsPcm()); assertTrue(route.hasFailed()); assertEquals(1, failures)
    }
    @Test fun closedEpochNeverCallsFailureAndNewPrepareCanChooseAfresh() {
        var actual: String? = "a"; var failures = 0
        val old = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { actual }, { failures++ }, {})
        assertTrue(old.verify()); old.close(); actual = null; old.routeChanged(); assertFalse(old.verify()); assertEquals(0, failures)
        assertTrue(AudioInputRouting(owner(), AudioInputPin(null), listOf(b), { "b" }, {}, {}).verify())
    }
    @Test fun selectedDeviceRemovedWhilePreparedFencesStartWithoutFallback() {
        var failures = 0
        val route = AudioInputRouting(owner(), AudioInputPin("a"), listOf(a,b), { "a" }, { failures++ }, {})
        route.removed(listOf("b")); assertFalse(route.hasFailed())
        route.removed(listOf("a")); assertFalse(route.verify()); assertEquals(1, failures)
    }

    @Test fun normalStopBeforeHeldReaderPermitPreservesTailAndRotation() {
        val running = AtomicBoolean(true); val beforePermit = CountDownLatch(1); val resume = CountDownLatch(1)
        val readerFailed = AtomicBoolean(false); val admitted = AtomicBoolean(true)
        var routeFailures = 0
        val first = owner(); val pin = AudioInputPin(null)
        val route = AudioInputRouting(first, pin, listOf(a), { "a" }, { routeFailures++ }, {})
        assertTrue(route.verify())
        val reader = Thread {
            beforePermit.countDown()
            if (!resume.await(2, TimeUnit.SECONDS)) { readerFailed.set(true); return@Thread }
            try { admitted.set(route.admitReader(running)) } catch (_: IllegalStateException) { readerFailed.set(true) }
        }
        reader.start(); assertTrue(beforePermit.await(2, TimeUnit.SECONDS))
        assertFalse(route.stopProducer(running)); resume.countDown(); reader.join(2000)
        assertFalse(reader.isAlive); assertFalse(readerFailed.get()); assertFalse(admitted.get())
        assertFalse(route.hasFailed()); assertEquals(0, routeFailures)
        // Same logical Prepare/pin may now create the orientation successor.
        assertTrue(AudioInputRouting(AudioInputOwner(first.preparation, 2), pin, listOf(a), { "a" }, {}, {}).verify())
    }
    @Test fun realDisconnectStillFailsReaderAdmission() {
        val running = AtomicBoolean(true); var actual: String? = "a"
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { actual }, {}, {})
        assertTrue(route.verify()); actual = null; route.routeChanged()
        assertThrows(IllegalStateException::class.java) { route.admitReader(running) }
    }

    @Test fun latchedDisconnectSurvivesStopBeforeQueuedFailureWithEmptyPump() {
        val running = AtomicBoolean(true); var actual: String? = "a"
        val queuedFailures = java.util.ArrayDeque<() -> Unit>()
        var delivered = false
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { actual },
            { queuedFailures.add { delivered = true } }, {})
        assertTrue(route.verify()); actual = null; route.routeChanged()
        assertFalse(delivered); assertEquals(1, queuedFailures.size)
        // Normal Stop reaches the control owner before that queued failure.
        val stopMustFail = route.stopProducer(running)
        assertTrue(stopMustFail); assertFalse(running.get())
        // This sticky result drives failed cleanup (no rotation/record finish),
        // independently of the empty PCM pump and a suppressed reader catch.
        assertTrue(route.hasFailed())
        assertThrows(IllegalStateException::class.java) { route.admitReader(running) }
        queuedFailures.removeFirst().invoke(); assertTrue(delivered)
    }

    @Test fun closeDuringNativeReadRejectsLateObservation() {
        val entered = CountDownLatch(1); val resume = CountDownLatch(1); val closed = CountDownLatch(1)
        val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val pin = AudioInputPin(null)
        val route = AudioInputRouting(owner(), pin, listOf(a), {
            entered.countDown(); resume.await(2, TimeUnit.SECONDS); "a"
        }, {}, { fail("Closed owner adopted late input") })
        val worker = Thread { result.set(route.verify()) }; worker.start()
        assertTrue(entered.await(2, TimeUnit.SECONDS))
        val closer = Thread { route.close(); closed.countDown() }; closer.start()
        val closeBeforeRelease = closed.await(1, TimeUnit.SECONDS)
        resume.countDown(); worker.join(2000); closer.join(2000)
        assertTrue(closeBeforeRelease); assertEquals(false, result.get()); assertNull(pin.resolvedId())
    }
    @Test fun newerNativeVerificationWinsOverOlderRead() {
        val entered = CountDownLatch(1); val resume = CountDownLatch(1)
        val reads = java.util.concurrent.atomic.AtomicInteger(); val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val pin = AudioInputPin(null)
        val route = AudioInputRouting(owner(), pin, listOf(a,b), {
            if (reads.incrementAndGet() == 2) { entered.countDown(); resume.await(2, TimeUnit.SECONDS); "b" } else "a"
        }, { fail("Old read must not taint newer input") }, {})
        assertTrue(route.verify())
        val worker = Thread { result.set(route.verify()) }; worker.start()
        assertTrue(entered.await(2, TimeUnit.SECONDS)); assertTrue(route.verify())
        resume.countDown(); worker.join(2000)
        assertEquals(false, result.get()); assertEquals("a", pin.resolvedId()); assertTrue(route.permitsPcm())
    }

    @Test fun activationCoalescesNotificationBeforePinningLatestInput() {
        val probe = HeldRouteRead(1, "a"); val pin = AudioInputPin(null)
        val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val route = AudioInputRouting(owner(), pin, listOf(a,b), { probe.read() }, { fail("Stable activation must not fail") }, {})
        val starter = Thread { result.set(route.verify()) }; starter.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        probe.id.set("b"); route.routeChanged(); assertNull(pin.resolvedId())
        probe.resume.countDown(); starter.join(2000)
        assertEquals(true, result.get()); assertEquals("b", pin.resolvedId()); assertTrue(route.permitsPcm())
    }
    @Test fun activationCannotAdmitUnavailablePendingRecheck() {
        val probe = HeldRouteRead(1, "a"); val pin = AudioInputPin(null)
        val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val route = AudioInputRouting(owner(), pin, listOf(a), { probe.read() }, {}, {})
        val starter = Thread { result.set(route.verify()) }; starter.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        probe.id.set(null); route.routeChanged(); probe.resume.countDown(); starter.join(2000)
        assertEquals(false, result.get()); assertNull(pin.resolvedId()); assertFalse(route.permitsPcm())
    }
    private fun transition(actual: String?, stop: Boolean = false, timeout: Boolean = false) {
        val probe = HeldRouteRead(2, "a"); val failures = java.util.concurrent.atomic.AtomicInteger()
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { probe.read() }, { failures.incrementAndGet() }, {})
        assertTrue(route.verify()); probe.id.set(actual)
        val event = Thread { route.routeChanged() }; event.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS)); assertFalse(route.permitsPcm())
        val running = AtomicBoolean(true); val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val waiter = Thread {
            try { result.set(route.admitReader(running)) } catch (_: IllegalStateException) { result.set(false) }
        }; waiter.start()
        val queuedResult = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val pump = Thread { queuedResult.set(route.awaitQueuedPcm()) }; pump.start()
        if (stop) assertFalse(route.stopProducer(running))
        else if (!timeout) probe.resume.countDown()
        waiter.join(2000); pump.join(2000); assertFalse(waiter.isAlive); assertFalse(pump.isAlive)
        assertEquals(!timeout && (stop || actual != null), queuedResult.get())
        if (stop || timeout) probe.resume.countDown()
        event.join(2000); assertFalse(event.isAlive)
        assertEquals(!stop && !timeout && actual != null, result.get())
        assertEquals(!stop && !timeout && actual != null, route.permitsPcm())
        if (timeout) assertTrue(route.failureDescription.contains("250 ms"))
        assertEquals(!stop && (timeout || actual == null), route.hasFailed())
        assertEquals(if (!stop && (timeout || actual == null)) 1 else 0, failures.get())
    }
    @Test fun heldSameInputEventWakesPCMWithoutLoss() { transition("a") }
    @Test fun heldRouteEventStopWakesCleanly() { transition("a", stop = true) }
    @Test fun heldRouteEventFaultWakesAndStaysClosed() { transition(null) }
    @Test fun heldRouteEventTimeoutRejectsLateSuccessOnce() { transition("a", timeout = true) }
    private class HeldRouteRead(private val hold: Int, initial: String?) {
        val entered = CountDownLatch(1); val resume = CountDownLatch(1)
        val id = java.util.concurrent.atomic.AtomicReference<String?>(initial)
        private val calls = java.util.concurrent.atomic.AtomicInteger()
        fun read(): String? {
            val result = id.get()
            if (calls.incrementAndGet() == hold) { entered.countDown(); resume.await(2, TimeUnit.SECONDS) }
            return result
        }
    }

    @Test fun eventReservationGatesBeforeNativeReadHandoff() {
        val handoffs = java.util.concurrent.atomic.AtomicInteger(); val reads = java.util.concurrent.atomic.AtomicInteger()
        val entered = CountDownLatch(1); val resume = CountDownLatch(1)
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { reads.incrementAndGet(); "a" }, {}, {}, {
            if (handoffs.incrementAndGet() == 2) { entered.countDown(); resume.await(2, TimeUnit.SECONDS) }
        })
        assertTrue(route.verify()); val event = Thread { route.routeChanged() }; event.start()
        assertTrue(entered.await(2, TimeUnit.SECONDS)); assertEquals(1, reads.get()); assertFalse(route.permitsPcm())
        resume.countDown(); event.join(2000)
        assertFalse(event.isAlive); assertEquals(2, reads.get()); assertTrue(route.permitsPcm())
    }

    @Test fun sameInputSettlementRefreshesInitiallyDiscardedActiveLabel() {
        val probe = HeldRouteRead(2, "a"); val deliveries = java.util.concurrent.ConcurrentLinkedQueue<String>()
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { probe.read() }, {}, { deliveries.add(it) })
        assertTrue(route.verify()); var label: String? = null
        val event = Thread { route.routeChanged() }; event.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        deliveries.poll()?.let { if (route.permitsPcm()) label = it }; assertNull(label)
        probe.resume.countDown(); event.join(2000); assertFalse(event.isAlive)
        deliveries.poll()?.let { if (route.permitsPcm()) label = it }; assertEquals("a", label)
    }
    @Test fun postDeadlineSettlementBeforeWaiterReacquiresCannotReopen() {
        val probe = HeldRouteRead(2, "a"); val time = java.util.concurrent.atomic.AtomicLong()
        val calls = java.util.concurrent.atomic.AtomicInteger(); val failures = java.util.concurrent.atomic.AtomicInteger()
        val registered = CountDownLatch(1)
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { probe.read() }, { failures.incrementAndGet() }, {}, now = {
            val snapshot = time.get(); if (calls.incrementAndGet() == 3) registered.countDown(); snapshot
        })
        assertTrue(route.verify()); val event = Thread { route.routeChanged() }; event.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val waiter = Thread { try { result.set(route.admitReader(AtomicBoolean(true))) } catch (_: IllegalStateException) { result.set(false) } }; waiter.start()
        assertTrue(registered.await(2, TimeUnit.SECONDS)); time.set(300_000_000); probe.resume.countDown()
        event.join(2000); waiter.join(2000); assertFalse(event.isAlive); assertFalse(waiter.isAlive)
        assertEquals(false, result.get()); assertFalse(route.permitsPcm()); assertEquals(1, failures.get())
        assertTrue(route.failureDescription.contains("250 ms"))
    }

    @Test fun waiterDeadlineSurvivesAnotherEventBeforeWakeReacquires() {
        val probe = HeldRouteRead(2, "a"); val time = java.util.concurrent.atomic.AtomicLong()
        val calls = java.util.concurrent.atomic.AtomicInteger(); val failures = java.util.concurrent.atomic.AtomicInteger()
        val registered = CountDownLatch(1); val woke = CountDownLatch(1); val resumeWaiter = CountDownLatch(1)
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { probe.read() }, { failures.incrementAndGet() }, {},
            afterWait = { woke.countDown(); resumeWaiter.await(2, TimeUnit.SECONDS) }, now = {
                val snapshot = time.get(); if (calls.incrementAndGet() == 3) registered.countDown(); snapshot
            })
        assertTrue(route.verify()); val event = Thread { route.routeChanged() }; event.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        val result = java.util.concurrent.atomic.AtomicReference<Boolean>()
        val waiter = Thread { try { result.set(route.admitReader(AtomicBoolean(true))) } catch (_: IllegalStateException) { result.set(false) } }; waiter.start()
        assertTrue(registered.await(2, TimeUnit.SECONDS)); probe.resume.countDown(); event.join(2000)
        assertTrue(woke.await(2, TimeUnit.SECONDS)); time.set(300_000_000); route.routeChanged()
        assertFalse(route.permitsPcm()); assertEquals(1, failures.get())
        resumeWaiter.countDown(); waiter.join(2000); assertFalse(waiter.isAlive); assertEquals(false, result.get())
    }
    @Test fun routeWaitNeverExtendsRealPcmBlockAge() {
        val pool = PcmBlockPool(blocks = 1, frames = 1)
        val captured = requireNotNull(pool.acquireReader()); pool.publish(captured, 2, 0, 0)
        val block = requireNotNull(pool.acquireEncoder(0) { 0 }); pool.checkAge(block, 0)
        val probe = HeldRouteRead(2, "a"); val time = java.util.concurrent.atomic.AtomicLong()
        val calls = java.util.concurrent.atomic.AtomicInteger(); val registered = CountDownLatch(1)
        val route = AudioInputRouting(owner(), AudioInputPin(null), listOf(a), { probe.read() }, {}, {}, now = {
            val snapshot = time.get(); if (calls.incrementAndGet() == 3) registered.countDown(); snapshot
        })
        assertTrue(route.verify()); val event = Thread { route.routeChanged() }; event.start()
        assertTrue(probe.entered.await(2, TimeUnit.SECONDS))
        val queued = AtomicBoolean(false); val incomplete = AtomicBoolean(false)
        val pump = Thread {
            try { check(route.awaitQueuedPcm(pool, block)); queued.set(true) }
            catch (_: IllegalStateException) { incomplete.set(true) }
            finally { pool.release(block) }
        }; pump.start()
        assertTrue(registered.await(2, TimeUnit.SECONDS)); time.set(220_000_000); probe.resume.countDown()
        event.join(2000); pump.join(2000)
        assertTrue(route.permitsPcm()); assertFalse(queued.get()); assertTrue(incomplete.get())
        assertFalse(pump.isAlive); pool.cancel()
    }

}
