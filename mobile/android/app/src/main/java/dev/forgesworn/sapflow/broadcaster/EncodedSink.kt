package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.MediaCodec as RustCodec

/** Called on an owned worker, never a camera/codec callback. The sample is
 * borrowed for this synchronous call; retain/clone only with a bounded contract.
 * close() must interrupt any pending operation and itself return promptly. */
interface EncodedSink : AutoCloseable {
    /** Coalesced atomic status only; never perform IO in this getter. */
    val status: String
    val diagnostics: String get() = ""
    /** Explicit Start action only. Construction/preparation must remain dormant. */
    fun start() {}
    /** Borrowed immutable descriptors, valid until sink close completes. */
    fun configure(video: RustCodec?, audio: RustCodec?) {}
    /** A transport became ready; controller posts a generation-fenced IDR request. */
    fun setKeyframeRequester(request: () -> Unit) {}
    /** Nonblocking logical-destination fence, before capture/epoch teardown. */
    fun stopDestination() {}
    /** Cold wake/resume hook only. Never reads media timestamps or performs IO. */
    fun revalidateAuthority() {}
    fun consume(sample: MediaSample)
    /** Graceful finalisation on the existing sink worker, after accepted media. */
    fun finish() = close()
    override fun close()
}
