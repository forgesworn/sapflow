package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.*
import org.junit.Test

class FirstKeyframeGateTest {
    @Test fun audioAndDependentVideoCannotOpenTheAttempt() {
        val gate = FirstKeyframeGate()
        assertFalse(gate.admit({ false }, { error("Audio keyframe metadata must not be queried") }))
        assertFalse(gate.admit({ true }, { false }))
        assertTrue(gate.admit({ true }, { true }))
    }
    @Test fun failedOrUncommittedSendKeepsTheIdrGate() {
        val gate = FirstKeyframeGate()
        assertTrue(gate.admit({ true }, { true }))
        assertFalse(gate.admit({ true }, { false }))
    }
    @Test fun committedIdrRemovesAllSteadyMetadataCrossings() {
        val gate = FirstKeyframeGate()
        gate.committed()
        repeat(1000) { assertTrue(gate.admit({ error("No steady video getter") }, { error("No steady keyframe getter") })) }
    }
}
