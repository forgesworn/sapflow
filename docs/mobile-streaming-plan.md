# Mobile broadcaster and streaming SDK delivery plan

Build a standalone broadcaster for iOS and Android on a reusable Sapflow SDK.
V4V is the first integrated destination. The app must also work with a manually
configured compatible SRT destination, so its basic broadcasting workflow does
not require a V4V account.

The first release delivers reliable broadcasting, local recording and origin
playback. A subsequent release adds qualified native RelaySwarm playback. The
useful first outcome is a creator completing a real show from a phone, with
viewers watching and a usable recording surviving an interrupted uplink.

## Confirmed scope

The owner selected these product boundaries on 20 October 2026:

- A standalone mobile broadcaster backed by the SDK.
- Reliable broadcasting and origin playback first, then native RelaySwarm.
- Native performance throughout, with deliberate reuse of suitable ForgeSworn
  libraries and measured copies, scheduling, power and sustained throughput.

The owner added these requirements on 21 October 2026:

- Use our own SRT and RTMP protocol implementations, extending them for mobile
  publishing; RTMPS uses our RTMP client over maintained TLS.
- Aim to outperform Larix, using matched physical-device measurements.
- Publish the app as open source, with a free Android edition on Zapstore.
- Always allow free streaming to V4V in every distribution, including Apple
  App Store and Google Play builds.

The [competition, feature and distribution review](mobile-competitive-review.md)
records current Larix, Moblin and IRLPro findings. Its recommendations for the
general release expand the engineering pilot below. Store pricing and the
proposed 30-minute allowance for other destinations remain product proposals.

The [native performance and library reuse contract](native-performance-contract.md)
is required for B0, B1, B3 and release qualification. It fixes native platform
interfaces, hardware codec use, single-encode fan-out, ownership and benchmark
requirements for existing ForgeSworn components.

The [reusable library and OBS plan](library-quality-plan.md) preserves the
broader goal of making these libraries independently useful and production
quality. Its LQ1 to LQ8 work queue covers APIs, packages, interoperability,
performance evidence and possible OBS or desktop consumers alongside the
mobile milestones. Specific OBS adapters and a desktop studio remain proposals.

This is an implementation plan. Its milestones are open. Sapflow's existing
[M0 to M7 goals](../GOALS.md), including the decision to keep experimenting
before a production replacement, retain their own acceptance requirements.

Use Sapflow Broadcaster as a working name. Choose the public product name and
app identifiers before signing distributable packages.

## Engineering pilot and general release

The creator can configure a destination, check camera and microphone, select a
supported quality preset, start a broadcast and see whether the destination
is actually receiving playable media. They can monitor audio levels, actual
outbound bitrate, connection state and recording status. Ending a show stops
reconnection, closes the destination session and finalises the recording.

The first engineering pilot includes these capabilities:

- iOS and Android apps using the same versioned Rust broadcaster SDK.
- Foreground capture with hardware H.264 video and native AAC audio encoding.
- One active destination and one rendition, starting with 720p30 and 1 s
  keyframes. Support fixed 480p and 720p presets on the qualified devices.
- Encrypted SRT publishing, with explicit latency and bitrate limits.
- A V4V connector for stream preparation, scoped publish credentials, status,
  explicit end and the existing replay workflow.
- Manual compatible SRT endpoint configuration, separate from V4V identity.
- Local preview and muted origin monitoring, with received-media confirmation
  distinguished from a connected socket.
- Bounded reconnection and bounded bitrate adaptation within the negotiated
  resolution and codec configuration.
- Independent local recording that continues through a network outage while
  the app retains capture permission and execution time.
- Visible interruption, storage and thermal states, with understandable
  recovery actions.
- Seekable recordings, explicit export/recovery and a verified server replay.

Camera broadcasting in the initial supported profile is a foreground
capability. Locking the phone, losing permission or an OS interruption must produce the defined paused
or interrupted state. Qualify resumption separately; do not advertise continuous
background camera capture.

SRT requires a usable UDP path. A blocked path must fail visibly and preserve
the local recording. The SRT pilot can precede RTMPS; the general release
requires our RTMP publishing client and RTMPS qualification under B3R.

