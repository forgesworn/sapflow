# Native broadcaster core contract

`sapflow-client` is the first runtime-free SDK core. Its current API validates
native encoder descriptions and compressed-sample framing, owns bounded shared
sample queues and drives a generation-fenced publishing lifecycle. It performs
no network, file, billing, codec or background-thread operations. This is an
implementation boundary for B0/B1/B3, not completion of those milestones.

## Native encoded input

`media::H264Config::new(sps, pps)` accepts a single bounded raw SPS/PPS pair,
including NAL headers and without Annex B start codes. It uses the existing
pinned `h264-reader = "=0.9.0"` syntax readers to validate the SPS and its matching
PPS. Do not pass an AVCDecoderConfigurationRecord. Configuration-record import
belongs to a separately validated native adapter. Four-byte big-endian AVCC
length prefixes are the only encoded video framing supported initially.

`media::AacConfig::new(asc)` accepts exactly two AudioSpecificConfig bytes for
AAC-LC, mono/stereo, an indexed sample rate and 1024 samples per access unit.
It explicitly refuses SBR/HE-AAC, PCE, explicit sample rates, 960-sample mode,
core-coder dependency and extension flags. AAC payloads are raw access units,
not ADTS frames. AAC entropy syntax remains the native encoder's responsibility.

`SampleTiming::new(pts, dts, duration, timescale)` checks a nonzero clock no
greater than 1 GHz, non-negative timestamps, positive duration and representable
nanosecond endpoints. The adapter rebases platform timestamp epochs and supplies
one common explicit scale per access unit. PTS and DTS may differ for video.
For AAC they must match, the clock must equal the configured sample rate and the
duration must be 1024. Track timescales can differ. Conversion truncates fractional
nanoseconds; preserve the original integer timestamps for actual muxing.

Create one `Arc<CodecConfig>` per encoder epoch and reuse its identity:

```rust
use std::sync::Arc;
use sapflow_client::{AacConfig, CodecConfig, EncodedSample, SampleTiming};

let config = Arc::new(CodecConfig::Aac(AacConfig::new(&[0x11, 0x90])?));
let time = SampleTiming::new(0, 0, 1024, 48_000)?;
let encoded: Arc<[u8]> = Arc::from([0x21, 0x10]); // Native encoder ownership handoff.
let sample = Arc::new(EncodedSample::new(config, time, false, encoded)?);
# Ok::<(), sapflow_client::media::MediaError>(())
```

`SharedSample` is `Arc<EncodedSample>`. The payload and codec configuration are
immutable reference-counted allocations. Sharing the same handle between the
network and recording queues copies neither the payload nor its metadata. The
native-to-Rust ownership conversion still needs a measured copy; this core does
not claim zero-copy FFI. The maximum compressed unit is 4 MiB, and each parameter
set is at most 4096 bytes. These are ingress protection limits, not qualified
resolution or frame-size promises.

Video framing validation rejects truncated/trailing lengths, zero-length NALs,
forbidden-bit NAL headers, unsupported NAL types, missing slices and IDR flag
mismatches. SPS/PPS remain out of band. The supported access-unit NAL types are
ordinary non-IDR/IDR slices, SEI, AUD and filler. This is framing/profile
validation of native encoder output, not a video decoder or full slice parser.

`media::SampleContinuity::validate` checks the first video unit is IDR and that
per-track DTS does not overlap the previous access unit. It refuses configuration
identity replacement. At an explicit discontinuity, clear the publishing queue,
segment/finalise the recording as appropriate, deliver the discontinuity to the
sinks and call `reset`. Next video must again be IDR. The adapter must apply
continuity validation before queue fan-out. Arbitrary live format changes are
not silently supported.

## Independent queue ownership

`queue::QueueLimits::new(bytes, samples, media_ns, residence_ns)` requires
positive budgets and at most 65,536 sample handles. Bytes count compressed
payload lengths; samples bound per-entry/configuration overhead. Media time is
`max(accumulated video duration, accumulated audio duration)`: concurrent tracks
each receive the full configured budget. It is not the sum of A/V durations.
Each per-track total is still a sum of access-unit durations, not a timestamp
range across gaps. Residence is measured from caller-supplied monotonic enqueue
time. Byte, sample, media-duration and residence limits remain independent.

