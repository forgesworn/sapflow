// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

/** Metadata-only experiment. No payload, native resource or production authority. */
internal class ReturnedPcmClock(
    private val owner: Any,
    private val generation: Long,
    val geometry: Geometry,
    private val format: Format = Format(),
) {
    data class Geometry(val sourceMediaOrigin48k: Long, val movieDelay720k: Long, val expectedFrames: Long) {
        init {
            require(sourceMediaOrigin48k >= 0 && movieDelay720k >= 0 && expectedFrames > 0)
            Math.addExact(sourceMediaOrigin48k, expectedFrames)
            Math.addExact(movieDelay720k, Math.multiplyExact(expectedFrames, 15))
        }
    }
    data class Format(val rate: Int = 48_000, val channels: Int = 1, val bytesPerSample: Int = 2)
    data class Position(
        val prefixFrames: Long, val frames: Long,
        val sourceStart48k: Long, val sourceEnd48k: Long,
        val movieStart720k: Long, val movieEnd720k: Long,
    )
    class Ticket internal constructor(
        val serial: Long, val nativeIndex: Int, val offset: Int, val size: Int,
        val capacity: Int, val rawNativePtsUs: Long, val rawFlags: Int,
        val frames: Long,
    ) {
        var position: Position? = null; internal set
        internal var sinkAttempted = false
        internal var sinkDisposition = 0 // 0 pending,1 accepted,2 known failure,3 unknown
        internal var releaseAttempted = false
        internal var releaseOutcome = 0
    }
    class Snapshot internal constructor(
        val returnedFrames: Long, val sinkAcceptedFrames: Long,
        val returnedRanges: Long, val releaseAttempts: Long, val releasesReturned: Long,
        val inputEosQueueReturned: Boolean, val outputEosObserved: Boolean,
        val ownerCloseAttempted: Boolean, val ownerCloseReturned: Boolean,
        val sinkCloseAttempted: Boolean, val sinkCloseReturned: Boolean,
        val activeSerial: Long?, val knownFault: String?, val unknownOwnership: Boolean,
        val sinkAcceptanceUnknown: Boolean, val nativeOwnershipUnknown: Boolean,
        val scheduleComplete: Boolean,
    ) {
        val contract get() = "DIAGNOSTIC_ONLY_OWNER_APPROVED_PROFILE_PENDING_IMPLEMENTATION"
        val rawNativeClockGatePassed get() = false
        val contentQualified get() = false
        val nativeSourceQualified get() = false
        val presentationEquivalent get() = false
        val appAdmission get() = false
    }
    private var returned = 0L
    private var accepted = 0L
    private var ranges = 0L
    private var attempts = 0L
    private var releases = 0L
    private var lastSerial = 0L
    private var active: Ticket? = null
    private var inputEos = false
    private var outputEos = false
    private var ownerAttempt = false
    private var ownerClosed = false
    private var ownerOutcome = 0
    private var sinkAttempt = false
    private var sinkClosed = false
    private var sinkOutcome = 0
    private val ownerThread = Thread.currentThread()
    private var fault: String? = null
    private var unknown = false
    private var sinkUnknown = false
    private var nativeUnknown = false

    init { require(generation > 0 && format == Format()) }
    private fun identity(value: Any, serial: Long) { require(value === owner && serial == generation && Thread.currentThread() === ownerThread) { "Stale owner/generation" } }
    private fun reject(message: String): Nothing { fault = fault ?: message; throw IllegalStateException(message) }
    private fun ticket(value: Ticket) { require(active === value) { "Foreign, stale or already released ticket" } }
    private fun open() { check(!ownerAttempt && !sinkAttempt && !unknown && fault == null) { "Clock is fenced" } }

    /** Call immediately after the real queueInputBuffer(EOS) returns successfully. */
    fun inputEosQueued(value: Any, serial: Long) {
        identity(value, serial); open()
        if (inputEos) reject("Duplicate input EOS")
        inputEos = true
    }

    /** No buffer is stored here. Native release remains the concrete owner's duty. */
    fun observe(
        value: Any, serial: Long, borrowSerial: Long, nativeIndex: Int,
        offset: Int, size: Int, capacity: Int, rawNativePtsUs: Long, flags: Int,
        actualFormat: Format = format,
    ): Ticket {
        identity(value, serial); open()
        if (active != null) reject("One borrowed range only")
        if (outputEos) reject("Output after EOS")
        if (borrowSerial <= lastSerial) reject("Duplicate/regressed borrow serial")
        if (actualFormat != format) reject("Output format changed")
        if (nativeIndex < 0 || offset < 0 || size < 0 || capacity < 0 ||
            offset > capacity || size > capacity - offset || size % 2 != 0 || offset % 2 != 0 || flags and 4.inv() != 0)
            reject("Invalid PCM range/flags")
        val frames = size / 2L
        val prefix = returned
        // Latch actual returned events before any sink or other external work.
        returned = Math.addExact(returned, frames)
        ranges = Math.addExact(ranges, 1)
        lastSerial = borrowSerial
        val result = Ticket(borrowSerial, nativeIndex, offset, size, capacity, rawNativePtsUs, flags, frames)
        active = result
        if (flags and 4 != 0) {
            outputEos = true
            if (!inputEos) fault = fault ?: "Output EOS without returned input EOS"
        }
        if (returned > geometry.expectedFrames) fault = fault ?: "Returned frames exceed declared N"
        try {
            val sourceStart = Math.addExact(geometry.sourceMediaOrigin48k, prefix)
            val movieStart = Math.addExact(geometry.movieDelay720k, Math.multiplyExact(prefix, 15))
            result.position = Position(prefix, frames, sourceStart, Math.addExact(sourceStart, frames),
                movieStart, Math.addExact(movieStart, Math.multiplyExact(frames, 15)))
        } catch (_: ArithmeticException) { fault = fault ?: "Clock coordinate overflow" }
        return result
    }

    /** One-use permission BEFORE external sink I/O; no external call is made here. */
    fun sinkAttempt(value: Any, serial: Long, range: Ticket) {
        identity(value, serial); ticket(range)
        check(!ownerAttempt && !nativeUnknown && !unknown && fault == null && !sinkAttempt) { "Sink I/O fenced" }
        if (range.releaseAttempted) reject("Sink I/O after release attempt")
        if (range.sinkAttempted) reject("Duplicate sink attempt")
        range.sinkAttempted = true
    }
    /** Report a real successful return from the already-authorised whole-range call. */
    fun sinkAccepted(value: Any, serial: Long, range: Ticket) {
        identity(value, serial); ticket(range)
        check(range.sinkAttempted && range.sinkDisposition == 0) { "Sink return without pending attempt" }
        range.sinkDisposition = 1
        accepted = Math.addExact(accepted, range.frames)
    }
    fun sinkFailed(value: Any, serial: Long, range: Ticket, uncertain: Boolean) {
        identity(value, serial); ticket(range)
        check(range.sinkAttempted && range.sinkDisposition == 0) { "Sink failure without pending attempt" }
        range.sinkDisposition = if (uncertain) 3 else 2
        fault = fault ?: if (uncertain) "Sink acceptance unknown" else "Sink failed"
        sinkUnknown = sinkUnknown || uncertain
        unknown = unknown || uncertain
    }
    fun releaseAttempt(value: Any, serial: Long, range: Ticket) {
        identity(value, serial); ticket(range)
        check(!ownerAttempt && !nativeUnknown) { "Native release I/O fenced by owner close/uncertainty" }
        if (range.sinkAttempted && range.sinkDisposition == 0) reject("Native release during sink call")
        check(!range.releaseAttempted) { "Release retry refused" }
        range.releaseAttempted = true
        attempts = Math.addExact(attempts, 1)
    }
    fun releaseReturned(value: Any, serial: Long, range: Ticket) {
        identity(value, serial); ticket(range)
        check(range.releaseAttempted && range.releaseOutcome == 0) { "Release return order/outcome" }
        range.releaseOutcome = 1
        if (range.sinkDisposition == 0) fault = fault ?: "Released before sink disposition"
        releases = Math.addExact(releases, 1)
        active = null
    }
    fun releaseFailed(value: Any, serial: Long, range: Ticket, uncertain: Boolean) {
        identity(value, serial); ticket(range)
        check(range.releaseAttempted && range.releaseOutcome == 0) { "Release failure order/outcome" }
        range.releaseOutcome = if (uncertain) 3 else 2
        fault = fault ?: if (uncertain) "Release ownership unknown" else "Release failed"
        nativeUnknown = true
        unknown = true // A failed release does not establish ownership closure.
        // Retain the one ticket strongly; there is no release retry or next borrow.
    }
    fun knownLoss(value: Any, serial: Long) { identity(value, serial); fault = fault ?: "Known loss" }
    fun ownerCloseAttempt(value: Any, serial: Long) {
        identity(value, serial); check(!ownerAttempt && !nativeUnknown) { "Owner close I/O fenced/retry refused" }
        val range = active
        if (range != null && (range.sinkAttempted && range.sinkDisposition == 0 ||
                    range.releaseAttempted && range.releaseOutcome == 0)) reject("Owner close during external call")
        ownerAttempt = true
    }
    fun ownerCloseReturned(value: Any, serial: Long) {
        identity(value, serial); check(ownerAttempt && ownerOutcome == 0) { "Owner close order" }; ownerOutcome = 1; ownerClosed = true
        if (active != null) fault = fault ?: "Owner closed with outstanding range"
    }
    fun ownerCloseUnknown(value: Any, serial: Long) {
        identity(value, serial); check(ownerAttempt && ownerOutcome == 0); ownerOutcome = 3; nativeUnknown = true; unknown = true; fault = fault ?: "Owner close unknown"
    }
    fun sinkCloseAttempt(value: Any, serial: Long) {
        identity(value, serial); check(!sinkAttempt) { "Sink close retry refused" }
        val range = active
        if (range != null && range.sinkAttempted && range.sinkDisposition == 0) reject("Sink close during sink call")
        sinkAttempt = true
    }
    fun sinkCloseReturned(value: Any, serial: Long) {
        identity(value, serial); check(sinkAttempt && sinkOutcome == 0) { "Sink close order" }; sinkOutcome = 1; sinkClosed = true
    }
    fun sinkCloseUnknown(value: Any, serial: Long) {
        identity(value, serial); check(sinkAttempt && sinkOutcome == 0); sinkOutcome = 3; sinkUnknown = true; unknown = true; fault = fault ?: "Sink close unknown"
    }
    fun snapshot(): Snapshot {
        check(Thread.currentThread() === ownerThread) { "Owner thread only" }
        val complete = fault == null && !unknown && active == null && inputEos && outputEos &&
            returned == geometry.expectedFrames && accepted == returned && attempts == releases && ownerClosed && sinkClosed
        return Snapshot(returned, accepted, ranges, attempts, releases, inputEos, outputEos,
            ownerAttempt, ownerClosed, sinkAttempt, sinkClosed, active?.serial, fault, unknown, sinkUnknown, nativeUnknown, complete)
    }
}
