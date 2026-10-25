package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.NativeAudioFileReceipt
import dev.forgesworn.sapflow.NativeAudioGenerationReceipt
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Test

class AudioNativeAacRunTest {
    private val p = AudioAacProfile
    private fun format() = AudioAacProfile.Format(p.known("audio/mp4a-latm"), p.known(48000),
        p.known(1), p.known("1188"), p.missing(AudioAacProfile.State.ABSENT), p.missing(AudioAacProfile.State.ABSENT))
    private fun setup() = AudioAacProfile.Setup(AudioAacProfile.Selection(35,
        p.known("synthetic.alias"), p.known("synthetic.canonical"), p.known(true),
        p.known(false), p.known(false), AudioAacProfile.Request(96000)), format(), format())

    private class Worker : AudioNativeAacWorker {
        val events = mutableListOf<String>()
        val generations = mutableListOf<NativeAudioGenerationReceipt>()
        val files = mutableListOf<NativeAudioFileReceipt>()
        var beginAttempts = 0
        var endAttempts = 0
        var completeAttempts = 0
        var failBegin = false
        var failEnd = false
        var failComplete = false
        var duringBegin: () -> Unit = {}
        var duringEnd: () -> Unit = {}
        var duringComplete: () -> Unit = {}
        override fun beginAudio(firstLeadingFrames: UInt) {
            beginAttempts++
            if (failBegin) throw IllegalStateException("injected begin failure")
            events.add("begin:$firstLeadingFrames")
            duringBegin()
        }
        override fun generationEnded(receipt: NativeAudioGenerationReceipt) {
            endAttempts++
            if (failEnd) throw IllegalStateException("injected generation failure")
            generations.add(receipt)
            events.add("end:${receipt.generation}")
            duringEnd()
        }
        override fun completeAudio(receipt: NativeAudioFileReceipt) {
            completeAttempts++
            if (failComplete) throw IllegalStateException("injected file failure")
            files.add(receipt)
            events.add("complete:${receipt.generationCount}")
            duringComplete()
        }
    }

    private inner class Generation(val fixture: Fixture, val serial: Long, register: Boolean = true,
        observeLeading: Boolean = true) {
        val token = Any()
        private val codec = Any()
        private val callback = Any()
        private val collector = AudioNativeAacGeneration(token, codec, serial, true)
        init {
            assertTrue(collector.bindCallback(token, codec, callback))
            assertTrue(collector.configured(token, codec, setup()))
            assertTrue(collector.format(token, codec, callback, AudioAacProfile.Phase.OUTPUT_FORMAT,
                format(), p.known(1024), p.missing(AudioAacProfile.State.ABSENT)))
            if (register) assertTrue(fixture.run.started(fixture.owner, token, serial, collector))
            if (register && observeLeading) assertTrue(fixture.run.leading(fixture.owner, token, 1024))
        }
        fun close(): AudioNativeAacGeneration.Receipt {
            assertTrue(collector.inputQueued(token, codec, 400, 112))
            assertTrue(collector.producersJoined(token, codec, 512))
            assertTrue(collector.inputEosReturned(token, codec))
            repeat(2) { index ->
                val eos = index == 1
                val output = collector.output(token, codec, callback, 0, 23, if (eos) 4 else 0, true, 23)
                assertTrue(output > 0)
                if (eos) assertTrue(collector.format(token, codec, callback, AudioAacProfile.Phase.OUTPUT_EOS,
                    format(), p.known(1024), p.known(512)))
                assertTrue(collector.acquired(token, codec, callback, output, 0))
                assertTrue(collector.outputReleased(token, codec, callback, output))
                assertTrue(collector.handoffEntered(token, codec, callback, output, 0))
                assertTrue(collector.handoffReturned(token, codec, callback, output, 0))
                assertTrue(collector.transferClosed(token, codec, callback, output, 0))
            }
            assertTrue(collector.callbacksStopped(token, codec, callback, true))
            return checkNotNull(collector.closed(token, codec, true, true))
        }
        fun closeEmpty() {
            assertTrue(collector.producersJoined(token, codec, 0))
            assertTrue(collector.callbacksStopped(token, codec, callback, true))
            assertNull(collector.closed(token, codec, true, true))
            assertTrue(fixture.run.nativeClosed(fixture.owner, token, 0, null))
        }
    }

