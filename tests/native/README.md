# Native binding checks

`CARGO_BUILD_JOBS=2 node scripts/build-native.mjs check` packages the handwritten
Swift and Kotlin facades against the actual host library, compiles the JNI shim,
and runs both consumers without JNA. The build reuses the checkout target; it
removes its own staging directory and does not replace Apple/Android packages
in check mode. Direct `ByteBuffer` input uses a borrowed address/length for
synchronous native calls; compressed ownership is copied once in Rust.

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

`Consumer.kt` detects which transport it is running. On the old path it checks
status-cache identity. On the handwritten path it checks the dedicated direct
buffer, sample, scalar and fanout JNI entries, including a non-zero direct
buffer position and an unchanged position/limit after the call. The remaining
ownership, fanout, recording, programme, RTMP and SRT checks run on both paths.
The Android AAR contains Rust and JNI libraries for every selected ABI, linked
with 16 KiB page alignment and requiring no JNA dependency.
