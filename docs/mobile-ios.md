# iOS broadcaster capture integration

The initial SwiftUI iPhone application lives in `mobile/ios`. It captures actual
camera/microphone media with AVFoundation, requires an identified hardware H.264
encoder through VideoToolbox, encodes AAC-LC with AVAudioConverter and writes
those same compressed access units through AVAssetWriter passthrough inputs.
It includes manual RTMP/RTMPS and SRT publishing adapters using the owned Rust
protocols over Network.framework TCP/TLS and UDP. Recording and publishing remain
independent. A logical publisher now binds the shared Rust reconnect policy
across socket attempts and orientation codec epochs. Physical iPhone recovery,
remote decoded observation and sustained capture qualification remain open.
The normal application also includes the explicit V4V creator flow below;
real signer, service and phone acceptance remain separate gates.

## Build

The app uses XcodeGen and the generated Sapflow and SignetAuth Swift packages.
From the repository root, build both packages, then generate outside the source
tree. SignetAuth has a separate bounded authentication runtime; it is never
created by camera or media callbacks.

```sh
node scripts/build-native.mjs apple
node scripts/build-signet-native.mjs apple
mkdir -p target/ios-app
xcodegen --spec mobile/ios/project.yml --project target/ios-app --cache-path target/ios-app/xcodegen-cache
xcodebuild -project target/ios-app/SapflowBroadcaster.xcodeproj \
  -scheme SapflowBroadcaster -sdk iphoneos -configuration Debug \
  -derivedDataPath target/ios-app/DerivedData CODE_SIGNING_ALLOWED=NO build
```

The initial floor is iOS 16. Portrait and both landscape orientations are
supported; upside-down portrait capture follows device orientation even where
the system keeps the application's interface upright.
The [orientation contract](capture-orientation.md) defines Automatic and the
fixed mounting choices, their immutable Prepare boundary and physical gates.
The bundle identifier is a development placeholder and no development team,
signing identity, provisioning profile or distributable package is assumed.
Generated projects, build products and DerivedData stay under `target/`.
A simulator build proves compilation only: hardware/camera availability is
checked at runtime and unsupported hardware must fail visibly.

## V4V creator flow

The application has three explicit destinations: Record only, Manual destination
and V4V. Record only remains the launch default. One app-owned creator controller
survives view updates and destination changes; camera callbacks never run its
authentication, journal or HTTP work. V4V streaming is always unlimited, free
and without a watermark. There is no subscription check in this path.

V4V requires a reviewed build profile. Debug and Release default to
`SAPFLOW_V4V_ENABLED=NO`, with empty API origin, playback origin and playback
prefix. Disabled builds display an unavailable message and do not open creator
journals, Keychain services or a signer runtime. An enabled profile requires
literal `YES`, three valid routing values and the real SignetAuth module.
Missing, partial or invalid configuration fails closed. A manual destination
cannot supply this profile.

The separate Qualification configuration maps
`SAPFLOW_QUALIFICATION_V4V_ENABLED`, `SAPFLOW_QUALIFICATION_V4V_API_ORIGIN`,
`SAPFLOW_QUALIFICATION_V4V_PLAYBACK_ORIGIN` and
`SAPFLOW_QUALIFICATION_V4V_PLAYBACK_PREFIX` into those four settings. Debug and
Release do not consume the qualification settings. Use approved, deployed
backend and playback routing for a real qualification; example domains only
establish build isolation. Do not put private routes or account data in source.

In an enabled build, choose V4V, explicitly connect a remote signer or use its
saved client, load the manageable events and choose one. Prepare obtains and
durably acknowledges the session grant before attaching the exact camera
preparation. Start broadcast remains a separate user action. Sign-in, an event
read, restored state or a late HTTP reply cannot start capture. A remote
session without matching local ownership is a visible conflict, not a grant
to adopt it. Restored credentials require fresh authentication; a restored
publish session requires a fresh acknowledged refresh before use.

An authenticated event may also show an active broadcast for which this app
has no local journal. Its separate **End existing broadcast** action freezes the
exact event, session, generation and revision shown by the confirmation dialog,
then performs a fresh exact-session status read. The read permit lasts at most
eight seconds and remains inside the command's original 60-second window. Only
an exact, still-active response can atomically save terminal-only ownership and
End intent before the ordinary durable End pump runs. This path never accepts a
publishing grant, constructs a publisher, prepares the camera or adopts capture.
A changed identity, late response, non-active phase or journal failure leaves
the journal without that session and sends no End mutation.

The control path admits one ordinary command and one primary HTTP request at
a time. Session HTTP operations have an original 60-second window and at most
four attempts. Explicit sign-in has a separate original 120-second limit that
includes queueing, native connection, consent, HTTP and adoption. End can fence
and reconcile independently of a cancelled sign-in's delayed cleanup.
Retry saved action reconciles
the existing durable operation rather than creating another Prepare. An
acknowledged refresh updates the publisher's current lease; transport progress
alone never extends it. Inactive mode cancels pending local approval and its
captured publisher without stopping another destination. Only exact uncertain
operations and terminal End reconciliation may continue while inactive.

An active publisher schedules its next refresh against a plan containing the
lease deadline, the capture binding's original generation and its exact camera
ticket. Command contention keeps that plan until the refresh is durably staged;
returning to the foreground immediately reconciles an overdue plan. A lost
reply retains the journaled refresh body and nonce for explicit retry. Stop,
End, binding retirement, authority expiry and an exact-ticket or generation
change discard the plan, so a delayed alarm or rejected queued command cannot
refresh a stopped publisher or prepare a successor capture.

