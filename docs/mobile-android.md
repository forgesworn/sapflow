# Android native capture adapter

`mobile/android` is the standalone Kotlin/Compose application consuming the
actual generated Sapflow AAR. Its initial qualified profile is still pending:
the implementation requests rear-camera 720p at 30 fps, hardware H264
baseline without B frames, a one-second GOP, and mono 48 kHz AAC-LC at 96 kbit/s.
The orientation candidate uses upright 720x1280 portrait or 1280x720 landscape
coded pixels, with a bounded GPU surface transform and no rotation-tag dependency.
The selected platform encoder names are visible. Unsupported camera timestamp
sources, surface combinations or hardware profiles produce explicit errors;
there is no software-video fallback.

## Manual destination profiles

The normal manual controls support explicit save, load and delete for up to 16
destinations, plus [reviewed version 1 JSON file import](manual-destination-profiles.md).
The local collection uses a separate Android Keystore key and AES-GCM encrypted
document in app-private no-backup storage. A missing existing key, damaged file
or unconfirmed write fails visibly; it does not reset the collection.

Document reading and storage run away from capture callbacks and UI work.
Import presents a redacted host/port/security review with independent plaintext
permission before Apply. Applying only fills the manual fields; Save, Prepare
and Start are separate actions. The original action generation and actual
capture availability fence stale replies across edits, foreground transitions
and preparation. V4V configuration and scoped grants are outside this format.
Physical document-provider and Keystore acceptance remain open.

## Build

The parent native-package build must first produce
`target/native-packages/android/sapflow-0.0.0.aar`. The app uses that AAR and
JNA 5.19.1, with metadata checks enabled. Build from the repository root:

```sh
mobile/android/build-debug.sh
```

A development-only device package can be selected with
`-PsapflowAar=/absolute/path/to/a/real-device-package.aar`. The override must be
absolute and must contain the real generated bindings and matching native
library from the same source. A device-only APK is not the complete four-ABI
SDK archive or a general-release artefact.

Gradle 8.13 is pinned with its distribution SHA-256. AGP 8.13.0 and Kotlin plus
the matching Compose compiler 2.2.20 use the existing house toolchain cache.
Compose BOM 2024.12.01 and Activity Compose 1.9.3 match the proven native house
dependency set. The app targets API 36 and requires API 29 to inspect actual
hardware-encoder capabilities. Build output and project caches belong under
`target/mobile-android`; the debug APK is
`target/mobile-android/app/outputs/apk/debug/app-debug.apk`.

The capture/SDK-only baseline is preserved separately at
`target/mobile-android/capture-baseline-debug.apk`, SHA-256
`3dd51a6d820d122fe297fa2368551fb135ef842828d6521b30a60f2943ed8fde`.
Its debug assembly, eight JVM tests and lint passed with zero errors and five
warnings. This is compilation evidence; it does not establish a physical run
duration or acceptance. The manual RTMPS integration is a subsequent build.

The manual RTMPS debug APK compiled against the real generated arm64-device
AAR, SHA-256
`71da5a1af89703a4519ed38906ca0194dd38fc001933a51b9fcf10fc36724431`.
Its APK SHA-256 is
`245614ac8861e6d5f44e1533e786ff3b81c30ff1f0677afe0167f176c5c22c9f`.
All 16 JVM tests passed, including explicit-start cancellation and bounded
cleanup with a stalled writer. Lint passed with zero errors and the same five
warnings as the baseline. The single-use Gradle process exited after the build.
This arm64 development build is separate from
the complete SDK archive and from physical camera-to-origin acceptance.
The recording/deadline candidate is preserved at
`target/mobile-android/recording-deadline-debug.apk`, SHA-256
`0b886f7969476ff4c3e6210cf8d80a952f82b3335b03143a7267590b4875b63e`.
Its actual assembly, 20 JVM tests and lint passed. The subsequent SRT/recording
candidate is `target/mobile-android/srt-recording-debug.apk`, SHA-256
`bec445d08e3805fb2734eb8e8fff2bd5a3a709fdc02ca46824993ff1a9fb07d1`.
It passed actual assembly, 27 JVM tests and lint with zero errors and five
existing warnings. Both consume real arm64 bindings/native code from AAR
SHA-256 `bb753fa14e0d39949dad4fb1aed206e8b63495d8d4d0dba42711fd1177ee0399`,
including recording, RTMP deadlines and microsecond SRT APIs. Their named JSON
receipts sit beside the APKs. These builds establish neither physical Android
storage behaviour nor physical encrypted SRT delivery. The corrected encoder-
clock build is preserved at `target/mobile-android/monotonic-clock-debug.apk`,
SHA-256 `4cbeace64f576a59fd7aa8762de4e7ef8d4971b3988fc9b0464df87da3cbd65d`.
It passed actual assembly, 28 JVM tests and lint with the same five warnings;
`monotonic-clock-receipt.json` records the matching AAR and clock regression.
Its physical recording, capture-age and SRT checks remain pending.

The subsequent queue-accounting/recovery diagnostic candidate is
`target/mobile-android/queue-recovery-debug.apk`, SHA-256
`a957fe5c78a377beae41fc4aa3f60420cad73e35b8fa16161653e62716f5555f`.
Actual assembly, 31 JVM tests and lint passed with zero errors and the same
five warnings. Its native library is byte-identical to the refreshed real
arm64 AAR, SHA-256
`5f082221a9ec0dc5fa6230206f80654417f14fd1e212b1ed3314a05b4e57ced6`.
`queue-recovery-receipt.json` records queue bounds, diagnostic scope and build
gates. Focused Rust validation covered 37 client and 29 FFI passing tests,
with three external-fixture tests explicitly ignored; strict Clippy passed.
This candidate preserves the two-second socket write watchdog and needs a
physical run to identify the original stall trigger and qualify recovery.

The Kotlin drainer-throughput candidate is preserved at
`target/mobile-android/drain-throughput-debug.apk`, SHA-256
`159643b2fab9988e0e896014e8e8f61dacbc04cb685c46eebf769a5981f96f6b`.
It passed actual assembly, 37 JVM tests and lint with zero errors and the same
five warnings, against the unchanged verified arm64 AAR above. All 33 Kotlin
source hashes were rechecked after coordinated shared-index restoration;
`drain-throughput-receipt.json` and its source-hash manifest retain the build
context. Rust/native APIs and watchdogs were unchanged. Its early 177-second
publisher snapshot had no resets, but the later ten-minute gate failed as
described below; the early snapshot was not sustained acceptance.

The bounded-batch candidate is preserved at
`target/mobile-android/bounded-batch-debug.apk`, SHA-256
`60819df1a94bfa964568724842c5c72c7ab01a6f88bf36435f6ffbe822308740`.
Its actual assembly, 42 JVM tests and lint passed. A parent-observed physical
run stopped after approximately 12m35s: the 10m50s checkpoint reported zero
publishing resets, zero recovery IDR requests and 277ms queue high water.
That is finite queue evidence only: the same run failed A/V synchronisation.
Independent live-part probing found video PTS approximately 493–501s while
audio was approximately 465–470s. PCM read throughput and hardware timestamps
were not observed in that frozen candidate, so a microphone overrun remains a
source-supported diagnosis rather than a measured hardware event.

The orientation/audio candidate is frozen at
`target/mobile-android/orientation-audio-debug.apk`, SHA-256
`f90e729a390bdc9b991a250d08f909638d425415a32b91c6e6ae22ff8bf81d57`.
Actual offline assembly, all 68 JVM tests and lint passed: zero errors and six
warnings (the five existing warnings plus the debug benchmark's untranslated
label). The native library remains byte-identical to the verified arm64 AAR
above. `orientation-audio-receipt.json` and `orientation-audio-build.log` retain
the gates. All 61 app-source/build inputs were hashed before the final build,
rechecked unchanged afterwards and preserved in
`orientation-audio-source.tar.gz`, SHA-256
`33a78fa2111ef8332a45bd6b2d57dbf21b5e729d436517661ae2f1df4379d171`.
The source archive excludes Gradle caches, generated artefacts and credentials.
The APK includes the parent's debug-only offline FFI benchmark activity; that
entry point is absent from the release manifest. These are development build
receipts, with physical orientation/audio acceptance still pending.

The free build has no billing SDK, trial timer, advertisements, proprietary
capture SDK or Google Play services dependency. Android/AndroidX open-source
components are used. Free V4V remains visible and available in every build;
destination discovery and V4V publication still require the shared destination
adapter to be attached.

## Capture, ownership and workers

Camera2 sends native camera buffers to one SurfaceTexture/OES input owned by
an EGL worker. That worker applies the validated rear-sensor/cardinal-pose
transform to the hardware MediaCodec input surface and the same-frame preview.
It letterboxes the preview and refuses an incompatible camera-input aspect.
No raw video enters a Kotlin byte array, Bitmap or FFI; no glReadPixels path
exists. A dedicated AUDIO-priority reader continuously captures PCM16 into eight
fixed direct blocks. A separate AUDIO-priority AAC pump copies each block once
into a MediaCodec input buffer and computes the RMS meter. PCM stays on Android.

Video callbacks and all fanout operations use one owned HandlerThread. AAC
configuration, audio-clock validation and Rust sample construction belong to a
separate AUDIO callback owner. That owner releases the MediaCodec output buffer
before posting an immutable sample handle. A coalesced handoff holds at most
16 shared handles/64 KiB across queued and borrowed ownership, with a 250ms
residence limit and at most eight consumers per handler turn. It retains no
platform output buffers or second compressed payload copy. A serial AUDIO
quiescence barrier transfers descriptor ownership for final cleanup; release
waits both audio workers, the barrier and all handoff owners. Input indices
have a fixed 32-element bound. The PCM pool adds exactly 16 KiB across reader,
ready queue and pump ownership, with a 200ms capture-age limit and no ring
increase. A compressed direct
ByteBuffer slice is borrowed synchronously by the generated Rust codec call;
Rust makes the one ownership copy into an immutable sample. AAC requires no
per-sample conversion buffer. For Annex-B H264, the adapter first performs one
additional framing-conversion copy into a reusable direct buffer capped at
4 MiB. Existing AVCC can be borrowed directly. Repeated SPS/PPS are stripped
from Annex-B samples and carried by the validated descriptor. Configuration
bytes are small separate control copies. No end-to-end zero-copy claim is made.

