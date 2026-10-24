# Native microphone-only capture

The native apps have an explicit microphone-only capture intent alongside the
existing camera profiles. The first B4R background slice can keep an explicitly
opted-in, already committed audio-only show running when the app leaves the
foreground. The full [mobile plan](mobile-streaming-plan.md) still requires
physical lock/background qualification, external-route qualification, standby,
safe profiles and sustained physical acceptance. Host tests do not satisfy those
requirements.

## Frozen intent and native ownership

Swift and Kotlin distinguish audio/video with a selected camera profile from
`audioOnly48kMono` / `AudioOnly48kMono`. The default remains 720p30 audio/video.
Audio-only has no video profile or dimensions. Selecting a different mode requires
the previous capture and rejected candidates to finish their actual cleanup.

The audio-only profile is AAC-LC, 48 kHz mono, 1,024 PCM frames per encoded unit,
with a 96 kbit/s encoding target. Its current minimum publish grant is an explicit
1 Mbit/s engineering transport allowance. That allowance is not a measured wire
rate, a guaranteed peak or an automatic quality fallback.

The selected intent is captured before asynchronous permissions, storage checks
or grant replies. It follows the exact preparation and Start ticket, publisher,
reconnect attempts, recording show and observation request. A delayed answer
cannot change that intent or revive an operation fenced by Stop or destination
End. Generic V4V v1 grants continue to authorise transport ceilings and scoped
credentials; they do not enforce a new server-side media-profile field.

Audio-only requests microphone permission. It creates no camera input/output,
preview surface, video encoder or GPU renderer. Device rotation does not create
an audio encoder epoch. The actual audio output establishes its timeline without
waiting for camera data, AVC configuration or a video keyframe. One native AAC
encode feeds both publishing and private recording through existing bounded
compressed-media seams.

Android uses AudioRecord and MediaCodec; iOS uses native audio capture and its
AVAudioConverter encoder. Background continuation is off by default and applies
only to audio-only capture whose Start has committed. Video and any audio-only
show whose Start has not committed stop when the last foreground owner leaves.
A committed show may still be establishing or re-establishing its authorised
transport.
There is no claim of physical locked-screen or external-device qualification.

## Foreground microphone selection

Both native apps offer System Default and a bounded local list of built-in,
wired and USB microphones. Refresh the list while capture is idle, then select
an input before Prepare. Prepare freezes that choice before asynchronous
permission, storage and destination work. Device names are display metadata;
the platform input identifier selects the device and is not a saved profile.
A disappeared selection stays unavailable until explicitly changed.

The actual input must be verified before PCM enters the existing encoder.
System Default pins its first verified input for that logical preparation,
including later orientation epochs. A changed or disconnected input closes the
current epoch's media gate and stops capture and publishing; it never falls back
or restarts automatically. Per-epoch callbacks cannot stop a successor capture.
The active-input display reports verified routing, not merely an accepted input
preference. Bluetooth and other unqualified routes are explicitly unsupported
in this bounded profile, including when chosen by System Default.

Route checks use local platform APIs at setup, first PCM and route/device events;
subsequent PCM admission uses cached scalar state. A route event closes that
admission before querying the platform outside its state lock. The existing
media owner may wait on a condition for at most 250 ms of monotonic time while
that cold transition is verified; same-input success reopens admission, Stop
wakes cleanly, and a timeout or true mismatch stays failed. No native query or
callback runs under that lock, and late results cannot reopen a stopped or
failed owner. Active waiters retain their original deadlines across later route
events. Initial notifications coalesce before Default pins a stable input.
Android's existing 200 ms PCM age ceiling remains stricter than the route wait;
a stale queued block is refused and the recording remains incomplete.
There is no per-buffer device
enumeration, new payload copy, Rust call or route timer. The 48 kHz mono AAC
profile and route-stop policy are unchanged. These source and automated checks
do not qualify physical USB devices, unplug/replug timing, Bluetooth,
background/lock operation or a sustained show. Those physical B4R gates remain
open.

## Background ownership and controls

Background continuation is an immutable Prepare choice and never starts capture.
Only the exact committed audio-only preparation may continue. Stop, capture
failure, route failure, grant expiry and destination authority fence that exact
show; a queued action or callback from an older show cannot affect its successor.
Authority deadlines use clocks that include device sleep. PCM age, encoded media
timestamps and telemetry retain their existing monotonic or hardware clocks.