`SampleFanout::new(publish_limits, record_limits, has_video)` exposes independently
drained publishing and recording queues. `push(shared_sample, now_ns)` returns
one `EnqueueOutcome` for each sink. A recording overflow does not reject network
ingress; network overflow does not reject recording ingress. A backwards clock
is rejected before either sink changes. Configure `has_video=false` for audio-only.

Publishing starts at an IDR when video is configured. On overflow it clears all
pending network samples and waits for the next IDR, dropping intervening audio
and dependent video. An incoming IDR can itself resume immediately after a flush.
A sample larger than an empty queue's byte/media budget is explicitly rejected.
This favours useful live media over replaying stale network backlog. The caller
should request a native keyframe when `waiting_for_keyframe()` becomes true.

Recording overflow permanently fails new ingress to that recording queue and
retains its accepted prefix for finalisation. It does not pretend a recording
with missing media remains healthy. Drain/finalise that prefix and create a new
recording queue only with explicit application recovery. No additional payloads
are retained after failure. The failed prefix may remain older than the residence
budget until finalisation; it is no longer an active ingest queue.

`BoundedQueue::pop(now_ns)` checks residence before yielding publishing media.
Call `tick(now_ns)` at/before the next residence deadline even when capture or
drain is idle. No queue can schedule its own timer. `clear` immediately releases
all held sample references; a failed recording queue remains failed. The adapter
owns its other references and platform buffers.

`QueueStats` reports current/high-water bytes and samples, separate queued video
and audio durations, their maximum as queued media duration, and media-duration
high water. It also reports accepted/dropped samples, total overflow events and
separate byte, sample-count, media-duration and residence overflow counters.
`NativeQueueStats` exposes publishing media milliseconds/high water, each overflow
reason counter and current keyframe-wait status; `FanoutOutcome` carries cumulative
publishing overflows so an adapter can request an IDR on a newly observed reset.
Counters saturate. Byte accounting
covers payload references per queue; process residency must also measure encoder,
configuration, allocation and transport storage. There is no unbounded per-show
history or sample list.

## Session and destination boundary

`session::Session` moves through `Idle`, `Preparing`, `Connecting`, `Sending`, `Live`,
`Reconnecting`, `Interrupted`, `Stopping`, `Stopped` and `Failed`. `start` returns
a new opaque `Generation`, supplied to every asynchronous result. `prepared`
requests `SessionAction::Connect`; `connected` records only the transport
handshake. Only destination-observed `media_confirmed` makes the session live.
Local preview, recording progress and replay publication remain separate facts.

`transport_failed` opens one reconnect window and returns a bounded exponential
retry time. `ReconnectPolicy::default()` uses a 90-second grace, 500 ms first
retry and 8-second maximum delay. Use `ReconnectPolicy::new` with the actual
destination-configured grace. The `NativeSession` FFI accepts the destination's
full `0..=3_600_000` millisecond range; larger values are refused rather than
clamped. Zero grace fails immediately without retry. Repeated failures and socket-only reconnects retain
the first deadline. At that deadline, `tick` refuses another connection. An
already accepted `Sending` attempt may continue with exhausted retry permission;
a handshake or disconnected attempt fails. `AwaitGraceDeadline` requests only the deadline timer when
there is insufficient time left for a retry; it never requests a connection at
or after expiry. Timely validated peer progress across the current attempt's
complete startup-media watermark clears the network window, without confirming
playback. Timely decoded media confirmation also clears the window; a later
independent outage receives a fresh deadline. See the exact attempt fences,
optional-ACK behaviour and native API in [transport recovery](transport-recovery.md). Retry is a request to the adapter, never a
hidden thread or network operation.

`replace_transport(generation, now_ns)` (FFI `replaceTransport`) requests an
intentional codec/socket replacement within an ongoing logical session. It clears
the old connected flag and retry timer and returns `Connect`. A healthy `Live`, `Sending` or
unconfirmed `Connecting` session returns to `Connecting` without starting a grace
window or counting a network failure. During an outage it stays `Reconnecting`
and preserves the exact original deadline and backoff history; at or after that
deadline it returns `GraceExpired` without another attempt. The replacement still
needs `connected` followed by destination-observed `media_confirmed` to become
live. Its first actual failure uses `transport_failed`; healthy rotation itself
must never call that failure method. Stop/cancel, generation fences and monotonic
clock checks also apply to replacement. Native must close/fence the old attempt
and enforce its own bounded preparation/handshake timeout.

