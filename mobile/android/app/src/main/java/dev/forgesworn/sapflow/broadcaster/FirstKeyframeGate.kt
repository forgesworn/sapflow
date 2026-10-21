package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean

/** Metadata crosses FFI only until a first video IDR is actually committed. */
internal class FirstKeyframeGate {
    private val waiting = AtomicBoolean(true)
    fun admit(isVideo: () -> Boolean, isKeyframe: () -> Boolean): Boolean = !waiting.get() || (isVideo() && isKeyframe())
    fun committed() { waiting.set(false) }
}
