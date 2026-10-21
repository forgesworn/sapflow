# Native decoded-origin observation

An origin status or fresh media resource establishes encoded availability.
Remote-media confirmation additionally requires actual decoded video and audio
from resources belonging to the current publishing attempt. The native apps
still need their authenticated fetch, attempt correlation and decoder wiring.

The macOS regression below establishes a smaller prerequisite: Apple's native
reader can decode Sapflow's exact initialisation object plus one completed
independent fMP4 part. It uses a test pattern and sine wave generated during
the run. It accesses no device or existing recording, renders no frames, plays
no sound and deletes its generated media afterwards.

## What the reader checks

`tests/native/OriginFragmentDecode.swift` requires one H264 track and one AAC
track, with bounded input size, dimensions, sample rate and output count.
Asynchronous asset metadata loading leaves the main dispatch queue available.
The reader requests cancellation at 2.75 seconds and has a separate three-second
deadline; the supervising harness also bounds each process call.

Video must produce at least two pixel-buffer outputs with increasing PTS.
Audio must produce decoded, signed, interleaved 16-bit PCM. For this AAC-LC
fixture, at least 2,048 decoded PCM frames cover two 1,024-frame audio units.
The reader verifies byte count, frame size, sample rate and native timing at
the first and last sample of each output buffer. The covered interval includes
the final sample's duration. Duplicate or overlapping intervals, inflated
duration and inconsistent byte counts are refused.

AVAssetReader can combine multiple decoded audio units in one output buffer.
The receipt reports that actual buffer count and the validated PCM frame count
separately. Silence remains valid audio; no amplitude test is applied.

The timing controls exercise short and coalesced buffers, exact boundaries,
overlap, invalid bytes and inflated timing. The integration run also requires
independent FFmpeg decoding and refusal of initialisation without media. The
Python regression tests cover exact independence attributes, absolute deadline
propagation and cleanup when individual child or scratch operations fail.

## Run on macOS

Use this worktree's existing Rust target. The output directory must be new.

```sh
cargo build --locked -j2 -p sapflow-server
mkdir -p target/native-origin-reader
swiftc -swift-version 5 -warnings-as-errors -O \
  tests/native/OriginFragmentDecode.swift \
  -framework AVFoundation -framework AudioToolbox \
  -framework CoreMedia -framework CoreVideo \
  -o target/native-origin-reader/origin-decode-probe
python3 -B tests/test_native_origin_reader.py
python3 -B tests/native-origin-reader.py \
  --origin-binary target/debug/sapflow \
  --reader-executable target/native-origin-reader/origin-decode-probe \
  --output target/native-origin-reader/generated-check
```

The harness starts its own loopback origin and generated publisher, with
recording and external callbacks disabled. It fetches the exact init and an
independent part, checks both decoders, then stops its children and removes its
temporary media. Cleanup failures fail the result and are retained in the
private receipt. Only code and sanitised scalar evidence may enter Git.

## Remaining integration

This is a finite macOS format check. It does not establish iPhone or Android
decoder behaviour, hardware decoding, network trust, attempt identity, viewer
presentation, capture-to-screen latency or the sustained B5 requirements.

A real observer must capture its SDK generation and attempt epoch before
fetching. It must validate authenticated session identity, the current
connection nonce and epoch, initialisation and approved playback routing;
perform bounded native decoding; then recheck authority and local ownership
before calling `mediaConfirmedForAttempt` with the captured identity. Stop,
End, expiry or a replacement must discard the old result. Decode failure keeps
the narrower sending state and leaves independent recording running.

The VideoToolbox require-hardware-decoder key is available from iOS 17, while
the current app supports iOS 16. This helper uses AVAssetReader without that
key. Production hardware admission and availability handling remain separate
work. See [transport recovery](transport-recovery.md) and the
[delivery plan](mobile-streaming-plan.md).
