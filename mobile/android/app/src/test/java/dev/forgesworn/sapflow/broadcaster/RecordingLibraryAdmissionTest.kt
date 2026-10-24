package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class RecordingLibraryAdmissionTest {
    @Test fun stalePickerCannotSelectOrCancelAnotherExport() {
        val gate = RecordingLibraryAdmission()
        val old = gate.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true)!!
        gate.cancel(old)
        val next = gate.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true)!!
        assertFalse(gate.picked(old)); gate.cancel(old)
        assertTrue(gate.picked(next)); assertTrue(gate.begin(next))
        assertTrue(gate.finish(next, true)); assertTrue(gate.canDeliver(next))
    }
    @Test fun cancelledQueuedWorkRetainsCapacityUntilWorkerAcknowledgesNoOwners() {
        val gate = RecordingLibraryAdmission()
        val request = gate.reserve(RecordingLibraryAdmission.Kind.PREFLIGHT)!!
        gate.cancel(request)
        assertNull(gate.reserve(RecordingLibraryAdmission.Kind.SCAN))
        assertFalse(gate.begin(request)); assertTrue(gate.shouldCancel(request))
        assertFalse(gate.finish(request, true)); assertFalse(gate.canDeliver(request))
        assertNotNull(gate.reserve(RecordingLibraryAdmission.Kind.PREFLIGHT))
    }
    @Test fun runningCopyCannotReleaseItsOwnCapacityByCancellation() {
        val gate = RecordingLibraryAdmission()
        val export = gate.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true)!!
        assertFalse(gate.begin(export)); assertFalse(gate.finish(export, true))
        assertTrue(gate.picked(export)); assertTrue(gate.begin(export))
        gate.cancel(export)
        assertNull(gate.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true))
        assertFalse(gate.finish(export, true)); assertTrue(gate.idle)
    }
    @Test fun failedCloseRetainsResourcesAndStaleCompletionCannotReleaseSuccessor() {
        val gate = RecordingLibraryAdmission()
        val old = gate.reserve(RecordingLibraryAdmission.Kind.SCAN)!!
        assertTrue(gate.begin(old)); assertFalse(gate.finish(old, false))
        assertTrue(gate.cleanupRetained); assertFalse(gate.idle)
        assertNull(gate.reserve(RecordingLibraryAdmission.Kind.PREFLIGHT))
        // A later actual owner receipt, rather than cancellation, releases it.
        assertTrue(gate.finish(old, true))
        val next = gate.reserve(RecordingLibraryAdmission.Kind.PREFLIGHT)!!
        assertFalse(gate.finish(old, true)); assertFalse(gate.canDeliver(old))
        assertTrue(gate.begin(next)); assertTrue(gate.finish(next, true))
    }
    @Test fun newRequestInvalidatesAlreadyPostedOldResult() {
        val gate = RecordingLibraryAdmission()
        val old = gate.reserve(RecordingLibraryAdmission.Kind.SCAN)!!
        assertTrue(gate.begin(old)); assertTrue(gate.finish(old, true))
        assertTrue(gate.canDeliver(old))
        val next = gate.reserve(RecordingLibraryAdmission.Kind.PREFLIGHT)!!
        assertFalse(gate.canDeliver(old))
        assertTrue(gate.begin(next)); assertTrue(gate.finish(next, true))
        assertTrue(gate.canDeliver(next))
    }
    @Test fun closedScreenCannotRestartOrDeliverAQueuedExport() {
        val gate = RecordingLibraryAdmission()
        val job = gate.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true)!!
        assertTrue(gate.picked(job)); gate.close()
        assertFalse(gate.begin(job)); assertTrue(gate.shouldCancel(job))
        assertFalse(gate.finish(job, true)); assertFalse(gate.canDeliver(job))
        assertNull(gate.reserve(RecordingLibraryAdmission.Kind.SCAN))
    }
}