End broadcast synchronously fences the captured publisher and any first Start
that has not committed. An existing local recording continues, including its
later rotation segments, until Stop and save. A publishing startup failure is
isolated from the recorder. The publisher starts before native sample delivery
can discover its codec configuration. Remote End and Stop and save are distinct
controls. Incomplete replay acknowledgement and replacement preparation are
separate explicit actions; acknowledgement requires fresh exact-session status.
An ended phase alone is labelled unconfirmed until the service returns the
separate End-complete receipt. Leaving a manual destination also fences its
captured publisher, even if Start committed before the UI received its running
update; a local recording that already began can continue.

The production factory uses distinct encrypted directories and Keychain services
for publish/End state and remote-signer client credentials. It opens them on the
same serial owner as the session engine and starts inactive. Corrupt or missing
halves fail visibly without resetting or falling back to another session.
Uncertain publisher, HTTP, signer and observation cleanup retain their actual
owners until completion.

Run the generated-fixture host checks without a phone or account:

```sh
bash mobile/ios/test-budget.sh
bash mobile/ios/test-v4v.sh
bash mobile/ios/test-origin-observation.sh
bash mobile/ios/test-observed-publisher.sh
bash mobile/ios/test-creator-owner.sh
bash mobile/ios/test-config.sh
```

The publisher and creator-owner checks use the actual packaged native modules.
They simulate transport, consent, journal and origin facts; they do not establish
remote-signer consent, Keychain behaviour, a deployed V4V journey or physical
camera/recording acceptance. Verify an actual built `Info.plist` using
`test-config.sh --verify-info disabled PATH/Info.plist`, or `--verify-info enabled`
followed by the path and the three expected routing values.

The complete normal application currently passes unsigned Debug, Qualification
and Release builds with both real packages and warnings treated as errors.
Built-property-list checks verify that the retained Debug, Qualification and
Release outputs are all connector-disabled. The separate configuration suite
checks enabled routing validation without claiming that this retained
Qualification app is enabled. The iOS unit-test bundle also compiles and links.
These are compiler/configuration gates; no test runner, phone, signer, account
or capture is exercised by them.

## Capture and ownership

The capture-control queue owns session configuration and state. A separate
serial sample queue receives native camera buffers and microphone PCM. The
raw media never crosses FFI. Camera preview uses AVCaptureVideoPreviewLayer.
The app keeps the screen awake during an active video run. Lock/background ends
video and any audio-only show whose Start has not committed. An explicitly
opted-in, already committed audio-only show may continue under the declared
`audio` background mode while its authorised transport connects or reconnects;
it never starts or resumes capture silently. Capture/audio
interruption still ends the exact run. There is no background-camera claim.

Choose capture quality before Prepare. The ordinary picker offers native
640×480 at 600 kbit/s video, or the default 1280×720 at 2.5 Mbit/s video.
Portrait swaps those dimensions. Both use 30 fps, Main-profile H.264, no frame
reordering, a maximum one-second/30-frame GOP and 48 kHz mono 96 kbit/s AAC-LC.
The 480p option is native 4:3 on iOS; Android's 848×480 option has a different
aspect ratio. There is no software rescale or automatic quality fallback.
A 1920×1080, 5 Mbit/s engineering path is excluded from the ordinary picker
until physical qualification. Its source and compile checks are not a claim
that the phone can sustain it.

Each preparation ticket carries immutable quality through permission replies,
first Start, recording segments, orientation epochs and publishing retries.
Changing the next selection requires actual camera cleanup, rather than a
possibly delayed UI snapshot. V4V grant admission happens before camera or
publisher creation: 480p requires at least 1 Mbit/s, 720p requires 4 Mbit/s,
and engineering 1080p requires 8 Mbit/s. An insufficient grant remains durable;
the user can explicitly select a lower quality and Prepare again without
repeating the acknowledged server mutation. A local publisher construction
failure also cannot retry that acknowledged HTTP mutation.

The native camera preset, frame rate, VideoToolbox target bitrate and GOP use
that exact profile. Both incoming pixel buffers and compressed H.264 format
dimensions must match its current orientation. The admission allowance above
the H.264/AAC targets is budgeting, not proof of peak bitrate or retransmission
capacity. Scoped SRT pacing still converts the granted bits per second to
bytes per second and cannot increase the manual ceiling. Physical hardware,
quality, pacing and sustained resource qualification remain separate gates.
The finite iOS origin probe confirms actual audio/video decode and the current
publishing attempt; it does not yet compare remote dimensions with the selected
capture profile.

### Elapsed time and socket outbound telemetry

The normal capture screen displays Capture elapsed, Broadcast elapsed and Socket
outbound. These are separate from transport acknowledgement and native decoded
playback readiness. Capture elapsed starts at the first committed Start, never
Prepare or a grant response. It includes orientation/reconnection gaps in the
same foreground logical capture, and freezes at Stop or interruption. It is not
an encoded-media or written-file duration. A local-only capture has no broadcast
clock. Destination End/expiry freezes Broadcast elapsed while independent local
capture and recording can continue; rotation cannot restart an ended clock.

Socket outbound counts successful existing Network.framework `contentProcessed`
completions: bytes consumed by the local network stack, not bytes delivered to a
NIC, peer or decoder. RTMP includes submitted protocol bytes; RTMPS excludes TLS
and TCP/IP overhead. SRT includes submitted UDP protocol/control/retransmission
bytes but excludes UDP/IP headers. The metric does not alter SDK readiness or
admission, and Rust's pre-submission counters are not used for it.

The counter adds only scalar work to existing completions, without another
packet callback, dispatch, clock read, FFI call, payload copy, allocation or
timer. One capture-owned 1 Hz reader spans reconnect and orientation epochs,
with at most one outstanding cold read. Two complete, monotonic samples within
three seconds are required for a rate; fresh no-delta samples produce zero.
Warmup, missing/stale/unsupported samples, counter overflow or rollback remain
unavailable. End does not leave a historical number labelled as current.

