package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.ArrayDeque

class AudioSourcePreservationTest {
    private class Rejected : IllegalArgumentException()
    private class Sample(val bytes: ByteArray, val tick: Long, val throwsOnClose: Boolean = false) : AutoCloseable {
        var closes = 0
        override fun close() { check(++closes == 1); if (throwsOnClose) error("sample close unknown") }
    }
    private class Views(val original: Sample, val rawPts: Long, val publishingTick: Long?, val throwsOnClose: Boolean = false) : AutoCloseable {
        var closes = 0
        override fun close() { check(++closes == 1); if (throwsOnClose) error("views close unknown") }
    }
    private class Fixture {
        var originals = 0
        var viewCalls = 0
        var rejectOriginal = false
        var unknownOriginal = false
        var rejectViews = false
        var unknownViews = false
        var sampleCloseUnknown = false
        var viewsCloseUnknown = false
        var lastSample: Sample? = null
        var lastViews: Views? = null
        val originalFactory: (ByteArray, Long) -> Sample = { bytes, tick ->
            originals++
            if (rejectOriginal) throw Rejected()
            if (unknownOriginal) error("original acquisition unknown")
            Sample(bytes, tick, sampleCloseUnknown).also { lastSample = it }
        }
        val viewsFactory: (Sample, Long, Long?) -> Views = { original, raw, publishing ->
            viewCalls++
            if (rejectViews) throw Rejected()
            if (unknownViews) error("views acquisition unknown")
            Views(original, raw, publishing, viewsCloseUnknown).also { lastViews = it }
        }
        fun slot() = AudioTimingTransfer(originalFactory, viewsFactory, { it is Rejected })
    }

    @Test fun negativePrimingPreservesOneOriginalAndWithholdsOnlyPublishing() {
        val f = Fixture(); val slot = f.slot(); val bytes = byteArrayOf(1, 2, 3)
        val clock = AudioGenerationClock(); val generation = clock.begin(1, 0, false)
        val publishing = clock.output(generation, -21333)
        slot.preserve(bytes, AudioSourceCursor().reserve(publishing), -21333, publishing)
        val views = slot.timingViews
        assertSame(bytes, views.original.bytes)
        assertSame(f.lastSample, views.original)
        assertEquals(0L, views.original.tick)
        assertEquals(-21333L, views.rawPts)
        assertNull(views.publishingTick)
        assertEquals(1, f.originals); assertEquals(1, f.viewCalls)
        assertEquals(1, views.original.closes); assertEquals(0, views.closes)
        slot.close(); assertEquals(1, views.closes); assertTrue(slot.reusable)
    }

    @Test fun applicationCodedAxisAndPublishingEpochRemainSeparateAcrossReplacement() {
        val f = Fixture(); val slot = f.slot(); val source = AudioSourceCursor()
        val clock = AudioGenerationClock(); val first = clock.begin(1, 0, false)
        for (raw in listOf(-21333L, 0L, 21333L)) {
            val publishing = clock.output(first, raw)
            val expected = source.nextTick
            slot.preserve(byteArrayOf(7), source.reserve(publishing), raw, publishing)
            assertEquals(expected, slot.timingViews.original.tick)
            assertEquals(if (publishing < 0) null else publishing, slot.timingViews.publishingTick)
            slot.close()
        }
        clock.closed(first)
        val second = clock.begin(2, clock.replacementStartUs(0), true)
        val early = clock.output(second, 1)
        assertEquals(AudioGenerationClock.PRIMING, early)
        slot.preserve(byteArrayOf(8), source.reserve(early), 1, early)
        assertEquals(3072L, slot.timingViews.original.tick); assertNull(slot.timingViews.publishingTick)
        slot.close()
        assertEquals(4096L, source.nextTick)
        assertEquals(2048L, clock.nextOutputTick)
    }

    @Test fun staleForeignAndClosedGenerationCannotAdvanceSourceOrAcquireSamples() {
        val f = Fixture(); val source = AudioSourceCursor(); val clock = AudioGenerationClock()
        val current = clock.begin(2, 0, false)
        val foreign = AudioGenerationClock().begin(2, 0, false)
        for (generation in listOf(foreign, current)) {
            if (generation === current) clock.closed(current)
            val tick = clock.output(generation, -1)
            assertEquals(AudioGenerationClock.STALE, tick)
            assertThrows(IllegalArgumentException::class.java) { source.reserve(tick) }
        }
        assertEquals(0L, source.nextTick); assertEquals(0, f.originals)
    }

