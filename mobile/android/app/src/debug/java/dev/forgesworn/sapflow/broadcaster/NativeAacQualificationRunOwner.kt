// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Generated-fixture lifetime only. No Activity, view, context or media facts.
 * The exact probe remains retained when its returned cleanup is unknown. */
internal class NativeAacQualificationRunOwner private constructor(
    private val slot: Slot, val invocation: Invocation,
) {
    data class Invocation(val action: String, val runId: String, val inputId: String?, val inputManifestSHA256: String?)
    val cancelled = AtomicBoolean(false)
    private val done = AtomicBoolean(false)
    private val cleaned = AtomicBoolean(false)
    val finished: Boolean get() = done.get()
    val cleanupConfirmed: Boolean get() = cleaned.get()
    val status = AtomicReference("Running generated native AAC observations")
    private val workerReference = AtomicReference<Thread?>()
    val worker: Thread? get() = workerReference.get()
    private val started = AtomicBoolean(false)
    private var retainedProbe: Any? = null

    /** Actual callback handoff and framework configuration state are both
     * required. Merely seeing a configuration change never grants continuity. */
    fun destroyed(changingConfigurations: Boolean, handedOff: Boolean) {
        if (!changingConfigurations || !handedOff) cancel()
    }
    fun cancel() {
        if (cancelled.compareAndSet(false, true)) workerReference.get()?.interrupt()
    }
    private fun start(probe: Any, work: () -> Boolean) {
        check(started.compareAndSet(false, true)) { "Qualification worker was already started" }
        retainedProbe = probe
        val thread = Thread({
            var confirmed = false
            try { if (!cancelled.get()) confirmed = work() }
            catch (error: Throwable) { status.set("Qualification worker failed: " + error.javaClass.simpleName) }
            finally {
                cleaned.set(confirmed)
                if (confirmed) retainedProbe = null
                done.set(true)
                slot.retire(this)
            }
        }, "sapflow-aac-qualification")
        check(workerReference.compareAndSet(null, thread))
        thread.start()
    }

    /** One process-wide slot in the Activity, separate fresh slots in host
     * tests. No numeric/thread retry and no new run while cleanup is unknown. */
    class Slot {
        private val active = AtomicReference<NativeAacQualificationRunOwner?>()
        fun hasOwner() = active.get() != null
        fun acquire(invocation: Invocation): NativeAacQualificationRunOwner? {
            val next = NativeAacQualificationRunOwner(this, invocation)
            return if (active.compareAndSet(null, next)) next else null
        }
        fun reattach(retained: NativeAacQualificationRunOwner, invocation: Invocation): NativeAacQualificationRunOwner? {
            if (retained.slot !== this) return null
            if (retained.invocation != invocation || retained.cancelled.get()) {
                cancelRetained(retained)
                return null
            }
            return retained.takeIf { active.get() === it || it.finished }
        }
        fun cancelRetained(retained: NativeAacQualificationRunOwner) {
            if (retained.slot === this && active.get() === retained) retained.cancel()
        }
        fun start(owner: NativeAacQualificationRunOwner, probe: Any, work: () -> Boolean) {
            check(owner.slot === this && active.get() === owner)
            owner.start(probe, work)
        }
        internal fun retire(owner: NativeAacQualificationRunOwner) {
            if (owner.slot === this && owner.finished && owner.cleanupConfirmed) active.compareAndSet(owner, null)
        }
    }
}
