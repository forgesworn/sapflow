# iOS broadcaster capture integration

The initial SwiftUI iPhone application lives in `mobile/ios`. It captures actual
camera/microphone media with AVFoundation, requires an identified hardware H.264
encoder through VideoToolbox, encodes AAC-LC with AVAudioConverter and writes
those same compressed access units through AVAssetWriter passthrough inputs.
It includes manual RTMP/RTMPS and SRT publishing adapters using the owned Rust
protocols over Network.framework TCP/TLS and UDP. Recording and publishing remain
independent. This is an initial single-attempt integration, with physical
phone-to-origin qualification and shared reconnect integration still open.

## Build

The app uses XcodeGen and the parent-generated Sapflow Swift package. From the
repository root, build the package first, then generate outside the source tree:

```sh
node scripts/build-native.mjs apple
mkdir -p target/ios-app
xcodegen --spec mobile/ios/project.yml --project target/ios-app --cache-path target/ios-app/xcodegen-cache
xcodebuild -project target/ios-app/SapflowBroadcaster.xcodeproj \
  -scheme SapflowBroadcaster -sdk iphoneos -configuration Debug \
  -derivedDataPath target/ios-app/DerivedData CODE_SIGNING_ALLOWED=NO build
```

The initial floor is iOS 16. Portrait and both landscape orientations are
supported; upside-down portrait capture follows device orientation even where
the system keeps the application's interface upright.
The bundle identifier is a development placeholder and no development team,
signing identity, provisioning profile or distributable package is assumed.
Generated projects, build products and DerivedData stay under `target/`.
A simulator build proves compilation only: hardware/camera availability is
checked at runtime and unsupported hardware must fail visibly.

## Capture and ownership

The capture-control queue owns session configuration and state. A separate
serial sample queue receives native camera buffers and microphone PCM. The
raw media never crosses FFI. Camera preview uses AVCaptureVideoPreviewLayer.
The app keeps the screen awake during an active run; an explicit phone lock,
background transition or capture/audio interruption ends the current run.
There is no background-camera claim or silent capture resumption.

The profile is 720×1280 in portrait or 1280×720 in landscape at 30 fps,
2.5 Mbit/s Main-profile H.264, no frame
reordering, a maximum one-second/30-frame GOP, and 48 kHz mono 96 kbit/s AAC-LC.
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
its accepted prefix for finalisation when Stop is requested. It does not block
publishing. The app reports recording failure explicitly. SDK ingress overflow or validation failure closes only publishing admission and
reports the failure while local recording continues. Capture/encoder failure
stops the common session and closes its recording.

Successful stop flushes native AAC and hardware video, stops the publisher, drains accepted
recording work and waits for AVAssetWriter finalisation. Files are retained in
the app's Documents directory; successful finalisation offers ShareLink export.
Files sharing is enabled for recovery/export through Files. Capture refuses to
start with 200 MiB or less free capacity and checks capacity every five seconds
on the control queue. A failed or interrupted file is never labelled complete.
Fragment configuration alone does not establish process-kill recovery.

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
finished segment remains in Files. The latest finished segment remains exportable
while the next records. This applies to local-only capture, RTMP/RTMPS and SRT.

This introduces a visible media gap and a new destination publishing session;
continuous playback across an aspect change has not been qualified. It does
not send replacement codec configuration into an existing immutable session.
Automatic restart is cancelled if finalisation has not completed within six
seconds. This fences resumption rather than forcibly interrupting an AVFoundation
driver or file operation; cleanup may finish later. Network stop retains its
existing three-second RTMP or 100 ms SRT bound, while the next connection has its
normal separate startup deadline. Stop, backgrounding, capture/audio interruption,
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

Local recording is the default. Turn off “Local recording only” to configure a
manual destination. Enter the RTMPS application URL and stream key separately,
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

`ManualRtmpPublisher` implements `EncodedPublishingSink.start/offer/stop`.
`CaptureEngine.setDestination` installs it while stopped, before Prepare. The
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
confirmation. No retry is scheduled automatically: a failed attempt requires
Stop, Prepare and Start. This intentionally does not duplicate V4V's 90-second
grace or claim reliable reconnect before the shared SDK session policy is bound.

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
recording through a failed uplink, and completes explicit stop. Bind and qualify
the shared reconnect policy separately; qualify the SRT UDP adapter on-device.
Unsigned compilation, native SDK validation and a recording implementation are
not physical-device or live-broadcast acceptance.

Platform API references: [VideoToolbox compression workflow](https://developer.apple.com/documentation/videotoolbox/vtcompressionsession-api-collection),
[Core Media compressed sample timing](https://developer.apple.com/documentation/coremedia/cmsamplebuffercreate(allocator:databuffer:dataready:makedatareadycallback:refcon:formatdescription:samplecount:sampletimingentrycount:sampletimingarray:samplesizeentrycount:samplesizearray:samplebufferout:)).

Network references: [default TLS options](https://developer.apple.com/documentation/network/nwprotocoltls/options),
[peer certificate hostname](https://developer.apple.com/documentation/security/sec_protocol_options_set_tls_server_name(_:_:)),
[asynchronous connection sends](https://developer.apple.com/documentation/network/nwconnection/send(content:contentcontext:iscomplete:completion:)-3mfmt).


## SRT caller integration

`ManualSrtPublisher` and SwiftUI now provide a single-attempt manual SRT caller.
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
Rust common reconnect/session policy still needs its native adapter.

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