`tick` consumes retry deadlines once and must be called even while disconnected.
The initial prepare/handshake operation needs an adapter-owned bounded timeout;
the reconnect window begins on its first reported transport failure. The adapter
must also enforce its destination credential expiry and prepare/refresh/status/end
request deadlines.

`stop` immediately advances generation, suppresses reconnect and returns
`EndDestination` with a new stop generation. Cancel native capture/transport
work, drain/finalise recording and deliver explicit destination end; only then
call `finish_stop`. `cancel` immediately fences old callbacks and marks local
state stopped. It does not claim that remote end was delivered. `interrupt`
fences callbacks and requires explicit end/cancel plus a new permission-validated
start. Capture permission and OS background execution remain platform authority.

Session generations distinguish complete start/stop/interruption epochs. Within
one generation, the transport adapter must cancel the previous connection attempt
before scheduling the next, and must fence its own superseded attempt callbacks.
A session token alone does not identify individual socket attempts.

`SessionStats` counts connections, observed media confirmations, failures, retries
and rejected stale callbacks. Reset queue/session objects for independent per-show
statistics, or calculate deltas from the snapshot.

Destination grants, scopes, endpoints and refresh/end operations belong to generic
application/transport adapters. The core recognises no V4V hostname, event path,
wallet rule or store entitlement. Every distribution must preserve always-free
V4V, and Zapstore remains unrestricted; no SDK billing/time limit is implemented.
An authenticated V4V connector classifies its own grant. Destination credentials
must never be copied into diagnostic statistics.

## Remaining integration and evidence

This package does not yet mux MPEG-TS/FLV/MP4, publish SRT/RTMP, supply TLS,
create a recording file, generate language bindings, run SwiftUI/Compose capture,
or play origin/RelaySwarm media. Those layers consume this boundary. The core
contains no unsafe code or new runtime; it reuses the checkout build target.

Invariant tests exercise descriptor rejection and truncation, shared ownership,
continuity, independent queue failure/recovery, sustained bounded backlog,
residence/clock handling, fixed reconnect deadlines, observed recovery and stale
stop/cancel/interruption callbacks. Parser entry points also require the repository
fuzz harness before exposed to imported/untrusted configuration data. Native
physical capture, installed XCFramework/AAR consumers, copy/CPU/energy profiling,
protocol interoperability and signed distribution acceptance remain open.

## MPEG-TS and SRT publishing

`srt::SrtPublisher` combines the existing bounded MPEG-TS muxer with our own
sans-IO SRT caller. `PublisherConfig` supplies fixed AAC/H.264 track configuration,
Caller packet/byte/age/pacing/crypto limits, and a separate sample residence cap
(default one second, configurable from nonzero through 60 seconds). Caller must
accept a 1316-byte payload and at least 1316 retained bytes; impossible chunk
budgets are rejected during construction. Encryption uses Caller configuration
and has no plaintext fallback when a secret is configured.

The adapter provides the destination address, monotonic `Instant`, UDP datagrams
and socket submission. `push(now, SharedSample)` accepts one shared compressed
sample only when connected and ready. A rejected sample is returned in
`RejectedSample` with its original Arc identity. Busy is backpressure, not a
request to clone or buffer another frame. Configuration changes require an
explicit new publisher/encoder epoch. Per-track timestamp checks remain in the
muxer; callback order across audio and video is not silently retimed.

Each `poll_transmit(now)` performs at most one mux step (seven TS packets), moves
at most one owned TS chunk into Caller and returns at most one paced datagram.
Submit a returned datagram immediately; on socket submission failure, stop this
publisher and let the existing session lifecycle decide recovery. The wrapper
adds no socket, worker, reconnect loop, congestion algorithm or retry policy.

A `None` datagram does **not** by itself mean idle. `has_immediate_work()` means
another bounded mux step can run now; `poll_timeout()` also returns an immediate
deadline in that case. Adapters should drain a bounded batch, yield, and schedule
the remaining immediate work. When the Caller queue is full, the exact rejected
chunk allocation remains pending, and immediate work is suppressed until an ACK
frees packet/byte capacity. A stray peer cannot unblock it. Continue servicing
Caller timers, incoming UDP and the sample residence deadline while blocked.