Android promotes the application-owned capture controller into a non-exported
microphone foreground service before Start may commit. Android 13 and later need
notification permission, and notifications must remain enabled. The service is
`START_NOT_STICKY`: it has no boot receiver, worker, alarm, binding or state
reconstruction, and the system cannot resurrect a stopped show. Its private,
ongoing notification has an exact per-show **Stop and save** action. The action
checks a random capability as well as the preparation ID before stopping capture;
an old notification cannot stop a replacement. API 29 uses the compatible
two-argument foreground call, while API 30 and later declare the microphone type.

iOS declares only the `audio` background mode. The application lifetime owns the
committed show across scene inactivity/backgrounding, and returning to the app
revalidates its grant and transport authority. SDK 18.2 exposes no truthful
locked-screen Stop control for background microphone recording:
`AVCaptureEventInteraction` is camera/foreground-specific and MediaPlayer remote
commands describe playback. Sapflow therefore provides the exact per-show Stop
inside the app and does not create fake playback, silence or Now Playing state.
This is an explicit platform limitation, not a claim that locked controls work.

On both platforms **Stop and save** remains separate from confirmed V4V **End**.
The journal keeps the exact show key lease until recorder and publisher cleanup
really finish. Background output uses the same live grant authority at native
SRT/RTMP submission boundaries; queued bytes are refused once that authority is
gone. Bytes already submitted to the operating-system network stack cannot be
recalled.

Host evidence covers stale controls, scene/Activity ownership, sleep-sized clock
jumps, grant expiry, queued transport output and retained cleanup. It does not
cover a locked physical phone, notification/privacy toggles, process or service
kill, network switching, external microphones, recording finalisation, thermal
load, battery drain or a full show. Those remain physical acceptance gates.

## Transport and observation

Own SRT constructs an explicitly audio-only TS epoch with nullable video and a
real AAC codec. Own RTMP declares `(hasVideo=false, hasAudio=true)` before codec
headers and media. Native adapters and reconnect coordinators retain that exact
topology. An audio-only attempt never waits for a video IDR. The corresponding
[origin contract](audio-only-origin.md) is required at the destination.

A V4V fresh-media receipt preserves the actual nonempty track booleans. The
mobile matcher requires the exact topology of its captured attempt, with the
existing session, generation, nonce, connection epoch, initialization, part and
expiry fences. Available encoded media is not decoded-playback proof.

The finite native audio-only probe creates an AAC decoder and requires at least
2,048 actual decoded PCM frames. Coalesced output can provide those frames;
callback count is not frame count. Audio-only PCM must be contiguous, and the
probe must close its native readers and codecs before reporting success. The
post-decode receipt must still match the original scope and initialization before
the final SDK confirmation gate can mark the attempt live. It invents no video
frames or hardware-video evidence.

Android independently checks the extracted canonical two-byte ASC `11 88`.
CoreMedia's iOS description exposes AAC subtype, object flags, sample rate,
channels and packet frame count. The iOS observer checks those native properties
and actual PCM; it does not claim to recover raw ASC bytes from a magic cookie.
Canonical ASC validation at origin ingest and the encoder configuration seam is
a separate evidence source. The existing iOS hardware-video availability guard
applies to audio/video observation, allowing audio-only observation on iOS 16.

## Executed decoder evidence

The finite production iOS decoder has passed a generated fragmented-MP4 matrix
on the Mac host. A one-second, 48 kHz mono AAC fixture produced 46,912 actual PCM
frames across six output buffers, zero video outputs and no hardware-video
claim. Stereo 48 kHz, mono 44.1 kHz and an audio/video file were refused for the
audio-only expectation. An audio-only file was refused for the audio/video
expectation. The audio/video regression produced 30 hardware-decoded video
outputs plus PCM. Every case reported actual cleanup; generated media and
compiler scratch were removed.

These inputs were generated directly by FFmpeg. This matrix does not exercise
microphone capture, our native encoder, an origin-served independent part,
authenticated media fetching or a physical phone. Its PCM count is decoder
evidence, not proof of exact source-duration or accepted-unit retention.