    private inner class Fixture {
        val owner = Any()
        val run = AudioNativeAacRun(owner, 7)
        val worker = Worker()
        fun generation(serial: Long, register: Boolean = true, observeLeading: Boolean = true) =
            Generation(this, serial, register, observeLeading)
        fun accept(generation: Generation, count: Int = 2) {
            repeat(count) { assertTrue(run.accepted(owner, generation.token)) }
        }
        fun closeNative(generation: Generation): AudioNativeAacGeneration.Receipt {
            val receipt = generation.close()
            assertTrue(run.nativeClosed(owner, generation.token, 512, receipt))
            return receipt
        }
        fun push() {
            run.beforeSample(worker)
            worker.events.add("push")
            run.afterSample(worker)
        }
        fun seal() { assertTrue(run.seal(owner)) }
    }

    @Test fun beginPrecedesPushAndGenerationEndsAtExactAcceptedFrontier() {
        val f = Fixture(); val g = f.generation(7)
        f.accept(g, count = 1); f.push(); f.accept(g, count = 1)
        val native = f.closeNative(g)
        assertTrue(f.worker.generations.isEmpty())
        f.push(); f.seal(); f.run.finish(f.worker)
        assertEquals(7L, native.generation)
        assertEquals(1uL, f.worker.generations.single().generation)
        assertEquals(listOf("begin:1024", "push", "push", "end:1", "complete:1"), f.worker.events)
        assertFalse(native.recorderConsumed)
        assertFalse(native.applicationAdmission)
    }

    @Test fun delayedWorkerCannotOvertakeTwoNativeGenerations() {
        val f = Fixture()
        val first = f.generation(7); f.accept(first); f.closeNative(first)
        val second = f.generation(9); f.accept(second); f.closeNative(second)
        repeat(4) { f.push() }
        f.seal(); f.run.finish(f.worker)
        assertEquals(listOf("begin:1024", "push", "push", "end:1", "push", "push", "end:2", "complete:2"), f.worker.events)
        assertEquals(listOf(1uL, 2uL), f.worker.generations.map { it.generation })
        assertEquals(1024uL, f.worker.files.single().sourceFramesAt48k)
        assertEquals(1536uL, f.worker.files.single().intentionalStandbyFrames)
    }

    @Test fun zeroInputGenerationIsSkippedWithoutReusingPhysicalSerialAsSdkSequence() {
        val f = Fixture(); f.generation(7).closeEmpty()
        val next = f.generation(9); f.accept(next)
        val native = f.closeNative(next)
        repeat(2) { f.push() }
        f.seal(); f.run.finish(f.worker)
        assertEquals(9L, native.generation)
        assertEquals(1uL, f.worker.generations.single().generation)
        assertEquals(1uL, f.worker.files.single().generationCount)
    }

    @Test fun equalScalarsFromForeignIssuerCannotCrossExpectedGeneration() {
        val f = Fixture(); val expected = f.generation(7); f.accept(expected)
        val foreign = f.generation(7, register = false).close()
        assertFalse(f.run.nativeClosed(f.owner, expected.token, 512, foreign))
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertTrue(f.worker.generations.isEmpty())
        assertTrue(f.worker.files.isEmpty())
    }

    @Test fun replayedNativeClosureCannotDeliverTheIssuedInstanceTwice() {
        val f = Fixture(); val g = f.generation(7); f.accept(g)
        val receipt = f.closeNative(g)
        assertFalse(f.run.nativeClosed(f.owner, g.token, 512, receipt))
        repeat(2) { f.push() }
        assertFalse(f.run.nativeClosed(f.owner, g.token, 512, receipt))
        f.seal(); f.run.finish(f.worker)
        assertEquals(1, f.worker.endAttempts)
        assertEquals(1, f.worker.completeAttempts)
    }

    @Test fun missingAcceptedOriginalCannotBeFilledByLaterGenerationOrReceiptScalars() {
        val f = Fixture(); val g = f.generation(7); f.accept(g, count = 1)
        val receipt = g.close()
        assertFalse(f.run.nativeClosed(f.owner, g.token, 512, receipt))
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertEquals(0, f.worker.beginAttempts)
        assertTrue(f.worker.generations.isEmpty())
    }

    @Test fun missingLeadingRefusesBeforeSdkBeginOrFirstPush() {
        val f = Fixture(); val g = f.generation(7, observeLeading = false)
        assertFalse(f.run.accepted(f.owner, g.token))
        assertThrows(IllegalStateException::class.java) { f.run.beforeSample(f.worker) }
        assertEquals(0, f.worker.beginAttempts)
        assertTrue(f.worker.events.isEmpty())
    }