The storage bound is one mux-owned SharedSample plus one TS chunk of at most
1316 bytes, in addition to Caller's configured packet/byte budget and the muxer's
bounded metadata/configuration scratch. The sample residence deadline runs from
submission until its final chunk enters Caller. Caller's age limit starts at
chunk enqueue, so the two independent residence caps add; they are not one
shared timestamp budget. A backwards input clock is clamped and counted and
cannot extend a deadline. Sample expiry ends the connection and releases media;
it does not silently discard a partial access unit and continue.

The input Arc is cloned once to make rejection ownership-preserving; payload
bytes are not cloned. TS chunk Vec ownership moves directly into Caller through
`Caller::try_enqueue`, whose error returns the exact allocation. Existing mux
staging/serialisation copies and the Caller wire copy still occur, as do any
native FFI/socket copies. `PublisherStats` exposes those nested counters,
backpressure events, input handle clones, pending chunk length/capacity, peak
capacity and discarded pending bytes. These are accounted bounded copies, not a
zero-copy claim.

`stop(now)` releases the retained sample, pending chunk, Caller retransmission
queues and crypto state immediately. Poll once for a best-effort shutdown;
remote end delivery is separate. `PublisherEvent::Transport(Connected)` means a
transport handshake only. Destination-observed media confirmation still drives
`session::Session::media_confirmed`; neither queued bytes nor ACKs prove playback.
Local mux/residence failures appear as `PublisherEvent::Failed` and a stable
`failure()` value. Physical-device, public-network and sustained-performance
qualification remain separate from the core integration tests.

The activated SDK passed 36 regular tests, including encrypted multi-chunk
publishing, independent audio/video decode timelines, allocation-preserving
backpressure and immediate stop/expiry cleanup. Strict Clippy passed for both
the SDK and own SRT crate. The explicitly run external test
`generated_aac_through_real_wrapper_and_reference_receiver_decodes_identically`
sent 25 generated AAC samples through the actual SDK muxer and AES-256 Caller to
the independent libsrt receiver. All 15,604 received TS bytes matched, and
FFmpeg decoded both original and received audio to PCM SHA-256
`bee03c95aeccde25b9bc6eaaa24edaabf50e4943159f40c5fa4792b208aee9db`.
This is local audio-only integration evidence; it does not qualify native
bindings, physical capture, impaired mobile networks or sustained operation.
The test stops its receiver and removes its own temporary files.

## Worker-owned recording mux

`crates/sapflow-ffi/src/recording.rs` implements the recording mux boundary.
It reuses the existing HLS fMP4 writer and already-resolved
`bytes = "=1.12.1"`. The generated native packages need regeneration before a
consumer can use these newly exported types. Focused validation passed on
21 October 2026: 18 FFI tests, 95 HLS tests and strict Clippy; the explicit
fixture test is normally ignored and was executed for each check below.

`NativeRecordingMuxer(hasVideo, hasAudio, mediaEpochNs,
maxFragmentBytes, maxFragmentMs)` discovers each expected codec from the same
immutable `MediaSample` handles used for publishing. `push` stages handles;
`takeOutput` returns optional `RecordingWrite { bytes, rewriteHeader }` values.
The platform recording worker must drain output after every `push` and before
calling `finish`. `finish` emits the accepted tail and a same-length header
rewrite carrying final duration. `cancel` discards unwritten output and handles.
All calls use `try_lock`; no file, socket, codec, timer or runtime is owned here.

The mux accepts video, audio or both, requires the first video sample to be
an IDR, and refuses codec identity replacement or overlapping decode times.
Video is represented at 90 kHz; audio keeps its configured clock. Integer
rescaling preserves sample endpoint timing to one target tick. AAC above
65,535 Hz is explicitly unsupported by the existing MP4 audio sample-entry
representation. A recording format change starts a separate file.

Payload bytes, sample count and cross-track media span are bounded separately.
The maximum configurable fragment payload is 16 MiB, with at most 4096 samples;
fragment span is configured between 100 and 5000 ms, normally 1000 ms. Samples
longer than that span are refused. Codec discovery uses the same bounded staging
area. Missing expected tracks fail explicitly rather than producing a false
complete recording. Cuts occur before a byte/count/span limit and at per-track
DTS gaps, using the next fragment's original base time rather than stretching
preceding samples. Final duration follows the latest presentation endpoint,
including initial offsets and gaps. There is no duration-related RAM growth.