The logical destination retains only current and closing counter baselines.
Its exact attempt context fences reads and final reports. Final accounting is
folded before the existing actual-close completion can create a successor.
Cancellation ordering is not assumed: a failed or unresolved final send is marked
incomplete and breaks rate continuity, without delaying capture/transport
cleanup or fabricating completed bytes. Sealed totals cannot grow from a late
callback. These source/host checks are not an installed-phone throughput or
performance qualification.

The encoder is selected from VTCopyVideoEncoderList entries explicitly marked
hardware accelerated and the session's actual EncoderID must match. On iOS
17.4 and later the app additionally requires and verifies hardware acceleration
through the newer VideoToolbox properties. No software video frames are encoded
as fallback. Unsupported camera/profile/encoder combinations fail before capture
is admitted to the recording/publishing path.

The first microphone and camera timestamps establish a shared Core Media epoch
using their minimum timestamp. Startup alone retains at most eight native raw
buffers/12 MiB until both tracks arrive, preserving early microphone PCM rather
than silently dropping it. Exceeding the bound or startup deadline fails capture.
Video PTS comes from the actual camera timestamps, rebased to that epoch. One
compressed video frame is retained until the next PTS establishes its actual
duration; this avoids pretending camera jitter is exact fixed cadence. Video is
then represented in a 90 kHz clock, with identical DTS/PTS because B-frames are
disabled. The last frame uses its declared nominal duration at explicit stop.
AAC starts at the first microphone timestamp in the same epoch and advances by
1024 samples per emitted packet. An audio input clock gap over 5 ms, format
change or invalid video timestamp fails the run rather than inventing a new
unrelated clock. Native AAC converter priming and physical A/V sync still require
measurement. Explicit stop sends end-of-stream to the native AAC converter and
drains a bounded tail; final padding/trim metadata remains a physical acceptance
check.

The encoded output fans out by retaining compressed CMSampleBuffer handles:

- The recording worker owns a separately bounded 8 MiB, 128-sample, two-second
  backlog. It writes H.264/AAC directly into nil-outputSettings AVAssetWriter
  inputs. No recording encoder is created. One-second movie fragments are
  requested and the final file is a seekable `.mov` after successful completion.
- The SDK worker owns a separate 4 MiB, 128-sample, one-second backlog. It passes
  a synchronous Data(bytesNoCopy:) view of a contiguous CMBlockBuffer into the
  generated borrowed-byte MediaCodec.sample call. Rust makes the ownership copy.
  Non-contiguous compressed output is explicitly refused. Temporary AAC codec
  output is copied once into platform-owned CMBlockBuffer storage; that is a
  separate native encoder-output cost to measure.

Both admission paths bound outstanding dispatch work before retaining another
sample. Their locks protect scalar reservations only; file writes and FFI work
run outside the locks and capture callbacks. Reservation identity handles
out-of-order completion correctly. Overflow closes admission once, so error
notifications cannot themselves grow an unbounded callback backlog.
Recording overflow/storage backpressure closes new recording ingress and retains
its accepted prefix as incomplete. A low-space result requests finalisation on
the recording worker while publishing continues. The app reports recording
failure explicitly. SDK ingress overflow or validation failure closes only publishing admission and
reports the failure while local recording continues. Capture/encoder failure
stops the common session and closes its recording.

Successful stop flushes native AAC and hardware video, stops the publisher, drains
accepted recording work and waits for actual AVAssetWriter finalisation. New
files live in private `Library/Application Support/Recordings`. The inline
recording library groups orientation segments by show and distinguishes writing,
recorder-finalised, incomplete and unverified files. Old Documents recordings
remain in place and are listed as unverified; they are not silently adopted.
Recorder-finalised does not establish decoded playback or a complete show.

Prepare and actual Start/rotation check catalogue capacity and selected-profile
storage on a cold IO worker before admitting media. The budget is a 60-second
bitrate horizon plus headroom and reserve, with the existing strict greater-than
200 MiB floor retained. This is a preflight estimate, not a future-space promise.
At most one five-second storage read is outstanding. Directory scans, storage
reads, writer construction/finalisation and receipt writes run off camera,
sample, UI and capture-control callbacks. Low or unavailable storage refuses
initial admission. After a recording fails or stops for low space, a later
rotation can still stop publishing because automatic restart requires successful
segment finalisation. Recording-optional restart policy remains follow-on work.

Reloading metadata is permitted during capture. Export requires capture to be
idle and opens a folder picker without passing it a private recording URL.
The exact selection reserves capture until the modal actually dismisses and
the cold worker finishes. Incomplete, orphaned and legacy files require explicit
confirmation. Export revalidates the selected file and receipt, creates a new
exclusive copy in the coordinated folder and copies in at most 64 KiB chunks.
It checks the created file's identity and byte count, synchronises it and closes
actual owners before releasing security-scoped access or admitting capture.
Cancellation preserves the private original; a partial destination copy may
remain. Unconfirmed cleanup retains ownership and blocks new work.

The catalogue bounds scans to 256 segments, 1024 directory entries and 4 KiB
receipts. A lifetime directory lock prevents a fresh local owner bypassing
pending cleanup. File/receipt identity checks detect replacement and changes;
they are not content-authenticity hashes. No automatic deletion, migration,
repair or replay publication occurs. Fragment configuration alone does not
establish process-kill recovery. Catalogue management and pagination remain open.

## Upright capture and rotation

The actual cardinal device pose selects capture orientation before the camera
starts. Face-up, face-down and unknown readings retain the last valid pose;
interface orientation is used only as an initial fallback. Rotation lock does
not deliberately force landscape recording. The video-data-output connection
physically rotates its buffers in hardware. VideoToolbox receives and checks
the resulting 720×1280 or 1280×720 pixel dimensions. Preview follows the same
capture orientation and uses aspect-fit display. The recording writer and Rust
publisher receive those same compressed, upright pixels; MP4 display transforms
and player rotation tags are not the publishing mechanism.

