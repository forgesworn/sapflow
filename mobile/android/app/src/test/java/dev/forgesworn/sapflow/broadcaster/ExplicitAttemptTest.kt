package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class ExplicitAttemptTest {
    @Test fun preparedDestinationDoesNotConnectAndExplicitStartRunsOnlyOnce() {
        var connections = 0
        var cancellations = 0
        val attempt = ExplicitAttempt({ connections++ }, { cancellations++ })
        assertEquals(0, connections)
        attempt.start()
        assertEquals(1, connections)
        assertThrows(IllegalStateException::class.java) { attempt.start() }
        assertEquals(1, connections)
        attempt.close(); attempt.close()
        assertEquals(1, cancellations)
    }

    @Test fun cancellingPreparedDestinationPreventsAnyLaterConnection() {
        var connections = 0
        var cancellations = 0
        val attempt = ExplicitAttempt({ connections++ }, { cancellations++ })
        attempt.close()
        assertThrows(IllegalStateException::class.java) { attempt.start() }
        assertEquals(0, connections)
        assertEquals(1, cancellations)
    }

    @Test fun failedStartupCancelsPartialResourcesAndCannotRestartImplicitly() {
        var resourceOpen = false
        var attempts = 0
        val attempt = ExplicitAttempt({ attempts++; resourceOpen = true; error("injected connect setup failure") }, { resourceOpen = false })
        assertThrows(IllegalStateException::class.java) { attempt.start() }
        assertFalse(resourceOpen)
        assertThrows(IllegalStateException::class.java) { attempt.start() }
        assertEquals(1, attempts)
    }
}