The iOS host observation suite passes 49 tests, including exact rational PCM
continuity, coalesced output, wrong-topology, nonce, expiry, Stop and retained
cleanup controls. Audio-only accepts no gap or overlap between native PCM
buffers, including a one-frame shift. Equivalent rational timestamps compare
equal without converting to floating-point microseconds. The production tool's
`--check-pcm-timing` controls exercise the same counter. The combined publisher
host suite passes 77 tests against the existing packaged Rust session policy;
transport/status/decode facts in that suite are injected. These suites overlap
and their counts must not be added as distinct coverage.

The V4V provider's isolated loopback HTTP regression passes 12 tests. It preserves
audio-only fresh-media flags, refuses an empty track set and keeps metadata
availability separate from phone credentials and playback confirmation. This
uses app-issued fixture bearer identities, a mock origin control service and
generated file-copy inputs. It does not qualify real signer login, actual
origin transport or media decoding.

Fresh Apple and ARM64 Android SDK packages build from the current library source
and pass their installed host consumers. The background-audio packet compiles all
three unsigned iOS app configurations and the actual iOS unit-test bundle with
warnings as errors; the bundle was not executed on iOS. Focused host suites pass
159 budget/lifecycle, 69 V4V, 50 observation, 86 publisher, 36 creator-owner and
14 configuration tests. The production Network.framework cancellation check also
passes. Android Debug and Qualification each pass 385 unit tests, all three APKs
assemble and package isolation passes. Qualification lint has 14 warnings and
Release lint has 13, both with zero errors. All four packaged ARM64 libraries pass
16 KiB load-alignment checks. None of these checks installs an app or establishes
physical phone, store or distribution acceptance.

## Private recording and unfinished qualification

Audio-only recording creates a file with real AAC and no dummy video track. It
retains the existing durable show/segment identity, storage admission, incomplete
prefix policy and explicit export flow. A native writer's completion, synced
metadata, independently decoded media and full-show retention are separate gates.
AAC encoder priming, container edit interpretation and exact accepted-unit
retention still require explicit qualification.

The generated host fixture gate is `bash mobile/ios/check-audio-only.sh`. It owns
its sine-wave source and scratch files, runs the production native encoder,
shares the same compressed output with SDK validation and recording, and checks
recording completion, incomplete prefixes, repeated finish and export. Its
independent media checks do not establish physical capture, background behaviour,
crash repair, real disk-pressure behaviour, thermal limits or battery performance.

The latest generated fixture requests and verifies explicit 48 kHz mono signed
16-bit packed PCM. It encoded 144000 input frames into 143 native AAC units in
one pass; the packaged Rust ingress accepted all 143, and all three recording
cases exported and independently decoded. The unchanged gate still exits 1
because the stored AAC payload sequence differs from the native encoded output.
Cold converter construction now reports bounded scalar ASBD and channel-layout
facts when it fails, without retrying, but that path was not exercised by this
run. An earlier byte-identical source/package run returned nil while constructing
`AVAudioConverter`; the retained evidence does not establish why. Separate
controlled writer experiments store all 143 native AAC units in order. For our
experimental grouped-trim file, native AVAssetReader presents the exact input
duration at zero marker lag for both 144000-frame and 144123-frame sources.
Default FFmpeg instead presents 144384 PCM frames with a real 2112-frame leading
marker lag and exposes only the first 141 native packets.

Independent references encoded by FFmpeg itself as valid AAC MP4 and MOV also
decode to 144384 frames under default FFmpeg settings. Exact default decoded
tail length is therefore not a universal criterion. Those references have zero
marker lag and carry standard leading-delay metadata; our experimental file's
leading delay remains a separate failure. Changing only AVAssetWriter's output
from MOV to MP4 produced identical inspected media metadata and did not fix it.

These experiments do not provide a production fix or pass the production
fixture gate. Intended complete-show presentation remains open, and the failed
receipts are retained without weakening the original checks.

Scoped Sapflow recording completions now carry non-empty immutable
`recordingTracks` from the actual writer initialization and exact recording ID;
legacy callback bytes remain unchanged. Audio-only maps to `audio/mp4`, while
any video-bearing file maps to `video/mp4`. Live fresh-media flags or the
caller's requested mode cannot relabel an older recording. The V4V receiver
does not yet persist or use this field, so topology-aware replay adoption and
automatic replay publication remain open.

No real camera footage, extracted camera frames or phone recordings may enter
source control. Generated fixture receipts belong in ignored private lab storage.
MIT licensing, unlimited free watermark-free V4V streaming and the unrestricted
Zapstore build remain product requirements. Store pricing remains undecided.