Rust codec handles are immutable within a transport epoch. A cardinal orientation
change during capture therefore ends the old epoch: disable camera/audio
delivery, stop capture, flush the encoders, await bounded publisher teardown and
finalise the current recording. A fresh hardware encoder, AAC encoder, recording
file and publisher then start at the latest requested pose, with a new shared
A/V timestamp epoch. Rapid turns coalesce during finalisation. Each successfully
finished segment remains in the private recording library. Metadata may be
reloaded while the next records; export requires capture to be stopped. This
applies to local-only capture, RTMP/RTMPS and SRT.

This introduces a visible media gap and a fresh destination connection while
retaining the same logical reconnect policy. Continuous playback across an
aspect change has not been qualified on iPhone. It does
not send replacement codec configuration into an existing immutable session.
Automatic restart is cancelled if finalisation has not completed within six
seconds. The original monotonic deadline is rechecked after blocking stages
and before adoption, even if its timer callback was delayed. This fences
resumption rather than forcibly interrupting an AVFoundation
driver or file operation; cleanup may finish later. Network stop retains its
existing three-second RTMP or 100 ms SRT drain bound. Completion additionally
requires Network.framework's actual `.cancelled` state. A separate logical-owner
four-second cleanup deadline reports failure and retains an unconfirmed owner;
it cannot claim cancellation success or start a successor. A later cancellation
may release that owner but never resurrect a failed or stopped session. Recording
finalisation still proceeds if publisher cleanup fails. The next admitted
connection has its normal separate startup deadline. Stop, backgrounding, capture/audio interruption,
recording failure or storage/permission failure cancels automatic resumption.
Attempt/capture generations fence old callbacks.

Physical acceptance must record actual dimensions, decoded pixel orientation
and A/V timing for each case:

| Case | Required evidence |
| --- | --- |
| Cold portrait, both landscape sides and upside-down | A non-mirrored `TOP / R` chart is upright in preview, independently decoded publishing output and exported H.264/AAC recording. SPS dimensions are 720×1280 or 1280×720 as appropriate; decoding with display rotation disabled remains upright. |
| Portrait → landscape → portrait, and 180° turns | Each previous segment is playable and seekable with both tracks; each new segment has matching dimensions and upright pixels. Exactly one encoder/publisher epoch follows each completed restart. Measure media gaps and origin/player recovery for RTMP/RTMPS and encrypted SRT. |
| Several turns during finalisation | One pending restart uses the latest valid pose, without simultaneous publishers or growing queues. Face-up/down preserves the last pose. |
| Stop, lock, background or audio interruption during rotation | Finalisation may finish, but no camera/publisher restarts from a late callback. No retained socket or timer remains after teardown. |
| Unsupported profile, low storage or finalisation beyond six seconds | Capture fails visibly, accepted recording prefixes remain recoverable, and automatic restart stays cancelled. |

