package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.os.Handler
import android.os.Looper
import java.io.OutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/** One application-owned cold IO slot, independent of capture and transport.
 * Idle threads expire. Recreation cannot overlap an uncertain earlier export. */
internal class RecordingLibraryOwner internal constructor(
    private val directory: Path,
    private val execute: (Runnable) -> Unit,
    private val deliver: (() -> Unit) -> Unit,
) {
    private enum class PreparationProblem { LOW_STORAGE, UNAVAILABLE }
    private class PreparationFailure(val reason: PreparationProblem) : IllegalStateException()
    data class Preparation(
        val profile: CaptureProfile,
        val checkedAtNs: Long,
        private val directory: Path,
        private val catalogue: RecordingCatalogue,
        private val show: RecordingCatalogue.Show,
    ) {
        fun recorder(): EncodedSink = PrivateRecordingSink(directory, catalogue, show)
        fun fresh(nowNs: Long): Boolean = nowNs >= checkedAtNs && nowNs - checkedAtNs <= 3_000_000_000L
    }
    class Selection internal constructor(
        internal val ticket: RecordingLibraryAdmission.Ticket,
        internal val entry: RecordingCatalogue.Entry,
        internal val allowUnverified: Boolean,
    )
    private data class Work<T>(val result: Result<T>, val cleanupComplete: Boolean = true)
    private val admission = RecordingLibraryAdmission()
    private var catalogue: RecordingCatalogue? = null // IO worker only
    @Volatile private var picker: Selection? = null
    @Volatile private var copying: Selection? = null
    val pendingSelection: Selection? get() = picker
    val idle: Boolean get() = admission.idle
    val cleanupRetained: Boolean get() = admission.cleanupRetained
    private fun catalogue(): RecordingCatalogue = catalogue ?: RecordingCatalogue(directory).also { catalogue = it }

    fun scan(complete: (Result<List<RecordingCatalogue.Entry>>) -> Unit): RecordingLibraryAdmission.Ticket? =
        submit(RecordingLibraryAdmission.Kind.SCAN, { catalogue().scan() }, complete)

    fun prepare(profile: CaptureProfile, complete: (Result<Preparation>) -> Unit): RecordingLibraryAdmission.Ticket? =
        submit(RecordingLibraryAdmission.Kind.PREFLIGHT, {
            val store = catalogue()
            store.checkRecordingCapacity()
            val check = RecordingStorage.check(RecordingStorage.budget(profile)) {
                runCatching { Files.getFileStore(directory).usableSpace }.getOrNull()
            }
            when (check.state) {
                RecordingStorage.State.AVAILABLE -> Unit
                RecordingStorage.State.LOW -> throw PreparationFailure(PreparationProblem.LOW_STORAGE)
                RecordingStorage.State.UNAVAILABLE -> throw PreparationFailure(PreparationProblem.UNAVAILABLE)
            }
            Preparation(profile, System.nanoTime(), directory, store, store.newShow(profile))
        }, complete)

    private fun <T> submit(kind: RecordingLibraryAdmission.Kind, action: () -> T, complete: (Result<T>) -> Unit): RecordingLibraryAdmission.Ticket? {
        val ticket = admission.reserve(kind) ?: return null
        return if (enqueue(ticket, {
            val result = runCatching(action)
            Work(result, cleanupComplete = catalogue?.cleanupUncertain != true && result.exceptionOrNull()?.suppressed.isNullOrEmpty())
        }, complete)) ticket else null
    }
    private fun <T> enqueue(ticket: RecordingLibraryAdmission.Ticket, action: () -> Work<T>, complete: (Result<T>) -> Unit): Boolean {
        try {
            execute(Runnable {
                if (!admission.begin(ticket)) { admission.finish(ticket, true); return@Runnable }
                val value = try { action() } catch (error: Exception) { Work(Result.failure(error), cleanupComplete = catalogue?.cleanupUncertain != true && error.suppressed.isEmpty()) }
                val allowed = admission.finish(ticket, value.cleanupComplete)
                if (allowed) deliver { if (admission.canDeliver(ticket)) complete(value.result) }
                else if (!value.cleanupComplete) deliver {
                    // The failed owner still occupies capacity; report this
                    // exact result without releasing or replacing that owner.
                    if (admission.cleanupRetained && !admission.shouldCancel(ticket)) complete(value.result)
                }
            })
            return true
        } catch (_: RejectedExecutionException) {
            admission.finish(ticket, true) // No IO owner was constructed.
            return false
        }
    }
    fun cancel(ticket: RecordingLibraryAdmission.Ticket) { admission.cancel(ticket) }

    /** The picker receives no path or file handle. This exact selection alone
     * can later open the private source on the IO worker. */
    fun select(entry: RecordingCatalogue.Entry, allowUnverified: Boolean): Selection? {
        val ticket = admission.reserve(RecordingLibraryAdmission.Kind.EXPORT, picker = true) ?: return null
        return Selection(ticket, entry, allowUnverified).also { picker = it }
    }
    fun cancel(selection: Selection) {
        if (picker === selection) picker = null
        admission.cancel(selection.ticket)
    }
    fun cancelCopy() { copying?.let { admission.cancel(it.ticket) } }
    fun export(selection: Selection, openOutput: () -> OutputStream?, complete: (Result<Long>) -> Unit): Boolean {
        if (picker !== selection) return false
        if (!admission.picked(selection.ticket)) return false
        picker = null; copying = selection
        val queued = enqueue(selection.ticket, {
            var source: RecordingCatalogue.Export? = null
            var output: OutputStream? = null
            var problem: Throwable? = null
            var copied = 0L
            var clean = true
            try {
                check(!admission.shouldCancel(selection.ticket)) { "Export cancelled" }
                source = catalogue().openExport(selection.entry, selection.allowUnverified)
                check(!admission.shouldCancel(selection.ticket)) { "Export cancelled" }
                output = checkNotNull(openOutput()) { "The selected document could not be opened" }
                copied = source.copyTo(output) { admission.shouldCancel(selection.ticket) }
                check(!admission.shouldCancel(selection.ticket)) { "Export cancelled" }
                output.flush()
            } catch (error: Exception) { problem = error; if (error.suppressed.isNotEmpty()) clean = false }
            finally {
                try { output?.close() } catch (error: Exception) { clean = false; if (problem == null) problem = error else problem.addSuppressed(error) }
                try { source?.close() } catch (error: Exception) { clean = false; if (problem == null) problem = error else problem.addSuppressed(error) }
                if (source?.cleanupComplete == false || catalogue?.cleanupUncertain == true) clean = false
                if (copying === selection) copying = null
            }
            Work(if (problem == null) Result.success(copied) else Result.failure(checkNotNull(problem)), clean)
        }, complete)
        if (!queued && copying === selection) copying = null
        return queued
    }

    companion object {
        @Volatile private var application: RecordingLibraryOwner? = null
        fun application(context: Context): RecordingLibraryOwner = application ?: synchronized(this) {
            application ?: run {
                val worker = ThreadPoolExecutor(1, 1, 10, TimeUnit.SECONDS, ArrayBlockingQueue(1), { work ->
                    Thread(work, "sapflow-recording-library").apply { isDaemon = true }
                }).apply { allowCoreThreadTimeOut(true) }
                val main = Handler(Looper.getMainLooper())
                RecordingLibraryOwner(context.applicationContext.filesDir.toPath().resolve("recordings"),
                    { worker.execute(it) }, { reply -> main.post { reply() } })
            }.also { application = it }
        }
        fun message(error: Throwable): String = when (error) {
            is PreparationFailure -> when (error.reason) {
                PreparationProblem.LOW_STORAGE -> "Not enough free storage for local recording. Free up space or turn off local recording, then Prepare again."
                PreparationProblem.UNAVAILABLE -> "Recording storage is unavailable. Try again or turn off local recording explicitly."
            }
            is RecordingCatalogue.CatalogueFailure -> when (error.reason) {
                RecordingCatalogue.Failure.CAPACITY -> "The private recording folder exceeds the catalogue limit. Existing files were preserved."
                RecordingCatalogue.Failure.BUSY -> "A recording or export still owns these files. Wait for it to finish."
                RecordingCatalogue.Failure.CHANGED, RecordingCatalogue.Failure.UNSAFE -> "The recording changed or could not be checked safely. Refresh the list and choose again."
                RecordingCatalogue.Failure.INELIGIBLE -> "This recording cannot be exported in its current state."
                RecordingCatalogue.Failure.CANCELLED -> "Export cancelled. Your private original is unchanged."
                RecordingCatalogue.Failure.IO -> "The recording could not be read. Your private original is unchanged."
            }
            else -> "The recording operation could not finish. Your private original is unchanged."
        }
    }
}