Rust's NativeSampleFanout shares the same immutable payload between its
4 MiB publishing and 8 MiB recording queues, each bounded to two seconds.
Media duration is tracked independently for audio and video and bounded by
their maximum, so interleaved A/V has approximately two seconds of capacity.
The previous sum counted concurrent tracks twice and could reset publishing
after approximately one second of A/V backlog. Byte, sample-count and
two-second scheduler-residence bounds remain unchanged. Independent tests
cover approximately two seconds of interleaved A/V, audio-only and video-only,
per-track draining, overflow and cancellation accounting.
Every fanout call runs on the codec handler, preserving the FFI's monotonic
clock order. A consumer worker owns one fixed batch of at most eight shared
sample handles in addition to those Rust queues. A publishing batch is capped
at 4 MiB and a recording batch at 8 MiB; these are separate ownership bounds
from the unchanged native queues. Collection cannot wait for more samples and
does not copy compressed payloads. Only one sample is borrowed by the sink at
a time; cancellation closes the batch's unconsumed remainder. There is no
additional unbounded staging queue. Native queue duration/residence bounds do
not establish combined queue-plus-batch or end-to-end latency. The
encoder output buffer is released in `finally`, before a consumer can stall
on disk or networking. UI snapshots and the meter refresh at 5 Hz. Rejection
counters distinguish video timing/initial-IDR, AAC priming, publishing queue
waiting for IDR and other queue rejection. They count ingress rejection,
not network loss or origin/player delivery; queue evictions and the publishing
sink's preacceptance drops require separate accounting.
The UI now labels the old queue rejection counter as **Other ingress rejects**.
Separate five-Hz diagnostics expose current publishing bytes/media duration,
high-water marks, all discarded queue samples and reset counts by byte,
sample-count, media-duration or residence cause. Fixed-size latency counters
report publishing worker dispatch, consume and handler-return delays, plus
RTMP lock wait, socket-write duration and native wire bytes. Delay maxima and
counts of operations at least 100 ms survive short stalls between UI samples;
there is no per-packet logging or unbounded history.
The subsequent drainer fix also reports timing sample counts and mean
microseconds. Low maxima alone do not prove sufficient sustained service rate.

Entering wait-for-IDR after a runtime queue reset posts one sync-frame request
on the codec handler. Requests are generation fenced, coalesced and limited
to one per 500 ms; initial startup wait does not request an extra IDR. The
encoder request is not proof that an IDR has appeared. The two-second socket
write watchdog and native stale-wire deadlines are unchanged. Diagnostics are
needed to distinguish slow IO, lock contention and handler scheduling before
claiming the physical stalls are resolved.
For the next physical run, retain startup and periodic diagnostic snapshots.
If resets recur, correlate their reason with consume/write/lock and handler
maxima rather than treating a sampled zero-byte queue as proof it never filled.
Decode the independent local recording and compare its frame/packet timestamps
through each origin gap to distinguish capture loss from publishing loss.

The queue-recovery candidate failed the physical steady-state gate. In the
reported 67-second observation, current publishing backlog was 219,439 bytes/
666 ms, high water 642,237 bytes/1,983 ms, with 466 dropped samples and three
media-duration resets. Write maximum was only 3 ms, lock maximum 7 ms and
consume maximum 123 ms; origin timestamp gaps were approximately two seconds.
These are finite observations, not sustained performance acceptance.

The old drainer kept its admission flag set after worker completion until a
per-sample handler continuation ran. Fresh capture callbacks therefore skipped
draining even when the worker was idle. Admission now becomes available as
soon as the worker closes its sole sample owner. Both fresh ingress and a
single coalesced handler continuation claim admission atomically; a stale
continuation cannot pop a second handle. This removes the mandatory handler
return trip from the next ingress's progress path without adding a media queue.
One explicit handoff owns an admitted handle awaiting its worker; cancellation
closes it if its task is removed before execution. Pure tests hold the handler
continuation indefinitely while processing 1,000 ingress/completion cycles,
and cover claim races, stale wake-ups, idle wake-up, stop and owner cancellation.
The change still requires physical throughput and local-recording validation.

At approximately 611 seconds that single-handle drainer had 47,169 ingress
samples and 45,369 publishing consumes, current backlog 1,066 ms, high water
1,999 ms, 11 resets (nine media-duration, two residence), 11 recovery requests
and 1,720 drops. Mean consume, dispatch and handler-return costs were 8,255,
259 and 6,157 microseconds. Socket writes averaged 220 microseconds with a
10 ms maximum. These finite observations identify inadequate aggregate
scheduling throughput despite low individual maxima; they do not implicate
network write latency.

The bounded batch revision collects at most eight existing shared handles on
the codec handler and sends them in FIFO order on the same sink worker. It
does not wait to fill a batch. The next handler continuation occurs once per
batch. One batch may be admitted per sink; queued and active batch cancellation
close every unconsumed handle while the currently borrowed sample stays owned
by its worker until that call returns. Graceful recording stop processes its
accepted batch before the remaining native queue and final file rewrite.
Diagnostics show batch count, mean/maximum size, retained handles/bytes and
batch service time while retaining per-sample consume timing.

An independent 600-second virtual scheduler using those measured costs needs
approximately 1.366 samples per handoff to keep up with 77.2 ingress units/s.
The batch helper model averages approximately 1.366 (a mixture of one and two)
with less than 26 ms peak backlog; the single-sample model falls behind.
This is deterministic model evidence, not an Android throughput result.
Unit cases also cover eight-handle admission, ninth-item retention in the
source queue, FIFO order, cancellation during a borrow, queued cancellation,
sink/cleanup failure and byte bounds. Native queue settings and protocol
watchdogs are unchanged, and physical steady-state acceptance remains open.