The recommended general v1 also includes B4R: audio-only broadcasting with
qualified background operation, tested external audio, basic camera controls,
fixed portrait/landscape choices, privacy pause/standby and saved destination
profiles with reviewed import. Qualify 1080p30 on capable devices; keep 720p30
as the common acceptance profile. This does not promise every input device,
live format changes or background camera operation.

HEVC/HDR/4K, screen capture, advanced overlays, multicamera, multiple phone
destinations, bonding and studio integrations follow demand and separate
acceptance. Existing V4V audio listening remains its own capability.

## Distribution and free V4V broadcasting

V4V broadcasting is unlimited and watermark-free in every official build. No
subscription, purchase restoration, billing server or trial timer may block
preparation, publishing, reconnecting, recording or ending a V4V show. Normal
V4V authentication and publishing grants still apply. This is an app pricing
promise, not a change to the destination's existing service policies.

The proposed Android Zapstore edition offers unrestricted app features and
unlimited broadcasting to compatible destinations without Google Play or a
billing dependency. Optional hosted services have their own explicit terms. Keep
store billing in optional application adapters, outside the reusable SDK,
native capture and transport callbacks. Keep open-source SDK use unrestricted
by an app-store entitlement.

The owner is considering 30 free minutes for other destinations in Apple and
Google store builds, followed by monthly or annual payment. This is not yet a
frozen pricing or reset rule. Compare it with free basic streaming plus paid
professional tools/services before implementing a cutoff. Moblin already
offers all streaming features free, so a timer alone is a weak advantage.

Classify a V4V destination from its authenticated connector and trusted grant,
not an editable label or hostname substring. Free V4V must remain available
after an unrelated store trial expires or a subscription cannot be checked.
Cover these cases in B4/B5 acceptance alongside the distribution requirements
in the competitive review.

## Repository responsibilities

| Repository or package | Responsibility |
| --- | --- |
| sapflow | Origin, existing HLS packaging and SRT code, new sender/client SDK, native API contract and protocol fixtures |
| Standalone broadcaster repository | SwiftUI iOS app, Jetpack Compose Android app, native capture/codec adapters, local preview and creator workflow |
| v4v | Destination authorisation, event lifecycle, audience stage, payment authority and replay publication |
| relayswarm | Existing session and signed-media contracts, browser compatibility, peer scheduling and acceptance |
| relayswarm-kit | Existing Swift interoperability implementation and independent comparison client |
| foregather-kit | Reusable native packaging pattern; invitation transfer only if the connector needs it |
| forgesworn-link and shelter-kit | Later recording transfer, storage and repair integration |

Use native SwiftUI and Compose applications for the standalone product.
Share media/session behaviour through Rust; each platform
owns its camera, microphone, audio routing, permissions and lifecycle.
React Native consumers can receive an adapter later without becoming a first
release dependency.

Keep the new client and FFI packages in the Sapflow workspace initially.
Proposed package names are sapflow-client and sapflow-ffi; native wrapper
packages expose the Swift/Kotlin APIs. Extract another repository only when
an actual consumer or release requirement justifies it.

Keep the current ban on unsafe code in the media/protocol implementation.
Establish how generated FFI code fits that policy in the native packaging
spike; do not silently disable workspace lint rules.

## Architecture

The phone's native capture and hardware encoder produce compressed samples.
A small native adapter supplies codec configuration, timestamps and keyframe
markers to the Rust client. Rust owns transport/session state, bounded queues,
MPEG-TS/SRT and FLV/RTMP packaging, transmission and common statistics.

### Owned protocol implementations

Reuse and extend sapflow-srt for the SRT caller/sender. Reuse our RTMP chunk,
AMF and compatible handshake machinery in sapflow-hls for the new publishing
client. The inspected implementations are a receiving SRT listener and an
RTMP ingest server; neither establishes that a mobile sender already exists.

Ship our protocol state machines rather than substituting the Larix SDK,
libsrt, srt-rs or another third-party broadcaster/RTMP SDK. This requirement
does not mean implementing codecs, TLS or cryptographic primitives ourselves.
Use maintained platform encoders, TLS with certificate validation and reviewed
cryptographic dependencies. Preserve the existing SRT source-inspection ban.

