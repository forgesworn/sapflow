package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioAacProfileTest {
    private val p = AudioAacProfile
    private fun selection(api: Int = 35, multiple: Boolean = false) = AudioAacProfile.Selection(api,
        p.known("codec.alias"), p.known("codec.canonical"), p.known(true), p.known(multiple),
        p.known(false), AudioAacProfile.Request(96000))
    private fun output(api: Int = 35): AudioAacProfile.Format {
        val batch: AudioAacProfile.Fact<Int> = p.missing(if (api >= 35) AudioAacProfile.State.ABSENT else AudioAacProfile.State.API_UNAVAILABLE)
        return AudioAacProfile.Format(p.known("audio/mp4a-latm"), p.known(48000), p.known(1), p.known("1188"), batch, batch)
    }
    private fun setup(api: Int = 35) = AudioAacProfile.Setup(selection(api), output(api), output(api))
    @Test fun api29SingleCapabilityAndApi35DefaultAreDistinctDocumentedReasons() {
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_CAPABILITY, p.evaluate(selection(29), output(29)))
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_API35_DEFAULT, p.evaluate(selection(35, true), output()))
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection(34, true), output(34)))
    }
    @Test fun missingFailedOrUnavailableCapabilityIsNeverFalse() {
        for (state in listOf(AudioAacProfile.State.ABSENT, AudioAacProfile.State.API_UNAVAILABLE,
            AudioAacProfile.State.READ_FAILED, AudioAacProfile.State.WRONG_TYPE)) {
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection().copy(multipleSupported=p.missing(state)), output()))
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection().copy(multipleRequired=p.missing(state)), output()))
        }
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection().copy(multipleSupported=AudioAacProfile.Fact(AudioAacProfile.State.VALUE)), output()))
    }
    @Test fun actualEncoderIdentityAndSupportedApiAreRequired() {
        for (value in listOf(selection(28), selection().copy(name=p.missing(AudioAacProfile.State.READ_FAILED)),
            selection().copy(canonicalName=p.known("")), selection().copy(canonicalName=p.known("x".repeat(257))),
            selection().copy(encoder=p.known(false)), selection().copy(multipleRequired=p.known(true))))
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(value, output()))
    }
    @Test fun returnedBatchValuesAndUnsupportedReadsDoNotBecomeDefault() {
        for (fact in listOf(p.known(0), p.known(1), p.known(4096), p.known(-1),
            p.missing<Int>(AudioAacProfile.State.READ_FAILED), p.missing<Int>(AudioAacProfile.State.WRONG_TYPE),
            p.missing<Int>(AudioAacProfile.State.API_UNAVAILABLE))) {
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection(), output().copy(batchMaximum=fact)))
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection(35,true), output().copy(batchThreshold=fact)))
        }
    }
    @Test fun changedRequestedBatchingAndGeometryRefuseRatherThanRetune() {
        val request = selection().request
        for (r in listOf(request.copy(batchMaximum=p.known(1)), request.copy(batchThreshold=p.known(1)),
            request.copy(rate=44100), request.copy(channels=2), request.copy(aacProfile=5),
            request.copy(maxInputBytes=8192), request.copy(bitrate=0)))
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection().copy(request=r), output()))
    }
    @Test fun actualReturnedAacGeometryAndCsdAreChecked() {
        for (f in listOf(output().copy(mime=p.known("audio/raw")), output().copy(rate=p.known(44100)),
            output().copy(channels=p.known(2)), output().copy(csd0=p.known("1190")),
            output().copy(csd0=p.missing(AudioAacProfile.State.ABSENT))))
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection(), f))
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(null, output()))
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, p.evaluate(selection(), null))
    }
    @Test fun originalNonemptyEosIsEligibleButConfigAndEmptyAreNotAus() {
        val eligible = p.evaluate(selection(), output())
        assertEquals(AudioAacProfile.Output.SINGLE_AU_ELIGIBLE, p.output(eligible, 7, 23, 4, true, 30))
        assertEquals(AudioAacProfile.Output.SINGLE_AU_ELIGIBLE, p.output(eligible, 0, 23, 0, true, 23))
        for ((size, flags) in listOf(2 to 2, 0 to 4, 0 to 0, 0 to 2))
            assertEquals(AudioAacProfile.Output.EXCLUDED, p.output(eligible, 0, size, flags, true, size))
    }
    @Test fun partialEarlyUnknownFlagsAndBadRangesAreNeverCounted() {
        val e = p.evaluate(selection(), output())
        for (flags in listOf(8, 12, 16, 1, Int.MIN_VALUE))
            assertEquals(AudioAacProfile.Output.EXCLUDED, p.output(e, 0, 23, flags, true, 23))
        assertEquals(AudioAacProfile.Output.EXCLUDED, p.output(e, 0, 23, 0, false, 23))
        for ((offset, size, capacity) in listOf(Triple(-1,23,23), Triple(0,-1,23), Triple(1,23,23), Triple(Int.MAX_VALUE,1,Int.MAX_VALUE)))
            assertEquals(AudioAacProfile.Output.EXCLUDED, p.output(e, offset, size, 0, true, capacity))
        assertEquals(AudioAacProfile.Output.UNKNOWN, p.output(e, 0, 23, 0, true, -1))
        assertEquals(AudioAacProfile.Output.UNKNOWN, p.output(AudioAacProfile.Eligibility.UNKNOWN, 0, 23, 0, true, 23))
    }
    @Test fun exactOwnerAndGenerationRejectForeignSetupAndLateMutation() {
        val owner = Any(); val foreign = Any(); val o = AudioAacProfile.Observation(owner, 7)
        assertFalse(o.configured(foreign, 7, setup()))
        assertFalse(o.configured(owner, 6, setup()))
        assertTrue(o.configured(owner, 7, setup()))
        assertTrue(o.format(owner, 7, AudioAacProfile.Phase.OUTPUT_FORMAT, output()))
        assertNull(o.freeze(foreign, 7))
        val frozen = checkNotNull(o.freeze(owner, 7))
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_CAPABILITY, frozen.eligibility)
        assertFalse(o.format(owner, 7, AudioAacProfile.Phase.OUTPUT_EOS, output().copy(rate=p.known(44100))))
        assertFalse(o.configured(owner, 7, setup(29)))
        assertSame(frozen, o.freeze(owner, 7))
    }
    @Test fun changedSelectionOrTerminalFormatIsStickyUnknown() {
        val owner = Any(); val o = AudioAacProfile.Observation(owner, 1)
        o.configured(owner, 1, setup()); o.format(owner,1,AudioAacProfile.Phase.OUTPUT_FORMAT,output())
        assertFalse(o.format(owner,1,AudioAacProfile.Phase.OUTPUT_EOS,output().copy(rate=p.known(44100))))
        o.format(owner,1,AudioAacProfile.Phase.OUTPUT_EOS,output())
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, o.eligibility(owner,1))
        val other = AudioAacProfile.Observation(owner,2)
        other.configured(owner,2,setup()); assertFalse(other.configured(owner,2,setup().copy(selection=selection().copy(name=p.known("replacement")))))
        other.format(owner,2,AudioAacProfile.Phase.OUTPUT_FORMAT,output())
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN, other.eligibility(owner,2))
    }
    @Test fun realGenerationObservationKeepsItsExistingBarrierAndFreezeOwnership() {
        val o = AudioGenerationObservation(1)
        assertFalse(o.profileConfigured(2,setup()))
        assertTrue(o.profileConfigured(1,setup()))
        assertTrue(o.profileFormat(1,AudioGenerationObservation.Phase.FORMAT,output()))
        assertNull(o.freeze(1))
        o.producersJoined(1); o.callbackBarrier(1,true); o.handoffIdle(1)
        val frozen = checkNotNull(o.freeze(1)); val profile = checkNotNull(frozen.selectedProfile)
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_CAPABILITY,profile.eligibility)
        assertFalse(o.profileFormat(1,AudioGenerationObservation.Phase.OUTPUT_EOS,output()))
        assertFalse(profile.nativeSourceQualified); assertFalse(profile.sdkReceiptIssued); assertFalse(profile.appAdmission)
        assertSame(frozen,o.freeze(1))
    }
    @Test fun laterGenerationCannotInheritEarlierProfileOrTrimAuthority() {
        val previous=AudioGenerationObservation(1); previous.profileConfigured(1,setup())
        previous.profileFormat(1,AudioGenerationObservation.Phase.FORMAT,output())
        val next=AudioGenerationObservation(2)
        assertFalse(next.profileConfigured(1,setup()))
        next.producersJoined(2);next.callbackBarrier(2,true);next.handoffIdle(2)
        val s=checkNotNull(next.freeze(2));assertEquals(AudioAacProfile.Eligibility.UNKNOWN,checkNotNull(s.selectedProfile).eligibility)
        assertEquals(AudioGenerationObservation.State.UNSEEN,s.formatLeading.state)
        assertEquals(AudioGenerationObservation.State.UNSEEN,s.eosPadding.state)
    }
    @Test fun terminalUnknownIsVisibleWithoutInventingNewValuesOrTrimAuthority() {
        val owner=Any();val o=AudioAacProfile.Observation(owner,1)
        o.configured(owner,1,setup());o.format(owner,1,AudioAacProfile.Phase.OUTPUT_FORMAT,output())
        val unknown=output().copy(csd0=p.missing(AudioAacProfile.State.ABSENT), rate=p.missing(AudioAacProfile.State.READ_FAILED))
        assertTrue(o.format(owner,1,AudioAacProfile.Phase.OUTPUT_EOS,unknown))
        val s=checkNotNull(o.freeze(owner,1))
        assertEquals(AudioAacProfile.State.READ_FAILED,checkNotNull(s.terminalOutput).rate.state)
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_CAPABILITY,s.eligibility)
        assertFalse(s.nativeSourceQualified);assertFalse(s.sdkReceiptIssued)
    }

    /** The same configure -> callback -> existing real freeze route as capture. */
    private fun integratedProfile(facts: AudioAacProfile.Setup): AudioAacProfile.Snapshot {
        val observation=AudioGenerationObservation(11)
        assertTrue(observation.profileConfigured(11,facts))
        assertTrue(observation.profileFormat(11,AudioGenerationObservation.Phase.FORMAT,output()))
        observation.producersJoined(11);observation.callbackBarrier(11,true);observation.handoffIdle(11)
        return checkNotNull(checkNotNull(observation.freeze(11)).selectedProfile)
    }
    @Test fun configuredBatchFactsCannotBeErasedByLaterCallbackAbsence() {
        for (multiple in listOf(false,true)) for (value in listOf(0,1,4096,8192)) {
            val facts=setup().copy(selection=selection(35,multiple))
            for (changed in listOf(
                facts.copy(output=facts.output.copy(batchMaximum=p.known(value))),
                facts.copy(output=facts.output.copy(batchThreshold=p.known(value))),
                facts.copy(input=facts.input.copy(batchMaximum=p.known(value))),
                facts.copy(input=facts.input.copy(batchThreshold=p.known(value))))) {
                val actual=integratedProfile(changed)
                assertEquals(changed,actual.setup)
                assertEquals(AudioAacProfile.State.ABSENT,checkNotNull(actual.output).batchMaximum.state)
                assertEquals(AudioAacProfile.Eligibility.UNKNOWN,actual.eligibility)
            }
        }
    }
    @Test fun configuredKnownGeometryContradictionsSurviveValidCallback() {
        val facts=setup()
        for (changed in listOf(
            facts.copy(output=facts.output.copy(rate=p.known(44100))),
            facts.copy(output=facts.output.copy(channels=p.known(2))),
            facts.copy(output=facts.output.copy(mime=p.known("audio/raw"))),
            facts.copy(output=facts.output.copy(csd0=p.known("1190"))),
            facts.copy(input=facts.input.copy(rate=p.known(44100))),
            facts.copy(input=facts.input.copy(channels=p.known(2))),
            facts.copy(output=facts.output.copy(csd0=p.missing(AudioAacProfile.State.WRONG_TYPE))))) {
            val actual=integratedProfile(changed)
            assertEquals(changed,actual.setup)
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN,actual.eligibility)
        }
    }
    @Test fun missingInitialCsdAndRawInputMimeDoNotInventAnOutputContradiction() {
        val facts=setup().copy(input=output().copy(mime=p.known("audio/raw"),csd0=p.missing(AudioAacProfile.State.ABSENT)),
            output=output().copy(csd0=p.missing(AudioAacProfile.State.ABSENT)))
        val actual=integratedProfile(facts)
        assertEquals(AudioAacProfile.State.ABSENT,checkNotNull(actual.setup).output.csd0.state)
        assertEquals("1188",checkNotNull(actual.output).csd0.value)
        assertEquals(AudioAacProfile.Eligibility.SINGLE_AU_CAPABILITY,actual.eligibility)
        assertFalse(actual.nativeSourceQualified);assertFalse(actual.sdkReceiptIssued);assertFalse(actual.appAdmission)
        assertEquals(AudioAacProfile.Eligibility.UNKNOWN,p.evaluateSetup(facts,null))
    }
    @Test fun unreadableConfiguredBatchingCannotBecomeAnUnsetDefault() {
        val facts=setup().copy(selection=selection(35,true))
        for (state in listOf(AudioAacProfile.State.READ_FAILED,AudioAacProfile.State.WRONG_TYPE,AudioAacProfile.State.API_UNAVAILABLE)) {
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN,integratedProfile(facts.copy(output=facts.output.copy(batchMaximum=p.missing(state)))).eligibility)
            assertEquals(AudioAacProfile.Eligibility.UNKNOWN,integratedProfile(facts.copy(input=facts.input.copy(batchThreshold=p.missing(state)))).eligibility)
        }
    }

}
