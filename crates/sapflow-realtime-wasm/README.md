# Opaque real-time scheduling boundary

This is a browser boundary for `sapflow_core::realtime`, not a codec, HLS
player, SFU, room service or transport. The runtime-free native Rust API lives
in `sapflow-core`; browser hosts use the same rules through a safe
`wasm-bindgen` boundary. No media keys, camera/microphone access, sockets,
recording, logging or wall clock are introduced.

Build from the repository root with the installed `wasm32-unknown-unknown`
Rust target and exactly `wasm-bindgen-cli 0.2.129`:

```sh
scripts/build-realtime-wasm.sh
node --test tests/realtime-wasm.test.mjs
```

The build reuses the checkout's `target/`. Stage all files from
`target/realtime-browser/` together. Its generated JavaScript and WASM are
local build outputs, not published SDK artefacts. The pinned wasm-bindgen
crate/CLI version was checked against the official 0.2.129 release; no other
new external dependency is introduced by this adapter.
The output also carries the Sapflow licence and generated third-party
licence texts for the browser boundary. A separate CI job builds and tests
the compiled WASM boundary; adding that job is not a hosted-CI passing result.

## Browser host API

```js
import { createRealtimeQueue } from './browser.mjs'

const queue = await createRealtimeQueue(wasmBytes, {
  maxBytes: 512 * 1024,
  maxFrames: 64,
  maxStreams: 32,
  maxResidenceMs: 150,
})
const now = performance.now()
const result = queue.offer({
  stream: 1, video: true, key: true, sequence: 0,
  deadlineMs: now + 100, timestamp: 0,
}, encryptedFrame, now)
const next = queue.pop(performance.now())
if (next) sendCiphertext(next.ciphertext, next.metadata)
queue.tick(performance.now())
queue.close()
```

The host supplies already encrypted complete frames, then fragments only
frames returned by `pop`. `offer` returns `accepted`, `dropped` and
`waitingForKeyframe`. `pop` returns `{metadata, ciphertext}` or `undefined`.
`tick` returns the number of released stale frames. `waitingForKeyframe(id)`
and `stats()` expose local bounded state. `close()` is idempotent and
irreversible; callers must close before abandoning a transport lifetime.
Calls after close, malformed scalars and backwards clocks throw a generic
error without reflecting user metadata. No timers are created: the host must
call `tick` even when capture stops or the network is blocked.

Metadata requires nonzero `stream`, boolean `video` and `key`, uint32
`sequence`, and monotonic `deadlineMs`. Audio has `video:false,key:false`.
Separate audio/video tracks use separate stream IDs. Up to sixteen short
scalar fields are copied for the host's authenticated frame envelope. No
objects, functions, getters or oversized strings are retained. Timestamp and
codec interpretation remains the host's responsibility. Input ciphertext is
copied on accepted-size offers, so later caller mutation cannot change queued
bytes. The original input, a temporary offered copy, popped frames and
transport/codec buffers are outside the resident queue budget.

Audio pops before video. Audio pressure invalidates queued video before
replacing old audio; video cannot evict audio. Video begins waiting for a
keyframe. A sequence gap, refused oversized/expired video frame, queue
pressure or expiry invalidates that stream's entire backlog and requires a
new keyframe. Other streams remain independent. Old/reordered sequence values
are dropped without invalidating a valid chain. Sequence numbers never wrap
within a stream lifetime. Reconstruct a queue for a new generation or exhausted
stream table; drained stream replay fences are deliberately retained.

Absolute maximums are 16 MiB of claimed ciphertext, 4096 frames, 256 stream
lifetimes and 60 seconds of queue residence. The ciphertext map owns only
resident frames; every Rust discard is drained synchronously. Native callers
must likewise free every `Changes::discarded` ID. Scalar counts do not certify
whole-process RAM, network latency, media correctness, confidentiality,
application authorisation or production call quality. Forwarders may falsify
metadata or drop bytes: end-to-end authentication, replay checking, frame
reassembly and decoder bounds belong to the consuming adapter.

The raw WASM class returns numeric status and requires `takeDrop()` to be
drained before another mutating operation. This refusal prevents an unbounded
pending-release list even when an untrusted caller ignores ownership changes.
Use the wrapper for ordinary application integration.
