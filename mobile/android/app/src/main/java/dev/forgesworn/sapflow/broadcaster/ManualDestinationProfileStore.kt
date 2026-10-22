package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.net.Uri
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import android.util.AtomicFile
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileNotFoundException
import java.nio.file.Files
import java.security.KeyStore
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/** No V4V data: separate alias/file, using only authenticated-envelope and
 * verified-durable-replacement primitives. All methods run on the cold owner. */
internal class ManualDestinationProfileStore(context: Context) : ManualProfileBytes {
    private val base = File(context.applicationContext.noBackupFilesDir, "manual-destinations.v1.vault")
    private val atomic = AtomicFile(base)
    private val alias = "sapflow.manual.destinations.v1"
    private val maximum = ManualDestinationProfiles.MAX_STORE + 64
    private fun keys() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    private fun key(create: Boolean): SecretKey {
        val keys = keys()
        keys.getKey(alias, null)?.let { return it as SecretKey }
        check(create && !base.exists() && !File(base.path + ".bak").exists() && !File(base.path + ".new").exists())
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setRandomizedEncryptionRequired(true).build())
            generateKey()
        }
    }
    private fun checkPaths() {
        for (path in listOf(base, File(base.path + ".bak"), File(base.path + ".new"))) check(!Files.isSymbolicLink(path.toPath()))
    }
    private fun bounded(input: FileInputStream): ByteArray = input.use {
        require(it.channel.size() in 29..maximum.toLong())
        val bytes = ByteArrayOutputStream(); val buffer = ByteArray(4096)
        while (true) { val n = it.read(buffer); if (n < 0) break; require(n > 0 && bytes.size() + n <= maximum); bytes.write(buffer, 0, n) }
        bytes.toByteArray()
    }
    private fun <T> directory(action: (java.io.FileDescriptor) -> T): T {
        val fd = Os.open(base.parentFile!!.absolutePath, OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW, 0)
        try {
            val info = Os.fstat(fd)
            check(OsConstants.S_ISDIR(info.st_mode) && info.st_uid == Process.myUid() && (info.st_mode and 0x3f) == 0)
            checkPaths(); return action(fd)
        } finally { Os.close(fd) }
    }
    override fun read(): ByteArray? = directory {
        val existed = base.exists() || File(base.path + ".bak").exists() || File(base.path + ".new").exists() || keys().containsAlias(alias)
        val input = try { atomic.openRead() } catch (_: FileNotFoundException) {
            check(!existed)
            return@directory null
        }
        bounded(input)
    }
    override fun write(bytes: ByteArray) = directory { fd ->
        require(bytes.size in 29..maximum)
        // AtomicFile's void rename result alone is insufficient evidence.
            V4vDurableCommit(replace = { data ->
                checkPaths(); val output = atomic.startWrite()
                try { output.write(data); Os.fchmod(output.fd, 0x180); output.fd.sync(); atomic.finishWrite(output) }
                catch (error: Exception) { atomic.failWrite(output); throw error }
            }, readCommitted = { checkPaths(); bounded(FileInputStream(base)) }, syncDirectory = { Os.fsync(fd) }).write(bytes)
    }
    fun repository(): ManualDestinationProfiles {
        val cipher = V4vJournalCipher(::key, ManualDestinationProfiles.MAX_STORE)
        return ManualDestinationProfiles(this, cipher::seal, cipher::open)
    }
}

/** Application lifetime capacity one. A stalled provider or uncertain close
 * retains the slot across Activity recreation; no Activity/Surface is retained. */