The fixed 30 Hz profile quantises camera presentation times onto 90 kHz frame
slots; duplicate slots are counted and dropped. Baseline encoding excludes
B frames, so DTS equals PTS. AAC uses 1024-sample units on its 48 kHz clock,
anchored to the first encoder output timestamp. Negative AAC priming timestamps
are discarded and counted rather than passed into the non-negative SDK timeline.
The selected camera currently requires a realtime sensor timestamp source, but
that does not determine encoder-surface timestamps. Android's default
MediaCodec output-surface timebase is MONOTONIC. Capture epoch, microphone
input anchoring and signed video age therefore use `System.nanoTime`, while
Rust queue residence and transport deadline clocks remain separate.
The earlier frozen APKs incorrectly mixed an `elapsedRealtime` epoch with
encoder PTS; accumulated suspend time can cause pre-epoch video rejection,
A/V offset and misleading age. A pure regression simulates a 16-second
BOOTTIME/MONOTONIC offset. Synchronisation, orientation, older supported
platforms and long-run clock drift still require physical measurement.
An independent native clock probe on the selected physical Android device
measured `CLOCK_BOOTTIME - CLOCK_MONOTONIC = 1,935,120,157 ns`, with a
312 ns sampling span. This is one finite clock-offset observation, not an
encoder-PTS, recording-sync or latency measurement. It confirms that the two
clocks cannot be treated as interchangeable on that device; it does not
explain the owner's reported 16-second playback delay by itself.
See [the platform output timestamp contract](https://developer.android.com/reference/android/hardware/camera2/params/OutputConfiguration#TIMESTAMP_BASE_DEFAULT).
The UI exposes signed encoded-video capture age. It separates encoder/callback
freshness from SDK, transport and player latency; it is not glass-to-glass latency.

A source-only clock audit after that probe found no remaining subtraction or
comparison between encoder PTS and BOOTTIME in the Android capture/publisher
path. Fanout enqueue/pop/residence use BOOTTIME consistently. RTMP startup,
native feed/tick and absolute-deadline calculations consistently use BOOTTIME
milliseconds, independently of rebased sample timestamps. SRT offers, socket
write bounds and native protocol calls consistently use MONOTONIC microseconds.
Recording derives file timing from the rebased sample timeline. Relative JVM
waits/timers are separate scheduling mechanisms; suspension behaviour must be
requalified if background capture is introduced. This audit did not execute
capture or alter the frozen APK.

## Sink seam and truthful status

`CaptureController.publishingSink` and `.recordingSink` accept `EncodedSink`
implementations before preparation. `consume` runs on separate owned workers,
with one borrowed immutable sample at a time from the fixed batch. It must complete within
the consumer's transport/storage bounds. `close` must promptly cancel pending
IO. The handle is borrowed for that call and is closed immediately afterwards.
An adapter needing asynchronous retention must define and enforce its own
bounded ownership contract; it must not introduce an unbounded queue.

Leaving the manual destination blank attaches no publisher. It performs real
capture, Rust sample validation, shared fanout and bounded draining, then
releases publishing handles. The UI says **SDK validation only** and **No
publisher attached**. A manual RTMPS application URL and stream key attach
`RtmpPublishingSink`, using the generated `NativeRtmpPublisher` and our own
RTMP implementation. Plain RTMP requires the separate development checkbox;
there is no fallback from TLS. The stream key stays in memory and is hidden
in the input field. URL userinfo, query strings and fragments are refused.
Preparation creates a dormant destination. Only the explicit Start action
constructs the RTMP connection, starts its startup deadline and begins network
IO; encoder sample arrival never implicitly starts a destination.

The platform TLS provider checks the normal certificate chain and the original
destination hostname using HTTPS endpoint identification. No trust manager
override exists. Cancellable platform DNS has a three-second deadline, TCP
connection has a three-second total address budget, TLS has five seconds,
and each write has a two-second socket-close watchdog. A network reader and
deadline worker are owned by that one attempt. Rust calls and socket output
are serialised outside capture callbacks, with at most one bounded wire output
buffer in flight. Prepublication media is dropped, and sending waits for a
fresh IDR after acceptance. The destination coordinator creates fresh attempts
when the shared reconnect policy permits; it never reuses a stopped attempt.

The UI reports **Origin accepted ... media delivery unconfirmed** after
Publish.Start. It does not invent an origin/player acknowledgement. A failed
network attempt drops publishing samples while capture and independent local
recording can continue. A recording queue/sink failure closes recording alone
and leaves publishing running. The single-attempt transport adapters sit behind
the shared session grace/reconnect coordinator described below. Its trusted
origin-media observation remains a separate V4V integration gate.

The sender enforces the negotiated Hard/Soft/Dynamic outstanding-byte window,
validates cumulative wrapping acknowledgements and retains partial wire output
within its byte budget. A partial take adds one accounted wire copy. Native
adapters must drive the absolute `nextDeadlineMs` with one real timer. Android
ticks before each send and expires still-owned media wire output after one second,
even if small acknowledgements continue. Socket writes have their separate
two-second deadline. These bounds do not establish one-second end-to-end latency.

The SRT destination option attaches `SrtPublishingSink` to our own Rust SRT
caller. The current scope is a numeric unicast IPv4/IPv6 socket address with an
explicit port and separate stream ID; DNS, multicast and URL query credentials
are refused. AES-256 is the default and requires a 10–79-byte UTF-8 passphrase.
Plaintext requires the explicit lab option and an empty secret. SRT encryption
does not establish server identity in the way validated TLS does.

Preparation is dormant. Explicit Start opens a connected nonblocking
DatagramChannel and waits at most 15 seconds for codec configuration. Its
Selector worker owns all SRT calls at actual microsecond precision. It has at
most one borrowed sample offer, one outgoing datagram of at most 1500 bytes,
16 bounded steps per turn and one native-deadline timer; there is no idle
polling or additional media queue. A zero-byte socket write enables writable
readiness and a 100 ms deadline. The native configuration uses 120 ms latency,
128 packets, 4 MiB, one-second packet/sample residence and a 1,250,000-byte/s
bandwidth ceiling. These separate bounds do not sum to a one-second latency
guarantee. Prehandshake samples are dropped and sending starts at a fresh IDR.
Failure ends that attempt while independent recording continues. Connected
means handshake completion; media reception remains unconfirmed.

Private MP4 recording is a separate explicit checkbox, enabled initially.
`PrivateRecordingSink` consumes the same immutable encoded handles through
`NativeRecordingMuxer`. Only its worker creates a unique exclusive file under
the app's private `files/recordings`, writes bounded mux output and performs the
final fixed-size duration-header rewrite, force and close. Preparation performs
no recording IO. Fragment assembly and generated owned-byte transfer have their
own accounted copies; recording never creates a second encoder or payload
staging queue. Disabling recording truthfully reports that no file is written.

Normal Stop drains the accepted Rust recording queue before finalising on its
worker, with a three-second grace and one-second abort wait. A recording failure
drains its accepted prefix asynchronously, keeping clocked queue pops on the
codec handler while file IO stays on the recording worker. Cancellation closes
the channel to interrupt pending IO; a failed/deadline-limited file is reported
as a retained partial prefix. Physical storage-full and interruption acceptance
remain open. Recreate closed sink adapters before preparing another session.

## Interruption and evidence gates

Camera/microphone permissions are requested according to the selected intent
before preparation. Leaving the Activity stops video and any audio-only show
whose Start has not committed. An explicitly opted-in, already committed
audio-only show can continue through the non-exported microphone foreground
service while its authorised transport connects or reconnects, after notification
and microphone authority are rechecked. Losing the video preview surface, camera
disconnection, microphone silencing, codec/configuration rejection or
shared-ingress failure stops the exact session. Resumption requires preparation
and an explicit Start; the service is `START_NOT_STICKY` and reconstructs no show.
When the exact capture stops, cleanup stops the camera, audio input and codecs,
cancels Rust queues, closes sinks and immutable handles, and joins its owned
workers. Activity destruction detaches the UI; only an eligible committed
audio-only capture remains owned by the application/service.

Unit tests cover independent Annex-B/AVCC fixtures, parameter-set extraction,
borrowed direct views, buffer reuse, priming/cadence/restart timing, strict
destination parsing and hostile length/framing limits. APK
assembly proves packaging/compilation only. Signed release packaging, installed
no-Play GrapheneOS execution, hardware selection, native buffer behaviour,
timestamp correctness, interruptions, actual origin/file output and the
two-hour performance/battery/thermal profile remain separate acceptance gates.

Implementation references:
[Camera2 simultaneous surfaces](https://developer.android.com/media/camera/camera2/multiple-camera-streams-simultaneously),
[MediaCodec lifecycle and buffers](https://developer.android.com/reference/android/media/MediaCodec),
and [matching Kotlin/Compose compiler setup](https://developer.android.com/develop/ui/compose/setup-compose-dependencies-and-compiler).
TLS references are the platform's
[security guidance](https://developer.android.com/privacy-and-security/security-ssl)
and [endpoint identification API](https://developer.android.com/reference/javax/net/ssl/SSLParameters).

## Independent protocol evidence

The native wrapper's five Rust tests cover typed configuration, common-epoch
audio/video offsets, composition offsets, gating, continuity, configuration
identity and bounded retry after saturation. The underlying publisher has
eighteen wire/state tests. An independent plaintext receiver check is repeatable:

```sh
cargo build --offline -p sapflow-hls --example rtmp-publish -j 2
python3 tests/rtmp-publish-interop.py
```

The black-box ffmpeg listener received 180 H264 frames and 283 AAC frames from
a 5,962,135-byte fixture; codec/rate checks and complete decoding passed. The
driver and decoder are separate implementations. ffmpeg's initial Dynamic
bandwidth limit has no preceding Hard and is ignored as the specification
requires. The harness therefore explicitly injects one independently specified
2,500,000-byte Hard control after S0/S1/S2 through a bounded loopback proxy.
The effective sender window was 2,500,000; the receiver cumulatively acknowledged
5,119,878 bytes. A separate uninjected run also decoded successfully with over
5 MB acknowledged, and an own-origin test governs a transfer exceeding 6 MB.
The harness removes its generated media, proxy threads/sockets and receiver
process even on failure. This is RTMP interoperability evidence, not TLS,
physical camera, destination authorisation or sustained-performance acceptance.

The strict governor gives no control-message exemption. Tiny windows, ACK wrap,
forged/future ACKs, saturation and ACK stalls are covered by independent wire
cases; stalls fail on a driven deadline or bounded backpressure. The own origin
now advertises initial Hard and counts pre-negotiation bytes in its first ACK.
See [RTMP section 5.4.5](https://rtmp.veriskope.com/pdf/rtmp_specification_1.0.pdf).

`mobile/android/qualification/srt-loop.py` compiles the exact production SRT
Selector/offer/parser sources against the real generated AAR classes and JNA,
then loads the matching host Rust SDK. An independent libsrt receiver accepted
AES-256 output; ffmpeg completely decoded 180 H264 frames and 282 AAC frames.
One of 283 offered AAC samples preceded the first IDR and was intentionally
dropped. The harness removes its temporary media/classes and receiver process.
This qualifies the production Kotlin protocol loop on the host JVM; Android
UDP/runtime, camera, storage and physical encrypted delivery remain separate.

## Next capture controls: bounded capability plan

Further controls wait for the first physical recording/capture-age check.
No additional controls below are implemented or exposed by the current UI.

1. Add bitrate and manual IDR requests on the codec handler using
   `PARAMETER_KEY_VIDEO_BITRATE` and `PARAMETER_KEY_REQUEST_SYNC_FRAME`.
   Validate bitrate range/mode first, and distinguish requested values from
   observed throughput/keyframes. These need no new Rust API. An SPS/PPS
   change requires a new configuration epoch, recording file and sink attempt.
   [MediaCodec parameters](https://developer.android.com/reference/android/media/MediaCodec).
2. Offer only the intersection of camera size/exact FPS, encoder surface,
   size/rate/alignment, profile, bitrate range and bitrate-mode support.
   Current selection checks 720p30 baseline but requests CBR without probing
   its support; this must be corrected before offering profiles. Keep no-B-frame
   baseline first; other profiles require native descriptor and destination
   qualification. OEM performance estimates are not sustained-FPS proof.
   [Video capabilities](https://developer.android.com/reference/android/media/MediaCodecInfo.VideoCapabilities),
   [encoder modes](https://developer.android.com/reference/android/media/MediaCodecInfo.EncoderCapabilities).
3. Probe front/rear cameras individually, including orientation, AF modes and
   timestamp behaviour. Recreate the camera session against the encoder surface,
   fence old callbacks and request an IDR. Preserve the media clock across a
   supported switch; configuration changes instead require a new attempt/file.
   A seamless switch needs physical qualification.
4. Audio-only mode now freezes its track intent before Prepare, skips camera,
   preview and camera permission, sets fanout `hasVideo=false`, and avoids IDR
   gating in both sinks. Own-origin HLS supports explicitly declared AAC-only
   topology; native recording and receiver source paths are implemented. Physical
   microphone, complete-show retention and receiver acceptance remain separate
   qualification gates; see [audio-only mobile delivery](audio-only-mobile.md).
5. The first background slice now moves only opted-in committed audio-only capture
   into an application-owned, non-exported microphone foreground service with an
   exact notification Stop action. A production-used cold lifecycle driver keeps
   duplicate commands, promotion failure, retained cleanup, notification teardown
   and stale successors ordered. An unpromoted service stops promptly while the
   application retains exact cleanup ownership; a successfully promoted service
   keeps its notification until closure. Physical privacy-toggle, screen-off, service
   kill, network-switch and storage-finalisation qualification remains open.
   Android's while-in-use restrictions remain applicable; no Google runtime is
   needed.
   [Foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types),
   [background-start restrictions](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start).

## Upright pixels and bounded orientation epochs

The orientation candidate handles all four cardinal poses relative to the
natural device orientation. Prepared preview does not start a publisher or
recording. A pose change while prepared rebuilds the native canvas; during
capture it stops the old camera/GPU producer, drains a maximum 500ms encoder
EOS tail, finalises the accepted recording prefix, and constructs fresh codec,
recorder and publisher objects. Each recording has a new private filename;
immutable codec descriptors and transport epochs never change in place.
Latest pose requests coalesce under the original six-second restart deadline.
Stop, foreground interruption, cleanup timeout and recording finalisation
failure cancel automatic resumption. A pose change during an inhibited initial
preparation can require explicit Prepare again; it does not invent a new
cleanup deadline. Storage errors remain visible independently of publishing.

GPU stop/close callbacks report actual detach/release only. An independent
watchdog reports stalled native driver calls; it cannot forcibly complete an
uninterruptible driver operation. Borrowed encoder surfaces remain owned until
real GPU detach. Unfinished cleanup retains owners and prevents a new epoch.
Transient abandoned preview surfaces are separate from encoder/context errors.
The four-pose decoded-pixel, aligned-preview, live-turn, Stop-during-turn,
background-during-turn and storage-failure matrix remains physical acceptance,
not a JVM or compilation claim. See [the design](mobile-android-orientation.md).

API33+ requests `OutputConfiguration.TIMESTAMP_BASE_MONOTONIC` explicitly for
SurfaceTexture input. API29–32 requires a REALTIME sensor source and a bounded,
bracketed BOOTTIME-to-MONOTONIC mapper. This differs from the old direct encoder
surface default; raw SurfaceTexture sensor timestamps must not enter the media
epoch unchanged. GPU presentation and AudioRecord hardware timestamps use the
same MONOTONIC epoch. Queue residence and network deadlines retain their own
existing elapsed-realtime clock contract.

## Audio capture safety and diagnostics

The frozen batch implementation timestamped AAC input from the count of PCM
frames successfully read, while waiting for encoder indices before each read.
If input callback starvation allowed AudioRecord's capture ring to overrun,
that scheme could compress missing real time into apparently contiguous AAC.
The new path isolates AAC availability/pump scheduling at AUDIO priority and
validates each read cursor against `AudioRecord.getTimestamp` using
`TIMEBASE_MONOTONIC`. It maps the actual first PCM frame through the hardware
frame-position/time pair and subtracts the shared capture epoch. Before-read
checks refuse a cursor beyond the actual `bufferSizeInFrames`; stale,
unavailable or regressing hardware timestamps and uncertain positions fail
explicitly. There is no silence insertion, stale-PCM retiming or larger-buffer
substitute. AAC output keeps the exact 1024-frame cadence with the existing
discontinuity guard.

Coalesced five-Hz diagnostics separately show PCM frames read, AAC input frames
and PTS, raw AAC output count/PTS, validated AAC count, capture elapsed time,
hardware frame position, unread backlog/capture capacity, timestamp age,
input-index wait maximum, read maximum and encoded callback-handler delay.
Tests cover hardware mapping, sustained 48k sample arithmetic, before-read
starvation/overrun, stale/regressing timestamps and atomic output ownership
across cancellation. A successful fail-fast check is a safety result, not
proof of sustainable audio throughput. Physical acceptance must independently
show approximately 48,000 PCM frames and 46.875 AAC access units per second,
bounded hardware backlog and stable A/V PTS across a sustained run and turns.
The Android hardware timestamp contract is documented in
[AudioRecord](https://developer.android.com/reference/android/media/AudioRecord#getTimestamp(android.media.AudioTimestamp,%20int)).

## PCM pool and failure cleanup correction

The orientation/audio candidate above failed its physical safety gate after
approximately 20 seconds. The last good UI snapshot reported 964,608 PCM/input
frames, 937 AAC output units, 20,147ms elapsed and an estimated hardware backlog
of 374/4096 frames; it was not the failing hardware observation. Maxima were
57,113us input-index wait, 91,009us AudioRecord read and 127,383us encoded callback
handler delay. The exact failing frame-position/time pair was missing, so this
does not establish definite ring overwrite. Hardware position can include
capture-pipeline latency. The conservative error correctly labelled uncertain
position as well as possible overrun. The origin prefix independently decoded
as 720x1280 with 546 video frames and 849 AAC units over approximately 18.2s;
no asymmetric chart was used, so upright four-pose acceptance remains open.

The next source packet separates continuous microphone reading from AAC input
availability. Exactly eight 1024-frame direct PCM blocks (16 KiB total) cover
all reader, queued and encoder-pump ownership. The reader does no MediaCodec,
JNA, RMS or input-index waiting. The separate AUDIO-priority pump waits for AAC
indices and performs one explicit platform-only PCM copy into the codec input.
The AudioRecord ring is unchanged (4096 frames on the measured phone). Blocks
retain hardware-mapped first-frame timestamps and fail at 200ms capture age;
exhaustion, stale PCM and uncertain hardware position never insert silence or
move old PCM to the present. Normal Stop may drain queued blocks under the same
age bound before encoder EOS; failure cancels the fixed pool. This cannot
promise survival of ART pauses beyond the native capture ring: a dedicated
Java reader still participates in runtime suspension.

Hardware safety failures now retain both exact before/after read cursors,
frame positions, timestamp/observation nanoseconds, counts, capacity and epoch.
The hot path stores primitives; diagnostic strings are formatted on failure,
with normal status coalesced at five Hz. Approximate ART GC counts, GC time,
blocking-GC counts/time and allocated-byte totals are sampled only at start
and failure. These process totals can correlate runtime pressure but do not
identify an individual stop-the-world pause.

The same failed run exposed a separate concrete cleanup race. Microphone
finally could post completion before deferred Stop; Stop then overwrote that
fact because its non-null Thread reference remained. No later completion could
unblock teardown, so the six-second fence expired and the origin eventually
closed on idle timeout. Completion facts are now monotonic across reader/pump
finally and deferred Stop, with deterministic ordering regressions. Failure
also aborts the publisher on an independent control thread before microphone
or GPU cleanup gates. Exact owner flags and the current control-cleanup phase
remain visible; Prepare is disabled while owners remain. This does not fake
GPU/codec release, and an actually blocked owner still prevents another epoch.

The PCM/cleanup candidate is frozen at
`target/mobile-android/pcm-pool-cleanup-debug.apk`, SHA-256
`34642ff2ff414bad5bee5199b9baa05cd929783a965a0a143c626ed5d7e5c6ef`.
Actual offline assembly, all 75 JVM tests and lint passed with zero errors and
the same six warnings. The native library is byte-identical to the unchanged
verified arm64 AAR. `pcm-pool-cleanup-receipt.json` and
`pcm-pool-cleanup-build.log` retain the checks. All 65 app-source/build inputs
were rechecked unchanged and archived in `pcm-pool-cleanup-source.tar.gz`,
SHA-256 `31f43a3b6993e0045289aeb6dd4b02b237d6bb7fe8b44aec70b0c258bf0c98bc`.
The fixed-pool tests cover a virtual 127ms delayed encoder, ordered unchanged
PCM timestamps, total ownership capacity, cancellation and 200ms stale-data
refusal. Completion tests cover the exact finally-before-deferred-Stop ordering
and both worker completion orders. These are source/build safety gates only;
sustained physical capture, stop ownership and four-pose acceptance remain
pending. Previous frozen APKs and receipts are preserved unchanged.

## AAC owner and fused RTMP follow-up

The PCM candidate failed after approximately 55.26s with its pool exhausted:
zero free blocks, seven ready and one encoder-owned. Hardware continuity was
still healthy: 2,650,112 PCM frames versus 2,641,920 AAC input frames, with
2,580 encoded units. This identifies a producer/encoder scheduling backlog,
without establishing that every cause is now known. The previously sampled
input wait maximum was 121,678us and encoded-handler delay 196,927us. Approximate
process GC totals increased from three runs/164ms/26MB allocated to six runs/
1211ms/463MB allocated; no blocking-GC run was reported. Those totals do not
prove an individual pause or a causal allocation source.

The same physical failure passed the corrected cleanup gate: reader, pump,
camera, GPU and output owners completed, Prepare became available, the origin
received normal unpublish/EOF, and its recording prefix finalised at about
53s. The source correction is therefore backed by finite failure-path hardware
evidence; sustained capture throughput remains failed, with no publishing
queue resets in that observation.

The next packet moves compressed AAC construction to the AUDIO callback owner
and releases the platform buffer before the sole fanout handler is scheduled.
The native encoder can regain its inputs even while that handler is busy. The
new immutable handle handoff is explicitly bounded as described above; count
and bytes remain charged until the currently borrowed handle is actually
closed. Cancellation closes only unborrowed owners. Residence checks cover
both dispatch and the synchronous fanout call: a blocked call cannot be safely
interrupted, so an overrun is reported after it returns without concurrent
handle closure. Normal EOS drains accepted handoff owners before finalisation;
failure cancels the unaccepted remainder and preserves the recording prefix.
Configuration and exact AAC cadence remain per epoch, with no shared mutable
audio clock and no stale PCM retiming.

The app also consumes the actual additive `sendAndTakeOutput` SDK API. One
native call performs RTMP tick, typed sample validation, send and bounded wire
output transfer, returning state, remaining bytes and the next deadline from
the same native guard. Kotlin arms that returned deadline before its unchanged
two-second socket-write watchdog. Existing TLS trust/hostname validation,
ACK/window policy and transport failure behaviour are preserved. First-IDR
metadata crosses FFI only until a successful first IDR send; steady-state
samples require neither video nor keyframe getter. This reduces crossings;
physical allocation, timing and sustained playback improvements still require
measurement. The parent debug-only benchmark includes complete legacy/fused
sample operations against the same in-memory fixture.

The AAC-owner/fused-RTMP candidate is frozen at
`target/mobile-android/aac-owner-fused-debug.apk`, SHA-256
`d0a56e369418346b41fbec580d60947d11c5464d1f967563a5fafc581de686ff`.
It consumes the verified additive fused-API AAR, SHA-256
`4d782d501ab8c171aaf83a7dcbe70675988c32f5551c3916e1149f530286fc1a`;
the packaged native library matches that AAR. All 71 exact app-source/build
inputs are preserved in `aac-owner-fused-source.tar.gz`, SHA-256
`059fc7913ea8a294c5d79a95029ff139d327df613675538a6cf285800428d4ed`,
with the accompanying source-hash manifest.

The local final offline build passed actual assembly and all 85 JVM tests.
Severe host swapping delayed the build, so the parent also built the exact
hash-verified archive on the M4 with the same pinned toolchain and AAR. That
run passed all 85 tests in 24 suites and lint with zero errors/seven warnings.
The warnings cover target-API guidance, two dependency-update hints,
fullSensor orientation guidance, backup rules, the missing application icon
and the debug benchmark's untranslated status text. The obsolete first local
lint run was cancelled; the final lint gate is the successful exact-source M4
run, not an inferred local result.

The parent re-signed the M4 APK with the existing development key and verified
the installed-app certificate plus all 149 non-signature payload entries.
Installation therefore does not require deleting existing app data. The
canonical `aac-owner-fused-receipt.json` retains signing/M4 verification, with
local test/assembly validation recorded separately. No source changed after
that archive was frozen. These gates establish the debug build and ownership
regressions; sustained physical audio, stable A/V PTS, four-pose visual
correctness and the new AUDIO-owner stop path remain acceptance checks.

That debug candidate failed the next physical run at about 30.6s: the
compressed-AAC handoff reached 13/16 handles and its oldest handle waited
256.158ms, beyond the unchanged 250ms limit. PCM kept pace with a pool high
of two blocks, publishing had zero queue resets and a 99ms high, and cleanup
completed all owners with prompt origin EOF. GC totals increased by 504ms
and approximately 188MB allocated; cumulative totals do not identify an
individual pause or its cause. The failure therefore remains a scheduling
and allocation investigation, with no wider buffers or timestamp changes.

The next controlled comparison is the internal `qualification` build type.
It inherits `release`, explicitly disables debugging, retains the existing
minification/resource-shrinking behaviour, and uses the local development
signing configuration for private updates. Its own manifest permits shell
profiling; only debug and qualification source sets include the offline
benchmark. The production release source set excludes that Activity. This
follows Android's recommendation to measure a non-debuggable, profileable
release-derived variant; no benchmark library or runtime dependency is
added. [Android performance build guidance](https://developer.android.com/topic/performance/benchmarking/macrobenchmark-overview#setup-app)

`mobile/android/build-qualification.sh` runs source-isolation checks, the
existing qualification JVM tests, assembly and lint. After assembly,
`mobile/android/verify-qualification.py` checks the actual APK manifest,
benchmark Activity and exact frozen AAR/native-library identity. The variant
retains every media source, queue bound, clock and watchdog from the failed
debug candidate. Its source archive and manifest use the
`aac-owner-fused-qualification` prefix under `target/mobile-android`.
Build and physical acceptance remain separate gates; adding this variant
does not establish that debug overhead caused the failure.

The later non-debuggable status-cache candidate failed physical orientation:
both the phone preview and an independently decoded origin frame were sideways
and stretched. Camera2's SurfaceTexture producer matrix already contains sensor
rotation; applying the same sensor correction again to shader vertices was the
concrete defect. The next source packet separates raw sensor dimensions from
natural-texture/encoded/preview geometry, retains the complete producer matrix,
and applies only the residual physical/display rotations. It adds bounded
250ms pose settlement with a real one-shot timer, including unchanged-angle
callback suppression and foreground cancellation. Source-oracle tests cover
producer composition, labelled corners, physical pixel aspect and four poses;
actual M4 build and phone/origin visual acceptance remain pending. Details and
the first-frame-only `SapflowGeometry` log contract are in
[the orientation contract](mobile-android-orientation.md).

The subsequent rotation run reached ERROR with all native owners released, but
still displayed the ordinary orientation-transition message. The original
cause cannot be recovered from that finite packet. Two diagnostic-loss paths
exist: recording failure during a pending rotation and a cleanup watchdog
followed by successful owner completion. The next diagnostics-only packet
latches the first terminal failure per capture epoch separately from transition
text. Final ERROR and repeated Stop retain that cause; manual Prepare starts a
new diagnostic epoch without clearing the bounded lifecycle history.

`SapflowLifecycle` logs fixed reason codes and scalar epoch, generation, pose,
stage and ownership flags. It never logs exception messages, destinations,
credentials, private recording paths or media. The history retains 16 entries;
repeated failure codes are logged once per epoch. No per-frame logging is added.
Flag bits 0–12 respectively mean stopping, teardown, camera closed, GPU stopped,
GPU closed, microphone workers stopped, AudioRecord stop returned, publisher
abort completed, encoder tail completed, AAC owner quiesced, zero AAC handoff
handles, terminal failure and cleanup timeout/retained ownership. A completed
cleanup is a separate observation and cannot erase the first failure.

The four added JVM tests cover failure during rotation, timeout followed by
successful cleanup, stale epochs/bounded immutable history and log privacy.
M4 qualification tests, compile/lint and renewed physical rotation acceptance
remain pending for this packet. It diagnoses the black-preview failure rather
than claiming its underlying scheduling or platform cause is fixed. Queue,
PCM/AAC, protocol, cleanup and pose-stability bounds remain unchanged, as do
the orientation transform and native/status-cache package.

The next qualification-only pose control exercises actual encoder/audio,
recording and publisher restart while the owner leaves the phone stationary.
It does not establish upright physical geometry: the injected pose deliberately
differs from the real device pose. The existing SurfaceTexture producer matrix,
shader geometry, queues, clocks, native package and 250ms pose dwell are retained.

Only `src/qualification` declares `QualificationPoseApplication`. It registers
a dynamic receiver while the existing MainActivity is visible, and requires a
non-debuggable, shell-profileable package plus sender `android.permission.DUMP`.
There is no manifest receiver to cold-start the app, Activity launch, destination
or credential input, prepare/start, unlock, capture-file access or background
control. The release/debug source sets exclude that Application; their default
pure pose-input policy rejects every override request. Android documents sender
permission checks for context-registered receivers, and AOSP's shell requests
DUMP. [Broadcast permissions](https://developer.android.com/develop/background-work/background-tasks/broadcasts#restrict-broadcasts-permissions),
[AOSP shell permissions](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/packages/Shell/AndroidManifest.xml)

The owner may send one package-targeted command to an already capturing app:

```sh
adb -s DEVICE_SERIAL shell am broadcast \
  -a dev.forgesworn.sapflow.broadcaster.QUALIFICATION_POSE \
  -p dev.forgesworn.sapflow.broadcaster --es pose CLOCKWISE_90
```

Accepted values are `NATURAL`, `CLOCKWISE_90`, `REVERSE`, `CLOCKWISE_270` and
`CLEAR`; no other extras or intent data are accepted. A pose is admitted only in
CAPTURING. Transition commands return INACTIVE without changing the override,
so automation must wait for CAPTURING before each step. The same selector and
one-shot 250ms timer then call the production orientation restart method.
Physical notifications are suppressed while overridden, but the normal sensor
selector continues its 250ms dwell internally using the same one-shot timer.
CLEAR therefore restores an already stable physical pose without waiting for
another unchanged sensor callback; an unfinished physical dwell still has to
complete. CLEAR also works in error/idle without requesting a native restart.
Normal UI Stop clears the override and fences synthetic commands immediately,
even before the asynchronous controller leaves CAPTURING. Physical settling
continues without notifying the stopped controller; explicit manual Prepare
synchronises its stable pose before preparing a new epoch. Stop/Prepare preserve
the qualification enablement and the 16-request foreground budget. Leaving the
foreground/destruction disables and resets the hook and cancels its timer.
CLEAR stays available after the request budget is exhausted.

Ordered broadcast result codes are 0 DISABLED, 1 INACTIVE, 2 INVALID, 3 LIMIT,
4 ACCEPTED and 5 CLEARED. `SapflowPoseQualification` logs only these fixed
results. No command repeats itself or performs network/file work. This is a
private qualification control, not a production remote-control interface.

The source isolation gate is `:app:verifyQualificationIsolation`. The next M4
packet must run all 102 qualification JVM tests (eight new pure pose-policy
cases), qualification/release assembly and qualification lint. The 20 Python
verifier tests pass locally; no Gradle/device run has been made for this packet.
Actual APK checks additionally require the hook Application and its DEX markers
in qualification, and reject the Application/action/benchmark markers in release:

```sh
python3 mobile/android/verify-qualification.py QUALIFICATION_APK FROZEN_AAR \
  --aapt2 AAPT2 --expected-aar-sha256 FROZEN_AAR_SHA256 \
  --expected-native-sha256 FROZEN_NATIVE_SHA256 --expect-pose-hook \
  --release-apk RELEASE_APK
```

This keeps existing verifier defaults usable for older receipts. Full compile,
artifact isolation, DUMP-gated delivery, active-state fencing and actual restart
continuity remain separate M4/physical gates until exercised by the parent.

The frozen `qualification-pose-hook-v2` packet subsequently passed the M4
qualification gate: 102 JVM tests in 26 suites, no failures/errors/skips, zero
lint errors and the seven existing warnings. Static isolation and actual APK
checks passed: the qualification Application/action are present only in the
qualification artifact, and the release artifact contains neither the hook
nor the benchmark and does not permit shell profiling. The AAR and native
library remain unchanged. The first attempt exposed an unflagged legacy
receiver registration; the final source uses the existing AndroidX
`ContextCompat.registerReceiver` overload with explicit exported status and
the DUMP sender permission across all supported versions.

The original qualification APK SHA-256 is
`8300f25db6ec6fc8ad8ce901437bccac609e842b68f9a5b9956df9a69d7d1db0`;
the unsigned release negative control is
`e81e09f616a975206279e9f9adff1fa9533c8cc9272f7789b63e7ff22885af37`.
The source archive and manifest, build log, original APKs and independent local
verification are retained under ignored
`target/mobile-android/qualification-pose-hook-v2-*`. These are internal build
receipts. No device access, additional signing, install, sender-permission
exercise or actual rotation-continuity acceptance was performed by this task.

The subsequent SRT normal-Stop correction is source-only. It is **not** in the
installed `f77c0636…` qualification APK, which remains the baseline for the
first stationary AES256 phone-to-origin run. That binary closes UDP before
polling the native shutdown control, so the origin can retain its publisher
until peer-idle expiry. No package, capture, renderer, audio or media-age bound
was changed by this correction.

Normal Stop now publishes one original 100ms deadline, closes admission and
wakes the Selector without interrupting its worker or closing UDP early. The
UDP owner cancels native media/keys, discards its previously returned media,
then polls at most one fresh 16-byte SRT SHUTDOWN control. It makes one
nonblocking write only: zero-write, partial write, exception or expiry ends the
attempt without retry. Local submission never means destination receipt. The
original deadline cannot be renewed; its watchdog hard-closes the channel and
interrupts the worker. Immediate failure still takes that hard-abort path,
and the control worker retains its one-second join/retained-owner check.
Prepare-only cancellation creates no native publisher, duplicate Close remains
idempotent, and normal Stop preserves a prior failure message.

This ordering avoids poisoning the graceful control write with an early
interrupt. Java documents that interrupting channel I/O can close the channel;
Selector wakeup releases selection without setting that interrupt status.
See [DatagramChannel.write](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/nio/channels/DatagramChannel.html#write(java.nio.ByteBuffer))
and [Selector.wakeup](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/nio/channels/Selector.html#wakeup()).

The exact production JVM SRT sources compile with warnings treated as errors
against frozen AAR `6102340f…`. Twenty pure JUnit tests pass, including ten new
shutdown cases covering original deadline, native-before-control ordering,
zero-write/no retry, missing control before handshake, hard abort, native and
write faults, media rejection, expiry during each callback, concurrent owner
fencing, Stop-before-Start and duplicate Close. The existing production-loop
host harness includes the new helper in its source list. No native calls,
socket activity, Gradle build or device operation ran for this gate. Full
qualification tests/lint/package and an independently observed prompt origin
shutdown/reconnect remain pending before this source correction is qualified.


### Automatic destination reconnect (source packet, not physical qualification)

`ReconnectingPublishingSink` keeps one logical `NativeSession(90_000)` across
encoded orientation epochs. Explicit Prepare remains dormant; Start connects.
A transport failure produces one typed event and retires that attempt before a
fresh socket/native publisher can be constructed. One timer drives the shared
retry actions and original absolute outage deadline, including while the next
handshake is pending. Repeated failures and orientation changes do not create a
new outage window. Healthy intentional orientation replacement uses the explicit
`replaceTransport` policy seam, which does not manufacture an outage.

Stop, background interruption and capture failure first fence the destination,
then finish its bounded transport cleanup. A delayed callback, timer or old media
epoch cannot reopen it. A cleanup failure retains the owner and cancels retries.
Transport loss does not restart camera/audio capture or the independent private
recorder. No disconnected media queue is added: existing sample/byte/residence
bounds remain, and every new attempt waits for an IDR. Transport acceptance also
posts a generation-fenced encoder sync-frame request, at most once per second.

`OriginMediaConfirmation(destination, epoch, attempt)` is an explicit typed seam for a future
trusted V4V observation. Manual destinations do not call it. TCP/TLS, SRT
handshake/ACKs, RTMP Publish.Start, writes and local encoding never confirm
playable media. The current SDK separates Sending, network recovery and decoded
media readiness. Complete declared startup media entering native ownership emits
Sending; only a validated peer acknowledgement across that current attempt's
startup watermark can clear the network outage window. It still leaves playback
unverified. RTMP declares the fixed H264/AAC profile before media, and native
ACK accounting includes its checked handshake-domain guard.

Two bounded per-attempt facts latch startup/progress arriving before transport
acceptance; the control owner applies connected, sending and peer recovery in
that order. Failed, replaced and stopped attempts discard those facts. Native
`attemptEpoch` is captured on Connect independently of the local callback token.
Only `snapshot.nextDeadlineNs` drives the control alarm; the retained historical
outage deadline never creates an exhausted-state timer loop. At the original
deadline, an accepted Sending attempt may continue with automatic retries
exhausted and peer progress unknown. A later failure or intentional replacement
cannot open another transport. An unaccepted/no-startup attempt expires. This
retains manual publishing without manufacturing decoded proof or extending the
original retry budget from local writes.

The pure controller tests use fake clocks, policy and transport factories. The
Java-owner tests use synthetic sinks without camera, native protocol calls or
media. The frozen 88-input v2 packet passed the actual M4 offline build against
AAR `71c62232…` exposing `NativeSession.replaceTransport`: 135 tests across 29
suites, no failures or skips, lint zero errors and seven warnings. Qualification
APK `2bc718f4…` is non-debuggable and shell-profileable; unsigned release APK
`2fc32f05…` excludes the qualification hook, offline benchmark and profileability.
Both contain the approved native library `a3d59bab…`. Exact hashes, commands and
reports are retained in ignored `target/mobile-android/reconnect-v2-*` evidence.
The owned Gradle process group terminated. No additional signing, installation,
network-drop test or physical acceptance was performed for this packet; earlier
frozen APKs do not include it.

### B4 creator connector (source implementation, not physical qualification)

The connector is disabled unless the builder supplies all three reviewed public
routing properties: `sapflowV4vApiOrigin`, `sapflowV4vPlaybackOrigin` and
`sapflowV4vPlaybackPrefix`. Origins must be HTTPS origins without user info,
paths, queries or fragments; the playback prefix is an absolute directory.
No private/production hostname or credential is committed. These properties
require operator approval for the intended deployment before packaging. There
is no in-app API endpoint editor, imported destination policy, Google service,
wallet authority, origin control token or callback secret. Manual destinations
remain separate. An authenticated grant from this connector alone carries V4V's
unlimited, free, watermark-free destination policy.

The process-owned control actor owns bounded platform HTTPS and an encrypted
Keystore AES-GCM/AtomicFile journal in no-backup app storage. A missing existing
key/ciphertext, corruption or failed write blocks the connector and fences its
publisher. It never resets an existing journal to an empty identity. Exact
Prepare/Refresh/End/recovery operations and idempotency keys are saved before
sending; uncertain responses retain that request. End has one reserved control
admission behind an in-flight request, fences publishing immediately, and retains
terminal intent before reconciliation. It never implies recording/callback
completion merely because the local socket closed. Restoring a grant requires a
fresh acknowledged higher-revision refresh before capture can start.

Sign-in uses an installed NIP-55 signer for kind 21236 only. The returned event
must match every requested field, canonical SHA-256 ID and BIP-340 signature.
No persona private key is imported or generated. Signer launches are single-file
with a 60-second budget, bounded abandoned-result fences and Activity result
reattachment. Sign-in is offered only outside active/prepared capture because
leaving the foreground intentionally stops capture. Ordinary grant refresh uses
the existing bearer without opening a signer. Bearer expiry requires explicit
reauthentication; no permission prompt or capture resume happens automatically.

The fixed encoder profile requires an immutable grant cap of at least 4 Mbps
(`maxPublishBps` is bits/second, unlike native SRT's byte-rate pacing field).
A smaller grant remains a reconcilable reserved session, but camera preparation
is refused; End remains available. The actual grant reconnect grace is passed
to the existing shared policy. Refresh does not recreate that coordinator or
reset an outage. Orientation still replaces only its media/transport epoch.
Private cold-path diagnostics expose scalar destination/epoch/attempt identities
and an explicit capture-Prepare serial without credentials or media.

Recording-failure acknowledgement is an explicit, separate action after a safe
terminal-origin receipt. It never marks End complete or obtains credentials.
A replacement requires another explicit Prepare carrying the saved old identity
and acknowledgement fence. No automatic scheduling, publishing or recovery
acknowledgement is performed.

Decoded confirmation remains **disabled** in this packet. Status, a fresh-media
descriptor, socket acceptance, ACKs and local encoding never call
`originMediaConfirmed`. The independently coordinated `forgesworn_attempt`
metadata/receipt and finite Android decoder must be integrated and tested before
that gate can be enabled; this source does not append an unvalidated suffix.

The signing-only KithMoot adaptation retains its MIT notice. ACINQ secp256k1
0.22.0 is pinned for verify-only BIP-340: actual bytecode is JVM8 and Kotlin
metadata 2.2.0, fitting the existing compiler. Upstream 0.23/0.24 require Kotlin
2.3.10; no broad toolchain upgrade is made. Its Android ARM64 and x86_64 ELF LOAD
alignments were independently inspected as 16 KiB. The first connector APK is
restricted to ARM64; other ABI releases remain unqualified. Apache-2.0 wrapper,
MIT libsecp256k1 and MIT adaptation notices are packaged in assets. JSON parsing
uses pinned kotlinx-serialization-json 1.7.3 for canonical event handling, with
body/depth/duplicate-key/number bounds. No BouncyCastle or whole KithMoot module
is imported.

Source tests cover uncertain mutation replay across restart, failed durable
writes, insufficient cap with working End, End during uncertain refresh,
explicit acknowledgement/replacement, expiry, corrupt-store refusal, hostile
JSON and real synthetic BIP-340/event-field rejection. The pure authenticated
journal envelope and artifact ABI/hash/alignment checks are independently tested.
The pre-policy API compile and 31 pure Kotlin / 21 artifact-verifier tests are
retained as intermediate gates. The combined SDK integration below subsequently
passed full Android tests, lint and package checks. Installed signer, an enabled
approved creator API journey, finite media decoder and physical connector/recovery
acceptance remain separate gates.

A dedicated monotonic grant-expiry alarm is independent of the HTTP/journal
executor. It is generation/session/revision fenced and superseded only after a
validated refresh has been durably saved. Start admission independently checks
the active owner's deadline under the same ownership lock, so delayed alarm
delivery cannot let a newer approval revive an expired prepared owner. A late
refresh commit synchronously fences that owner. Expiry fences the current
publisher; a late HTTP result cannot start it again. Completed End with no pending command
cancels idle control polling. HTTPS uses finite connect/read budgets and an
8-second disconnect watchdog; a platform DNS/native call that ignores cancellation
retains the single blocked owner rather than creating unbounded replacement
threads. Such a stall cannot delay the separate grant-expiry admission fence.

Queued grant/start replies carry a single-use owner permission and the exact
prepared-capture binding. End, Stop, expiry and foreground interruption retire
that permission synchronously; a reply for capture A cannot prepare/start
capture B. A higher revision may authorise the same prepared session only with
matching immutable credentials, limits and grace and a live monotonic deadline.

The Android journal does not trust `AtomicFile.finishWrite`'s void result alone:
it sets mode 0600, syncs the output, verifies bounded actual-base ciphertext
readback and syncs the parent directory. Silent rename or directory-sync failure
therefore cannot authorise sending a mutation. Base/backup/new symlinks are
refused on both load and save; the parent must be a non-symlink directory owned
by the app with no group/other permission bits. An unconfirmed commit retains
an unusable engine; a later process must
reopen and validate the retained journal rather than reset it.


### Combined B4 and typed transport-recovery build

The frozen 108-input combined source packet passed the M4 offline qualification
and release build against AAR `39d2af97…` (SDK source `1634a809`). All 174 JVM
tests across 33 suites passed with no failures or skips; the Python APK-verifier
suite passed 21 tests. Lint reported zero errors and ten warnings: retained target,
dependency-update, orientation, backup, icon and debug-label guidance, the explicit
ARM64-only ChromeOS limitation, and two optional URI KTX style suggestions.

The original qualification APK is `1d1820e8248db58be579750ca0eefb21ec0002beccc19de46850e088c7e55cfc`;
the unsigned release APK is `dd5fc5eed79bec8d78d85495206491d5e787522a4bbdc45674da8222d0220d16`.
Both contain the exact SDK library `4a45c951…` and pinned ACINQ verifier; every
packaged ARM64 ELF LOAD alignment is at least 16 KiB. Actual manifest/Dex checks
confirm qualification is non-debuggable and shell-profileable, while release
contains neither the pose hook nor offline benchmark and is not shell-profileable.
Build commands, test XML, lint XML, source archive and package-verifier receipts
are retained under ignored `target/mobile-android/v4v-transport-recovery-*`.

This build uses the disabled default V4V profile, performs no additional signing
or installation, and makes no physical or all-ABI release claim. Decoded-media
confirmation remains disabled. The actor/permit/journal packet's independently
reviewed v4 snapshot is retained separately from the subsequent typed SDK delta.

### Private localhost TLS qualification

A separate opt-in build configuration can enable the V4V connector against a
private HTTPS localhost fixture without changing the default or production
profile. Supply all four public properties together:

```sh
./gradlew assembleQualification assembleRelease \
  -PsapflowQualificationV4vCaFile=/absolute/path/to/public-ca.pem \
  -PsapflowQualificationV4vApiOrigin=https://localhost:4443 \
  -PsapflowQualificationV4vPlaybackOrigin=https://localhost:4443 \
  -PsapflowQualificationV4vPlaybackPrefix=/live/
```

The CA file must be absolute, regular, non-symlink, at most 16 KiB and contain
one currently valid X.509 CA certificate, with no private-key/other PEM blocks.
The build emits canonical public DER, a network-security configuration and a
qualification-only manifest under the app build directory. Only exact
`localhost` trusts that CA, without subdomains or cleartext; other HTTPS hosts
retain platform system trust. Both origins must be literal HTTPS localhost,
optionally with a valid port. The existing production profile properties are
unchanged, and qualification fields never alter release/default BuildConfig.
There is no trust-all socket factory, hostname override, OS CA installation or
runtime URL import.

This uses Android's documented [per-domain custom trust anchors](https://developer.android.com/privacy-and-security/security-config),
not debug overrides, since qualification is non-debuggable. The opt-in artifact
verifier requires expected public API/playback origins, prefix and canonical CA
DER SHA-256. It checks the actual binary manifest/XML/resource references,
CA bytes and qualification Dex, then proves the fixture resources, certificate
and origin strings are absent from release. An invocation assembling both
variants must pass the existing release hook/benchmark isolation checks too.

Private fixture certificates and generated files are build inputs/outputs kept
outside the source archive. A valid-TLS package gate establishes neither a
running API nor installed signer, creator permissions, media decode or physical
connector acceptance. The earlier 108-input APK/receipt remains immutable.

The private-TLS source packet passed the M4 default and opt-in builds: each ran
174 JVM tests across 33 suites with zero failures/skips, and lint reported zero
errors with the same ten warnings. The artifact-verifier suite passed 33 tests,
including shortened compiled resource paths and refusal of alternate qualified
trust resources. Six actual Gradle configuration checks refused partial inputs,
a non-local origin, a relative or symlink CA, oversized input and malformed CA.
An explicit public placeholder profile retained its release fields and compiled
origin strings while qualification used only its own fixture profile.

The opt-in original qualification APK is
`6fc3581d3e743903b83bf4604036315d0acc9fb7ab5a7e5a3d482eb52de7063f`.
The default qualification APK and unsigned release remained byte-identical to
`1d1820e8…` and `dd5fc5ee…` above. A warm qualification rebuild after removing
all fixture properties also reproduced the exact default APK, with empty
profile fields and no fixture trust/configuration/origin. Generated BuildConfig
field readback separately checks the playback prefix; the APK verifier proves
the compiled origins and trust configuration. Evidence and the certificate-free
108-input source archive are retained under ignored
`target/mobile-android/qualification-v4v-tls-v4-*`.

These are package/build gates only. No fixture listener, system trust change,
signing replacement, installation, phone interaction or running V4V/NIP55
journey was performed by this packet. The public fixture CA was supplied
explicitly; this work neither generated nor accessed private TLS keys.


The explicit quality source packet adds a pre-Prepare choice. 720p remains the
initial default: 1280×720 (720×1280 portrait), H264 baseline hardware CBR at
2,500,000 bits/s, AAC LC mono 48 kHz at 96,000 bits/s, 30 fps and a one-second
GOP. Its V4V minimum grant remains 4,000,000 bits/s. The added 480p preset uses
848×480 (480×848 portrait), 600,000 bits/s H264 with the same audio, cadence and
GOP, and requires at least 1,000,000 bits/s. These are explicit fixed choices,
with no automatic downgrade or change to the backend's limits.

The raw Camera2 source remains the existing sensor-basis 720p source. GPU
rotation still uses the validated complete producer matrix and world-oriented
pose convention. At 480p the 16:9 image fits into 848×477 pixels, leaving three
black rows in total; portrait uses 477×848 with three black columns. Preview
fits the source image aspect independently. 720p remains a full-canvas fit.
Hardware selection requires surface input, baseline AVC, CBR, the selected
bitrate, alignment and 30 fps support in both encoded orientations. An
unsupported profile fails explicitly; capability metadata is not physical
qualification.

Quality is immutable from Prepare through all orientation epochs. A synchronous
Prepare reservation also closes the UI-to-codec-handler enqueue gap: its exact
profile is held until the queued task starts or is retired. Stop, End and screen
cleanup cancel queued admission; an old task cannot release a newer reservation.
Selection is
available only when native cleanup permits a new Prepare, retires queued grant
answers and invalidates old publisher admission. V4V prepared-owner and Start
permissions bind the exact profile as well as the existing session, generation,
revision, immutable grant and monotonic expiry checks. The control journal and
End/refresh/reconciliation remain available for grants too small for the chosen
quality; capture admission rejects them before retiring another owner. Session
control does not implicitly choose a quality.

Rates are in bits/s at the grant boundary. The scoped SRT data pacer uses
`min(1_250_000, floor(maxPublishBps / 8))` bytes/s, shared by original and
retransmitted SRT data including its header. The origin admission limiter counts
unique reassembled TS bytes over its existing window; it does not count every
NIC byte. The 480p elementary target totals 696,000 bits/s, leaving 304,000 bits/s
below its minimum grant for TS overhead and variation. This is budgeting, not
proof that a particular encoder fits the cap. Native residence, wire-age,
queue-byte, PCM and cleanup bounds remain unchanged, and overload fails rather
than silently retiming media. Existing coalesced status shows selected codec
rates, cap and SRT pacer without new per-frame native calls.

This packet passed independent source review, 51 focused JVM tests and the
full M4 offline build against AAR `55f55bee…`: 183 tests across 34 suites with no
failures/skips, lint zero errors (ten warnings), actual APK/native 16 KiB checks
and qualification/release isolation. The original qualification APK is
`e5815c5465de3083d8986f9b3640dabe3d24c63823177f9d8d799767094a1fc0`;
the unsigned release is
`2c30c8f2715d15e05268e14fe2efa0fb305980743ac6203a2db997a642e1bac2`.
The certificate-free 110-input source archive and reports are retained under
ignored `target/mobile-android/quality-presets-v2-*`.

The historical signed 8e9… and private-TLS 6fc… APKs remain unchanged. The new
480p profile still needs actual hardware, encoded four-pose, sustained cap and
playback acceptance; no signing, installation or phone action is part of these
source/build checks. The selected ABI is ARM64; an all-ABI release is not
qualified by this packet.


The next Android source packet binds future decoded observations to a captured
transport attempt. Each Connect captures the SDK generation and attempt epoch
before any transport/probe starts, alongside the local destination, codec epoch
and attempt token. Scope becomes observable only after control-owner acceptance;
Stop, codec close and the first reported failure fence it immediately, including
while cleanup is queued. A cold linearisation gate covers that final validation
and SDK confirmation together with synchronous failure, Stop and codec-close
flags; it never covers transport close/join, IO or IDR callbacks. Delayed
completion must still pass the current local context and captured SDK identity. The final SDK call is
`mediaConfirmedForAttempt(capturedGeneration, capturedAttemptEpoch, nowNs)`;
it never substitutes a newer snapshot at completion. The server's connection
epoch is a separate namespace and is not an SDK attempt number.

For V4V only, every actual retry/rotation transport factory obtains a fresh
cryptographic 128-bit lowercase hexadecimal nonce. It derives an ephemeral
canonical SRT ID from the validated grant's colon credentials:
`#!::m=publish,r=live/<event>,u=<session>,s=<password>,forgesworn_attempt=<nonce>`.
The durable grant, refresh comparisons and Start's immutable credential checks
retain the original colon ID. Manual RTMP/SRT factories and stream IDs remain
unchanged. One current scope is held per logical destination; a retired factory
cannot overwrite its successor's correlation owner. The SDK context is not
persisted and no attempt ID is added to the durable grant. The existing status
journal can retain the origin's visible echoed ID as metadata; it must never
substitute for a fresh status response or reconstruct an observation scope.
No wire credentials or event identifiers are added to logs.

The cold correlation helper checks authenticated receipt identity/revision,
active/live/open protocol state, the exact echoed nonce, positive server epoch,
and the existing strict seven-field fresh-media descriptor with canonical
same-event paths. Missing/null metadata is unknown, not success. Its result is
encoded availability only. The future finite decoder must obtain fresh GET
status, map paths exclusively through the approved public playback profile,
validate correlation before and after genuine video/audio decode, and then
submit the originally captured context. No decoder/probe or production caller
of the confirmation seam is enabled by this packet; it makes no new Live,
playback or recovery-acceptance claim. The nonce adds correlation, not admission,
encryption, anti-replay or protection against another holder of the credentials.

Original reconnect deadlines, network-watermark semantics, media residence and
queue limits, grant expiry, Stop/End barriers and capture/recording independence
are retained. The signed quality APK remains an immutable separate artefact.
The focused host gate passed 99 JVM tests with Kotlin 2.2 and the actual
`55f55bee…` AAR classes, including SDK attempt fencing, executor ownership and deterministic failure/Stop
races at the final confirmation boundary.
The full M4 offline Android API/package gate then passed 200 tests across 35
suites, lint zero errors (ten warnings), actual ARM64 native 16 KiB checks and
qualification/release isolation using the same AAR. The original qualification
APK is `8163a1aaa84396abf9159ad0918b91619add52a1da34e9aa569f2590dbcbe706`;
the unsigned release is
`557f31700ece8bc356aec424a28cb6941a00e825a3cb35c83d20309456ded617`.
The 112-input source archive, tests and package receipts are retained under
ignored `target/mobile-android/v4v-attempt-v3-*`. These source/package gates
remain separate from future signing, network execution, real decoded proof
and physical qualification.


The next source packet adds a finite Android origin check for V4V destinations.
It leaves manual transport bytes, capture, audio, GPU rendering and quality
selection unchanged. An accepted attempt obtains ephemeral, status-only API
authority from the control actor, then uses its own cancellable HTTPS client;
End and grant refresh retain their independent control owner. Only a fresh
session-specific GET can supply availability. The check allows three bounded
status attempts before decode and one after it, plus at most three
nonblocking control-permit admissions, within an original 20-second
monotonic deadline. Missing metadata remains unverified, without continuous
polling, a second encode or a continuously running decoder.

Canonical media paths are mapped exclusively through the approved playback
profile. The configured prefix replaces `/live/`: `/live/event-fixture/file`
with prefix `/media/live/` becomes `/media/live/event-fixture/file` on the
approved HTTPS origin. A proxy mount `/origin/` therefore uses the full prefix
`/origin/live/`. Neither grant playback URLs nor receipt metadata select an
authority. The media fetcher has no bearer argument, refuses ambient cookies,
redirects and compressed responses, and keeps normal platform TLS and hostname
validation. Init and completed independent part downloads are limited to
256 KiB and 2 MiB, with five-second request watchdogs inside the total deadline.
Public-route authorisation failures do not forward API credentials.

Downloaded immutable bytes back a `MediaDataSource`; no existing phone file,
private recording, camera image, temporary file or decoded frame is read or
written. Top-level Sapflow `ftyp+moov` / `moof+mdat` shapes are checked before
native parsing. Separate MediaExtractors select exactly AVC and AAC-LC, with
profile dimensions in either admitted orientation, 48 kHz mono audio, bounded
access units and an independent first video sample. A finite buffer-mode
MediaCodec check requires two advancing H264 outputs and at least 2048 decoded mono PCM
frames (4096 bytes) with their actual 48 kHz time extent. One coalesced PCM
buffer qualifies; multiple buffers must have consistent byte/time cadence. It does not render frames or
play sound. The selected AVC decoder must report hardware acceleration on
Android API 29+, including the actual created implementation; native AAC may
report software. PCM cadence permits at most one 48 kHz sample (21 microseconds)
of timestamp quantisation between outputs; the supported ASC is exactly 1188
(AAC-LC, 48 kHz mono, 1024-frame mode). This vendor classification is not physical performance proof.
The existing performance contract requires hardware video encoding and does
not require hardware AAC. Actual names, software flag, PCM bytes and output
PTS are retained only in bounded observation diagnostics.

The native work has a four-second checked deadline, at most 128 encoded inputs
per track, 512 KiB per access unit, 32 output buffers per track and 256 KiB PCM. The finite
check normally stops after two video outputs and 2048 audio PCM frames. Platform native calls have
no guaranteed interruptible cancellation: a separate deadline closes admission immediately and schedules HTTP cancellation
on one independent cancellation owner. No platform disconnect runs on UI,
expiry, capture or transport callbacks. The single native worker retains its
slot until both actual codec/extractor and HTTP cleanup return. One latest
accepted scalar scope may wait behind cleanup, keeping its original deadline;
it is revalidated before admission and never queues a second native owner. Failed release retains the platform wrappers
and disables further checks in that process. No queued second decoder can
bypass a stalled or uncertain owner. This is an explicit platform limitation,
not a hard promise that a blocked driver stops within four seconds.

Success additionally requires fresh post-decode receipt correlation with the
same local destination, codec epoch, attempt token, captured SDK generation and
attempt epoch, exact nonce, session/generation, server connection epoch and
immutable initialization path/ID. Revision and part may advance monotonically.
Both declared and decoded audio are mandatory for these A/V capture profiles.
Stop, End, expiry, replacement and failure synchronously fence observation;
the final SDK call rechecks the current atomic grant deadline under the cold
attempt gate. Neither a status receipt nor a successful fetch alone invokes
`mediaConfirmedForAttempt`. Native cleanup must finish before submitting actual
decoded evidence. Observation errors remain unverified and do not fail capture,
recording or the transport's network recovery policy.

Source/API and pure tests are separate from Android decoder acceptance. No
Pixel, emulator, phone media, fixture, sign/install or runtime request was used
for this source packet. Sapflow fragmented-MP4 compatibility, actual hardware
outputs, both-track native decode, bounded device cleanup and causal Live
confirmation still require an explicitly authorised physical or generated-media
Android gate. macOS/FFmpeg format inspection cannot supply that evidence.

The focused V4 source gate passed 117 JVM tests with Kotlin 2.2 `-Werror`,
Android API 36 and the actual frozen `55f55bee…` SDK classes. It compiles all
non-Compose main sources, including the real MediaExtractor/MediaCodec adapter
and V4V owner. Tests include coalesced PCM, PCM extent/cadence, stale scopes,
new initialization/connection, delayed grant expiry at the final SDK gate,
late control authority, busy control admission, blocked native cleanup and
blocked/failed HTTP disconnect. A real reconnect-executor regression proves
that observer disconnect cannot postpone the transport failure or its original
outage deadline. This remains a source/API gate; no Android native decoder was
executed and no new APK is qualified by these tests.

The exact frozen 118-input V4 packet subsequently passed the full offline M4
Android gate: 218 JVM tests across 37 suites, zero failures/errors/skips,
lint zero errors (ten warnings), qualification/release isolation and all four
packaged ARM64 libraries' 16 KiB ELF LOAD alignment checks. The actual SDK
remains AAR `55f55bee…`, classes `0c29482e…`, native `608cd7ca…`.
The retained original qualification APK is
`4acb3e633ebcad7fca8f52d19fa1e42b007c8b65e5559a49037c3e7fc543d655`;
the unsigned release is
`a2fa27f9a1889179a2e9cb67216c40642e57151c27aaa13ab89298827a741a9f`.
Source archives, local original APKs, XML reports, package verification and
terminal/cleanup receipts are under ignored
`target/mobile-android/origin-native-probe-v4-*`. Both own Gradle launcher and
single-use daemon terminated; no unrelated daemon was stopped. This packet
performed no extra signing, install, device, network fixture or native decoder
execution. Default V4V configuration remains disabled, and native Android
fragment/codec/cleanup/causal-confirmation acceptance remains open.

### Live readouts and private recording library

The capture screen now displays Capture elapsed, Broadcast elapsed and measured
Socket outbound. Capture time begins only at committed Start, includes rotation
and retry gaps, and freezes on Stop/interruption. Destination End freezes its
broadcast clock while independent recording can continue. Socket accounting is
local successful write completion, with fresh current-socket rates separated
from potentially incomplete historical totals. Reconnect backoff, partial
writes and stale samples never become false zero-throughput measurements.
The detailed accounting and ownership contract is in the
[SDK contract](mobile-sdk-contract.md#native-live-telemetry).

Private recordings are grouped by show and ordered segment. Finalised means
the recorder completed its mux/file lifecycle; incomplete and orphaned files
remain visibly unverified. Preparing local recording checks catalogue capacity
and free space off the UI/codec threads for the selected immutable profile.
The check does not promise a recording duration. Recording is never silently
disabled because storage is low. Late permissions, grants or storage replies
cannot restart a cancelled Prepare or choose a different destination.

Export uses the system document picker only when capture is stopped. An
application-owned single IO slot keeps the exact file selection through
Activity recreation, revalidates it before opening the document provider, then
copies bounded chunks and closes actual owners. Cancel or failure keeps the
private original; a partial provider document may remain. Unverified files
require explicit confirmation, without repair or automatic completion claims.
Healthy live ownership and ordinary busy refusals are distinct from uncertain
cleanup. Unconfirmed actual closure blocks replacement work. The current
catalogue is bounded to 256 segments; capacity failure preserves files, and
management/pagination remain follow-on work.

The combined local gate passes 305 JVM tests in 46 suites in each of Debug and
Qualification, with no failures, errors or skips. All three application
configurations assemble; qualification isolation and both lint tasks pass.
These actual Gradle suites include separate production/test Kotlin modules,
real generated-byte file tests, close/cancel/export ownership, storage
preflight, stale permission replies and telemetry retry continuity. Earlier
focused gates are overlapping subsets, not additional unique test totals.

The separate macOS native proof uses only generated H.264/AAC and the actual
packaged mux/recorder classes. Completed and clean-abort segments independently
decode, demonstrating that particular accepted prefix. It does not prove
torn-file repair, complete-show recovery, Android filesystem/SAF behaviour or
physical playback. This work has not replaced the owner's Pixel app or capture.

## Foreground input routing checkpoint

The normal controls now expose System Default plus a bounded AudioManager input
inventory of built-in, wired and USB devices. Prepare freezes the selected local
device ID before permission, storage and destination work. `setPreferredDevice`
is a request: only the captured AudioRecord's actual `routedDevice`, checked
after `startRecording` and before the first read is admitted, establishes routing.
Routing and removal callbacks close the exact epoch's scalar media gate before
queued cleanup. Cached checks also fence queued PCM before AAC admission.
Default retains its first verified input across orientation epochs; a change or
disconnect stops capture without fallback or automatic restart. Device IDs are
ephemeral and names never authorise routing.

Cold route transitions close PCM admission before querying outside the state
lock. Existing reader/pump owners can wait on a condition up to 250 ms; current
success reopens the gate, Stop wakes cleanly and timeout/mismatch remains failed.
Accepted queued PCM can drain on ordinary Stop; a latched route fault instead
uses the existing incomplete recording close and cancels rotation. No route
timer or new thread is created for this transition. The route budget does not
extend the PCM pool's existing 200 ms maximum age: queued blocks are checked
after the wait and immediately before codec submission. Stale PCM stops the
epoch and retains an incomplete recording instead of being queued as fresh.

No route enumeration, timer, additional payload copy or FFI poll was added per
PCM block. The existing 48 kHz mono path and actual-close barriers remain.
Bluetooth and other unqualified Default routes are unsupported. Actual
USB/hot-unplug, locked-screen and sustained routing behaviour remain physical
B4R gates; the implemented source contract is recorded in the
[audio-only mobile scope](audio-only-mobile.md#background-ownership-and-controls).
