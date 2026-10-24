# Real SDK checkpoint tests

On macOS 14 or later with full Xcode and the Rust toolchain from
`rust-toolchain.toml`, run from any directory:

```sh
python3 -B mobile/ios/test-recording-checkpoints-sdk.py
```

The path above is relative to the checkout. CI invokes it in the `iOS SDK
checkpoint tests` job on `macos-15`, after installing the project toolchain and
restoring the ordinary Rust cache, with `CARGO_BUILD_JOBS=1`. The existing Ubuntu
jobs remain separate because they cannot execute XCTest. No private
receipts, user paths, cross-platform SDK packages, JNA, phones or credentials are
required. The script builds the current host SDK with the locked dependency graph,
generates fresh Swift bindings and runs all 16 checkpoint-owner plus 8 cold-probe
tests against that actual library. It does not change their assertions.

Cargo uses this checkout's existing `target/`; no `CARGO_TARGET_DIR` or incremental
override is introduced. Do not run concurrently with another native SDK build:
the host library and bindgen executable are shared build outputs. The Swift module
cache at `target/ios-module-cache` is preserved. Each run creates one exclusive
`target/sapflow-checkpoint-sdk-*` scratch directory. Success removes that scratch
only after all owned process groups are closed. Failure/unknown closure retains
it and prints the exact path with compiler/test/process logs; retain the needed
failure evidence, then remove only that task-created scratch after safe closure.

The existing `test-recording-checkpoints.sh` remains an unchanged historical
receipt-driven gate. This ordinary entrypoint qualifies host filesystem/ownership
and real SDK composition tests only. Its generated sample bytes are not decodable
AAC/native source facts. Native presentation, physical capture, recovery/aggregate
loss, default adoption, platform packages and release gates remain separate.
