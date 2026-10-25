// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

/** Pure policy events are not real native/codec evidence. Production keys are
 * retained privately by the actual AudioCaptureOwner and recorder. */
class RecordingSourceGateTest {
    private class Fixture(val epoch: Long = 7) {
        val gate = RecordingSourceGate(epoch)
        val owner = Any(); val prepared = Any(); val generation = Any()
        val key = gate.bindProducer(owner, prepared, epoch)
        val sink = Any(); val recorder = gate.bindRecorder(sink)
        fun closed(frames: Long = 1024, inputEos: Boolean = true, outputEos: Boolean = true): RecordingSourceGate.Terminal? {
            assertTrue(gate.generationStarted(key, generation))
            gate.generationClosed(key, generation, frames, inputEos, outputEos)
            return gate.terminal(key, owner, prepared, epoch)
        }
        fun ready(): Pair<RecordingSourceGate.FinishPermit, Any> {
            val terminal = checkNotNull(closed())
            val writer = Any(); recorder.writerOpened(writer)
            recorder.accepted(); recorder.consumed()
            return checkNotNull(recorder.drained(terminal)) to writer
        }
    }
    @Test fun actualClosedEventIsRequiredBeforeAnySourceTerminal() {
        val f = Fixture(); assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
        assertTrue(f.gate.generationStarted(f.key, f.generation))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch)); assertNull(f.recorder.drained(null))
    }
    @Test fun joinedNoInputClosesWithoutInventingEosOrAFile() {
        val f = Fixture(); val terminal = checkNotNull(f.closed(0, false, false))
        assertEquals(RecordingSourceGate.Kind.NO_INPUT_CLOSED, terminal.kind)
        assertEquals(0L, terminal.acceptedNativeInputFrames)
        val permit = checkNotNull(f.recorder.drained(terminal)); f.recorder.commitEmpty(permit)
        assertThrows(IllegalStateException::class.java) { f.recorder.commitEmpty(permit) }
    }
    @Test fun nonemptyGenerationRequiresBothActualEosEvents() {
        for ((input, output) in listOf(false to false, true to false, false to true)) {
            val f = Fixture(); assertNull(f.closed(1024, input, output)); assertNull(f.recorder.drained(null))
        }
    }
    @Test fun failedOrRetainedClosureCannotIssueARecordingPermit() {
        val f = Fixture(); assertTrue(f.gate.generationStarted(f.key, f.generation)); f.gate.fail()
        assertFalse(f.gate.generationClosed(f.key, f.generation, 1024, true, true))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
        val retained = Fixture(); assertTrue(retained.gate.generationStarted(retained.key, retained.generation))
        assertNull(retained.gate.terminal(retained.key, retained.owner, retained.prepared, retained.epoch))
    }
    @Test fun everyPhysicalGenerationMustCloseBeforeTheRunSeal() {
        val f = Fixture(); assertTrue(f.gate.generationStarted(f.key, f.generation))
        assertTrue(f.gate.generationClosed(f.key, f.generation, 0, false, false))
        val second = Any(); assertTrue(f.gate.generationStarted(f.key, second))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
        assertTrue(f.gate.generationClosed(f.key, second, 2048, true, true))
        val terminal = checkNotNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
        assertEquals(2L, terminal.generations); assertEquals(2048L, terminal.acceptedNativeInputFrames)
    }
    @Test fun drainPermitRequiresEveryAcceptedHandleToReturnFromConsume() {
        val f = Fixture(); val terminal = checkNotNull(f.closed())
        f.recorder.accepted(); f.recorder.accepted(); f.recorder.consumed()
        assertNull(f.recorder.drained(terminal)); f.recorder.consumed()
        assertNotNull(f.recorder.drained(terminal))
    }
    @Test fun delayedLossAfterTheControllerSnapshotStillRefusesCommit() {
        val f = Fixture(); val (permit, writer) = f.ready(); val oldCanFinish = true
        f.recorder.failed(); assertTrue(oldCanFinish)
        synchronized(f.gate.guard) { assertThrows(IllegalStateException::class.java) { f.gate.requireCommitLocked(permit, writer) } }
    }
    @Test fun foreignOwnerPreparationEpochAndKeyNeverMatchTheActualSource() {
        val f = Fixture(); assertNotNull(f.closed())
        assertNull(f.gate.terminal(f.key, Any(), f.prepared, f.epoch))
        assertNull(f.gate.terminal(f.key, f.owner, Any(), f.epoch))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch + 1))
        assertNull(f.gate.terminal(RecordingSourceGate.SourceKey(), f.owner, f.prepared, f.epoch))
        assertNotNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
    }
    @Test fun constructedOrForeignTerminalCannotBecomeTheIssuedReceipt() {
        val f = Fixture(); val real = checkNotNull(f.closed())
        val foreign = checkNotNull(Fixture().closed())
        val constructed = RecordingSourceGate.Terminal(f.gate, real.kind, real.generations, real.acceptedNativeInputFrames)
        assertNull(f.recorder.drained(foreign)); assertNull(f.recorder.drained(constructed))
        assertNotNull(f.recorder.drained(real))
    }
    @Test fun permitIsBoundToActualWriterAndConsumedExactlyOnce() {
        val f = Fixture(); val (permit, writer) = f.ready()
        synchronized(f.gate.guard) {
            assertThrows(IllegalStateException::class.java) { f.gate.requireCommitLocked(permit, Any()) }
            f.gate.commitLocked(permit, writer)
            assertThrows(IllegalStateException::class.java) { f.gate.commitLocked(permit, writer) }
        }
    }
    @Test fun staleGenerationClosureCannotReplaceTheCurrentBarrier() {
        val f = Fixture(); assertTrue(f.gate.generationStarted(f.key, f.generation))
        assertFalse(f.gate.generationClosed(f.key, Any(), 1024, true, true))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
        assertTrue(f.gate.generationClosed(f.key, f.generation, 1024, true, true))
        assertNotNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
    }
    @Test fun acceptedWorkAfterDrainCannotHideBehindAnOldPermit() {
        val f = Fixture(); val (permit, writer) = f.ready(); f.recorder.accepted()
        synchronized(f.gate.guard) { assertThrows(IllegalStateException::class.java) { f.gate.commitLocked(permit, writer) } }
    }
    @Test fun unqueuedConsumptionFailsClosedInsteadOfInventingAcceptance() {
        val f = Fixture(); val terminal = checkNotNull(f.closed()); f.recorder.consumed()
        assertNull(f.recorder.drained(terminal))
    }
    @Test fun committedCutCannotBeReplayedOrTurnedIntoAnotherPermit() {
        val f = Fixture(); val (permit, writer) = f.ready()
        synchronized(f.gate.guard) { f.gate.commitLocked(permit, writer) }
        f.gate.fail(); assertNull(f.recorder.drained(permit.terminal))
        synchronized(f.gate.guard) { assertThrows(IllegalStateException::class.java) { f.gate.commitLocked(permit, writer) } }
    }
    @Test fun nativeInputTotalOverflowNeverIssuesAnEofReceipt() {
        val f = Fixture(); assertTrue(f.gate.generationStarted(f.key, f.generation))
        assertTrue(f.gate.generationClosed(f.key, f.generation, Long.MAX_VALUE, true, true))
        val second = Any(); assertTrue(f.gate.generationStarted(f.key, second))
        assertFalse(f.gate.generationClosed(f.key, second, 1, true, true))
        assertNull(f.gate.terminal(f.key, f.owner, f.prepared, f.epoch))
    }
    @Test fun reentrantScalarLossBeforeTheCommitCutCannotDeadlockOrAdmit() {
        val f = Fixture(); val (permit, writer) = f.ready()
        synchronized(f.gate.guard) {
            f.recorder.failed()
            assertThrows(IllegalStateException::class.java) { f.gate.commitLocked(permit, writer) }
        }
    }

    @Test fun audioNoInputIsNotAnInferenceThatNoRecordingHandlesExist() {
        val f = Fixture(); val terminal = checkNotNull(f.closed(0, false, false)); val writer = Any()
        f.recorder.writerOpened(writer); f.recorder.accepted(); f.recorder.consumed()
        val permit = checkNotNull(f.recorder.drained(terminal))
        synchronized(f.gate.guard) { f.gate.commitLocked(permit, writer) }
        // These handles may be video in an A/V run. No VIDEO_ONLY mode or
        // audio programme/trim authority is manufactured by this policy test.
    }

    @Test fun onlyUnsealedFailedRecordingMayAllowPublishingReplacement() {
        val gate = RecordingSourceGate(7); val owner = Any(); val prepared = Any()
        val key = gate.bindProducer(owner, prepared, 7); val generation = Any()
        assertFalse(gate.publishingOnlyAfterLoss(key))
        assertTrue(gate.generationStarted(key, generation))
        assertTrue(gate.generationClosed(key, generation, 0, false, false))
        assertNotNull(gate.terminal(key, owner, prepared, 7))
        assertFalse(gate.generationStarted(key, Any()))
        gate.fail()
        assertFalse(gate.publishingOnlyAfterLoss(key))
        val failed = RecordingSourceGate(7); val failedKey = failed.bindProducer(Any(), Any(), 7)
        failed.fail()
        assertTrue(failed.publishingOnlyAfterLoss(failedKey))
        assertFalse(failed.publishingOnlyAfterLoss(key))
    }

    @Test fun actualBorrowCloseFailureAfterConsumeInvalidatesMatchingFrontiers() {
        val f = Fixture(); val terminal = checkNotNull(f.closed()); f.recorder.accepted()
        val fault = AssertionError("native close returned no confirmed outcome")
        var offered = false; var attempts = 0
        val batch = OwnedBatch.collect(1, pop = {
            if (offered) null else { offered = true; AutoCloseable { attempts++; throw fault } }
        }, sizeOf = { 1 })
        val observed = assertThrows(AssertionError::class.java) {
            f.recorder.fenceFailure { batch.use { assertTrue(it.consumeNext { f.recorder.consumed() }) } }
        }
        assertSame(fault, observed)
        assertEquals(1, attempts)
        // Both scalar frontiers reached one, but the real owned close failed.
        assertNull(f.recorder.drained(terminal))
    }
}
