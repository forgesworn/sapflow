package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicInteger

class TerminalAdmissionSignalTest {
    @Test fun terminalBeforeListenerDeliversOnceAndDuplicateListenersCannotReplaceOwner(){
        val signal=TerminalAdmissionSignal();signal.closeAdmission()
        var first=0;var second=0
        signal.listen{first++};signal.listen{second++};signal.closeAdmission()
        assertEquals(1,first);assertEquals(0,second)
    }
    @Test fun concurrentCloseAndRegistrationHaveOneReceiptAndNoCallbackUnderScalarLock(){
        repeat(100){
            val signal=TerminalAdmissionSignal();val go=CountDownLatch(1);val calls=AtomicInteger()
            val a=Thread{go.await();signal.listen{calls.incrementAndGet();signal.closeAdmission()}}
            val b=Thread{go.await();signal.closeAdmission()}
            a.start();b.start();go.countDown();a.join(1000);b.join(1000)
            assertFalse(a.isAlive);assertFalse(b.isAlive);assertEquals(1,calls.get())
        }
    }
}
