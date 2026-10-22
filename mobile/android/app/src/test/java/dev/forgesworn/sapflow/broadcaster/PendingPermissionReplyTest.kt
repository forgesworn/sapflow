package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class PendingPermissionReplyTest {
    @Test fun lateReplyCannotBeAppliedToANewerPrepare() {
        val route = PendingPermissionReply<Any>()
        val cancelled = Any()
        val successor = Any()
        assertTrue(route.reserve(cancelled))
        // Cancelling capture does not label or retract the system reply.
        assertFalse(route.reserve(successor))
        assertSame(cancelled, route.receive())
        assertFalse(route.outstanding)
        assertTrue(route.reserve(successor))
        assertSame(successor, route.receive())
    }

    @Test fun duplicateOrRecreatedReplyHasNoSelection() {
        val route = PendingPermissionReply<Any>()
        val intent = Any()
        assertNull(route.receive())
        assertTrue(route.reserve(intent))
        assertSame(intent, route.receive())
        assertNull(route.receive())
        assertNull(PendingPermissionReply<Any>().receive())
    }
}
