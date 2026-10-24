# Native binding checks

`CARGO_BUILD_JOBS=2 node scripts/build-native.mjs check` generates fresh Swift
and Kotlin bindings against the actual host library and runs both consumers.
The build reuses the checkout target; it removes its own staging directory and
does not replace Apple/Android packages in check mode.

For a fresh Apple or Android package location, add
`--output-root=/absolute/checkout/target/native-packages-run1` to the corresponding
build command. The destination must be a new canonical direct child of `target/`
with a `native-packages-` prefix and a safe suffix. It is reserved before building;
existing package outputs are preserved. Cargo continues to reuse the checkout's
`target/`. Run `node --test scripts/native-build-options.test.mjs` to check option,
path and output-preservation behaviour.

Both consumers also exercise selective fanout admission closure.
`closePublishingAdmission` and `closeRecordingAdmission` stop new admission to
one branch without cancelling the other or clearing already accepted samples.
The existing drain and residence rules still apply. Closure is irreversible for
that queue, including after `clear`; a new show needs a new queue. Successful
native return is a local fence receipt, not transport completion or remote
erasure. A `Busy` error is not a successful receipt.

Rust callers matching `EnqueueOutcome` exhaustively must handle the new
`AdmissionClosed` variant. This pre-1.0 addition leaves the native
`FanoutOutcome` record and existing queue limits unchanged.

The Kotlin generation step applies
[`native-kotlin-status-leases.mjs`](../../scripts/native-kotlin-status-leases.mjs)
to the exact pinned UniFFI 0.32.2 Rust-call helper. The implementation is in
[`native-kotlin-status-leases.kt`](../../scripts/native-kotlin-status-leases.kt).
It refuses missing, changed, duplicate or already transformed helpers. Run
`node --test tests/native/kotlin-status-leases.test.mjs` for those guards.

Four status records, each including its nested RustBuffer descriptor, are
cached lazily per calling thread. Acquisition and release clear code, capacity,
length and pointer fields in place. A lease remains held through status/error
lifting, which can make nested native buffer-free calls. `finally` unwinds in
LIFO order; threads never share status records. No native wire buffer or media
payload is cached here, and no native pointer is disposed by the pool.

The bound is on the **cache**, not on active recursive calls. Deeper nesting
uses the original fresh-status allocation and scoped GC/cleaner lifetime;
those records are not retained in the cache. Active call-stack memory follows
the original JVM behaviour. There is intentionally no hard recursion error:
one could interrupt generated argument lowering after a receiver or argument
Arc was cloned but before native code consumed it. Overflow and later recovery
are tested with real constructors, Arc arguments, errors, frees and closes.
Thread-local caches become collectible when their calling threads terminate;
the pool does not create a thread or require a shutdown callback.

`StatusLeases.kt` is compiled in the same module as the fresh bindings during
the host check. It exercises cached/embedded identity, dirty-field reset,
nested success/error/free and panic statuses, exception unwinding, overflow,
LIFO refusal/recovery, three concurrent owners and close versus an in-flight
native call. Panic status tests use real Rust-owned error buffers but do not
add an SDK panic entry point. `Consumer.kt` also checks cache identity through
reflection so the separately compiled consumer verifies actual AAR classes
when Android packaging is explicitly requested. Existing ownership, fanout,
recording, RTMP and SRT consumer coverage remains in place.

This is a controlled allocation experiment motivated by sampled Android ART
profiles. It does not alter the native ABI, protocol, media, queue bounds or
copy ledger, and it is not a claim that all JNA allocations disappear. Physical
allocation attribution and sustained capture must be repeated against the
refreshed package before calling it a performance fix.
