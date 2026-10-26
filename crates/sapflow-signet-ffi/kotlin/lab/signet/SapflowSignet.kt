@file:JvmName("Sapflow_signet_ffiKt")

package lab.signet

import java.nio.CharBuffer
import java.nio.charset.CodingErrorAction
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

interface Disposable {
    fun destroy()

    companion object {
        fun destroy(vararg args: Any?) {
            for (arg in args) {
                when (arg) {
                    is Disposable -> arg.destroy()
                    is ArrayList<*> -> for (element in arg) if (element is Disposable) element.destroy()
                    is Map<*, *> -> for (element in arg.values) if (element is Disposable) element.destroy()
                    is Iterable<*> -> for (element in arg) if (element is Disposable) element.destroy()
                }
            }
        }
    }
}

inline fun <T : Disposable?, R> T.use(block: (T) -> R) =
    try {
        block(this)
    } finally {
        try {
            this?.destroy()
        } catch (_: Throwable) {
        }
    }

sealed class SignetException : Exception() {
    override val message: String get() = ""

    class InvalidInput : SignetException()
    class InvalidCredential : SignetException()
    class InvalidProof : SignetException()
    class Busy : SignetException()
    class Deadline : SignetException()
    class Cancelled : SignetException()
    class Refused : SignetException()
    class Unavailable : SignetException()
    class Closed : SignetException()
}

internal object Native {
    init {
        System.loadLibrary("sapflow_signet_ffi")
        System.loadLibrary("sapflow_signet_jni")
        check(abiVersion() == 1) { "Unsupported sapflow-signet ABI version: ${abiVersion()}" }
    }

    @JvmStatic external fun abiVersion(): Int
    @JvmStatic external fun credentialRestore(bytes: ByteArray): Long
    @JvmStatic external fun credentialFree(handle: Long)
    @JvmStatic external fun credentialEncoded(handle: Long): ByteArray
    @JvmStatic external fun nativePairBunker(uri: ByteArray, deadlineMs: Int): Long
    @JvmStatic external fun nativeRestore(credential: Long, deadlineMs: Int): Long
    @JvmStatic external fun nativeFree(handle: Long)
    @JvmStatic external fun nativeConnect(handle: Long, requestId: Long)
    @JvmStatic external fun nativeSignLogin(
        handle: Long,
        requestId: Long,
        challenge: ByteArray,
        origin: ByteArray,
        appName: ByteArray,
        createdAtSeconds: Long,
    )
    @JvmStatic external fun nativeWaitEvent(handle: Long, timeoutMs: Int): Array<Any>?
    @JvmStatic external fun nativeRequestClose(handle: Long)
    @JvmStatic external fun nativeWaitClosed(handle: Long, timeoutMs: Int): Boolean
    @JvmStatic external fun nativeIsClosed(handle: Long): Boolean
}

private fun utf8(value: String): ByteArray {
    val encoded = Charsets.UTF_8.newEncoder()
        .onMalformedInput(CodingErrorAction.REPORT)
        .onUnmappableCharacter(CodingErrorAction.REPORT)
        .encode(CharBuffer.wrap(value))
    val copy = ByteArray(encoded.remaining())
    encoded.get(copy)
    return copy
}

internal class NativeHandle(private val raw: Long, private val release: (Long) -> Unit) {
    private val calls = AtomicLong(1)
    private val destroyed = AtomicBoolean(false)

    val isDestroyed: Boolean get() = destroyed.get()

    fun <T> call(block: (Long) -> T): T {
        while (true) {
            val count = calls.get()
            if (count == 0L) throw IllegalStateException("Signet object has already been destroyed")
            if (count == Long.MAX_VALUE) throw IllegalStateException("Signet call counter would overflow")
            if (calls.compareAndSet(count, count + 1)) break
        }
        try {
            return block(raw)
        } finally {
            if (calls.decrementAndGet() == 0L) release(raw)
        }
    }

    fun destroy() {
        if (destroyed.compareAndSet(false, true) && calls.decrementAndGet() == 0L) release(raw)
    }
}

interface SignetCredentialInterface {
    fun encoded(): ByteArray
    companion object
}

open class SignetCredential internal constructor(handle: Long) : Disposable, AutoCloseable, SignetCredentialInterface {
    internal val native = NativeHandle(handle, Native::credentialFree)

    val uniffiIsDestroyed: Boolean get() = native.isDestroyed