`Bytes::from_owner` retains an `EncodedPayload(SharedSample)` owner, so feeding
Sapflow's existing `fmp4::Sample` API adds no per-consumer payload clone. The
existing fMP4 writer assembles payload into an `mdat` buffer, then copies that
buffer into the final fragment. Both assembly copies remain accounted. Converting
its uniquely owned output `Bytes` into `Vec<u8>` transfers ownership. UniFFI
record serialisation and Swift/Kotlin byte-output transfer add separately
accounted copies; exact binding and platform-write costs remain measurement work.
Fragment assembly and metadata allocation costs also remain benchmark work.

Android should create a session file with `FileChannel` and `CREATE_NEW` on the
owned recording worker, write ordinary chunks fully, seek to zero for the final
header rewrite, force and close. No disk operation belongs on codec callbacks
or UI work. Queue overflow stops recording ingress independently of publication;
finish any accepted prefix where storage permits, retain incomplete files on
failure, and report incomplete status. Worker cancellation/finalisation needs an
explicit finite control deadline. Successful muxing alone does not prove
physical-device recording, seek compatibility, low-storage handling or durability
after process termination.

Independent FFmpeg checks decoded a three-second synthetic 720p30 H.264/AAC
recording (90 video and 141 audio frames), audio-only recording (141 frames),
and a recording with the middle second removed (60 video and 94 audio frames).
All encoded packet hashes and PTS values matched the selected source, and all
video frame hashes matched independently decoded source frames. Both video files
also sought to 2.2 seconds and decoded the expected source frame. Duration stayed
3.008 seconds in the gapped file, preserving the gap. The retained receipt/logs
are under `target/native-recording-check/`; owned fixture/build-input scratch was
removed and no server was started. These desktop mux/IO checks do not qualify
Android FileChannel lifecycle, physical codecs, storage failure or durability.

## Native live telemetry

The native applications expose three separate facts: Capture elapsed,
Broadcast elapsed and Socket outbound. Capture elapsed starts at the actual
first committed Start and follows that foreground capture intent through
rotation and reconnect gaps. Stop or foreground interruption freezes it.
Broadcast elapsed is absent for record-only capture and freezes at actual
destination End, expiry or terminal failure while independent recording may
continue. Neither clock is the written file's media duration. Prepare, a
pending Start and a transport handshake do not start either clock.

