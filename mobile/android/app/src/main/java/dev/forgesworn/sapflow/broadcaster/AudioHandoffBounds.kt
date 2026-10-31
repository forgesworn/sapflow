package dev.forgesworn.sapflow.broadcaster

/** Bounds for handing encoded AAC from the audio callback thread to the codec
 * handler. The codec handler also starts the camera, renderer and encoders. In
 * a debuggable build that holds it for about 300 ms just after Start (Pixel 10
 * Pro XL, 31 October 2026: 263 to 343 ms), and at 250 ms and 16 slots every
 * capture ended. The non-debuggable build on the same phone peaked at 10 ms,
 * so this is debug-build cost, not a release fault. One second of AAC (48 kHz,
 * 1024-sample frames: 47 units) matches SRT's own one-second sample residence,
 * so debug builds stay usable while a handler that is genuinely stuck still
 * fails within a second. */
internal object AudioHandoffBounds {
    const val SLOTS = 48
    /** Mono AAC units are at most 4096 bytes each. */
    const val BYTES = SLOTS * 4096L
    const val MAX_AGE_NS = 1_000_000_000L
}