    override fun encoded(): ByteArray = native.call(Native::credentialEncoded)

    override fun destroy() = native.destroy()

    override fun close() = destroy()

    @JvmName("finalize")
    protected fun finalizeSignetCredential() = close()

    companion object {
        @JvmStatic
        @Throws(SignetException::class)
        fun restore(bytes: ByteArray): SignetCredential = SignetCredential(Native.credentialRestore(bytes))
    }
}

interface NativeSignetInterface {
    fun connect(requestId: ULong)
    fun isClosed(): Boolean
    fun requestClose()
    fun signLogin(requestId: ULong, challenge: String, origin: String, appName: String, createdAtSeconds: ULong)
    fun waitClosed(timeoutMs: UInt): Boolean
    fun waitEvent(timeoutMs: UInt): SignetEvent?
    companion object
}

open class NativeSignet internal constructor(handle: Long) : Disposable, AutoCloseable, NativeSignetInterface {
    private val native = NativeHandle(handle, Native::nativeFree)

    val uniffiIsDestroyed: Boolean get() = native.isDestroyed

    @Throws(SignetException::class)
    override fun connect(requestId: ULong) = native.call { Native.nativeConnect(it, requestId.toLong()) }

    override fun isClosed(): Boolean = native.call(Native::nativeIsClosed)

    override fun requestClose() = native.call(Native::nativeRequestClose)

    @Throws(SignetException::class)
    override fun signLogin(
        requestId: ULong,
        challenge: String,
        origin: String,
        appName: String,
        createdAtSeconds: ULong,
    ) = native.call {
        Native.nativeSignLogin(
            it,
            requestId.toLong(),
            utf8(challenge),
            utf8(origin),
            utf8(appName),
            createdAtSeconds.toLong(),
        )
    }

    @Throws(SignetException::class)
    override fun waitClosed(timeoutMs: UInt): Boolean = native.call {
        Native.nativeWaitClosed(it, timeoutMs.toInt())
    }

    @Throws(SignetException::class)
    override fun waitEvent(timeoutMs: UInt): SignetEvent? = native.call { handle ->
        Native.nativeWaitEvent(handle, timeoutMs.toInt())?.let { value ->
            when (value[0] as Int) {
                1 -> SignetEvent.Ready(
                    (value[1] as Long).toULong(),
                    value[2] as String,
                    SignetCredential(value[3] as Long),
                )
                2 -> SignetEvent.Proof((value[1] as Long).toULong(), value[2] as ByteArray)
                3 -> SignetEvent.Failed((value[1] as Long).toULong(), signetError(value[2] as Int))
                else -> throw IllegalStateException("Unknown native Signet event")
            }
        }
    }

    override fun destroy() = native.destroy()

    override fun close() = destroy()

    @JvmName("finalize")
    protected fun finalizeNativeSignet() = close()

    companion object {
        @JvmStatic
        @Throws(SignetException::class)
        fun pairBunker(uri: String, deadlineMs: UInt): NativeSignet =
            NativeSignet(Native.nativePairBunker(utf8(uri), deadlineMs.toInt()))

        @JvmStatic
        @Throws(SignetException::class)
        fun restore(credential: SignetCredential, deadlineMs: UInt): NativeSignet =
            credential.native.call {
                NativeSignet(Native.nativeRestore(it, deadlineMs.toInt()))
            }
    }
}

private fun signetError(code: Int): SignetException = when (code) {
    1 -> SignetException.InvalidInput()
    2 -> SignetException.InvalidCredential()
    3 -> SignetException.InvalidProof()
    4 -> SignetException.Busy()
    5 -> SignetException.Deadline()
    6 -> SignetException.Cancelled()
    7 -> SignetException.Refused()
    8 -> SignetException.Unavailable()
    9 -> SignetException.Closed()
    else -> SignetException.Unavailable()
}

sealed class SignetEvent : Disposable {
    data class Ready(val requestId: ULong, val persona: String, val credential: SignetCredential) : SignetEvent()
    data class Proof(val requestId: ULong, val json: ByteArray) : SignetEvent()
    data class Failed(val requestId: ULong, val error: SignetException) : SignetEvent()

    override fun destroy() {
        when (this) {
            is Ready -> Disposable.destroy(requestId, persona, credential)
            is Proof -> Disposable.destroy(requestId, json)
            is Failed -> Disposable.destroy(requestId, error)
        }
    }
}
