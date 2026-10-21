// Sapflow generation transformation for UniFFI 0.32.2. Keep native error
// buffers separate from JNA Structure backing memory; never dispose aliases.
// Four lazy cached statuses per thread. Deeper nesting uses the original fresh
// status/GC lifetime, so exhaustion cannot interrupt Arc argument consumption.
internal class SapflowRustCallStatusStack {
    private val cached = arrayOfNulls<UniffiRustCallStatus>(4)
    internal var depth = 0
        private set

    internal fun acquire(): UniffiRustCallStatus {
        val index = depth
        val status = if (index < cached.size) {
            cached[index] ?: UniffiRustCallStatus().also { cached[index] = it }
        } else {
            UniffiRustCallStatus()
        }
        reset(status)
        depth = index + 1
        return status
    }

    internal fun release(status: UniffiRustCallStatus, enteredDepth: Int) {
        check(depth == enteredDepth + 1) { "Rust-call status leases must unwind in LIFO order" }
        if (enteredDepth < cached.size) {
            check(cached[enteredDepth] === status) { "Wrong cached Rust-call status lease" }
        }
        // The normal checker has already lifted/freed any Rust error buffer.
        // Reset Java fields in-place, preserving the embedded Structure/layout.
        reset(status)
        depth = enteredDepth
    }

    private fun reset(status: UniffiRustCallStatus) {
        status.code = 0
        status.error_buf.capacity = 0
        status.error_buf.len = 0
        status.error_buf.data = null
    }
}

internal object SapflowRustCallStatusPool {
    private val local = object : ThreadLocal<SapflowRustCallStatusStack>() {
        override fun initialValue() = SapflowRustCallStatusStack()
    }
    internal fun current(): SapflowRustCallStatusStack = local.get()
}

// Internal visibility permits host tests of the actual generated helper.
// Error lifting remains inside the lease: it can perform nested buffer frees.
internal inline fun <U, E: kotlin.Exception> uniffiRustCallWithError(errorHandler: UniffiRustCallStatusErrorHandler<E>, callback: (UniffiRustCallStatus) -> U): U {
    val stack = SapflowRustCallStatusPool.current()
    val enteredDepth = stack.depth
    val status = stack.acquire()
    try {
        val return_value = callback(status)
        uniffiCheckCallStatus(errorHandler, status)
        return return_value
    } finally {
        stack.release(status, enteredDepth)
    }
}
