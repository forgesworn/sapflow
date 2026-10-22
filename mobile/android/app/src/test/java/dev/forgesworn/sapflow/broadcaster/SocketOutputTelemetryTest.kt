package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class SocketOutputTelemetryTest {
    private val second = 1_000_000_000L
    private fun context(epoch: Long = 1, attempt: Long = 1) = PublishingAttemptContext(1, epoch, attempt, SdkAttemptIdentity(1uL, attempt.toULong()))
    private class Source : SocketOutputSource {
        var count: Long? = 0; var incomplete = false; var reads = 0
        var timestamp: Long? = null; var ended: Long? = null
        var onRead: (() -> Unit)? = null
        override fun socketSnapshot(nowNs: Long): SocketOutputSample {
            reads++; onRead?.invoke()
            return SocketOutputSample(this, timestamp ?: nowNs, count, SocketOutputAccounting.SRT_DATAGRAM, incomplete, SocketOutputPhase.ACTIVE, endedAtNs = ended)
        }
    }
    @Test fun fullWriteOnlyCounterUsesActualResultWithoutChangingCallPolicy() {
        val counter = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        assertEquals(0, counter.write(100) { 0 })
        assertEquals(0L, counter.socketSnapshot(0).completedBytes)
        assertEquals(100, counter.write(100) { 100 })
        assertEquals(100L, counter.socketSnapshot(1).completedBytes)
        assertEquals(9, counter.write(100) { 9 })
        assertEquals(100L, counter.socketSnapshot(2).completedBytes)
        assertTrue(counter.socketSnapshot(2).incomplete)
    }
    @Test fun thrownWritePreservesExceptionAndKnownPrefixButNotACompleteRate() {
        val counter = SocketOutputCounter(SocketOutputAccounting.RTMPS_APPLICATION)
        counter.write(8) { 8 }
        val failure = IllegalStateException("synthetic write")
        val actual = assertThrows(IllegalStateException::class.java) { counter.write(8) { throw failure } }
        assertSame(failure, actual)
        assertEquals(8L, counter.socketSnapshot(1).completedBytes)
        assertTrue(counter.socketSnapshot(1).incomplete)
    }
    @Test fun sealedCounterIsFinalAndCheckedOverflowIsUnknown() {
        val counter = SocketOutputCounter(SocketOutputAccounting.RTMP_APPLICATION)
        counter.completed(12); counter.seal(); counter.completed(4)
        assertEquals(12L, counter.socketSnapshot(1).completedBytes)
        assertEquals(SocketOutputPhase.CLOSED, counter.socketSnapshot(1).phase)
        val overflow = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        overflow.completed(Long.MAX_VALUE); overflow.completed(1)
        assertNull(overflow.socketSnapshot(1).completedBytes); assertTrue(overflow.socketSnapshot(1).incomplete)
    }
    @Test fun completedShutdownWriteCountsEvenWhenDeadlineExpiresImmediatelyAfterWrite() {
        val shutdown = SrtShutdown(); var now = 0L; shutdown.request(now)
        val counter = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        val control = ByteArray(16).apply { this[0] = 0x80.toByte(); this[1] = 5 }
        val result = shutdown.finish({ now }, {}, { control }) { buffer ->
            counter.write(buffer.remaining()) { now = SrtShutdown.BUDGET_US; 16 }
        }
        assertEquals(SrtShutdown.Result.EXPIRED, result)
        assertEquals(16L, counter.socketSnapshot(1).completedBytes)
    }
    @Test fun logicalTotalRetainsClosingBytesAndRetiresExactlyOnceAcrossRetryAndRotation() {
        val ledger = LogicalSocketOutput(); val a = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(), a); a.completed(100)
        ledger.closing(context()); a.completed(16)
        assertEquals(116L, ledger.socketSnapshot(second).completedBytes)
        a.seal(); ledger.retired(context(), second); ledger.retired(context(), second)
        val b = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM); val next = context(2, 2)
        ledger.attach(next, b); b.completed(40)
        ledger.retired(context(), second) // Old final notification cannot retire successor.
        assertEquals(156L, ledger.socketSnapshot(2 * second).completedBytes)
        assertEquals(next, ledger.socketSnapshot(2 * second).context)
        ledger.ended(3 * second); ledger.ended(5 * second)
        b.completed(16); b.seal(); ledger.retired(next, 6 * second)
        val final = ledger.socketSnapshot(7 * second)
        assertEquals(172L, final.completedBytes); assertEquals(3 * second, final.endedAtNs); assertEquals(SocketOutputPhase.CLOSED, final.phase)
    }
    @Test fun unconfirmedCleanupKeepsClosingSourceAndNeverClaimsCompleteAccounting() {
        val ledger = LogicalSocketOutput(); val counter = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(), counter); counter.completed(100); ledger.closing(context()); ledger.unconfirmed(context())
        counter.completed(20)
        val value = ledger.socketSnapshot(second)
        assertEquals(120L, value.completedBytes); assertTrue(value.incomplete); assertEquals(SocketOutputPhase.CLOSING, value.phase)
    }
    @Test fun nonsealedOrUnsupportedFinalSourceIsNotSilentlyComplete() {
        val ledger = LogicalSocketOutput(); val counter = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(), counter); counter.completed(4); ledger.retired(context(), second)
        assertTrue(ledger.socketSnapshot(second).incomplete)
        ledger.attach(context(2, 2), null)
        assertNull(ledger.socketSnapshot(second).completedBytes); assertTrue(ledger.socketSnapshot(second).incomplete)
    }
    @Test fun boundedTopologyRechecksReturnUnknownInsteadOfSpinningOrDoubleCounting() {
        val ledger = LogicalSocketOutput(); val source = Source()
        source.onRead = { ledger.closing(context()) }
        ledger.attach(context(), source)
        val sample = ledger.socketSnapshot(second)
        assertEquals(2, source.reads); assertFalse(sample.stable); assertNull(sample.completedBytes)
    }
    @Test fun sourceCannotRestampStaleCountersOrBecomeActiveAfterTerminalEnd() {
        val ledger = LogicalSocketOutput(); val source = Source().apply { timestamp = 0; count = 12 }
        ledger.ended(second); ledger.attach(context(), source)
        val held = ledger.socketSnapshot(2 * second)
        assertEquals(SocketOutputPhase.CLOSING, held.phase); assertEquals(second, held.endedAtNs)
        assertNull(held.completedBytes); assertTrue(held.incomplete)
    }
    @Test fun oneHzReadsGiveWarmupThenFreshZeroAndNoPerDisplayDispatch() {
        val source = Source(); val sampler = SocketOutputSampler(); val owner = Any()
        assertEquals(SocketOutputAvailability.WARMING_UP, sampler.sample(0, owner, source, true).availability)
        for (tick in 1..4) {
            val cached = sampler.sample(tick * 200_000_000L, owner, source, true)
            assertEquals(tick * 200L, cached.sampleAgeMs)
        }
        assertEquals(1, source.reads)
        val zero = sampler.sample(second, owner, source, true)
        assertEquals(2, source.reads); assertEquals(SocketOutputAvailability.FRESH, zero.availability); assertEquals(0L, zero.bitsPerSecond)
        source.count = 125_000
        assertEquals(1_000_000L, sampler.sample(2 * second, owner, source, true).bitsPerSecond)
    }
    @Test fun staleLongWindowIsUnknownThenRequiresACompleteFreshWindow() {
        val source = Source(); val sampler = SocketOutputSampler(); val owner = Any()
        sampler.sample(0, owner, source, true); source.count = 100
        val stale = sampler.sample(4 * second, owner, source, true)
        assertEquals(SocketOutputAvailability.STALE, stale.availability); assertNull(stale.bitsPerSecond)
        assertEquals(0L, sampler.sample(5 * second, owner, source, true).bitsPerSecond)
        source.timestamp = second
        val old = sampler.sample(6 * second, owner, source, true)
        assertEquals(SocketOutputAvailability.UNKNOWN, old.availability); assertNull(old.bitsPerSecond)
    }
    @Test fun rollbackCounterRegressionAndArithmeticOverflowCannotBecomeFreshZero() {
        val source = Source(); val sampler = SocketOutputSampler(); val owner = Any()
        source.count = 10; sampler.sample(second, owner, source, true)
        assertNull(sampler.sample(0, owner, source, true).bitsPerSecond)
        sampler.sample(2 * second, owner, source, true); source.count = 9
        assertNull(sampler.sample(3 * second, owner, source, true).bitsPerSecond)
        source.count = 0; sampler.sample(4 * second, owner, source, true); source.count = Long.MAX_VALUE
        assertNull(sampler.sample(5 * second, owner, source, true).bitsPerSecond)
    }
    @Test fun incompleteAndEndedNeverExposePreviousRateAsCurrent() {
        val source = Source(); val sampler = SocketOutputSampler(); val owner = Any()
        sampler.sample(0, owner, source, true); source.count = 100
        assertNotNull(sampler.sample(second, owner, source, true).bitsPerSecond)
        source.incomplete = true
        assertNull(sampler.sample(2 * second, owner, source, true).bitsPerSecond)
        source.incomplete = false; source.ended = 2 * second
        val end = sampler.sample(3 * second, owner, source, true)
        assertTrue(end.ended); assertNull(end.bitsPerSecond); assertEquals(100L, end.completedBytes)
    }
    @Test fun replacingLogicalShowCannotReuseOldBaselineAndLocalOnlyIsNotZeroBitrate() {
        val source = Source(); val sampler = SocketOutputSampler(); val old = Any()
        sampler.sample(0, old, source, true); source.count = 100
        assertNotNull(sampler.sample(second, old, source, true).bitsPerSecond)
        assertEquals(SocketOutputAvailability.WARMING_UP, sampler.sample(2 * second, Any(), source, true).availability)
        assertEquals(SocketOutputAvailability.NOT_ATTACHED, sampler.sample(3 * second, Any(), null, false).availability)
        assertNull(sampler.sample(4 * second, Any(), null, false).bitsPerSecond)
    }
    @Test fun failedOrPartialAttemptKeepsLowerBoundTotalButCompleteRetryWarmsUpItsOwnRate() {
        for (throws in listOf(false, true)) {
            val ledger = LogicalSocketOutput(); val sampler = SocketOutputSampler(); val owner = Any()
            val old = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
            ledger.attach(context(), old); old.completed(100)
            sampler.sample(0, owner, ledger, true)
            if (throws) assertThrows(IllegalStateException::class.java) { old.write(50) { throw IllegalStateException("synthetic") } }
            else old.write(50) { 12 }
            assertNull(sampler.sample(second, owner, ledger, true).bitsPerSecond)
            old.seal(); ledger.retired(context(), second)
            val next = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
            ledger.attach(context(1, 2), next); next.completed(40)
            val warm = sampler.sample(2 * second, owner, ledger, true)
            assertEquals(SocketOutputAvailability.WARMING_UP, warm.availability)
            assertTrue(warm.incomplete); assertEquals(140L, warm.completedBytes)
            next.completed(100)
            val fresh = sampler.sample(3 * second, owner, ledger, true)
            assertEquals(SocketOutputAvailability.FRESH, fresh.availability)
            assertEquals(800L, fresh.bitsPerSecond)
            assertTrue(fresh.incomplete); assertEquals(240L, fresh.completedBytes)
        }
    }
    @Test fun evenCompleteReplacementCannotBridgePreviousSocketRateWindow() {
        val ledger = LogicalSocketOutput(); val sampler = SocketOutputSampler(); val owner = Any()
        val old = SocketOutputCounter(SocketOutputAccounting.RTMP_APPLICATION)
        ledger.attach(context(), old); old.completed(100); sampler.sample(0, owner, ledger, true)
        old.seal(); ledger.retired(context(), second)
        val next = SocketOutputCounter(SocketOutputAccounting.RTMP_APPLICATION)
        ledger.attach(context(2, 2), next); next.completed(200)
        val warm = sampler.sample(second, owner, ledger, true)
        assertEquals(SocketOutputAvailability.WARMING_UP, warm.availability); assertNull(warm.bitsPerSecond)
        assertEquals(300L, warm.completedBytes)
        val fresh = sampler.sample(2 * second, owner, ledger, true)
        assertEquals(0L, fresh.bitsPerSecond); assertFalse(fresh.incomplete)
    }
    @Test fun ownerlessBackoffAndClosingNeverBecomeFreshZeroAndRetryRequiresTwoSamples() {
        val ledger = LogicalSocketOutput(); val sampler = SocketOutputSampler(); val owner = Any()
        val old = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(), old); old.completed(100); sampler.sample(0, owner, ledger, true)
        ledger.closing(context())
        assertNull(sampler.sample(second, owner, ledger, true).bitsPerSecond)
        old.seal(); ledger.retired(context(), second)
        for (time in 2L..3L) {
            val gap = sampler.sample(time * second, owner, ledger, true)
            assertEquals(SocketOutputAvailability.UNKNOWN, gap.availability)
            assertNull(gap.bitsPerSecond); assertEquals(100L, gap.completedBytes); assertFalse(gap.incomplete)
        }
        val next = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(1, 2), next)
        assertEquals(SocketOutputAvailability.WARMING_UP, sampler.sample(4 * second, owner, ledger, true).availability)
        assertEquals(0L, sampler.sample(5 * second, owner, ledger, true).bitsPerSecond)
    }
    @Test fun writerSealedBeforeControllerRetirementCannotRemainAFreshActiveRate() {
        val ledger = LogicalSocketOutput(); val sampler = SocketOutputSampler(); val owner = Any()
        val counter = SocketOutputCounter(SocketOutputAccounting.SRT_DATAGRAM)
        ledger.attach(context(), counter); counter.completed(100)
        sampler.sample(0, owner, ledger, true)
        assertEquals(0L, sampler.sample(second, owner, ledger, true).bitsPerSecond)
        counter.seal() // UDP finally may finish before the controller receives Failed.
        for (time in 2L..3L) {
            val closed = sampler.sample(time * second, owner, ledger, true)
            assertEquals(SocketOutputAvailability.UNKNOWN, closed.availability)
            assertNull(closed.bitsPerSecond); assertEquals(100L, closed.completedBytes)
        }
        ledger.retired(context(), 4 * second)
        assertNull(sampler.sample(4 * second, owner, ledger, true).bitsPerSecond)
    }
}
