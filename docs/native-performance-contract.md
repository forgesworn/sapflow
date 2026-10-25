# Native performance and ForgeSworn library reuse

Native performance is a product requirement for the standalone broadcaster.
The owner reinforced this requirement on 21 October 2026, including use of
the existing ForgeSworn libraries. The first release remains broadcasting
and origin playback; native RelaySwarm follows under the agreed release plan.

A native language alone does not establish performance. Release evidence must
account for hardware codec use, copies, allocations, scheduling, memory,
latency, battery and sustained behaviour on the supported physical devices.

## Required architecture

- Use SwiftUI with Swift platform adapters on iOS and Jetpack Compose with
  Kotlin platform adapters on Android. The standalone app does not introduce
  a WebView, JavaScript or cross-platform rendering runtime.
- Use AVFoundation, Core Media and VideoToolbox on iOS, and camera surfaces
  with MediaCodec on Android. Prefer native surface/pixel-buffer preview and
  encoding paths that avoid copying raw frames through application memory.
- Require a hardware encoder for each advertised device/profile. Record the
  actual selected implementation; report unsupported profiles rather than
  silently switching to software video encoding.
- Keep raw camera frames and PCM audio in the platform capture/codec path.
  Rust receives compressed samples and owns common protocol/session logic.
- Encode once and share the encoded sample ownership between publishing and
  local recording. Monitoring decodes the origin separately only when used;
  it must not trigger a second encode for the local recording.
- Keep UI work, signing, relay discovery, destination HTTP calls and storage
  maintenance outside capture/codec callbacks. Those callbacks have bounded
  work and never wait for a network or disk operation.
- Reuse Sapflow's media and protocol implementation. Keep platform surfaces,
  permissions and hardware codecs in their native adapters; the choice of
  Rust does not justify routing platform-native buffers through extra layers.
- Use our own SRT sender and RTMP publishing state machines. RTMPS uses our
  RTMP implementation over maintained TLS with certificate validation. Existing
  SRT listener and RTMP ingest support must be extended and qualified as clients;
  third-party broadcaster/protocol SDKs are comparison tools, not replacements.