Socket outbound counts successful local socket completions from the existing
transport adapters. iOS uses the existing Network.framework
[`contentProcessed`](https://developer.apple.com/documentation/network/nwconnection/sendcompletion/contentprocessed(_:))
callback. Android counts a complete successful application write or whole
[`DatagramChannel.write`](https://developer.android.com/reference/java/nio/channels/DatagramChannel)
return. RTMP counts protocol bytes; RTMPS excludes TLS, TCP and IP overhead.
SRT includes its datagram headers, control and retransmission bytes but excludes
UDP and IP overhead. This is neither NIC throughput, remote receipt nor decoded
playback. The existing current-attempt origin observer remains the source of
decoded-media confirmation.

The hook adds scalar accounting to existing send completions, with no payload
copy, extra FFI call, packet timer or packet dispatch. Cold reads occur at most
once a second. A rate requires two complete monotonic samples, each fresh
within three seconds and separated by no more than three seconds. Warm-up,
stale clocks, unsupported accounting, overflow, ambiguous/partial writes and
absence of an active socket have unavailable rates. Zero requires a valid
complete interval with no completed bytes. Positive rates below 1 kbit/s display
as `<1 kbit/s`. A historical incomplete total remains labelled incomplete;
a new complete socket warms up its own rate without bridging the failed socket.

Attempts are correlated to their actual destination, epoch and token. Actual
close folds their final scalar accounting before admitting a successor. Final
totals may be lower bounds when cleanup or writes are unconfirmed. There is no
unbounded attempt history. End retains the final total with an unavailable rate.
On iOS one outstanding cold read is allowed per show; Android reads bounded
scalar snapshots from its existing display refresh without queuing statistics
jobs. Statistics cannot admit media, revive a stopped intent, bypass policy or
delay resource cleanup. Physical energy and sustained throughput costs remain
measurement work rather than an inference from this source design.

## Android private recording catalogue and export

The Android recording adapter groups independently written segments by a
private show identity and ordered segment identities. Rotation produces a new
segment in the same show. Small bounded sidecar receipts distinguish active,
recorder-finalised, incomplete and unverified files. Finalisation requires
actual mux finish/drain, file force and close; it does not establish decoded
playability, a complete show, remote replay publication or crash repair.
Missing, corrupt or mismatched receipts never promote an orphan to complete.

Prepare performs its local recording capacity and free-space check on a cold
IO worker, using the immutable selected profile. The initial storage budget is
a 60-second bitrate horizon plus headroom and reserve, not a duration limit or
a promise that future writes will fit. Low or unavailable storage visibly
refuses preparation. Turning recording off is an explicit user action. The
worker result is time-bounded and correlated to the original foreground
Prepare; late permission, grant and storage replies cannot revive cancellation
or silently select a different destination. The recorder checks capacity again
when it actually begins writing.

The catalogue bounds scans to 256 segments and 1024 directory entries. Capacity
failure preserves existing files. Its current list supports inspection and
explicit copying, without automatic deletion or repair; catalogue management
and pagination remain follow-on work. Listing a live recorder is permitted,
but the document picker requires capture to be stopped so choosing an export
destination cannot interrupt a live show. Unfinished and unverified files need
an explicit confirmation and retain their status after export.

One application-owned cold IO slot survives Activity recreation. The document
picker receives no private path or file handle. Its original opaque selection
is retained until the system reply is consumed; an old reply cannot choose a
successor file. The worker revalidates exact file and receipt identities before
opening the provider, copies in 64 KiB chunks, detects truncation, growth and
replacement, and closes actual input/output owners before releasing capacity.
Cancellation preserves the private original; a partly written provider document
may remain. A failed or uncertain close retains capacity instead of treating a
timeout or repeated no-op close as proof of cleanup.

The process registry prevents a second local channel being opened on an owned
lock inode, including rejected writers and aliases. This matters because the
Java [`FileLock`](https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/nio/channels/FileLock.html)
contract permits closing another channel to release process locks on the same
file. Healthy live ownership and abnormal cleanup are separate scalar states:
a successful live scan or an ordinary busy refusal must not permanently block
the library. Cross-process locking, receipt bounds and private originals remain
independent of the media/network queues. Generated host-file tests do not
qualify Android filesystem behaviour, document providers, process termination,
physical storage failure or device media playback.

## iOS private recording catalogue and export

The iOS adapter uses the same show/ordered-segment and explicit unverified-file
policy with native compressed AVAssetWriter inputs. New media is app-private;
legacy Documents files stay in place and unverified. Bounded sidecar receipts
record actual writer completion, file synchronisation and close. Prior loss
remains incomplete even when the container finishes. Receipts are not decoded
playability, whole-show or authenticity proof. There is no automatic repair,
deletion, migration or replay publication.

Catalogue scans are bounded to 256 segments, 1024 directory entries and 4 KiB
receipts. One app-owned catalogue holds a lifetime directory lock. Healthy live
ownership permits metadata scans; uncertain cleanup retains ownership. Prepare,
actual Start and rotation recheck capacity and selected-profile storage before
media admission. iOS retains its strict greater-than-200 MiB floor alongside the
60-second bitrate budget, headroom and reserve. The Android and iOS space floors
are separate platform policies, not an SDK guarantee. Low-space recording stops
preserve an incomplete prefix while the current publisher continues; later
rotation still requires a successfully finalised segment to restart.

Export and actual capture reservation share a scalar admission gate. The folder
picker requires idle capture and receives no private URL or handle. Its exact
selection stays reserved through actual dismissal; a reply, Cancel or owner
close alone cannot release it. A cold worker acquires security-scoped access,
coordinates the selected directory, revalidates the original file and receipt,
and makes an exclusive copy in chunks no larger than 64 KiB. Actual byte count,
created inode, sync and close are checked before success. Scope ends only after
actual coordination and resource cleanup. Unknown close retains the owners,
scope and directory lock; a new owner cannot bypass that state. Cancellation
preserves the private original and may leave a partial destination copy.

Generated host-file tests exercise the catalogue, gate, copy, scope and close
failure seams. Generated macOS media also exercises the actual production
CompressedRecorder/AVAssetWriter and independently decodes finalised and
incomplete prefixes. These checks do not qualify physical iOS storage, document
providers, UI dismissal, process termination, crash recovery or power loss.