    @Test fun failedBeginIsNeitherReturnedNorRetried() {
        val f = Fixture(); val g = f.generation(7); f.accept(g)
        f.worker.failBegin = true
        assertThrows(IllegalStateException::class.java) { f.run.beforeSample(f.worker) }
        assertThrows(IllegalStateException::class.java) { f.run.beforeSample(f.worker) }
        assertEquals(1, f.worker.beginAttempts)
        assertTrue(f.worker.events.isEmpty())
    }

    @Test fun failedGenerationCallCannotBeReplayedAsAccepted() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g)
        f.worker.failEnd = true
        f.push()
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertEquals(1, f.worker.endAttempts)
        assertTrue(f.worker.generations.isEmpty())
    }

    @Test fun failedFileCallCannotBeReplayedOrPromotedToAdmission() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g)
        repeat(2) { f.push() }
        f.seal(); f.worker.failComplete = true
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertEquals(1, f.worker.completeAttempts)
        assertTrue(f.worker.files.isEmpty())
    }

    @Test fun foreignWorkerAndWorkerThreadCannotInheritBoundAuthority() {
        val f = Fixture(); val g = f.generation(7); f.accept(g)
        f.run.beforeSample(f.worker)
        val foreign = Worker()
        assertThrows(IllegalStateException::class.java) { f.run.beforeSample(foreign) }
        assertEquals(0, foreign.beginAttempts)
        val error = AtomicReference<Throwable?>()
        val thread = Thread {
            try { f.run.beforeSample(f.worker) } catch (value: Throwable) { error.set(value) }
        }
        thread.start(); thread.join(5000)
        assertFalse(thread.isAlive)
        assertNotNull(error.get())
        f.run.afterSample(f.worker)
        assertEquals(1, f.worker.beginAttempts)
    }

    @Test fun pendingGenerationLedgerIsBoundedWithoutPerOutputTokens() {
        val f = Fixture()
        for (serial in 1L..17L) {
            val g = f.generation(serial); f.accept(g)
            val receipt = g.close()
            assertEquals(serial < 17L, f.run.nativeClosed(f.owner, g.token, 512, receipt))
        }
        assertFalse(f.run.seal(f.owner))
        assertTrue(f.worker.generations.isEmpty())
    }

    @Test fun nativeCloseDuringPausedWorkerWaitsForActualSecondConsumption() {
        val f = Fixture(); val g = f.generation(7); f.accept(g, count = 1)
        val firstDone = CountDownLatch(1)
        val releaseSecond = CountDownLatch(1)
        val error = AtomicReference<Throwable?>()
        val thread = Thread {
            try {
                f.push()
                firstDone.countDown()
                check(releaseSecond.await(5, TimeUnit.SECONDS))
                f.push()
                f.run.finish(f.worker)
            } catch (value: Throwable) {
                error.set(value)
                firstDone.countDown()
            }
        }
        thread.start()
        assertTrue(firstDone.await(5, TimeUnit.SECONDS))
        f.accept(g, count = 1)
        val native = g.close()
        assertTrue(f.run.nativeClosed(f.owner, g.token, 512, native))
        assertTrue(f.worker.generations.isEmpty())
        f.seal()
        releaseSecond.countDown()
        thread.join(5000)
        assertFalse(thread.isAlive)
        assertNull(error.get())
        assertEquals(listOf("begin:1024", "push", "push", "end:1", "complete:1"), f.worker.events)
    }

    @Test fun allEmptyRunClosesWithoutBeginOrNativeFileReceipt() {
        val f = Fixture(); f.generation(7).closeEmpty(); f.seal()
        assertEquals(AudioNativeAacFinish.EMPTY, f.run.finish(f.worker))
        assertEquals(AudioNativeAacFinish.EMPTY, f.run.finish(f.worker))
        assertEquals(0, f.worker.beginAttempts)
        assertEquals(0, f.worker.endAttempts)
        assertEquals(0, f.worker.completeAttempts)
        assertTrue(f.worker.events.isEmpty())
    }

    @Test fun videoAcceptedBeforeAudioWaitsThenPreservesTheMediaFrontier() {
        val f = Fixture(); val g = f.generation(7)
        assertTrue(f.run.acceptedVideo())
        assertTrue(f.run.awaitingFirstAudio())
        assertTrue(f.worker.events.isEmpty())
        f.accept(g, count = 1)
        assertFalse(f.run.awaitingFirstAudio())
        f.run.beforeSample(f.worker)
        f.worker.events.add("video")
        f.run.afterSample(f.worker)
        f.push(); f.accept(g, count = 1); f.closeNative(g); f.push()
        f.seal()
        assertEquals(AudioNativeAacFinish.FILE_RECEIPT, f.run.finish(f.worker))
        assertEquals(listOf("begin:1024", "video", "push", "push", "end:1", "complete:1"), f.worker.events)
    }

    @Test fun cancellationDuringSuccessfulBeginPreventsTheFirstPushAndRetry() {
        val f = Fixture(); val g = f.generation(7); f.accept(g)
        f.worker.duringBegin = { f.run.fail() }
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertEquals(listOf("begin:1024"), f.worker.events)
        assertEquals(1, f.worker.beginAttempts)
    }

    @Test fun recordingLossAfterBalancedAcceptedPrefixStillBlocksFileCompletion() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g)
        repeat(2) { f.push() }; f.seal()
        f.run.fail() // A rejected video was never part of the accepted counters.
        assertFalse(f.run.awaitingFirstAudio())
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertEquals(1, f.worker.endAttempts)
        assertEquals(0, f.worker.completeAttempts)
        assertTrue(f.worker.files.isEmpty())
    }

    @Test fun cancellationDuringSuccessfulGenerationCallPreventsFurtherSuccess() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g)
        f.worker.duringEnd = { f.run.fail() }
        f.push()
        assertThrows(IllegalStateException::class.java) { f.push() }
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertEquals(1, f.worker.endAttempts)
        assertEquals(0, f.worker.completeAttempts)
    }

    @Test fun cancellationDuringSuccessfulFileCallbackCannotReturnCompletionOrRetry() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g)
        repeat(2) { f.push() }; f.seal()
        f.worker.duringComplete = { f.run.fail() }
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertThrows(IllegalStateException::class.java) { f.run.finish(f.worker) }
        assertEquals(1, f.worker.completeAttempts)
    }

    @Test fun concurrentCancellationDuringFileCallbackCannotReturnCompletion() {
        val f = Fixture(); val g = f.generation(7); f.accept(g); f.closeNative(g); f.seal()
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val error = AtomicReference<Throwable?>(); val outcome = AtomicReference<AudioNativeAacFinish?>()
        f.worker.duringComplete = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        val thread = Thread {
            try { repeat(2) { f.push() }; outcome.set(f.run.finish(f.worker)) }
            catch (failure: Throwable) { error.set(failure); entered.countDown() }
        }
        thread.start()
        try { assertTrue(entered.await(5, TimeUnit.SECONDS)); f.run.fail() }
        finally { release.countDown(); thread.join(5000) }
        assertFalse(thread.isAlive); assertNull(outcome.get())
        assertTrue(error.get() is IllegalStateException); assertEquals(1, f.worker.completeAttempts)
    }

    @Test fun consumptionCannotBeReplayedOrReportedWithoutItsPendingPush() {
        val f = Fixture(); val g = f.generation(7); f.accept(g)
        assertThrows(IllegalStateException::class.java) { f.run.afterSample(f.worker) }
        assertEquals(0, f.worker.beginAttempts)
        val other = Fixture(); val next = other.generation(7); other.accept(next)
        other.run.beforeSample(other.worker)
        assertThrows(IllegalStateException::class.java) { other.run.beforeSample(other.worker) }
        assertTrue(other.worker.generations.isEmpty())
    }

    @Test fun failedNativeCloseIsNotRetriedAndSuccessfulCloseDoesNotRepeat() {
        var attempts = 0
        val failed = NativeCloseOnce()
        assertThrows(IllegalStateException::class.java) {
            failed.close { attempts++; throw IllegalStateException("injected unknown close") }
        }
        assertTrue(failed.closeUnknown)
        assertThrows(IllegalStateException::class.java) { failed.close { attempts++ } }
        assertEquals(1, attempts)
        val returned = NativeCloseOnce()
        returned.close { attempts++ }
        returned.close { attempts++ }
        assertEquals(2, attempts)
        assertFalse(returned.closeUnknown)
    }
}