[VideoToolbox](https://developer.apple.com/documentation/videotoolbox)
and [MediaCodec](https://developer.android.com/reference/android/media/MediaCodec)
are the platform codec foundations. Android's encoder input surface supports
a hardware-rendered path; this is different from reading camera frames into
an application byte array.

## Buffer and scheduling contract

Document the owner and lifetime at every transition: platform encoder output,
FFI call, Rust sample, transport queue, recording queue, encrypted packet and
completed origin part/segment.

For the encoded-sample boundary:

1. Prefer a borrowed synchronous input where the pinned bindings support it.
2. If asynchronous transmission/recording needs ownership after the call,
   make one accounted copy into a reusable owned buffer before returning.
3. Share that owned sample across consumers, using immutable reference-counted
   buffers where suitable. Do not clone the payload for each consumer.
4. Release the platform codec buffer promptly. No codec buffer may be retained
   while waiting for retransmission, a slow disk or a network round trip.
5. Count any additional conversion, packetisation, encryption and output copies
   separately. A boundary optimisation is not a claim of zero copies end to end.

UniFFI's [borrowed-byte interface](https://mozilla.github.io/uniffi-rs/latest/types/bytes.html)
is a candidate for the ingress call, subject to a compile/runtime check against
the chosen pinned version. Its borrow lasts only for the synchronous call;
asynchronous work needs owned storage. Validate direct Kotlin buffers and
Swift input behaviour in the actual installed consumers. Do not assume that
the version already used by another ForgeSworn package supports every feature
in the latest documentation.

Keep commands, configuration and coalesced statistics on a convenient control
API. Do not serialise each media payload through JSON, base64, a JavaScript
bridge or a general event bus.

Size pools and queues in bytes and media time. Define overflow behaviour for
video, audio, recording and retransmission independently. Reuse packet/storage
buffers after warm-up; instrument unexpected allocation churn. Use ownership
transfer or bounded queues to keep capture callbacks independent of global
locks and asynchronous runtime scheduling.

Share one explicitly owned network executor where components allow it. Record
library-owned threads and timers; do not create a runtime per stream or per
helper library. Drive idle work from readiness and real deadlines rather than
busy polling. Coalesce statistics/UI updates and keep normal per-packet logging
off. Cancelling the session releases timers, buffers, handles and workers.

## ForgeSworn library adoption map

These are adoption decisions for the plan, not claims that all the integrations
or mobile performance measurements already exist.

| Existing component | Adopt or evaluate | Performance contract |
| --- | --- | --- |
| sapflow-srt | Extend the current sans-IO receiver's protocol layers with the caller/sender required by B3 | Reuse packet, sequence, timing and bounded protocol machinery; independent sender interop and pacing remain required |
| sapflow-rtmp | Reuse our chunk/AMF/handshake and publishing-client implementation for B3R | Bounded TCP queues, measured FLV packaging and TLS costs; source and finite interoperability checks remain separate from physical publishing acceptance |
| sapflow-hls | Reuse codec configuration, encoded-sample handling, fragmented MP4 and HLS packaging | Share immutable payloads; benchmark muxing/assembly copies and keep a recording sink independent of uplink stalls |
| sapflow-core | Reuse compatible pure policy/types behind the appropriate adapter | Keep V4V-specific paths and callbacks out of the general client interface |
| RelaySwarm and RelaySwarmKit | Reuse signed-media/session contracts and the existing Swift implementation in B6 | Hash/cache once per immutable object where valid; bounded verification and fallback; prove native/browser compatibility |
| ForgeSworn Link | Use the native endpoint/engine and transport adapters for a measured recording-transfer integration | Direct/relay throughput, CPU, memory and handover measured with realistic files; preserve its separate wire protocol |
| Shelter Kit | Reuse streaming content-addressed storage, fetch and repair interfaces | Retain bounded streaming, hashing, atomic commit and quota behaviour; do not materialise a whole recording in RAM |
| Foregather | Reuse the native packaging/consumer pattern; use invitation transfer only when needed | Pairing stays outside media callbacks; keep authority with the application |
| Stash Rust | Evaluate its existing FSWNENC2/vault-compatible format for private recording backup | Reuse compatible crypto/format work; provide a bounded file-streaming API before large-recording adoption |
| Fray | Evaluate only protocol-independent relay policy or measured scheduling primitives | Its game transport and message limits are not the media protocol; do not import game session machinery into the broadcaster |

Each proposed new dependency needs a short record: exact source/version,
responsibility, authority boundary, existing consumer, copying/runtime costs,
licences, compatibility tests and the benchmark that supports its adoption.
Prefer extending a suitable existing ForgeSworn component over duplicating its
contract in the broadcaster. Where the component does not fit, record the
specific mismatch and extract only a justified reusable seam.

Do not copy a second implementation of relay transport, recording framing,
pairing, storage verification or retry policy into the app simply because it
is convenient. Reuse must still preserve the consuming protocol's timing and
authority requirements.

## Current source findings to verify in B0

The existing [Sapflow profile](profiling.md) identifies muxer copying,
allocation, packet syscalls and scheduler wake-ups as meaningful costs in its
recorded server workloads. Those measurements guide investigation; they are
not measurements of the proposed phone app.

The HLS muxer currently retains individual parts and also assembles a whole
segment buffer. Review the cost of that representation at the frozen source,
along with fragmented MP4 sample assembly. Evaluate shared/chained output and
buffer reuse before introducing another caching/copying layer. Avoid rebuilding
or hashing equivalent output per viewer.

Shelter's BlobFetcher contract already requires a bounded returned stream.
Preserve it when supplying a Link adapter. A frame format described as chunked
does not imply that its public API streams: the inspected Stash Rust save/seal
interfaces consume a complete byte slice and restore returns an owned vector.
Its format is useful prior work; those interfaces need an explicit streaming
design before they suit multi-gigabyte recordings.

The installed Android ARM64 binding now exposes a combined RTMP send/output
operation. Its generated success path still performs four native calls:
publisher-handle clone, sample-handle clone, method and Rust-buffer free.
The equivalent separate tick/send/output/queue/deadline/state sequence needs
16. These are generated-boundary counts, not four protocol executions.

This changes call overhead rather than payload ownership. Direct encoder
input makes one copy into Rust-owned sample storage; fanout shares immutable
handles. RTMP writes the FLV prefix and payload directly into chunked wire output.
Serialising the owned return and lifting it into a Kotlin byte array add two
wire-byte copies. Partial acknowledgement-window assembly can add another,
as can Android Annex-B to AVCC conversion when required. Reallocations, TLS
and kernel copies are additional. The combined result adds 17 bytes of
marshalling metadata to its wire payload with no deadline, or 25 with one.
These source counts must remain separate from measured whole-app performance.

A finite process-scoped ART diagnostic of the non-debuggable Android control
attributed 64.42% of sampled managed allocation bytes to explicit JNA
Structure, Memory, marshalling and cleanup contexts. A separate native-heap
trace attributed 3.43% of native allocation bytes to actual JNA allocation
sites. These percentages describe different heaps; neither generic JNI stack
frames nor ART allocation records establish retained native storage. Full
validation, instrumentation limits and private trace receipts are recorded in
[delivery status](mobile-delivery-status.md).

The first binding experiment reuses a fixed per-thread cache of nested call
status structures, resetting fields in place after error lifting. Cache
overflow retains baseline allocation semantics so generated receiver and
argument clones are still consumed correctly. Require reproducible generation,
nested error/free and concurrent-owner tests, actual Swift/Kotlin consumers,
and controlled physical comparison. Do not increase media bounds, suppress
guards or claim sustained improvement from a synthetic helper benchmark.

Existing source entry points:

- [SRT protocol](../crates/sapflow-srt/src/lib.rs)
- [Our RTMP protocol](../crates/sapflow-rtmp/src/lib.rs)
- [HLS muxer](../crates/sapflow-hls/src/muxer.rs)
- [Fragmented MP4 assembly](../crates/sapflow-hls/src/fmp4.rs)
- forgesworn-link recording delivery worktree: crates/link-engine and crates/link-blossom
- shelter-kit: src/fetch.rs and src/store.rs
- stash-rs: src/frame.rs and src/lib.rs
- foregather-kit: crates/foregather-ffi and docs/native.md
- fray: README.md and its existing private build/design boundaries
- relayswarm-kit: Sources/RelaySwarmMedia and Sources/RelaySwarmTransport

## Physical benchmark profiles

Keep the product feature list separate from the engineering capacity tests.
Use a non-debuggable build with the intended optimisation configuration;
an internal profileable variant can expose bounded diagnostic entry points
without adding them to the distributed release. Record the installed APK,
native library, signing identity and ART compilation state. Debug-build
functional failures and synthetic timings remain labelled as debug evidence.
Android's [measurement guidance](https://developer.android.com/topic/performance/measuring-performance)
describes the performance impact and repeatable compilation setup.

| Profile | Purpose | Minimum run |
| --- | --- | --- |
| 720p30 with AAC, publishing and recording | First-release sustained correctness and efficiency | Two hours on each primary qualified phone |
| 1080p30 with AAC | Measure headroom and locate the first bottleneck | 30 minutes on capable physical devices |
| 1080p60 with AAC | Stress the SDK, binding and scheduling path | 30 minutes on capable physical devices; record unavailable hardware profiles |
| Network reduction and recovery | Bound backlog and recover useful live media | Frozen loss/RTT/bandwidth profiles plus the existing outage/handover matrix |
| Recording transfer while broadcasting | Prove later Link/storage work cannot starve the live path | Rate-limited transfer with paired broadcast-only baseline |

1080p stress results alone do not advertise product 1080p support. Record the
actual resolution, frame rate, bitrate, profile, audio format and hardware
implementation. Re-negotiate the UI/product support list only from qualified
device evidence.

For every profile compare capture/encode only, recording only, publishing only,
publishing plus recording, and optional monitoring. For B6 also compare peers
off/on. Use identical delivered media quality and the same security settings
when comparing implementations.

## Initial native performance budgets

These are proposed engineering budgets to freeze in B0 from the selected
physical device matrix. A budget is not a result. Changing a frozen budget
requires a recorded product decision and preserved failure evidence.

| Measure | Required initial budget or rule |
| --- | --- |
| Raw-frame path | No application copy of raw video across FFI/UI/JavaScript; platform surface/pixel-buffer handling is measured separately |
| Encoded ingress | At most one accounted ownership copy across the encoder-to-Rust boundary; no second payload copy just to share with recording |
| SDK processing | p95 at most 1 ms and p99 at most 2 ms of active SDK CPU per video access unit, including its attributed audio/packet work; exclude hardware codec time, deliberate SRT pacing and time asleep |
| Binding contribution | At most 10% of active SDK CPU in the matched profile; measure native conversion plus generated binding cost |
| Memory and allocation | Frozen byte/time bounds per queue/pool, no growth with show duration and no unbounded per-packet allocation churn |
| Scheduling | No network, file, signing or relay operation blocks capture callbacks; every worker/timer has an owner and cancellation result |
| Encoding | Actual hardware implementation recorded; single encode feeds publishing and recording |
| Frame delivery | No SDK-induced missed frame deadline on the nominal no-loss profile; attribute capture, codec, SDK, network and viewer drops separately |
| Power | Energy per delivered encoded minute no worse than the matched native reference beyond its measured repeat-run uncertainty |
| Thermal stability | No thermal shutdown, hidden quality reduction or sustained departure from the declared profile during the qualified run |

Measure CPU with thread-aware profiling and divide by actual processed units;
do not infer active processing cost from wall-clock time around asynchronous
pacing. Keep aggregate CPU, time distributions and work attribution together.
For audio-only mode, freeze an audio-unit/per-media-second budget in B0 rather
than applying a video-access-unit denominator to a stream with no video.
Include memory residency, native/managed allocations, GC pauses on Android,
copy counts/bytes, packet counts, retransmissions, wake-ups, queue ages,
recording writes and battery/temperature observations.

The prior generic allowance of 10% more energy than the reference is replaced
by the native-reference parity requirement above. Optimisation targets lower
energy and lower SDK overhead while preserving delivered quality and recovery.

Use Instruments on iOS and Perfetto plus the available Android allocation,
power and codec tools. Preserve release-build symbols for attribution. Run
repeat measurements on the same phone with the same brightness, screen state,
radio, charging, temperature and monitor settings; report measurement spread
and device capability limits.

The SDK processing budget does not replace the separate startup, capture-to-
screen, synchronisation and recovery targets in the delivery plan. Intentional
transport latency and avoidable application overhead must be reported apart.

## Work required before implementation can expand

B0 must produce the adoption record, copy/ownership diagram, callback/thread
map, baseline profiles and budgets.

B1 must install the actual packages on both platforms, prove the encoded
handoff and single-encode fan-out, and publish the copy/allocation/runtime
measurements. A language-level or desktop microbenchmark is insufficient.

B3 must retain those budgets while adding real encryption, retransmission,
recording and destination integration. An unencrypted sender benchmark cannot
close the production performance gate.

B5 requires both reliability acceptance and this native performance contract.
B6 and B7 must repeat the relevant budgets when adding peer delivery, storage
or wider formats; those features cannot consume the live path's headroom
without a measured, documented change.

No product claim of superior performance follows from dependency choice.
Attach the matched result to each claim and retain its supported device,
profile and network scope.

## Outperforming Larix and other native broadcasters

The owner wants better performance than Larix. Treat that as a measurable
product goal alongside reliability. Native Swift and Kotlin competitors can
use the same hardware encoders; Rust or fewer features alone do not prove an
advantage. Keep baseline parity as the release floor and report the superiority
goal separately until measured improvement exists.

Proposed improvement targets for B0 to freeze after the first matched baseline:

| Measure | Improvement target against Larix |
| --- | --- |
| Energy | 20% lower whole-device energy per delivered minute at matched media quality and screen/radio state |
| Memory | 25% lower peak proportional/resident process memory using the platform's consistent metric, with no duration-related growth |
| Recovery | 25% lower p95 time from usable-path restoration to decoded remote media on the frozen impaired-network profiles |
| Delivery and quality | No regression in delivered frame cadence, A/V sync, recording integrity, startup or capture-to-screen delay; retain the absolute release requirements |

These percentages are proposed targets, not measurements or promises. Do not
trade lower resolution, bitrate, stabilisation, delivered media or a smaller
retransmission reserve for an unlabelled efficiency claim. Count attempts that
fail to deliver rather than excluding them from the energy denominator.

Use current Larix on both platforms, Moblin on iOS and IRLPro on Android. Pin
versions and settings in the receipt, including the encoder implementation,
codec/profile, keyframes, SRT latency/security, recording, preview, monitoring,
stabilisation and overlays. Use the same origin and viewer for app comparisons;
compare origin choices in a separate experiment. A missing matched feature
limits that comparison rather than becoming a benchmark win.

Run at least five paired nominal runs of at least 30 minutes per primary phone,
alternating/randomising app order and returning to a controlled thermal state.
Retain the separate two-hour qualification runs. Report individual runs,
measurement uncertainty and confidence intervals for aggregate comparisons.
Use enough repeated recovery events to estimate p95; five events are not a
useful tail estimate. Describe ambient conditions and the actual energy
measurement method. Battery percentage alone cannot establish a small win.

Report whole-device and attributed app/SDK costs together where tooling allows;
do not subtract an idle baseline from one app only. For loss, handover and
bandwidth-collapse profiles include delivered seconds, late/drop counts,
blackout duration, retransmitted bytes and time spent at reduced quality.

Freeze both a simple contribution profile and a representative creator profile.
Keep optional production features off in all apps for the former, and match
them for the latter. An overlay-free result cannot support a claim about a
feature-heavy IRL show. Publish the reproducible profile and failures with any
claim of superiority, scoped to the qualified devices and app versions.