The recording sink receives encoded samples independently of the network
sender, so a blocked uplink cannot cause recording backpressure or unlimited
memory growth. Use the existing fragmented MP4 work where it meets the phone's
recovery and playback requirements.

The origin packages the received stream for HLS. Native AVPlayer and Media3
adapters, and the existing browser player, consume origin media. The later
RelaySwarm adapter supplies verified media with a deadline and origin fallback.

Keep raw camera frames outside Rust bindings and any JavaScript bridge.
Measure copying, allocation and callback overhead for compressed samples.
Document which component owns each buffer and how cancellation releases it.

Keep V4V event paths and callback conventions in the destination adapter.
The reusable client accepts a bounded destination description and stream id;
manual publishing must not depend on V4V's event naming or server callbacks.

V4V's authorisation, split calculations, entitlement and settlement rules stay
at their existing authority boundary. The broadcaster consumes a narrow
destination API; it does not copy those rules or become a wallet.
The local preview and actual remote playback must have distinct status.

## Milestones and dependencies

The B identifiers belong to this plan and do not renumber Sapflow's M goals.

| ID | Deliverable | Depends on | Completion evidence |
| --- | --- | --- | --- |
| B0 | Frozen contracts, test profiles and baseline | Existing source review | Versioned interface, selected devices, measured reference and recorded budgets |
| B1 | Native SDK and platform feasibility | B0 | Packaged Swift/Kotlin consumers and physical capture-to-recording proof on both platforms |
| B2 | Resilient and authorised origin | B0 | Existing ingest gates plus real player continuity, encryption and destination authorisation checks |
| B3 | SRT sender and reusable client SDK | B0; B1 validates the boundary; B2 supplies the integration target | Black-box interoperability, loss/reorder tests and native SDK consumers |
| B3R | Our RTMP publishing client and RTMPS | B0, B1 and shared B3 sample/session interfaces | Independent receiver interoperability, certificate validation, bounded TCP queues and physical network qualification |
| B4 | Standalone creator workflow | B1, B2, B3 | A complete phone-to-origin-to-viewer-to-replay journey on iOS and Android |
| B4R | General-release creator essentials | B4; B3R for RTMPS destinations | Qualified audio-only/background and external input, camera controls, pause/standby, profiles and free-V4V behaviour |
| B5 | Qualified general release | B4, B3R, B4R | Sustained physical runs, installed release packages, SDK consumption, distribution behaviour and release readback |
| B6 | Native RelaySwarm release | B5 plus RelaySwarm's existing acceptance gates | Verified native/browser media, playback fallback, consent and measured net benefit |
| B7 | Wider formats and recording distribution | B5; B6 only where a feature actually uses peers | Separate rendition, advanced input and storage acceptance |

B1 and B2 can progress independently after B0. Sender mechanics in B3 can
progress alongside the native packaging work once the compressed-sample
interface is stable. B4 should begin as a thin vertical slice as soon as both
platforms can feed the sender.

The native peer-to-player feasibility experiment can run during B1, but a
failure there must not block the agreed origin-first release.

### B0 Contracts and baseline

Record the current source revisions and evidence paths before implementing.
Read the current source rather than treating dated handovers as runtime truth.

Freeze:

- The sample format: codec configuration, timescales, PTS/DTS, keyframes,
  discontinuity and configuration-change events.
- The broadcaster state machine: idle, preparing, connecting, live,
  reconnecting, interrupted, stopping, stopped and failed.
- A destination interface for prepare, credential refresh, observed status
  and explicit end, plus a manually configured destination implementation.
- Queue budgets, overflow actions, retry deadlines and cancellation ownership.
- The reconnect contract with V4V. Its current default grace is 90 s; the
  connector must consume the configured value and preserve the rule that a
  repeated drop does not extend the original grace deadline.
- The distinction between preview, transport-connected, media-confirmed,
  recording and replay-published states.

Select the actual device/OS matrix and minimum supported versions. Begin with
one physical iPhone and one physical Android; widen to an older supported
iPhone and a representative midrange Android before the general v1 claim.

Measure a reference broadcaster on each device with the same encoder settings,
screen state, thermal starting point and network profile. Record energy,
process memory, encoding drops, bitrate and capture-to-screen delay. Use an
optimised equivalent MediaMTX configuration for origin comparisons.

