package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioNativeAacGenerationTest {
    private val p = AudioAacProfile
    private fun format() = AudioAacProfile.Format(p.known("audio/mp4a-latm"), p.known(48000),
        p.known(1), p.known("1188"), p.missing(AudioAacProfile.State.ABSENT), p.missing(AudioAacProfile.State.ABSENT))
    private fun setup() = AudioAacProfile.Setup(AudioAacProfile.Selection(35,
        p.known("synthetic.alias"), p.known("synthetic.canonical"), p.known(true),
        p.known(false), p.known(false), AudioAacProfile.Request(96000)), format(), format())
    private inner class Run(preserve: Boolean = true, initialHead: AudioAacProfile.Fact<Int> = p.known(1024),
        initialSetup: AudioAacProfile.Setup = setup(), private val callbackFormat: AudioAacProfile.Format = format(),
        initialAccepted: Boolean = true) {
        val owner = Any()
        val codec = Any()
        val callback = Any()
        val ledger = AudioNativeAacGeneration(owner, codec, 7, preserve)
        init {
            assertTrue(ledger.bindCallback(owner, codec, callback))
            assertTrue(ledger.configured(owner, codec, initialSetup))
            assertEquals(initialAccepted, ledger.format(owner, codec, callback, AudioAacProfile.Phase.OUTPUT_FORMAT,
                callbackFormat, initialHead, p.missing(AudioAacProfile.State.ABSENT)))
        }
        fun input(raw: Long = 400, zero: Long = 112) {
            assertTrue(ledger.inputQueued(owner, codec, raw, zero))
            assertTrue(ledger.producersJoined(owner, codec, Math.addExact(raw, zero)))
            assertTrue(ledger.inputEosReturned(owner, codec))
        }
        fun begin(eos: Boolean = false, flags: Int = if (eos) 4 else 0, size: Int = 23,
            offset: Int = 0, capacity: Int = 23, started: Boolean = true,
            terminalTail: AudioAacProfile.Fact<Int> = p.known(512), terminalHead: AudioAacProfile.Fact<Int> = p.known(1024),
            terminalAccepted: Boolean = true,
        ): Long {
            val serial = ledger.output(owner, codec, callback, offset, size, flags, started, capacity)
            assertTrue(serial > 0)
            if (eos) assertEquals(terminalAccepted, ledger.format(owner, codec, callback, AudioAacProfile.Phase.OUTPUT_EOS,
                callbackFormat, terminalHead, terminalTail))
            return serial
        }
        fun finishTransfer(serial: Long, slot: Int = 0) {
            assertTrue(ledger.acquired(owner, codec, callback, serial, slot))
            assertTrue(ledger.outputReleased(owner, codec, callback, serial))
            assertTrue(ledger.handoffEntered(owner, codec, callback, serial, slot))
            assertTrue(ledger.handoffReturned(owner, codec, callback, serial, slot))
            assertTrue(ledger.transferClosed(owner, codec, callback, serial, slot))
        }
        fun healthyOutputs() { finishTransfer(begin()); finishTransfer(begin(eos = true)) }
        fun close(): AudioNativeAacGeneration.Receipt? {
            ledger.callbacksStopped(owner, codec, callback, true)
            return ledger.closed(owner, codec, true, true)
        }
    }

    @Test fun returnedNativeEventsIssueOnlyOpaqueGenerationScopeOnce() {
        val r = Run(); r.input(); r.healthyOutputs()
        val receipt = checkNotNull(r.close())
        assertEquals(7L, receipt.generation); assertEquals(512L, receipt.inputFrames)
        assertEquals(400L, receipt.rawFrames); assertEquals(112L, receipt.zeroFrames)
        assertEquals(2L, receipt.accessUnits); assertEquals(2048L, receipt.encodedFrames)
        assertEquals(1024, receipt.leadingFrames); assertEquals(512, receipt.trailingFrames)
        assertFalse(receipt.recorderConsumed); assertFalse(receipt.applicationAdmission)
        assertTrue(r.ledger.ownsReceipt(r.owner, r.codec, receipt))
        assertFalse(r.ledger.ownsReceipt(Any(), r.codec, receipt))
        val foreign = Run(); foreign.input(); foreign.healthyOutputs()
        assertFalse(r.ledger.ownsReceipt(r.owner, r.codec, checkNotNull(foreign.close())))
        assertNull(r.ledger.closed(r.owner, r.codec, true, true))
        assertFalse(r.ledger.inputQueued(r.owner, r.codec, 1, 0))
    }
    @Test fun wrongOwnerCodecAndGenerationCannotManufactureOrMutateReceipt() {
        val r = Run()
        assertFalse(r.ledger.inputQueued(Any(), r.codec, 400, 112))
        assertFalse(r.ledger.inputQueued(r.owner, Any(), 400, 112))
        assertEquals(0L, r.ledger.output(r.owner, r.codec, Any(), 0, 23, 0, true, 23))
        assertNull(r.ledger.closed(Any(), r.codec, true, true))
        try {
            AudioNativeAacGeneration.Receipt(r.ledger, Any(), 7, 512, 400, 112, 2, 2048, 1024, 512)
            fail("Arbitrary source scalars cannot create authority")
        } catch (_: IllegalArgumentException) { }
        val invalidGeneration = AudioNativeAacGeneration(r.owner, r.codec, 0, true)
        assertNull(invalidGeneration.closed(r.owner, r.codec, true, true))
        r.input(); r.healthyOutputs(); assertNotNull(r.close())
    }
    @Test fun nonemptyEosCountsAsOriginalAuAndEmptyEosDoesNot() {
        val nonempty = Run(); nonempty.input()
        nonempty.finishTransfer(nonempty.begin())
        nonempty.finishTransfer(nonempty.begin(eos = true))
        assertEquals(2L, checkNotNull(nonempty.close()).accessUnits)
        val empty = Run(); empty.input()
        empty.finishTransfer(empty.begin()); empty.finishTransfer(empty.begin())
        val end = empty.begin(eos = true, size = 0, capacity = -1)
        assertTrue(empty.ledger.outputReleased(empty.owner, empty.codec, empty.callback, end))
        assertEquals(2L, checkNotNull(empty.close()).accessUnits)
        val invalid = Run(); invalid.input()
        invalid.finishTransfer(invalid.begin()); invalid.finishTransfer(invalid.begin())
        val refused = invalid.begin(eos = true, size = 0, capacity = -1)
        assertFalse(invalid.ledger.acquired(invalid.owner, invalid.codec, invalid.callback, refused, 0))
        assertTrue(invalid.ledger.outputReleased(invalid.owner, invalid.codec, invalid.callback, refused))
        assertNull(invalid.close())
    }
    @Test fun configOutputNeedsOnlyActualReleaseAndDoesNotBecomeAnAu() {
        val r = Run(); r.input()
        val config = r.begin(flags = 2, size = 2, capacity = -1)
        assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, config))
        r.healthyOutputs(); assertEquals(2L, checkNotNull(r.close()).accessUnits)
    }
    @Test fun missingHeadCannotBeBackfilledByAnEosValueOrLengthDifference() {
        for (state in listOf(AudioAacProfile.State.ABSENT, AudioAacProfile.State.READ_FAILED,
            AudioAacProfile.State.API_UNAVAILABLE)) {
            val r = Run(initialHead = p.missing(state)); r.input(); r.healthyOutputs(); assertNull(r.close())
        }
    }
    @Test fun missingOrOutOfRangePaddingNeverDefaultsToZero() {
        val tails = listOf(p.missing<Int>(AudioAacProfile.State.ABSENT),
            p.missing(AudioAacProfile.State.READ_FAILED), p.known(-1), p.known(1024))
        for (tail in tails) {
            val r = Run(); r.input(); r.finishTransfer(r.begin())
            r.finishTransfer(r.begin(eos = true, terminalTail = tail,
                terminalAccepted = tail.state != AudioAacProfile.State.VALUE || (tail.value ?: -1) in 0..1023))
            assertNull(r.close())
        }
    }
    @Test fun partialUnknownFlagsEarlyAndMalformedBorrowNeverCountAsEligibleOriginal() {
        for ((flags, offset, capacity, started) in listOf(
            listOf(8, 0, 23, 1), listOf(1, 0, 23, 1), listOf(0, 1, 23, 1),
            listOf(0, -1, 23, 1), listOf(0, 0, -1, 1), listOf(0, 0, 23, 0))) {
            val r = Run(); r.input()
            val s = r.begin(flags = flags, offset = offset, capacity = capacity, started = started == 1)
            assertFalse(r.ledger.acquired(r.owner, r.codec, r.callback, s, 0))
            r.ledger.outputReleased(r.owner, r.codec, r.callback, s)
            assertNull(r.close())
        }
    }
    @Test fun configuredBatchingContradictionCannotDisappearAtCallback() {
        val r = Run(initialSetup = setup().copy(output = format().copy(batchMaximum = p.known(8192))))
        r.input(); val serial = r.begin()
        assertFalse(r.ledger.acquired(r.owner, r.codec, r.callback, serial, 0))
        r.ledger.outputReleased(r.owner, r.codec, r.callback, serial)
        assertNull(r.close())
    }
    @Test fun nativeOutputReleaseUnknownPreventsAuthorityEvenAfterAcquisition() {
        val r = Run(); r.input(); val s = r.begin()
        assertTrue(r.ledger.acquired(r.owner, r.codec, r.callback, s, 0))
        assertTrue(r.ledger.fault(r.owner, r.codec)) // actual releaseOutputBuffer threw
        assertFalse(r.ledger.callbacksStopped(r.owner, r.codec, r.callback, true))
        assertNull(r.ledger.closed(r.owner, r.codec, true, true))
    }
    @Test fun handoffReturnAndCheckedCloseAreSeparateRequiredEvents() {
        for (omit in listOf("entry", "return", "close")) {
            val r = Run(); r.input(); r.finishTransfer(r.begin())
            val s = r.begin(eos = true)
            assertTrue(r.ledger.acquired(r.owner, r.codec, r.callback, s, 0))
            assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, s))
            if (omit != "entry") r.ledger.handoffEntered(r.owner, r.codec, r.callback, s, 0)
            if (omit == "close") r.ledger.handoffReturned(r.owner, r.codec, r.callback, s, 0)
            if (omit != "close") r.ledger.transferClosed(r.owner, r.codec, r.callback, s, 0)
            assertNull(r.close())
        }
    }
    @Test fun viewsCloseUnknownRetainsOccupiedSlotAfterCallbackReturned() {
        val r = Run(); r.input(); r.finishTransfer(r.begin()); val s = r.begin(eos = true)
        assertTrue(r.ledger.acquired(r.owner, r.codec, r.callback, s, 15))
        assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, s))
        assertTrue(r.ledger.handoffEntered(r.owner, r.codec, r.callback, s, 15))
        assertTrue(r.ledger.handoffReturned(r.owner, r.codec, r.callback, s, 15))
        r.ledger.fault(r.owner, r.codec) // no transferClosed after unknown native close
        assertNull(r.close())
    }
    @Test fun recycledSlotRejectsStaleSerialAndDuplicateEvents() {
        val r = Run(); r.input(); val first = r.begin(); r.finishTransfer(first)
        val second = r.begin(eos = true); assertTrue(r.ledger.acquired(r.owner, r.codec, r.callback, second, 0))
        assertFalse(r.ledger.transferClosed(r.owner, r.codec, r.callback, first, 0))
        assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, second))
        assertTrue(r.ledger.handoffEntered(r.owner, r.codec, r.callback, second, 0))
        assertFalse(r.ledger.handoffEntered(r.owner, r.codec, r.callback, second, 0))
        assertNull(r.close())
    }
    @Test fun missingNativeBarriersAndFailedReleaseRefuseAfterOtherwiseHealthyOutput() {
        for (failure in listOf("stop", "codec", "mic")) {
            val r = Run(); r.input(); r.healthyOutputs()
            r.ledger.callbacksStopped(r.owner, r.codec, r.callback, failure != "stop")
            assertNull(r.ledger.closed(r.owner, r.codec, failure != "codec", failure != "mic"))
        }
    }
    @Test fun acceptedInputJoinAndOverflowAreCheckedWithoutSolvingForTail() {
        val mismatch = Run(); assertTrue(mismatch.ledger.inputQueued(mismatch.owner, mismatch.codec, 400, 112))
        assertFalse(mismatch.ledger.producersJoined(mismatch.owner, mismatch.codec, 513))
        assertNull(mismatch.close())
        val overflow = Run()
        assertTrue(overflow.ledger.inputQueued(overflow.owner, overflow.codec, Long.MAX_VALUE, 0))
        assertFalse(overflow.ledger.inputQueued(overflow.owner, overflow.codec, 1, 0))
        assertNull(overflow.close())
        val wrongExtent = Run(); wrongExtent.input(401, 112); wrongExtent.healthyOutputs()
        assertNull(wrongExtent.close()) // actual supplied T512 cannot be fitted to511
    }
    @Test fun missingInputOrOutputEosAndLegacyModeCannotIssue() {
        val noInputEos = Run()
        assertTrue(noInputEos.ledger.inputQueued(noInputEos.owner, noInputEos.codec, 400, 112))
        assertTrue(noInputEos.ledger.producersJoined(noInputEos.owner, noInputEos.codec, 512))
        noInputEos.healthyOutputs(); assertNull(noInputEos.close())
        val noOutputEos = Run(); noOutputEos.input(); noOutputEos.finishTransfer(noOutputEos.begin())
        noOutputEos.finishTransfer(noOutputEos.begin()); assertNull(noOutputEos.close())
        val legacy = Run(preserve = false); legacy.input(); legacy.healthyOutputs(); assertNull(legacy.close())
    }
    @Test fun terminalFormatAndTrimConflictsCannotPreserveEarlierEligibility() {
        val r = Run(); r.input(); r.finishTransfer(r.begin())
        val s = r.ledger.output(r.owner, r.codec, r.callback, 0, 23, 4, true, 23)
        assertFalse(r.ledger.format(r.owner, r.codec, r.callback, AudioAacProfile.Phase.OUTPUT_EOS,
            format().copy(rate = p.known(44100)), p.known(1024), p.known(512)))
        assertFalse(r.ledger.acquired(r.owner, r.codec, r.callback, s, 0))
        assertNull(r.close())
        val trim = Run(); trim.input(); trim.finishTransfer(trim.begin())
        trim.ledger.output(trim.owner, trim.codec, trim.callback, 0, 23, 4, true, 23)
        assertFalse(trim.ledger.format(trim.owner, trim.codec, trim.callback, AudioAacProfile.Phase.OUTPUT_EOS,
            format(), p.known(1025), p.known(512)))
        assertNull(trim.close())
    }
    @Test fun allSixteenPendingTransfersAreBoundedAndCannotBeReusedBeforeClose() {
        val r = Run(); r.input(14848, 0) // 16*1024 - H1024 - T512
        val serials = LongArray(16)
        for (index in 0 until 16) {
            val s = r.begin(eos = index == 15); serials[index] = s
            assertTrue(r.ledger.acquired(r.owner, r.codec, r.callback, s, index))
            assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, s))
        }
        for (index in 0 until 16) {
            assertTrue(r.ledger.handoffEntered(r.owner, r.codec, r.callback, serials[index], index))
            assertTrue(r.ledger.handoffReturned(r.owner, r.codec, r.callback, serials[index], index))
            assertTrue(r.ledger.transferClosed(r.owner, r.codec, r.callback, serials[index], index))
        }
        assertEquals(16L, checkNotNull(r.close()).accessUnits)
        val occupied = Run(); occupied.input()
        val first = occupied.begin()
        assertTrue(occupied.ledger.acquired(occupied.owner, occupied.codec, occupied.callback, first, 0))
        assertTrue(occupied.ledger.outputReleased(occupied.owner, occupied.codec, occupied.callback, first))
        val second = occupied.begin(eos = true)
        assertFalse(occupied.ledger.acquired(occupied.owner, occupied.codec, occupied.callback, second, 0))
        assertNull(occupied.close())
    }
    @Test fun genuineZeroTrimValuesAndSupportedBoundsAreNotMissingValues() {
        val noTrim = Run(initialHead = p.known(0)); noTrim.input(2048, 0)
        noTrim.finishTransfer(noTrim.begin())
        noTrim.finishTransfer(noTrim.begin(eos = true, terminalHead = p.known(0), terminalTail = p.known(0)))
        val zeroReceipt = checkNotNull(noTrim.close())
        assertEquals(0, zeroReceipt.leadingFrames); assertEquals(0, zeroReceipt.trailingFrames)
        val bound = Run(initialHead = p.known(8191)); bound.input(1026, 0)
        repeat(9) { bound.finishTransfer(bound.begin()) }
        bound.finishTransfer(bound.begin(eos = true, terminalHead = p.known(8191), terminalTail = p.known(1023)))
        assertEquals(10240L, checkNotNull(bound.close()).encodedFrames)
    }
    @Test fun unknownOrMalformedTrimAndNoInputStayUnqualified() {
        val malformed = Run(); malformed.input(); malformed.finishTransfer(malformed.begin())
        malformed.begin(eos = true, terminalHead = p.missing(AudioAacProfile.State.WRONG_TYPE), terminalAccepted = false)
        assertNull(malformed.close())
        val empty = Run()
        assertTrue(empty.ledger.producersJoined(empty.owner, empty.codec, 0))
        assertNull(empty.close()) // existing no-input closure is not a native AAC timing receipt
    }

    @Test fun preStartConfigOrEmptyOutputCannotBeErasedByLaterHealthyCallbacks() {
        for (config in listOf(true, false)) {
            val r = Run(); r.input()
            val early = r.begin(flags = if (config) 2 else 0, size = if (config) 2 else 0,
                capacity = -1, started = false)
            assertTrue(r.ledger.outputReleased(r.owner, r.codec, r.callback, early))
            r.healthyOutputs()
            assertNull(r.close())
        }
    }

    @Test fun setupCannotMoveAfterNativeOutputToRepairEligibility() {
        val owner = Any(); val codec = Any(); val callback = Any()
        val ledger = AudioNativeAacGeneration(owner, codec, 7, true)
        assertTrue(ledger.bindCallback(owner, codec, callback))
        val output = ledger.output(owner, codec, callback, 0, 2, 2, true, -1)
        assertTrue(output > 0)
        assertTrue(ledger.outputReleased(owner, codec, callback, output))
        assertFalse(ledger.configured(owner, codec, setup()))
        assertNull(ledger.closed(owner, codec, true, true))
    }

    @Test fun valueStateWithMissingTrimPayloadIsMalformedNotZero() {
        val r = Run(initialHead = AudioAacProfile.Fact(AudioAacProfile.State.VALUE), initialAccepted = false)
        assertNull(r.close())
    }

    @Test fun api35EffectiveZeroPairKeepsDistinctEligibilityAndCanQualify() {
        val zero = format().copy(batchMaximum = p.known(0), batchThreshold = p.known(0))
        val base = setup()
        val facts = base.copy(selection = base.selection.copy(multipleSupported = p.known(true)), input = zero, output = zero)
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_API35_EFFECTIVE_ZERO, p.evaluate(facts.selection, zero))
        val r = Run(initialSetup = facts, callbackFormat = zero)
        r.input(); r.healthyOutputs()
        assertEquals(2L, checkNotNull(r.close()).accessUnits)
    }

    @Test fun prematureIssueAttemptSealsTheGenerationAgainstLateRepair() {
        val r = Run()
        assertNull(r.ledger.closed(r.owner, r.codec, true, true))
        assertFalse(r.ledger.inputQueued(r.owner, r.codec, 400, 112))
        assertNull(r.ledger.closed(r.owner, r.codec, true, true))
    }
}