    @Test fun legacyTransferUsesExactExistingSampleAndDoesNotConstructViews() {
        val f = Fixture(); val slot = f.slot(); val clock = AudioGenerationClock()
        val generation = clock.begin(1, 0, false)
        assertEquals(AudioGenerationClock.PRIMING, clock.output(generation, -1))
        val tick = clock.output(generation, 42667)
        val original = Sample(byteArrayOf(9, 4), tick)
        slot.attachLegacy(original, tick)
        assertEquals(2048L, slot.tick); assertSame(original, slot.sample)
        assertEquals(0, f.originals); assertEquals(0, f.viewCalls)
        slot.close(); assertEquals(1, original.closes)
    }

    @Test fun sourceOverflowRefusesBeforeAdvancingTheDeclaredAxis() {
        val source = AudioSourceCursor(Long.MAX_VALUE - 1023)
        assertThrows(ArithmeticException::class.java) { source.reserve(AudioGenerationClock.PRIMING) }
        assertEquals(Long.MAX_VALUE - 1023, source.nextTick)
        assertThrows(IllegalArgumentException::class.java) { AudioSourceCursor(-1) }
    }

    @Test fun typedOriginalRejectionReturnsNoOwnerAndAllowsTheSameSlotToBeUsed() {
        val f = Fixture(); val slot = f.slot(); f.rejectOriginal = true
        assertThrows(Rejected::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertTrue(slot.reusable); assertEquals(0, f.viewCalls)
        f.rejectOriginal = false; slot.preserve(byteArrayOf(2), 0, -1, -1); slot.close()
        assertEquals(2, f.originals); assertEquals(1, f.lastSample!!.closes)
    }

    @Test fun typedViewsRejectionClosesTheOneOriginalAndPreservesFirstFailure() {
        val f = Fixture(); val slot = f.slot(); f.rejectViews = true
        val failure = assertThrows(Rejected::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertEquals(0, failure.suppressed.size)
        assertEquals(1, f.lastSample!!.closes); assertTrue(slot.reusable)
        f.sampleCloseUnknown = true
        val withCleanupFailure = assertThrows(Rejected::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertEquals(1, withCleanupFailure.suppressed.size)
        assertTrue(slot.ownershipUnknown)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertEquals(1, f.lastSample!!.closes)
    }

    @Test fun unknownOriginalAcquisitionQuarantinesWithoutGuessingCloseOrRetry() {
        val f = Fixture(); val slot = f.slot(); f.unknownOriginal = true
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertTrue(slot.ownershipUnknown); assertFalse(slot.reusable)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertEquals(1, f.originals); assertEquals(0, f.viewCalls)
    }

    @Test fun unknownViewsAcquisitionRetainsOriginalAndFencesFurtherNativeCalls() {
        val f = Fixture(); val slot = f.slot(); f.unknownViews = true
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertEquals(0, f.lastSample!!.closes); assertTrue(slot.ownershipUnknown)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertThrows(IllegalStateException::class.java) { slot.timingViews }
        assertEquals(1, f.viewCalls); assertEquals(0, f.lastSample!!.closes)
    }

    @Test fun originalCloseFailureRetainsBothHandlesWithoutClosingViewsOrReusingSlot() {
        val f = Fixture(); val slot = f.slot(); f.sampleCloseUnknown = true
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertTrue(slot.ownershipUnknown); assertEquals(1, f.lastSample!!.closes)
        assertEquals(0, f.lastViews!!.closes)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertEquals(0, f.lastViews!!.closes)
    }

    @Test fun viewsCloseFailureDoesNotRetryNativeCloseOrAllowReattachment() {
        val f = Fixture(); val slot = f.slot(); f.viewsCloseUnknown = true
        slot.preserve(byteArrayOf(1), 0, -1, -1)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertThrows(IllegalStateException::class.java) { slot.attachLegacy(Sample(byteArrayOf(1), 0), 0) }
        assertEquals(1, f.lastViews!!.closes); assertEquals(1, f.lastSample!!.closes)
    }

    @Test fun existingHandoffRetainsViewsUntilBorrowReturnsAndClosesRefusedOrFailedOwnersOnce() {
        val f = Fixture(); val slot = f.slot(); val refused = f.slot()
        val posted = ArrayDeque<Runnable>(); var failures = 0; var consumed = 0
        val queue = CoalescedOwnedQueue<AudioTimingTransfer<Sample, Views, ByteArray>>(1, 4096, 250_000_000,
            { 0L }, { posted.addLast(it); true }, { borrowed ->
                consumed++; assertEquals(1, borrowed.timingViews.original.closes)
                assertEquals(0, borrowed.timingViews.closes)
                error("fanout refused")
            }, { failures++ }, {})
        slot.preserve(byteArrayOf(1), 0, -1, -1); val firstViews = slot.timingViews
        queue.offer(slot, 1)
        refused.preserve(byteArrayOf(2), 1024, 0, 0); val refusedViews = refused.timingViews
        assertThrows(IllegalStateException::class.java) { queue.offer(refused, 1) }
        assertEquals(1, refusedViews.closes); assertEquals(0, firstViews.closes)
        posted.removeFirst().run()
        assertEquals(1, consumed); assertEquals(1, failures); assertEquals(1, firstViews.closes)
        assertEquals(0, queue.outstanding)
    }

    @Test fun intentionalWithholdingIsNeitherPublishingDropNorPrivacyReadiness() {
        assertTrue(AudioSourceAdmission.publishingWithheld(true, AudioGenerationClock.PRIMING))
        assertFalse(AudioSourceAdmission.countPublishingDrop(true, AudioGenerationClock.PRIMING, false))
        assertFalse(AudioSourceAdmission.publishingWithheld(true, 0))
        assertTrue(AudioSourceAdmission.countPublishingDrop(true, 0, false))
        assertTrue(AudioSourceAdmission.countPublishingDrop(false, 0, false))
        assertFalse(AudioSourceAdmission.countPublishingDrop(false, 0, true))
    }

    @Test fun diagnosticFailureAfterOriginalReturnStillOwnsAndClosesThatOriginal() {
        val f = Fixture()
        val slot = AudioTimingTransfer(f.originalFactory, f.viewsFactory, { it is Rejected }, { error("diagnostic refused") })
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        assertFalse(slot.ownershipUnknown); assertFalse(slot.reusable)
        assertEquals(1, f.originals); assertEquals(0, f.viewCalls); assertEquals(0, f.lastSample!!.closes)
        slot.close(); assertTrue(slot.reusable); assertEquals(1, f.lastSample!!.closes)
    }

    @Test fun preservingValidatedBytesDoesNotManufactureTrimPackingInputFramesOrEos() {
        val f = Fixture(); val observation = AudioGenerationObservation(1)
        val slot = AudioTimingTransfer(f.originalFactory, f.viewsFactory, { it is Rejected }, { observation.constructed(1) })
        for (kind in AudioGenerationObservation.Trim.values()) observation.trim(1,
            AudioGenerationObservation.Phase.FORMAT, kind, 36, AudioGenerationObservation.State.ABSENT)
        observation.output(1, 3, -21333, 0, false, false, true)
        observation.borrowedOutputRange(1, 0, 3, 3)
        slot.preserve(byteArrayOf(1, 2, 3), 0, -21333, AudioGenerationClock.PRIMING)
        slot.close()
        observation.producersJoined(1); observation.callbackBarrier(1, true); observation.handoffIdle(1)
        val snapshot = checkNotNull(observation.freeze(1))
        assertEquals(1L, snapshot.constructedSamples)
        assertEquals(1L, snapshot.cardinalityUnknownBuffers)
        assertEquals(AudioGenerationObservation.State.ABSENT, snapshot.formatLeading.state)
        assertEquals(AudioGenerationObservation.State.ABSENT, snapshot.formatPadding.state)
        assertEquals(0L, snapshot.submittedFrames)
        assertEquals(0L, snapshot.inputEosQueued); assertEquals(0L, snapshot.outputEosBuffers)
        assertEquals(0L, snapshot.primingFilteredBuffers)
    }

    @Test fun sixteenExistingSlotsCarryOneHandoffAndRemainBorrowedUntilEachTurnReturns() {
        val f = Fixture(); val slots = List(16) { f.slot() }; val posted = ArrayDeque<Runnable>()
        val seen = mutableListOf<Long>()
        val queue = CoalescedOwnedQueue<AudioTimingTransfer<Sample, Views, ByteArray>>(16, 65536, 250_000_000,
            { 0L }, { posted.addLast(it); true }, { slot ->
                assertEquals(0, slot.timingViews.closes); seen.add(slot.timingViews.original.tick)
            }, { throw it }, {})
        slots.forEachIndexed { index, slot ->
            slot.preserve(byteArrayOf(index.toByte()), index * 1024L, -index.toLong(), AudioGenerationClock.PRIMING)
            queue.offer(slot, 4096)
        }
        assertEquals(1, posted.size); assertEquals(16, queue.outstanding)
        posted.removeFirst().run(); assertEquals(8, queue.outstanding)
        assertTrue(slots.take(8).all { it.reusable }); assertTrue(slots.drop(8).none { it.reusable })
        posted.removeFirst().run()
        assertEquals((0..15).map { it * 1024L }, seen)
        assertEquals(16, f.originals); assertEquals(16, f.viewCalls)
        assertTrue(slots.all { it.reusable }); assertTrue(posted.isEmpty())
    }

    @Test fun unknownAcquisitionBeforeHandoffBlocksTheActualGenerationRetirementBarrier() {
        val f = Fixture(); val slot = f.slot(); val slots = arrayOf(slot)
        val lifecycle = AudioGenerationLifecycle(); val generation = checkNotNull(lifecycle.begin())
        f.unknownViews = true
        assertThrows(IllegalStateException::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        lifecycle.fail(generation)
        assertEquals(AudioGenerationLifecycle.Action.STOP_PRODUCERS, lifecycle.retire(generation, false, 1, 100))
        assertEquals(AudioGenerationLifecycle.Action.QUIESCE, lifecycle.producersClosed(generation, false))
        assertEquals(AudioGenerationLifecycle.Action.NONE, lifecycle.callbacksClosed(generation))
        // This is EncodedAudioEpoch.outstanding, used by BOTH real owner and controller.
        assertEquals(1, AudioSourceRetirement.outstanding(0, slots))
        val action = if (AudioSourceRetirement.outstanding(0, slots) == 0) lifecycle.handoffIdle(generation)
            else AudioGenerationLifecycle.Action.NONE
        assertEquals(AudioGenerationLifecycle.Action.NONE, action)
        assertEquals(AudioGenerationLifecycle.Stage.CALLBACKS, lifecycle.currentStage())
        assertFalse(lifecycle.actualClosed(generation))
        assertEquals(0, f.lastSample!!.closes)
    }

    @Test fun viewsCloseUnknownAfterQueueReachesZeroCannotPublishHandoffIdleOrDropTheOwner() {
        val f = Fixture(); val slot = f.slot(); val slots = arrayOf(slot)
        val posted = ArrayDeque<Runnable>(); var idleCallbacks = 0; var failures = 0
        val lifecycle = AudioGenerationLifecycle(); val generation = checkNotNull(lifecycle.begin())
        lifecycle.retire(generation, false, 1, 100); lifecycle.producersClosed(generation, false)
        lifecycle.callbacksClosed(generation)
        lateinit var queue: CoalescedOwnedQueue<AudioTimingTransfer<Sample, Views, ByteArray>>
        queue = CoalescedOwnedQueue(16, 65536, 250_000_000, { 0L }, { posted.addLast(it); true }, {},
            { failures++; lifecycle.fail(generation) }, {
                idleCallbacks++
                if (AudioSourceRetirement.outstanding(queue.outstanding, slots) == 0) lifecycle.handoffIdle(generation)
            })
        f.viewsCloseUnknown = true
        slot.preserve(byteArrayOf(1), 0, -1, -1); val views = slot.timingViews
        queue.offer(slot, 1); queue.close(drain = true)
        posted.removeFirst().run()
        assertEquals(0, queue.outstanding); assertEquals(1, idleCallbacks); assertEquals(1, failures)
        assertEquals(1, AudioSourceRetirement.outstanding(queue.outstanding, slots))
        assertEquals(AudioGenerationLifecycle.Stage.CALLBACKS, lifecycle.currentStage())
        assertFalse(lifecycle.actualClosed(generation)); assertTrue(slot.ownershipUnknown)
        assertThrows(IllegalStateException::class.java) { slot.close() }
        assertEquals(1, views.closes)
    }

    @Test fun knownViewsRefusalClosesOriginalAndAllowsActualResourceRetirementWithoutProgrammeAuthority() {
        val f = Fixture(); val slot = f.slot(); val slots = arrayOf(slot)
        f.rejectViews = true
        assertThrows(Rejected::class.java) { slot.preserve(byteArrayOf(1), 0, -1, -1) }
        val lifecycle = AudioGenerationLifecycle(); val generation = checkNotNull(lifecycle.begin())
        lifecycle.fail(generation) // A failed recording can still have known resource cleanup.
        lifecycle.retire(generation, false, 1, 100); lifecycle.producersClosed(generation, false)
        lifecycle.callbacksClosed(generation)
        assertEquals(0, AudioSourceRetirement.outstanding(0, slots))
        assertEquals(AudioGenerationLifecycle.Action.RELEASE, lifecycle.handoffIdle(generation))
        assertEquals(AudioGenerationLifecycle.Action.CLOSED, lifecycle.released(generation, true, 2))
        assertTrue(lifecycle.actualClosed(generation)); assertTrue(lifecycle.isFailed())
        assertEquals(1, f.lastSample!!.closes); assertTrue(slot.reusable)
    }
}
