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
cargo build --locked -j2 -p sapflow
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

## Native iOS observation component

The iOS source now includes a finite production observation pipeline, separate
from the installed app and its unfinished V4V creator UI. It is a component
for the destination owner to compose, not physical iPhone acceptance.

- `OriginObservationPolicy` binds the captured destination, codec epoch,
  local token, SDK generation/attempt and per-connection nonce. The original
  durable grant is unchanged; only the transport uses its canonical SRT ID.
- `V4vOriginStatusHTTP` obtains fresh exact-session status through a dedicated
  instance of the existing bounded control client. The ephemeral authority is
  capped by bearer and grant expiry. It never closes the creator's End or
  refresh client and never treats the journal as fresh evidence. Observation
  does not mutate the session engine; a stale GET after a concurrent Refresh
  cannot discard the new grant or stop the publisher.
- `OriginMediaHTTP` fetches only the two canonical resources on the reviewed
  HTTPS playback route. It accepts no bearer or arbitrary headers, follows no
  redirects, and uses no cookies or shared cache. Initialisation is at most
  256 KiB and the combined input at most 8 MiB.
- `OriginMediaDecoder` demuxes the bounded input with AVAssetReader, requires
  VideoToolbox hardware H264 decode and checks the selected hardware property.
  AAC is decoded to native signed interleaved PCM; platform software AAC is
  allowed. Its one new private scratch file contains downloaded origin bytes,
  never an existing phone recording. It renders no picture and plays no sound.
- `V4vOriginObserver` checks fresh status before and after actual decode,
  permits newer parts in the same origin epoch/init, then closes its resources
  before confirming through the shared cold gate. `OriginSDKConfirmation`
  calls `mediaConfirmedForAttempt` with the captured SDK identity.

The reviewed playback prefix replaces the canonical `/live/` directory. For
example, `/live/event-example/abc_part2.mp4` maps to the approved origin plus
`/media/live/event-example/abc_part2.mp4` when the approved prefix is
`/media/live/`. Prefix `/live/` keeps the path unchanged; a proxy mounted at
`/origin/` needs the full prefix `/origin/live/`. No routing is inferred from
grant display URLs, user input or the private control listener. A configured
prefix alone does not establish that a public proxy actually serves the media.

The complete attempt has an original eight-second observation deadline and
at most twelve initial availability/status attempts. Each decoder is capped
at three seconds; it validates real increasing video outputs and exact native
PCM timing, including coalesced output. Stop, End, expiry, transport failure
and codec close must share the same confirmation gate in the destination owner.
Final authority time is sampled inside that gate, after acquiring its lock;
an older observer's cancellation cannot invalidate a successor attempt.
No observer failure changes the protocol retry budget or stops local recording.

Cancellation fences confirmation immediately and requests asynchronous
cleanup. The owner must retain an observer until `cleanupComplete`, including
after an error. A two-second cleanup-reporting limit does not turn an unfinished
native call into released capacity. Actual URLSession invalidation callbacks,
native cancellation completion and owned scratch removal establish cleanup.
The generated-media harness binds decoder temporary files to its own scratch,
so its parent can also remove them after a subprocess timeout or forced exit.

Hardware-decoder enforcement and its selected-hardware property require iOS 17.
The iOS 16 app target still compiles; this component reports observation as
unsupported there. It never silently substitutes software video decoding.
Audio-only, HEVC, presentation, sustained synchronisation and physical decoder
qualification remain separate work.

Run the actual host policy/pipeline tests with:

```sh
bash mobile/ios/test-origin-observation.sh
```

Exercise the production decoder against newly generated origin media with:

```sh
bash tests/native-production-origin-reader.sh \
  target/native-production-origin-check target/debug/sapflow
```

The first production macOS run produced six hardware-decoded video outputs and
7,104 PCM frames in one native audio buffer, refused header-only input, and
confirmed cleanup. Independent FFmpeg decoding also passed. This is a finite
macOS component gate; phone capture, playback routing, actual creator auth and
application wiring still need their own end-to-end evidence.