These cases require a physical iPhone, an independent origin/player and a
recording probe. Compilation and pure pose/restart tests do not establish them.
Apple documents physical buffer rotation for
[AVCaptureVideoDataOutput connection orientation](https://developer.apple.com/documentation/avfoundation/avcaptureconnection/videoorientation).

## Manual RTMPS publishing

Local recording is the default. Choose Manual destination to configure a
destination. Enter the RTMPS application URL and stream key separately,
then Prepare and explicitly Start broadcast. Destination editing freezes while
prepared/running, so an edited field cannot silently change what Start sends to.
Stop before changing destinations. The application URL must have an application
path and cannot contain user/password, query or fragment. A destination-authorised
query in the separate stream key remains supported. Fields have strict size and
control-character bounds.

RTMPS uses Network.framework with default system certificate trust, required peer
authentication, the exact destination hostname for certificate-name verification,
and TLS 1.2 or later. There is no custom verify callback, trust-all mode or
certificate exception. A certificate/hostname/handshake failure closes this
attempt and preserves recording. There is no automatic downgrade. Plain RTMP is
available only after explicitly allowing it for that destination and supplying
an `rtmp://` URL; the UI states that media/key then travel without encryption.

Save profile explicitly stores at most 16 named profiles. Endpoint/name/transport
metadata goes into the app's UserDefaults. The stream key goes into a generic
password Keychain item marked `WhenUnlockedThisDeviceOnly`; it is not synced,
exported, included in diagnostic errors or embedded in the application URL.
Delete removes that profile and its Keychain item. Selecting a saved profile
fills the form; it does not start a connection. A locked/unavailable Keychain
fails visibly rather than using an old key silently.

Manual settings also accept a
[reviewed version 1 JSON destination file](manual-destination-profiles.md).
The bounded cold reader retains document access through actual close, then
offers a redacted host/port/security review. Apply fills the form only, with
independent permission for plaintext. Destination edits, leaving the foreground
and capture preparation invalidate the original import ticket. Saved-profile
load/save/delete use actual capture availability guards too; corrupt metadata
fails visibly instead of appearing as an empty profile list. Physical provider
and Keychain behaviour still require device qualification.

`ReconnectingPublisher` implements the logical `EncodedPublishingSink` and
creates a fresh `ManualRtmpPublisher` per attempt. `CaptureEngine.setDestination`
installs it while stopped, before Prepare; replacement is refused while an old
publisher still has unconfirmed cleanup. The
sink sends only validated immutable generated `MediaSample` handles; raw media
never enters the networking queue. Rust's `NativeRtmpPublisher` owns RTMP
handshake, command state, configuration and FLV framing. AVFoundation timestamps
already share the capture epoch, so the Rust publisher uses `mediaEpochNs=0`.
Its `Publishing` state means the destination accepted publication, not that a
viewer decoded media.

The dedicated network queue has these bounds:

- Sample admission: 4 MiB, 128 handles and one-second residence. Pre-publication
  samples are discarded; the accepted stream begins at the next video IDR.
- One asynchronous read at a time, up to 64 KiB per read, passed to the bounded
  Rust parser. No network wait runs on capture, recording, FFI ingress or UI.
- Rust retains at most 8 MiB of queued wire output. At most one additional owned
  `takeOutput()` buffer, also at most 8 MiB, is in a Network.framework send.
  Another output buffer is taken only after that write's completion.
- Fixed single-attempt deadlines: 15 seconds to connect, 15 seconds for the
  Rust handshake/command startup, five seconds for a stalled native write, and
  three seconds for best-effort explicit stop. Rust additionally expires the
  oldest owned wire-media backlog after one second, even if tiny peer ACKs
  continue. The adapter calls `tick(nowMs)` before each media send and reschedules
  one generation/source-revision-fenced timer after feed/send/output changes.
  It follows `nextDeadlineMs()` and cancels the source when no deadline remains;
  an idle published connection has no repeating polling timer.

Network/protocol/backpressure failure closes publishing admission and transport,
releases queued sample handles and leaves AVAssetWriter recording running.
Late callbacks carry an attempt generation and cannot affect a newly started
attempt. Stop queues Rust closeStream and drains bounded output, then closes the
connection, or cancels it at the fixed stop deadline. Remote end remains
unconfirmed; a manual connection is not a V4V lifecycle/end adapter.

The UI labels successful publication as “Sending”, with RTMP wire bytes accepted
by the socket measured over a rolling interval. That rate excludes TLS/IP
framing and is not an origin/player acknowledgement. It always says playback is
unconfirmed. An origin viewer/monitor is still required for decoded media
confirmation. The logical owner uses `NativeSession` with the manual 90-second
grace. Failure starts the original retry window before asynchronous cleanup;
repeated failures cannot extend it. A fresh attempt waits for actual old-socket
cancellation. Healthy orientation replacement uses `replaceTransport`, retaining
an existing deadline without manufacturing an outage.

RTMP declares both fixed capture tracks before media. The fused send/output
record and cold receive-event drain expose complete startup submission and
validated peer watermark progress. SRT supplies the corresponding typed events.
The coordinator latches at most two facts for the current attempt and applies
`connected`, `sending` and `transportRecovered` in that order. Generation,
codec-epoch and attempt tokens reject old observations. No sample-level FFI
polling or second encoder is added. See the exact conservative RTMP ACK guard
and network-versus-decoding distinction in [transport recovery](transport-recovery.md).

Only timely peer media progress clears the network window; it never calls
`mediaConfirmed`. An accepted sending attempt with absent/sparse ACKs may remain
active after retry permission exhausts, with an explicit uncertain/exhausted
status. A later failure or rotation then cannot reconnect automatically.
`nextDeadlineNs` drives one logical timer and becomes empty after exhaustion;
there is no idle polling. Explicit Stop closes logical admission synchronously,
cancels future retries and prevents queued Start/callback work opening a socket.
A new explicit Prepare is required after terminal publishing failure. Recording
and capture can continue through network failure.

`RustEncodedIngress` reuses one generated MediaCodec per encoder configuration
and refuses mid-run format replacement. The recorder remains a native compressed
passthrough sink. There is no bundled host, credential, billing rule, trial timer,
V4V path or subscription mechanism. Always-free V4V and unrestricted Zapstore
remain distribution promises, separate from destination authentication.

## Desktop adapter interoperability helper

`mobile/ios/check-publisher.sh <fixture.mp4>` builds the production
ManualRtmpPublisher/RustEncodedIngress sources against the real generated Apple
package on macOS. Supply `SAPFLOW_CHECK_ENDPOINT` (application URL),
`SAPFLOW_CHECK_STREAM_KEY` (test-only key) and, only for an explicitly plaintext
test listener, `SAPFLOW_CHECK_ALLOW_PLAINTEXT=1`. The caller owns the independent
receiver and must check its decoded output. The helper starts no server and
removes its own Swift build scratch. An explicitly supplied
`SAPFLOW_CHECK_WORKSPACE` reuses one owned Swift build directory across checks;
the caller must remove it afterward.

The helper reads genuinely encoded H.264 without B-frames and AAC-LC48k mono/stereo
from a fixture of at most thirty seconds through AVAssetReader. It paces the
compressed samples, waits for actual RTMP publication acceptance, drives the same
native adapter and requests bounded stop. Its logs omit endpoint/key. This tests
the real native socket/FFI route with fixture media; it is not camera capture,
physical phone or trusted-TLS origin acceptance. Set
`SAPFLOW_CHECK_EXPECT_TLS_REJECTION=1` against a caller-owned untrusted TLS fixture
to require certificate-handshake failure before publication, without overriding
trust. It does not install a root certificate or change Keychain trust settings.

## Evidence and remaining acceptance

The private recording library checkpoint on 22 October 2026 passes 117 pure
capture/budget tests, 26 generated-file catalogue/storage tests and 33
library/export tests on macOS with warnings treated as errors. The independent
subprocess lock test is macOS-only; it is not an executed iOS test. Seventeen
generated-bundle Python tests exercise the privacy-resource verifier. All three
complete unsigned app configurations compile and link with zero warnings using
the same 84 source/package inputs. The actual 109-input iOS XCTest bundle also
compiles and links with zero warnings; it was not executed on iOS. The actual
root `PrivacyInfo.xcprivacy` is
verified in each normal app archive, separately from source and generated
bundles. This is a required-reason packaging checkpoint; collection/tracking
assessment and App Store readiness remain open.

A generated-only macOS check uses the actual production CompressedRecorder,
AVAssetWriter and catalogue with compressed H.264/AAC samples. Its finalised
3.02-second file and two incomplete 1.26-second prefixes independently decode
with video and audio. Exports match their originals, bounded copy chunks stay
within 64 KiB and repeated finish listeners receive actual completion. The
recording-only stop uses the explicit incomplete-finish seam, not real disk
pressure. All generated media and build scratch were removed. Receipts remain
under `target/ios-recording-native/v3/` and `target/ios-recording-library/`.
This does not establish document-provider/modal behaviour, physical iOS
recording, seeking, thermal performance, crash recovery or power-loss bounds.

The 22 October 2026 audio-only recording checkpoint writes new microphone-only
recordings as M4A while video-bearing recordings remain MOV. The immutable
catalogue identity and export destination preserve the selected extension, and
historical receipts without a container continue to resolve to MOV. The encoder
reports its actual AAC converter priming and tail extent to the recorder; no
priming or padding count is hard-coded, and the shared publishing payloads,
timestamps and AudioSpecificConfig are unchanged. The generated M4A retained all
143 native AAC access units in their original order and passed the packaged Rust
ingress. Native playback returned the exact 144000-, 143360-, 143123- and
511-frame input extents; the three non-periodic comparison cases had zero-lag
correlations above 0.99994. FFmpeg may retain the file's declared trailing
padding, so its decoded tail length is not used as a universal exact-presentation
assertion. The checkpoint passed 38 catalogue/storage/timing and 35
library/admission/export host tests with warnings as errors, typechecked all 51
production Swift files against iOS 16 APIs, and retained the unchanged generated
A/V MOV path through three strict independent decodes. Evidence is under
`target/ios-audio-recording-retention/v1/` and
`target/ios-audio-only-fixture/20261022T122618Z-50ecbe54/`. This is generated
macOS evidence; physical iPhone, background, lock-screen, route-change,
long-show, crash, power-loss and thermal acceptance remain open.

Verified on 21 October 2026 with Xcode 16.2 and the actual generated Sapflow
XCFramework: the unsigned arm64 iOS application and XCTest bundle passed
`xcodebuild ... -destination 'generic/platform=iOS' CODE_SIGNING_ALLOWED=NO
build-for-testing`, with no compiler warnings. This compiles tests but does not
execute them on iOS. The original five pure Swift budget/profile XCTest cases passed on macOS
through `mobile/ios/test-budget.sh`, without a simulator or capture fixture.

The generated project, build log and unsigned app are retained under
`target/ios-app/` for the signing/device handoff. The app is
`target/ios-app/DerivedData/Build/Products/Debug-iphoneos/SapflowBroadcaster.app`;
evidence logs are `target/ios-app/build.log` and `target/ios-app/budget-tests.log`.
The production Network.framework adapter passed an independent macOS fixture
check over explicitly selected plaintext RTMP: 180 video packets and 283 AAC
packets were independently decoded, and all 180 decoded video frame hashes
matched the source fixture. Preparing the destination for sixteen seconds
before explicit Start left it dormant and did not consume its startup deadline.
A separate untrusted TLS listener received `SSLV3_ALERT_BAD_CERTIFICATE`; the
native client failed before RTMP publication with default trust unchanged.
This proves the macOS socket/FFI route and a negative certificate check. It does
not qualify trusted RTMPS or physical camera/network behaviour.

The receipt is `target/ios-app/interop-verification.json`; native check logs are
`target/ios-app/network-check.log` and `target/ios-app/tls-check.log`. All owned
fixture listeners, media, temporary private keys and helper build scratch were
removed after recording the results. No simulator was started. Remove the retained app/DerivedData
after its signing/device handoff is complete and evidence is recorded.

The app includes tests for independent byte/count/time admission bounds,
out-of-order release, permanent closure, duplicate release, TLS defaults,
explicit plaintext choice, endpoint/credential ambiguity and profile bounds. The same pure
Swift reservation class can run on macOS without starting a simulator; iOS
XCTest execution and real capture remain separate checks.

Before a broadcaster release, install the signed app on a physical iPhone and
prove camera/microphone permission handling, selected encoder identity, real
encoded samples admitted through the installed SDK, playable/seekable H.264/AAC
recordings, export, lock/interruption/route-change behaviour, low-storage and
backlog failure, repeated start/stop and process-termination recovery. Measure
actual copies, allocations, CPU, A/V sync, thermal state and sustained power.
Then prove the attached manual RTMP/RTMPS implementation delivers actual
phone-to-origin-to-viewer media, rejects invalid TLS trust/hostname, preserves
recording through a failed uplink, and completes explicit stop. Qualify the bound
shared reconnect policy and the SRT UDP adapter on-device.
Unsigned compilation, native SDK validation and a recording implementation are
not physical-device or live-broadcast acceptance.

Platform API references: [VideoToolbox compression workflow](https://developer.apple.com/documentation/videotoolbox/vtcompressionsession-api-collection),
[Core Media compressed sample timing](https://developer.apple.com/documentation/coremedia/cmsamplebuffercreate(allocator:databuffer:dataready:makedatareadycallback:refcon:formatdescription:samplecount:sampletimingentrycount:sampletimingarray:samplesizeentrycount:samplesizearray:samplebufferout:)).

Network references: [default TLS options](https://developer.apple.com/documentation/network/nwprotocoltls/options),
[peer certificate hostname](https://developer.apple.com/documentation/security/sec_protocol_options_set_tls_server_name(_:_:)),
[asynchronous connection sends](https://developer.apple.com/documentation/network/nwconnection/send(content:contentcontext:iscomplete:completion:)-3mfmt).


## SRT caller integration

`ManualSrtPublisher` provides each individual SRT attempt beneath the shared
logical publisher; SwiftUI selects manual encrypted SRT independently of RTMP.
The initial destination accepts explicit numeric unicast IPv4/IPv6 peers and
ports; DNS/rendezvous/listener modes are not attached. AES-256 is the default and
requires a 10 to 79 byte printable ASCII passphrase. Plaintext requires a separate
explicit choice and an empty passphrase. SRT encrypts media without authenticating
peer identity; stream ID and control packets remain clear. Both stream ID and
passphrase are stored together in the unlocked-device-only Keychain only on Save;
profile metadata and logs contain neither. Existing saved RTMP profiles remain
readable. There is no automatic fallback from encrypted to plaintext SRT.

The current capture profile always has both video and audio. `RustEncodedIngress`
freezes its same shared codec handles once both native configurations arrive;
only then does the adapter construct `NativeSrtPublisher` and its MPEG-TS epoch.
Prepare remains dormant. Explicit Start starts one connected Network.framework
UDP socket and a fifteen-second socket/configuration startup bound. Rust drives
handshake, encryption, MPEG-TS packaging, ACKs, retransmissions and pacing. Socket
readiness or an SRT handshake never claims decoded playback.

The adapter owns a separate 4 MiB/128-handle/one-second admission budget and a
bounded native pending list. It drops pre-handshake media and waits for the next
IDR. One datagram of at most 1500 bytes may be outstanding in Network.framework;
a datagram submission is cancelled after 100 ms. Receive uses one complete UDP
message at a time, refuses oversized input before Rust ingress and continues
servicing deadlines. The connected socket fixes the numeric peer. At most sixteen
SDK mux steps run before yielding the serial queue. Immediate work schedules one
coalesced continuation; readiness, received messages and actual Rust deadlines
wake later work. Immediate mux deadlines do not cause a polling loop while a
socket submission is outstanding.

SRT uses actual `uptimeNanoseconds / 1000` microseconds and `nextDeadlineUs`,
not a multiplied millisecond clock. The shared one-shot timer schedules absolute
nanoseconds. Default Rust limits are 128 packets/4 MiB of transport retention,
one-second packet age, one-second SDK sample residence, 120 ms SRT latency and
1,250,000 packet bytes/s including retransmissions. These native/SDK/transport
residence bounds are distinct and can add; they are not an end-to-end one-second
latency claim. TS staging, serialization, encryption and FFI output copies remain
accounted separately from shared compressed sample ownership.

Failure closes only publishing. Explicit stop cancels media and attempts at most
one best-effort SRT shutdown, bounded by the same submission deadline; recording
continues independently and destination end remains unconfirmed. Timer callbacks,
UDP callbacks, sample admission and queued continuations carry attempt generations.
The logical publisher uses the shared Rust reconnect/session policy; transport
ACK progress remains distinct from decoded-media confirmation.

Ten actual pure Swift XCTest cases pass on macOS, including cancelled/rearmed
Dispatch timers, latest-generation timer delivery, SRT numeric/secret bounds and
legacy profile decoding. The real refreshed package (archive SHA-256
`71f4d0c1d046336b21f68e207a788770f0854b85def0548a097e32540d6434a6`)
compiled the unsigned arm64 iOS app and XCTest bundle with no warnings. This
build compiles the iOS tests; it does not execute them on a phone.

The production macOS NW UDP adapter then passed an independent encrypted libsrt
listener check. The listener decoded all 120 video and 189 AAC frames; every
video frame hash matched the source. Maximum observed datagram size was 1332
bytes, and encryption key flags were set. Sixteen seconds of dormant Prepare
before explicit Start passed. With a wrong passphrase the independent listener
returned handshake rejection code 1010, with zero sender media packets and zero
received TS bytes. A fresh-package RTMP regression also decoded all 120 video
and 189 audio frames with exact video hashes, using explicit plaintext loopback.

The SRT/deadline snapshot receipt is `target/ios-app/srt-interop-verification.json`, with
`srt-build.log`, `srt-helper-build.log`, `srt-network-check.log`, sanitised
`srt-listener-check.log`, frame-hash logs and `rtmp-deadline-check.log` alongside.
All owned listener/proxy/client processes and fixture media/helper-build scratch
were removed after receipt creation. This receipt describes the preceding
SRT/deadline build. Earlier TLS-negative and RTMP receipts remain explicitly
labelled as the earlier package snapshot. No physical iPhone capture, recording,
trusted RTMPS, power/thermal or reconnect acceptance follows from these checks.

The desktop helper also accepts `SAPFLOW_CHECK_TRANSPORT=srt`,
`SAPFLOW_CHECK_SRT_STREAM_ID` and `SAPFLOW_CHECK_SRT_PASSPHRASE`; provide a numeric
SRT endpoint and an independently owned receiver. The helper preconfigures the
same shared Rust codec handles without a preliminary payload copy, waits for
SRT handshake completion, then paces real samples. Its final 250 ms tail wait is
fixture-driver work only; the production adapter's explicit stop is bounded
cancellation. The caller must independently decode and check both media tracks.

## Orientation change validation

The orientation implementation adds four pure tests covering physical-pose
mapping and encoded dimensions, dormant prepared changes, coalescing to the
latest pose, and cancellation/late-completion fencing. All fourteen actual pure
Swift tests pass on macOS. The updated unsigned arm64 application and XCTest
bundle also passed real-package `build-for-testing` without warnings. The
current build and test logs are `target/ios-app/orientation-build.log` and
`target/ios-app/orientation-tests.log`; the retained unsigned app now contains
the orientation implementation. iOS tests have been compiled, not run on a phone.

An independent ffmpeg listener also received a real encoded 720×1280 portrait
fixture through the production macOS RTMP adapter: all sixty video frames and
95 AAC frames decoded, and all decoded video hashes matched the source with
display autorotation disabled. Two stop callbacks completed within the same
four-second fixture bound. This verifies compressed portrait transport and local
teardown callbacks, not the camera pose or physical iPhone output. The receipt
is `target/ios-app/orientation-verification.json`, with network and frame-hash
logs alongside. Its executable and Debug application-dylib hashes identify the
current unsigned build. The owned listener, fixture media and helper scratch
were removed after verification; no simulator, device or live origin was used.

## Logical reconnect source qualification

The reusable `ReconnectController` has no SDK, socket, media, timer or file
runtime. Its injected policy/attempt ports exercise the actual lifecycle owner
with deterministic asynchronous cleanup. `ReconnectingPublisher` binds those
ports to the real generated SDK and existing bounded Network.framework adapters.
It retains immutable codec handles until the old attempt's actual cancellation;
no successor can share an unfinished transport or its borrowed configuration.

The pure macOS Swift gate currently executes 30 tests, including sixteen new cases
for dormant preparation, reordered acceptance/progress facts, first-failure
ordering, cleanup before retry, synchronous Stop versus queued Start admission,
orientation replacement, stale
attempts, exhausted retry permission, serial-clock observations after an
interleaved timer, clock rejection and late cleanup after the
fixed owner deadline, an overdue native deadline delivered after actual cleanup,
and bounded one-shot progress draining when RTMP startup submission occurs
during an outstanding write. Fused media results avoid extra progress polls;
existing network completion/output/deadline work drains queued facts. These tests
establish source invariants, not a physical
network recovery or a decoded playback result. Existing fixture receipts above
refer to their earlier single-attempt snapshots.

The refreshed Apple package uses SDK source `1634a809` (the subsequent server
HTTP-test helper change does not change its native inputs). Its archive SHA-256
is `9eb2f7e3faf0fa9be2242b0358bccd6daa2b373d41d10b8691e43912cc49c7f8`.
The real external Swift consumer passed; the actual unsigned arm64 application
and XCTest bundle passed `build-for-testing` with no warnings. The thirty pure
Swift tests ran on macOS; the iOS test bundle was compiled only.

`mobile/ios/check-publisher-lifecycle.sh` compiles the production SRT adapter
against that real package and runs three no-media loopback UDP ready/cancel
cycles. All six Stop completion callbacks arrived after actual cancellation and
reported released owners. This catches suppression of Network.framework's
`.cancelled` event after cancellation admission closes. The check creates no
camera, codec, SRT handshake or media, and removes its listener and build scratch.
It establishes this host cancellation boundary, not iPhone network recovery.

Current receipts and logs are `target/ios-reconnect-receipt.local.json`,
`target/ios-reconnect-source-hashes.json`, `target/ios-reconnect-pure-tests.log`,
`target/ios-reconnect-lifecycle-check.log`, `target/ios-reconnect-build.log` and
`target/ios-reconnect-apple-build.log`. The preceding Apple package was retained
under `target/native-packages/apple-before-ios-reconnect` during replacement
qualification. That task-owned backup was subsequently removed after all eighteen
file hashes were reverified. Its original file-hash manifest and package
provenance remain recorded in `target/ios-reconnect-previous-apple-hashes.json`
and `target/ios-reconnect-previous-apple-cleanup.local.json`.
No signed build, physical capture, radio failure/recovery,
orientation recovery, trusted decoded-media confirmation, thermal or sustained
power qualification follows from these source and host checks.

## UI wording after the frozen physical snapshot

The manual destination form now describes the bound logical reconnect owner,
its shared 90-second outage window and independent local recording. Repeated
failures retain the original window. Timely validated acknowledgement of
submitted startup media can restore network retry permission; decoded media
confirmation remains a separate observation. If an accepted sending attempt lacks that
acknowledgement when the window expires, it can continue with exhausted retry
permission. A subsequent failure or intentional transport replacement then ends
publishing until a new explicit Prepare. Sending alone never establishes decoded
Live playback. The per-attempt adapters still describe individual child attempts;
the logical owner supplies the application's reconnect status.

This follow-up changes three displayed strings and documentation only. It does
not change session, transport, recording or UI control logic. The development-
signed application installed for physical testing remains the frozen `a399d13`
source snapshot with the earlier explanatory wording. Its signed-app manifest
and install/launch receipts identify that artefact separately. Neither that
application nor the separate UI qualification runner was rebuilt, re-signed or
reinstalled for this wording change. Physical acceptance must follow the frozen
artefact's recorded identity rather than the newer source wording.

## Foreground input routing checkpoint

The normal controls now expose System Default plus bounded built-in, wired and
USB input choices. An idle refresh reads `AVAudioSession.availableInputs` under
the capture category without activating capture. Prepare freezes the input UID,
sets `preferredInput` and verifies `currentRoute.inputs` after capture starts and
before first PCM. Route notifications recheck the exact native epoch's input;
unchanged input/output-only events do not stop capture. Default pins the actual
input across orientation epochs of the same preparation. Mismatch or removal
stops recording and publishing without fallback. Listeners are removed before
deliberate deactivation; stale owners cannot stop later preparations.

A cold route transition closes PCM admission while its native lookup runs
outside the state lock. The existing sample owner can wait up to 250 ms on a
relative, monotonic condition; success/Stop/failure wakes it without polling or
a new timer. Timeout is a sticky route failure. Initial events coalesce before
pinning, and late verification cannot reopen a closed or newer owner.

The active microphone label is separate from the selected preference. Bluetooth
and other routes outside this built-in/wired/USB profile are unsupported, even
under Default. Actual USB microphones, hot-unplug, route callback timing and
physical background/locked-screen behaviour remain B4R gates. The implemented
ownership and app-only Stop contract are described in the
[audio-only mobile scope](audio-only-mobile.md#background-ownership-and-controls).
