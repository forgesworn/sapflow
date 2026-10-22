package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioInputPolicyTest {
    private fun input(id: String, kind: AudioInputOption.Kind = AudioInputOption.Kind.USB, name: String = "Microphone") =
        checkNotNull(AudioInputOption.create(id, name, kind))
    @Test fun descriptorsBoundWorkAndUseIdsNotNames() {
        assertNull(AudioInputOption.create("", "Mic", AudioInputOption.Kind.USB))
        assertNull(AudioInputOption.create("a\n", "Mic", AudioInputOption.Kind.USB))
        assertNull(AudioInputOption.create("a\u202e", "Mic", AudioInputOption.Kind.USB))
        assertNull(AudioInputOption.create("a\uDB40\uDC01", "Mic", AudioInputOption.Kind.USB))
        assertEquals("AB", input("usb", name = "A\u202eB\uDB40\uDC01").name)
        assertNull(CaptureStartAdmission().reserve(audioInputId = "a\u202e"))
        assertNull(AudioInputOption.create("é".repeat(129), "Mic", AudioInputOption.Kind.USB))
        assertNotNull(AudioInputOption.create("é".repeat(128), "Mic", AudioInputOption.Kind.USB))
        assertNull(AudioInputOption.create("usb", "x".repeat(1025), AudioInputOption.Kind.USB))
        assertEquals("AB", input("usb", name = "A\nB").name)
        assertEquals(80, input("usb", name = "x".repeat(100)).name.length)
        assertEquals(32, AudioInputOption.inventory((0..99).map { input(it.toString()) }).size)
        assertEquals(listOf("a", "b"), AudioInputOption.inventory(listOf(input("a"), input("a"), input("b"))).map { it.id })
    }
    @Test fun systemDefaultWaitsForActualCurrentOwner() {
        val policy = checkNotNull(AudioInputPolicy.create(1, available = listOf(input("built-in", AudioInputOption.Kind.BUILT_IN))))
        assertNull(policy.requestedId); assertFalse(policy.permitsMedia(1))
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(1, "built-in"))
        assertEquals("built-in", policy.observedId(1)); assertTrue(policy.permitsMedia(1))
    }
    @Test fun unavailableExplicitAndUnknownObservedInputsFailClosed() {
        assertNull(AudioInputPolicy.create(1, "missing", listOf(input("usb"))))
        assertNull(AudioInputPolicy.create(1, "", emptyList()))
        val policy = checkNotNull(AudioInputPolicy.create(1, available = emptyList()))
        assertEquals(AudioInputPolicy.Observation.FAILED, policy.observe(1, "unknown"))
        assertFalse(policy.permitsMedia(1))
    }
    @Test fun matchingDisplayNameCannotReplaceActualId() {
        val policy = checkNotNull(AudioInputPolicy.create(1, "usb", listOf(input("usb"), input("built-in"))))
        assertEquals(AudioInputPolicy.Observation.FAILED, policy.observe(1, "built-in"))
        assertEquals(AudioInputPolicy.Observation.IGNORED, policy.observe(1, "usb"))
        assertFalse(policy.permitsMedia(1)); assertNull(policy.observedId(1))
    }
    @Test fun disconnectFailsOnceWithoutFallback() {
        val policy = checkNotNull(AudioInputPolicy.create(1, "usb", listOf(input("usb"))))
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(1, "usb"))
        assertEquals(AudioInputPolicy.Observation.FAILED, policy.observe(1, null))
        assertEquals(AudioInputPolicy.Observation.IGNORED, policy.observe(1, null))
        assertFalse(policy.permitsMedia(1))
    }
    @Test fun systemDefaultCannotAdoptAnotherAvailableRoute() {
        val policy = checkNotNull(AudioInputPolicy.create(1, available = listOf(input("wired", AudioInputOption.Kind.WIRED), input("usb"))))
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(1, "wired"))
        assertEquals(AudioInputPolicy.Observation.FAILED, policy.observe(1, "usb"))
        assertFalse(policy.permitsMedia(1))
    }
    @Test fun staleObservationAndCloseCannotAffectCurrentOwner() {
        val policy = checkNotNull(AudioInputPolicy.create(2, "usb", listOf(input("usb"))))
        assertEquals(AudioInputPolicy.Observation.STALE, policy.observe(1, null)); policy.close(1)
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(2, "usb"))
        assertTrue(policy.permitsMedia(2)); assertFalse(policy.permitsMedia(1)); assertNull(policy.observedId(1))
    }
    @Test fun closedOwnerCannotRevive() {
        val policy = checkNotNull(AudioInputPolicy.create(1, available = listOf(input("usb"))))
        policy.close(1)
        assertEquals(AudioInputPolicy.Observation.IGNORED, policy.observe(1, "usb"))
        assertFalse(policy.permitsMedia(1))
    }
    @Test fun prepareFreezesChoiceAndCleanupFencesSuccessor() {
        val admission = CaptureStartAdmission()
        val a = checkNotNull(admission.reserve(audioInputId = "usb-a"))
        assertEquals("usb-a", a.audioInputId); assertTrue(admission.claim(a)); assertTrue(admission.beginEpoch(a, 1))
        assertSame(a, admission.stop()); admission.beginCleanup(a, false); admission.cleanup(a, false)
        assertNull(admission.reserve(audioInputId = "usb-b"))
        admission.cleanup(a, true)
        val b = checkNotNull(admission.reserve(audioInputId = "usb-b"))
        assertTrue(admission.claim(b)); assertTrue(admission.beginEpoch(b, 2)); assertEquals("usb-b", b.audioInputId)
        data class Owner(val preparation: CaptureStartAdmission.Preparation, val epoch: Long)
        val current = Owner(b, 2)
        val policy = checkNotNull(AudioInputPolicy.create(current, "usb-b", listOf(input("usb-b"))))
        assertEquals(AudioInputPolicy.Observation.STALE, policy.observe(Owner(a, 1), null))
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(current, "usb-b"))
        assertTrue(admission.owns(b))
        assertTrue(admission.prepared(b, 2)); assertNotNull(admission.start(b))
    }
    @Test fun defaultCompatibilityAndOldEpochCannotStopRotation() {
        val admission = CaptureStartAdmission(); val a = checkNotNull(admission.reserve())
        assertNull(a.audioInputId); assertTrue(admission.claim(a)); assertTrue(admission.beginEpoch(a, 1)); assertTrue(admission.prepared(a, 1))
        assertTrue(admission.commit(checkNotNull(admission.start(a))))
        admission.beginCleanup(a, true); admission.cleanup(a, true); assertTrue(admission.beginEpoch(a, 2))
        data class Owner(val preparation: CaptureStartAdmission.Preparation, val epoch: Long)
        val current = Owner(a, 2)
        val policy = checkNotNull(AudioInputPolicy.create(current, available = listOf(input("usb"))))
        val old = Owner(a, 1)
        assertEquals(AudioInputPolicy.Observation.STALE, policy.observe(old, null)); policy.close(old)
        assertEquals(AudioInputPolicy.Observation.ACCEPTED, policy.observe(current, "usb"))
        assertTrue(policy.permitsMedia(current)); assertTrue(admission.prepared(a, 2)); assertNotNull(admission.start(a))
        assertNull(CaptureStartAdmission().reserve(audioInputId = "bad\n"))
    }
}