internal class ManualDestinationProfileIO private constructor(context: Context) {
    private val app = context.applicationContext
    val picker = ManualProfilePicker<Uri>() // One untagged platform result across Activity recreation.
    private val admission = ManualProfileIOAdmission()
    private val worker = ThreadPoolExecutor(1, 1, 10, TimeUnit.SECONDS, ArrayBlockingQueue(1), { r -> Thread(r, "sapflow-manual-profiles").apply { isDaemon = true } }).apply { allowCoreThreadTimeOut(true) }
    private val alarms = ScheduledThreadPoolExecutor(1) { r -> Thread(r, "sapflow-manual-import-deadline").apply { isDaemon = true } }.apply { removeOnCancelPolicy = true }
    private val main = Handler(Looper.getMainLooper())
    private var repository: ManualDestinationProfiles? = null
    private fun repository() = repository ?: ManualDestinationProfileStore(app).repository().also { repository = it }
    fun <T> submit(action: (ManualDestinationProfiles) -> T, deadlineNs: Long? = null, cancel: (() -> Unit)? = null, reply: (Result<T>) -> Unit): Boolean {
        if (!admission.reserve()) return false
        val job = ManualProfileIOJob<T>(admission, deadlineNs)
        var alarm: java.util.concurrent.ScheduledFuture<*>? = null
        try {
            if (deadlineNs != null) alarm = alarms.schedule({
                val timedOut = job.timeout(SystemClock.elapsedRealtimeNanos())
                if (timedOut != null) {
                    // Post BEFORE cancel: a provider's cancellation Binder may
                    // itself block. Neither callback nor timeout releases IO.
                    main.post { reply(timedOut) }
                    try { cancel?.invoke() } finally { job.cancellationReturned() }
                }
            }, (deadlineNs - SystemClock.elapsedRealtimeNanos()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
            val timeout = alarm
            worker.execute {
                val result = try { Result.success(action(repository())) } catch (_: Exception) { Result.failure(ManualProfileFailure()) }
                val delivery = job.complete(result, SystemClock.elapsedRealtimeNanos(), !uncertain.get())
                timeout?.cancel(false)
                if (delivery != null) main.post { reply(delivery) }
            }
        } catch (_: Exception) {
            job.complete(Result.failure(ManualProfileFailure()), SystemClock.elapsedRealtimeNanos(), true)
            alarm?.cancel(false); return false
        }
        return true
    }
    private val uncertain = AtomicBoolean(false)
    private var retainedInput: java.io.InputStream? = null
    private var retainedDescriptor: android.content.res.AssetFileDescriptor? = null
    fun readDocument(uri: Uri, deadline: Long, signal: CancellationSignal): ManualDestinationProfile {
        require(uri.scheme == "content")
        var descriptor: android.content.res.AssetFileDescriptor? = null
        var input: java.io.InputStream? = null
        val parsed = try {
            descriptor = checkNotNull(app.contentResolver.openAssetFileDescriptor(uri, "r", signal))
            require(descriptor.declaredLength == -1L || descriptor.declaredLength in 1..ManualDestinationProfile.MAX_BYTES.toLong())
            check(!signal.isCanceled && SystemClock.elapsedRealtimeNanos() < deadline)
            input = descriptor.createInputStream()
            // Public API21 poll avoids idle reads. A malicious provider could
            // consume a shared pipe after poll; the independent logical timer
            // still reports at the original deadline and retains this actual
            // worker/descriptor until read and close really return (API29 too).
            val fd = (input as FileInputStream).fd
            val bytes = ManualProfileImport.read(input, SystemClock::elapsedRealtimeNanos, deadline) {
                while (true) {
                    val remaining = deadline - SystemClock.elapsedRealtimeNanos()
                    check(remaining > 0 && !signal.isCanceled)
                    val poll = StructPollfd().apply { this.fd = fd; events = OsConstants.POLLIN.toShort() }
                    if (Os.poll(arrayOf(poll), minOf(250, (remaining / 1_000_000 + 1).toInt())) > 0) break
                }
            }
            try { check(!signal.isCanceled); ManualDestinationProfile.parse(bytes) } finally { bytes.fill(0) }
        } finally {
            var closeError: Exception? = null
            try { input?.close() } catch (error: Exception) { closeError = error }
            try { descriptor?.close() } catch (error: Exception) { closeError = error }
            if (closeError != null) {
                retainedInput = input; retainedDescriptor = descriptor
                uncertain.set(true); throw ManualProfileFailure()
            }
        }
        check(!signal.isCanceled && SystemClock.elapsedRealtimeNanos() < deadline)
        return parsed
    }
    companion object {
        @android.annotation.SuppressLint("StaticFieldLeak") // Constructor retains applicationContext only; no Activity/Surface.
        @Volatile private var instance: ManualDestinationProfileIO? = null
        fun application(context: Context): ManualDestinationProfileIO = instance ?: synchronized(this) {
            instance ?: ManualDestinationProfileIO(context.applicationContext).also { instance = it }
        }
    }
}
