package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class AudioBroadcastLifecycleTest {
    private class Show(val id: Long) {
        val capability = id.toString(16).padStart(32, '0')
        var retained = true
        var active = true
        var mayContinue = true
        var admissionAllowed = true
    }
    private class Owners : AudioBroadcastLifecycle.Owners<Show> {
        var current: Show? = null
        var permissions = true
        var promotions = 0
        var revalidations = 0
        var stopCalls = 0
        var stopDispatches = 0
        var retirements = 0
        var interruptions = 0
        override fun match(id: Long, capability: String?) = current?.takeIf { it.id == id && it.capability == capability }
        override fun owns(value: Show) = current === value && value.retained
        override fun revalidateAuthority() { revalidations++ }
        override fun promoted(value: Show): Boolean {
            promotions++
            return owns(value) && value.active && value.admissionAllowed && permissions
        }
        override fun check(value: Show): Boolean {
            revalidateAuthority()
            if (!owns(value)) return false
            if (!permissions || !value.mayContinue) stop(value, "No longer eligible")
            return owns(value)
        }
        override fun stop(value: Show, reason: String) {
            check(reason.isNotEmpty())
            stopCalls++
            if (owns(value) && value.active) { value.active = false; stopDispatches++ }
        }
        override fun retired(value: Show) {
            retirements++
            if (current === value) current = null
        }
        override fun serviceDestroyed(value: Show) {
            if (owns(value) && value.active) { interruptions++; value.active = false }
            if (current === value) current = null
        }
    }
    private class Platform : AudioBroadcastLifecycle.Platform<Show> {
        var notification: Show? = null
        var promotionCalls = 0
        var rejectPromotion = false
        var removals = 0
        val stops = mutableListOf<Int>()
        var pendingChecks = 0
        override fun promote(value: Show) {
            promotionCalls++
            if (rejectPromotion) throw SecurityException("Generated permission failure")
            notification = value
        }
        override fun removeNotification() { removals++; notification = null }
        override fun stopService(startId: Int) { stops += startId }
        override fun scheduleCheck(delayMs: Long) { check(delayMs == 1_000L); pendingChecks = 1 }
        override fun cancelCheck() { pendingChecks = 0 }
    }
    private class Fixture {
        val owners = Owners()
        val platform = Platform()
        val driver = AudioBroadcastLifecycle(owners, platform)
        fun show(id: Long = 1) = Show(id).also { owners.current = it }
        fun arm(value: Show, startId: Int = 1) = driver.command(AudioBroadcastLifecycle.Command.ARM, value.id, value.capability, startId)
        fun stop(value: Show, startId: Int = 2) = driver.command(AudioBroadcastLifecycle.Command.STOP, value.id, value.capability, startId)
    }

    @Test fun duplicateArmDuringStoppingRetainsNotificationAndCleanupOwner() {
        val f = Fixture(); val a = f.show(); f.arm(a); f.stop(a)
        repeat(1_000) { f.arm(a, 3 + it) }
        assertEquals(1, f.platform.promotionCalls)
        assertEquals(1, f.owners.promotions)
        assertEquals(1, f.owners.stopDispatches)
        assertEquals(0, f.owners.retirements)
        assertSame(a, f.platform.notification)
        assertEquals(1, f.platform.pendingChecks)
        assertTrue(f.platform.stops.isEmpty())
        a.retained = false; f.driver.check()
        assertEquals(1, f.owners.retirements)
        assertNull(f.platform.notification)
        assertEquals(listOf(1_002), f.platform.stops)
    }

    @Test fun duplicateStopAndTicksDoNotQueueRepeatedStopWorkDuringBlockedCleanup() {
        val f = Fixture(); val a = f.show(); f.arm(a)
        repeat(1_000) { f.stop(a); f.driver.check() }
        assertEquals(1, f.owners.stopCalls)
        assertEquals(1, f.owners.stopDispatches)
        assertSame(a, f.platform.notification)
        assertEquals(1, f.platform.pendingChecks)
        a.retained = false; f.driver.check(); f.driver.check()
        assertEquals(1, f.platform.removals)
        assertEquals(0, f.platform.pendingChecks)
    }

    @Test fun failedAdmissionAfterSuccessfulPromotionRetainsNotificationUntilActualClose() {
        val f = Fixture(); val a = f.show(); a.admissionAllowed = false
        f.arm(a)
        assertFalse(a.active)
        assertSame(a, f.platform.notification)
        assertEquals(0, f.owners.retirements)
        repeat(10) { f.driver.check() }
        assertTrue(f.platform.stops.isEmpty())
        a.retained = false; f.driver.check()
        assertNull(f.platform.notification)
        assertEquals(1, f.owners.retirements)
    }

    @Test fun promotionExceptionStopsServicePromptlyWithoutRetiringApplicationCleanup() {
        val f = Fixture(); val a = f.show(); f.platform.rejectPromotion = true
        f.arm(a)
        assertFalse(a.active)
        assertEquals(0, f.owners.promotions)
        assertEquals(0, f.owners.retirements)
        assertEquals(0, f.platform.pendingChecks)
        assertEquals(listOf(1), f.platform.stops) // Do not wait for native cleanup or a timer.
        f.driver.check(); f.driver.taskRemoved()
        assertEquals(0, f.platform.pendingChecks)
        f.arm(a) // Cannot retry promotion or resurrect this stopped preparation.
        assertEquals(1, f.platform.promotionCalls)
        assertEquals(1, f.owners.stopDispatches)
        f.driver.destroy(); f.driver.check()
        assertEquals(0, f.owners.retirements)
        assertEquals(0, f.owners.interruptions)
        assertSame(a, f.owners.current)
        assertTrue(a.retained) // The application retains actual cleanup independently.
        assertEquals(0, f.platform.removals)
    }

    @Test fun stopBeforeArmNeverPromotesOrResurrectsPendingStart() {
        val f = Fixture(); val a = f.show()
        f.stop(a); f.arm(a)
        assertFalse(a.active)
        assertEquals(0, f.platform.promotionCalls)
        assertEquals(0, f.owners.retirements)
        assertEquals(1, f.owners.stopDispatches)
        assertEquals(1, f.platform.pendingChecks)
    }

    @Test fun staleAndMalformedCommandsCannotStopOrReplaceSuccessor() {
        val f = Fixture(); val a = f.show(); f.arm(a); f.stop(a)
        a.retained = false; f.driver.check()
        val b = f.show(2); f.arm(b, 3)
        val stops = f.platform.stops.toList()
        f.stop(a, 4); f.arm(a, 5)
        f.driver.command(AudioBroadcastLifecycle.Command.STOP, b.id, a.capability, 6)
        f.driver.command(null, b.id, b.capability, 7)
        assertTrue(b.active)
        assertSame(b, f.platform.notification)
        assertEquals(stops, f.platform.stops)
        assertEquals(2, f.platform.promotionCalls)
    }

    @Test fun successorBeforeOldCheckReleasesOnlyClosedLeaseWithoutStoppingService() {
        val f = Fixture(); val a = f.show(); f.arm(a)
        a.retained = false // Actual close, but the old timer has not run.
        val b = f.show(2); f.arm(b, 2)
        assertSame(b, f.platform.notification)
        assertTrue(b.active)
        assertEquals(1, f.owners.retirements)
        assertTrue(f.platform.stops.isEmpty())
        f.driver.check() // Even an old queued check only inspects the current lease.
        assertSame(b, f.platform.notification)
        assertEquals(1, f.platform.pendingChecks)
    }

    @Test fun taskRemovalKeepsEligibleCommittedShowAndRevalidatesAuthority() {
        val f = Fixture(); val a = f.show(); f.arm(a)
        val before = f.owners.revalidations
        f.driver.taskRemoved()
        assertTrue(f.owners.revalidations > before)
        assertTrue(a.active)
        assertSame(a, f.platform.notification)
        assertTrue(f.platform.stops.isEmpty())
    }

    @Test fun taskRemovalOfIneligibleShowStopsOnceButRetainsItsActualCleanup() {
        val f = Fixture(); val a = f.show(); f.arm(a); a.mayContinue = false
        repeat(100) { f.driver.taskRemoved() }
        assertEquals(1, f.owners.stopDispatches)
        assertSame(a, f.platform.notification)
        assertEquals(0, f.owners.retirements)
        a.retained = false; f.driver.check()
        assertNull(f.platform.notification)
    }

    @Test fun permissionRevocationOnCheckRetainsNotificationDuringActualStop() {
        val f = Fixture(); val a = f.show(); f.arm(a); f.owners.permissions = false
        f.driver.check()
        assertFalse(a.active)
        assertSame(a, f.platform.notification)
        assertEquals(1, f.owners.stopDispatches)
        assertEquals(0, f.owners.retirements)
    }

    @Test fun unexpectedDestroyInterruptsOnceAndLateCallbacksCannotResurrect() {
        val f = Fixture(); val a = f.show(); f.arm(a)
        f.driver.destroy(); f.driver.destroy()
        f.driver.check(); f.driver.taskRemoved(); f.arm(a, 2); f.stop(a, 3)
        assertEquals(1, f.owners.interruptions)
        assertTrue(a.retained) // Destruction does not prove native cleanup.
        assertEquals(0, f.platform.pendingChecks)
        assertEquals(1, f.platform.promotionCalls)
        assertEquals(0, f.owners.retirements)
    }

    @Test fun destructionDuringNormalStopDoesNotReclassifyItAsUnexpectedFailure() {
        val f = Fixture(); val a = f.show(); f.arm(a); f.stop(a)
        f.driver.destroy()
        assertEquals(0, f.owners.interruptions)
        assertTrue(a.retained)
        assertEquals(1, f.owners.stopDispatches)
    }

    @Test fun emptyProcessAndNullCommandNeverCreateOrPromoteAnOwner() {
        val f = Fixture()
        f.driver.command(null, -1, null, 1)
        f.driver.command(AudioBroadcastLifecycle.Command.ARM, 1, "01".repeat(16), 2)
        f.driver.taskRemoved(); f.driver.destroy()
        assertNull(f.owners.current)
        assertEquals(0, f.platform.promotionCalls)
        assertEquals(0, f.owners.stopDispatches)
        assertEquals(listOf(1, 2), f.platform.stops)
        assertEquals(0, f.platform.pendingChecks)
    }
}
