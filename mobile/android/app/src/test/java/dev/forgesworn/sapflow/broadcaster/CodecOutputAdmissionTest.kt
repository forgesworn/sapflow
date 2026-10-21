package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class CodecOutputAdmissionTest {
    @Test fun closeCannotOvertakeAcceptedBuffersOrAdmitLateCallbacks() {
        val admission = CodecOutputAdmission(2)
        assertEquals(CodecOutputAdmission.Result.ACCEPTED, admission.acquire())
        assertEquals(CodecOutputAdmission.Result.ACCEPTED, admission.acquire())
        assertEquals(CodecOutputAdmission.Result.SATURATED, admission.acquire())
        assertFalse(admission.close())
        assertEquals(CodecOutputAdmission.Result.CLOSED, admission.acquire())
        admission.release()
        assertFalse(admission.close())
        admission.release()
        assertTrue(admission.close())
    }

    @Test fun closeRacingProducerRetainsEveryAdmittedOwner() {
        repeat(100) {
            val admission = CodecOutputAdmission()
            val admitted = java.util.concurrent.atomic.AtomicBoolean()
            val producer = Thread { admitted.set(admission.acquire() == CodecOutputAdmission.Result.ACCEPTED) }
            producer.start()
            val idle = admission.close()
            producer.join()
            if (admitted.get()) { assertFalse(idle); admission.release() }
            assertTrue(admission.close())
        }
    }
}