Use Larix on both platforms, Moblin on iOS and IRLPro on Android as the initial
app comparison set. Record exact versions, enabled features and limitations;
use unrestricted, properly licensed runs where the baseline needs them.
Freeze the native performance contract's comparison method and improvement
targets before optimisation. An unmeasured expectation of superiority is not
acceptance evidence.

### B1 Native feasibility

Build a tiny packaged consumer for each platform before the product UI,
following the native performance contract:

1. Capture and hardware-encode 720p30 video plus AAC audio.
2. Pass compressed samples through the proposed bindings for 30 minutes,
   including cancellation and repeated start/stop.
3. Produce a seekable recording and demonstrate recovery after terminating
   the app process.
4. Measure FFI copies, queue high-water marks, memory and timestamps.
5. Exercise permission refusal, audio interruption, lock/resume and a camera
   configuration change with an explicit supported or refused result.
6. Install from the actual XCFramework/AAR-based package, rather than relying
   only on a desktop Rust test or an app's source checkout.
7. Prove one hardware encode feeds recording and publishing, and record copy,
   allocation, callback and runtime costs. Exercise 1080p30/60 as engineering
   capacity profiles on capable hardware alongside the 720p30 product profile.

Use Foregather's packaging and installed-consumer approach as a starting point.
[UniFFI](https://mozilla.github.io/uniffi-rs/latest/) generates language bindings;
cross-compilation, packaging and device execution remain explicit work.

For the later peer release, conduct a separate bounded experiment that feeds
verified media into AVPlayer and Media3 with cancellation and HTTP fallback.
Android's [data-source extension](https://developer.android.com/media/media3/exoplayer/customization)
provides a documented starting point. Record the chosen iPhone approach and
its physical-device limitations before committing to the player adapter.

### B2 Origin resilience and authorisation

Complete the relevant existing M6/M7 work without weakening its gates:

- Correct the changing EXT-X-TARGETDURATION behaviour. Keep the declared
  bound stable and define a restart/refusal policy when an encoder exceeds
  it. [RFC 8216 section 6.2.1](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.1)
  requires the value to remain constant.
- Preserve valid PART-TARGET timing under real capture jitter and stalls.
- Preserve a logical broadcast across a temporary encoder loss with correct
  HLS sequence/discontinuity and recording behaviour. Existing AVPlayer,
  Safari, Media3 and hls.js viewers must recover without reopening the page.
- Distinguish a transient transport loss from the creator's explicit End.
  Test the race between End, a delayed callback and a reconnect.
- Define codec/resolution/audio configuration changes: support a valid
  transition or refuse it visibly before corrupting the current playback.
- Add event-scoped publisher authorisation, expiry, renewal and revocation,
  with duration/storage limits and denial for an ended or unknown event.
- Complete own-SRT encryption and interoperability before making it default,
  retaining the existing M6 transition requirements.

SRT media encryption does not hide all control traffic or replace application
authorisation. Its [specified scope](https://datatracker.ietf.org/doc/html/draft-sharabayko-srt-01#section-6.1.1)
is the data-packet payload. Treat stream identifiers and handshake metadata as
observable. Provision per-session encryption secrets over the authenticated
destination channel; do not carry a durable account secret in a stream id.
Document how short-lived admission information and encryption jointly prevent
an observed stream id from granting a new publish session.

### B3 SRT sender and client SDK

Implement caller mode, handshake and encryption negotiation, MPEG-TS writing,
packet pacing, bounded retransmission state, ACK/NAK handling, congestion
response, too-late drop, cancellation and statistics.

Keep protocol logic sans-IO where practical. The platform adapter owns sockets,
timers and OS network-change signals. Use maintained cryptographic primitives
and independent vectors; do not implement AES or key-wrap primitives ourselves.

Preserve Sapflow's source boundary: develop our SRT protocol from the
specification and observed wire behaviour. Run libsrt and srt-rs only as black
boxes; never read or copy their implementation source.

Test both independent directions: our sender to a reference receiver, and a
reference sender to our receiver. Two implementations that share our own
mistake must not be the only interoperability evidence.

Extend the existing six-seed loss harness with reordering, duplication,
bandwidth reduction, longer RTT, address/network changes and encryption
refresh. Keep full loss profiles and failure records. Unit/fuzz tests,
black-box loopback and physical carrier tests have separate verdicts.

Ship a minimal CLI/example consumer before connecting the full app.

Version and distribute the SDK independently from the app. Keep a pinned
consumer example, supported target/ABI list, dependency licences and checksums
with each release. An internal workspace dependency is not a consumable SDK
release.

### B3R Our RTMP and RTMPS publishing client

Implement the client handshake and connect/createStream/publish state machine,
FLV sample/configuration writing, acknowledgements, ping/control handling,
timeouts, bounded TCP output and cancellation using our existing wire modules.
Keep the current ingest server's behaviour and tests intact; extract shared
pure code if client/server coupling would otherwise obscure ownership.

RTMPS wraps our client in maintained TLS with hostname/certificate validation.
Test against an independent receiver as well as Sapflow, including certificate
failure, stalled writes, reconnect and bitrate reduction under backpressure.
Protocol fixtures/fuzzing and physical runs must cover both platforms.

A manually selected RTMPS destination is sufficient for general v1. Automatic
SRT-to-RTMPS failover needs an explicitly authorised alternative, one logical
show and a defined stop-before-switch policy; do not silently downgrade TLS
or create simultaneous duplicate publishers. Plain RTMP is an explicit
destination choice, never an automatic security downgrade.

### B4 Standalone broadcaster

Build these creator flows around the SDK:

- Destinations: manual SRT configuration and V4V connection in the pilot;
  add RTMPS when B3R passes. Validate the endpoint and require explicit user
  confirmation before transmitting.
- Prepare: event selection, device preview, microphone level, quality preset,
  recording storage check and connection test.
- Live: elapsed time, audio level, actual bitrate, remote media confirmation,
  recording status and clear reconnect/interruption states.
- Finish: explicit End, local recording finalisation, export/recovery, remote
  recording status and replay link where the destination supplies one.

Use the V4V API for event lifecycle and replay actions. Preserve its existing
idempotency and authority rules; it remains responsible for payment and
recipient decisions. A manual destination must run without the V4V connector.

Keep origin monitoring muted by default and avoid feeding received programme
audio back into the microphone. A recording recovery upload must not silently
replace an already published replay.

Require actual decoded audio/video progress before displaying remote media
confirmation. Where a destination exposes no such observation, display the
narrower connected/sending status honestly.

### B4R General-release creator essentials

The [explicit AAC-only origin contract](audio-only-origin.md) defines the
server/SDK prerequisite. Native mode selection, background operation and physical
acceptance retain the requirements below.

Qualify these additions before the general release; the first SRT engineering
pilot remains a smaller vertical slice:

- Audio-only AAC publishing, recording and origin playback without requiring
  a video keyframe. Prove background/lock behaviour under each supported OS's
  actual permissions and execution policy.
- At least one documented external audio route on each platform, including
  tested USB audio where supported. Expose supported routing/gain, stereo/mono
  and a music-appropriate processing mode; do not assume every accessory works.
- Focus/exposure/white-balance controls where supported, zoom and front/rear
  switching. Select portrait or landscape before starting; keep the advertised
  encoder configuration stable while live.
- Pause and pre-start standby that send an intentional replacement picture
  and silence, with matching recording policy. A failed replacement must not
  reveal camera frames or microphone audio.
- Saved destinations and reviewed profile/QR/deep-link import. Validate scope
  and endpoint, protect short-lived secrets and require an explicit start;
  importing a profile must not itself transmit media.
- No timer, watermark or subscription dependency for V4V in any build; trial
  expiry and billing/network failure for other destinations cannot change it.

Test advertised 1080p30 profiles against the same sustained quality and resource
requirements. Engineering capacity results alone do not add a supported preset.

### B5 First release qualification

Treat the following as initial acceptance targets. B0 may revise a target
from measured baseline evidence before implementation is committed to it.
After the test profile is frozen, retain every failure and record any target
change explicitly; never silently relax a failing requirement.

| Measure | Initial target and method |
| --- | --- |
| Nominal stream | 720p30 H.264, AAC 48 kHz, 1 s keyframes, 2.5 Mbit/s video and 128 kbit/s audio; record negotiated settings |
| Reference network | At least 10 Mbit/s usable uplink, 60 ms RTT, no injected loss; SRT latency 2 s; measure actual conditions |
| Startup | p95 at most 8 s from Start to confirmed remote decoded media across 20 starts per primary device |
| Capture to screen | p95 at most 5 s on the reference profile, measured with a visible clock and real viewer |
| Sustained playback | Two hours per primary physical device with audio/video progress, no unexpected stop and no stall on the reference profile |
| Synchronisation | Absolute audio/video offset at most 100 ms at start, after recovery and at the end of the sustained run |
| Reconnection | 3 s, 10 s and 30 s outages plus ten Wi-Fi/cellular changes; resume remote decoded media within 10 s after a usable path returns, within the destination grace |
| Explicit stop | No retry or resurrected event after End, including delayed callbacks and app resume |
| Recording | Continues during foreground uplink loss, seeks and exports; after process termination recover through the last completed fragment, with at most 2 s of trailing media loss |
| Resources | Every queue has a tested byte/time bound; no growing backlog or persistent memory growth during the two-hour run |
| Power and heat | Energy per delivered encoded minute no worse than the comparable native reference beyond measured repeat-run uncertainty, with at least three matched runs; no thermal shutdown or hidden sustained profile reduction |
| Native media overhead | Meet the separate copy, FFI, SDK CPU, queue and scheduling budgets in the native performance contract |
| Permissions and interruption | Refusal, revocation, call/audio interruption, lock/resume and low storage produce the documented state and recovery |
| Destination isolation | Wrong event, expired/revoked grant, wrong encryption secret and ended event cannot publish; manual mode does not inherit V4V credentials |
| Free V4V | Continue beyond 30 and 60 minutes with no watermark or payment; expired/unrestored subscription, unavailable billing and an expired external-destination trial do not block start/reconnect/end |
| Distribution | Signed Zapstore/direct Android build runs without Google Play; store build purchase/restore/cancel behaviour cannot interrupt a free V4V show |

Loss profiles must also pass their frozen quality and recovery criteria.
Keep degraded-network allowances separate from the no-loss stall requirement.
Record charging, screen brightness, radios, OS, firmware, ambient temperature,
bitrate, codec profile and actual media duration for power comparisons.

Run the origin on Linux behind its intended TLS proxy. Measure 250 and 750
full media consumers on a separate load machine with enough link capacity.
750 receivers at the nominal bitrate need about 2 Gbit/s of media alone;
the current part-plus-segment harness can require more. A gigabit link or
playlist-only cohort cannot close that capacity gate.

Compare equivalent origin settings, including MediaMTX's CDN/session mode,
recording, ingest, TLS and authentication. Keep origins, generators and
network bottlenecks identifiable in each receipt.

Release acceptance has distinct records for:

- SDK tests, fuzzing and CI at the candidate revision.
- Installation of versioned Swift/Kotlin packages in fresh consumers.
- Physical iOS and Android runs and owner acceptance of the creator workflow.
- Signed app packages, installed upgrade behaviour and distribution readback.
- Pilot/deployment health, rollback and a separate production replacement
  decision under the existing Sapflow goals.

Use an installed beta to reach physical acceptance before general distribution.
Record TestFlight/store/direct-APK results by distribution route. An archive
or simulator run alone does not establish a shipped mobile release.

### B6 Native RelaySwarm release

Deliver:

- Origin-side signed commitments for Sapflow's exact immutable output.
  Preserve the existing commitment schemas, author/delegate trust,
  revocation, URI/range, sequence, discontinuity and part context.
- A native receiving/scheduling layer with bounded peer deadlines,
  verified-byte delivery and origin fallback.
- Native-to-browser and browser-to-native interoperability against the
  public session contract. Link's QUIC lane is a separate protocol and
  cannot substitute for browser WebRTC interoperability.
- AVPlayer/Media3 integration, playback cancellation and seek/rejoin behaviour.
- Receive-first mobile policy, explicit capped upload consent, metered-data
  handling and an origin-only switch.

Preserve RelaySwarm's existing P01 to P12 requirements, including the matching
short qualifier before the 7,200 s and churn run. Run the new mobile matrix
in addition to those checks; do not use the new app to bypass existing failed
gates.

Measure matched origin-only and peer-assisted runs. Count TURN, signalling,
commitments and peer traffic, alongside startup, stalls, energy and heat.
Ship the option only where measured benefit and reliable playback justify it.
Origin playback remains usable with no peers and after an opt-out.

### B7 Wider formats and recording distribution

Track each expansion separately:

- Multiple aligned renditions and switching; decide where transcoding lives.
- Automatic authorised transport failover beyond the manually selected B3R path.
- Advanced audio routing/accessories, HEVC/HDR/4K and higher video profiles.
- Bonding and studio features only after independent requirements, receiver
  integration and measured reliability/energy acceptance.
- Explicit encrypted recording upload/fetch through Link/Shelter, with
  retention, integrity and recovery. Preserve public replay and private
  backup as separate grants.

KithMoot's interactive call transport remains outside the broadcast protocol.
Share a proven recording or native packaging component where appropriate,
while preserving each application's media and authority contracts.

## First implementation packets

Each packet should leave a reviewable result with a specific acceptance record.

1. B0 contract and baseline: state transitions, destination interface, device
   matrix, source manifest, ForgeSworn adoption map, ownership/copy and thread
   diagrams, and frozen test profiles and native performance budgets.
2. B1 packaging proof: Swift/Kotlin installed consumers, hardware capture,
   compressed-sample handoff, single-encode fan-out and native performance
   measurements with start/stop/cancellation evidence, before a full app.
3. B2 playlist correctness: stable target duration with encoder-overrun policy
   and a real player regression.
4. B2 reconnect continuity: same logical show, real viewer recovery, recording
   policy and agreement with V4V's configured 90 s default grace.
5. B3 sender CLI: bounded unencrypted lab journey against the black-box
   reference, followed by encryption and application admission. Keep it a
   lab milestone until those latter requirements pass.

A new coding session starts from the earliest incomplete dependency and
checks its source/evidence first. Do not interpret this plan as instruction
to create multiple worktrees, workers, emulators or background services.

## Evidence and release records

Retain compact receipts containing source revision, dependency/artifact hashes,
device and network profile, commands, duration, verdict, metric definitions,
failure traces and cleanup. Keep private credentials and infrastructure
addresses outside committed evidence.

All Rust builds reuse each checkout's own target directory and retain the
existing CARGO_INCREMENTAL setting. Stop task-owned fixtures and emulators,
remove owned staging after retaining results, and remove only clean, pushed,
merged task worktrees with Git's worktree commands.

Current local planning references:

| Component | Inspected revision |
| --- | --- |
| sapflow | 2a6eb8f9aa6b3ad4226ad2b509a8820efc431e0e |
| v4v | 2b5daec42a2b86bae17df0cf02777ddca0695cd5 |
| relayswarm | dfedbb6fbae96b77bafb2156a5fb04849c12b4db |
| relayswarm-kit | 2f0b26ace4096a5dca87baf715274f91c331ac3a |
| foregather-kit | a7daeb6976e02b6a0fe159272f272f31d7fd8ee9 |
| forgesworn-link recording delivery worktree | 6d9a9212dbf868bdd6dfa4624ea17ff88b42b8c4 |
| shelter-kit | 827bc71a349124296b65bbc14df1e1f39f48dc13 |

These are checkout references, not assertions about current releases or
deployments. The preceding source review passed 176 Sapflow workspace tests
at bbcb6a9; this planning change does not reclassify that as phone acceptance.

Relevant continuation material: Sapflow's GOALS.md, docs/interop.md,
docs/benchmarks.md, docs/threat-model.md and docs/v4v-contract.md;
V4V's docs/frontend-boundary.md, docs/live-stage.md and mobile/README.md;
RelaySwarm's docs/AGENT-GOALS.md, docs/PRODUCTION-READINESS.md and
docs/NIP-DRAFT.md; RelaySwarmKit's README.md; Foregather's docs/native.md.

## Scheduling

Schedule by these gates rather than committing a release date from source
size alone. Estimate the sender/encryption and phone lifecycle work after B1
and the first independent B3 journey. Once those pass, size the remaining UI,
qualification and distribution work against the actual supported matrix.

The first SRT pilot converges at B4. General release additionally needs B3R,
B4R and B5. B6 is the agreed peer-playback release and does not hold the
origin-first broadcaster release.
