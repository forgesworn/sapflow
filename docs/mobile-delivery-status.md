# Mobile delivery status

The delivery goal is active. This record separates implemented code, local
tests, native packages, actual broadcasting, physical acceptance and release.
It does not replace any requirement in the [delivery plan](mobile-streaming-plan.md)
or [native performance contract](native-performance-contract.md).

Privacy standby is the next source packet. Its
[contract](privacy-standby.md) requires synchronous raw-input fencing,
independently paced replacement picture and silence, explicit Resume and
matching recording. It is not yet an app control. Cold AAC replacement needs
per-generation timing and recording proof before native integration.

The current generated macOS AAC baseline passes the unchanged strict
stored-payload and presentation oracles for six recording cases. Native output
is shared with actual packaged Rust validation and the real recording writer.
The aligned, partial and 511-frame nonperiodic cases retain exact native PCM
extents and exceed 0.99994 zero-lag correlation in both native and independent
decoding. This supersedes the older pre-repair payload failure for this exact
source; it does not qualify a cold encoder transition or a physical phone.
Evidence is retained at
`target/ios-audio-only-fixture/20261022T175340Z-9c5c8319/receipt.local.json`.

An independently reviewed Android standby transaction model now passes 32
warnings-as-errors JVM tests and the same 32 tests in the real Debug Gradle
project. It covers priority privacy fencing, explicit Start/Resume, delayed
callbacks, immutable retiring-AAC identity, sink admission and original
deadlines. It is not connected to capture, codecs, the UI or the SDK queues;
trusted native receipt adapters and terminal branch fences remain to be built.

Generated macOS feasibility experiments expose why cold AAC needs separate
recording accounting. Discarding the old encoder leaves one decoded access
unit of old overlap in the tested continuous decoder; applying another file
head trim at an interior join fails the prototype writer. A second prototype
preserves all coded units, accounts for interior transition slots against the
requested standby interval and applies only the initial file trim and final
tail trim. One, two and three joins retain exact native decoded frame counts
and payload sequences, with independently decoded silence at each predetermined
fresh-generation boundary; an insufficient interval is refused. This is an
offline generated feasibility result, not actual app integration, real-time
pacing, independently verified native PCM timestamp continuity, Resume-to-live
testing, Android codec proof or physical qualification.
Receipts are retained under `target/privacy-standby/ios-feasibility/`.

The [native camera packet](native-camera-controls.md) now implements Front/Rear
selection before Prepare, live source replacement and capability-bound zoom,
focus, exposure and white-balance controls on both apps. A successful source
change retains one hardware encoder, canvas, audio path, recording, publisher
and grant. Front preview is mirrored separately from unmirrored encoded output;
Android uses the facing-aware sensor/pose convention and complete producer crop.
Requested settings and native acknowledgement remain distinct from optical
convergence or remote playback. Microphone-only remains camera-free.

Source retirement, first-frame deadlines, true video timestamp gaps, delayed
callbacks, Stop and incomplete recordings have explicit ownership gates. The
six-second operation ceiling retains the existing sub-two-second successful
video-gap requirement and MPEG-TS limits. A source fault fences new camera/audio
admission on its exact run. Normal Stop cannot rearm an old switch deadline or
turn a successfully cancelled switch into an incomplete recording.

The reviewed iOS V3 and Android V4 snapshots pass 205 Swift host tests and 67
focused JVM tests with warnings as errors. All three unsigned iOS configurations
and the actual Debug test bundle compile with compiler warnings as errors;
the packaged creator-owner consumer passes 47 tests. The actual products pass
required-reason privacy and byte-identical manual-profile corpus checks. The
iOS device test bundle was compiled, not executed on a phone.

Android Debug and Qualification each pass 449 tests in 57 suites with no failures,
errors or skips. All three APK assemblies, lint and isolation pass. The actual
ARM64 media library matches the current AAR, with 16 KiB ZIP and ELF alignment
verified. Lint has 15/14 warnings and zero errors, including a cosmetic Compose
function-naming warning in the new controls. Receipts are retained under
`target/camera-controls/`, with final app gates in `delivery-v2/`. No device
installation, capture interruption, private media, signer or live service was
used. Physical source switching, preview/decoded geometry, controls, audio and
recording continuity remain open, alongside standby, QR/deep-link and other
B4R/B5 work. This does not claim a performance advantage over another app.

The latest upstream integration is pinned to `004be18`. RTMP now lives in
`sapflow-rtmp`; its publishing client and receiver retain this branch's bounded
queues, explicit audio-only topology and terminal semantics. `TrackSet` lives
in runtime-free `sapflow-core`, with the existing HLS exports retained, avoiding
a production dependency cycle. Both lock files were regenerated with Cargo;
external package identities are unchanged, and the licence notice regenerates
without a change.

The optional `SAPFLOW_HTTP_IMPL=own` viewer loop is integrated while Hyper/Tokio
remains the default. The merged workspace passes 624 tests with four normally
ignored, strict all-target Clippy, formatting and locked compilation of every
fuzz target. No long fuzz run is claimed. New real-socket parity coverage checks AAC live playback, terminal-part
wake-up, retained A/V-to-AAC initialization/part/segment bytes, GET/HEAD metadata,
closed and replaced presentations, public control/API refusal and malformed or
future blocking requests. Connection-close handling now honours `close`
regardless of token/header order. Both backends pass the generated SRT/RTMP
smoke, independent HLS decoding, callbacks, reconnect with one recording and
encrypted passphrase acceptance/refusal. Receipts are retained under
`target/mobile-origin-integration/v3/`; these remain local synthetic gates.

The `0.1.0` Apple, four-ABI Android and ARM64 development media SDKs are rebuilt
and pass their installed ownership consumers. Their previous same-version
archives are preserved with the `v2` receipts. The native app results and SDK
hashes in that earlier record below belong to its exact prior source snapshot;
the camera packet's current app results are recorded above. No phone capture,
signer, private media or live service was used by this integration.

The source also integrates upstream library publication preparation at
`efea332`, including workspace version `0.1.0`, public Rust API documentation
and the server Cargo package name `sapflow`. The server's directory and binary
paths remain unchanged. Both lock files were regenerated with Cargo, preserving
all external dependency entries and the Signet patches. Merge resolution keeps
the feature branch's executable Rust behaviour; additive audio-only/topology
documentation now passes the new strict documentation lint.

Apple, four-ABI Android and ARM64 development media SDK packages build as
`0.1.0` and pass their installed host ownership consumers. Android's normal
default and SRT qualification helper derive the AAR filename from the workspace
version; the explicit absolute override and independent app marketing version
are retained. Earlier SDK archives remain available. The merged workspace
passes 605 tests with four normally ignored, strict Clippy, formatting and
locked compilation of all fuzz targets. The synthetic origin smoke passes
SRT/RTMP, independent HLS decoding, reconnect with one recording and encrypted
passphrase acceptance/refusal. Finite compilation and generated loopback checks
remain separate from long fuzzing, physical shows and publication. No crate,
app or branch has been published by this integration. Receipts remain under
`target/mobile-origin-integration/v2/`.

Against those rebuilt media packages, all three unsigned iOS app configurations
and the actual Debug unit-test bundle compile with compiler warnings as errors;
the 47-test packaged creator-owner regression passes. Android Debug and
Qualification each pass 419 tests in 55 suites with no failures, errors or
skips. All three APK assemblies, Qualification/Release lint and isolation,
actual ARM64 native equality and 16 KiB ZIP/ELF checks pass. Lint retains 14/13
warnings and zero errors. Required-reason iOS privacy resources and the bundled manual
profile corpus are verified in the actual products. The separate reviewed
SignetAuth package is retained. No phone capture was interrupted. This integration
did not install an app, execute the iOS unit bundle or exercise a live service.

The source now adds [pre-Prepare orientation selection](capture-orientation.md)
on both apps: Automatic remains the default, with fixed Portrait and landscape
mounting choices. The exact preparation retains fixed geometry through
permissions, Start and later sensor/display events. Exact ownership rejects
delayed Automatic events from a retired preparation. Microphone-only ignores
the setting. The adapters reuse the existing rear-camera transform and one
hardware video encoder; fixed mode does not track the horizon.

This packet passes 166 Swift host tests and the 47-test creator-owner regression,
all-production-source iOS 16 typechecking, all three unsigned iOS app builds
with compiler warnings as errors, and actual Debug test-bundle compilation.
The iOS bundle was not executed on a device. Android passes 37 focused host
tests, 48 selected Gradle tests and 419 tests in 55 suites in each of Debug and
Qualification, with zero failures, errors or skips. All three APK assemblies,
Qualification/Release lint and isolation, actual ARM64 native bytes and 16 KiB
ZIP/ELF checks pass. Lint retains 14/13 warnings and zero errors. Independent
source review is clear on both exact source snapshots. Local receipts remain
under `target/orientation-choice/`. Physical decoded pixels, preview fit,
fixed-mode epoch/recording continuity and Automatic viewer recovery remain open.
No phone installation, capture, private media or live service was used.

The current source adds
[saved manual destinations and reviewed JSON file import](manual-destination-profiles.md)
on both platforms. Android stores up to 16 profiles in a separate Keystore-backed
encrypted document. Both importers enforce one 16 KiB versioned format, present
only validated host/port/security for review, require independent plaintext
permission and keep Apply separate from Save, Prepare and Start. Exact import
tickets and actual capture availability reject stale replies. Android retains
its own document picker's ticket through the expected external-activity
departure, defers reading until foreground return and prevents an old result
from being adopted by a recreated activity. V4V configuration and scoped grants
remain outside manual imports.

Independent review also exposed and this packet repairs two existing iOS
credential-pairing seams. New Keychain values bind the secret to its exact
profile identity, endpoint, transport and plaintext setting, so interrupted
metadata/protected writes cannot pair an old peer with a new secret. Resolution
also checks the exact cached profile selected by the caller against fresh
metadata. Historical raw secrets remain readable until their next explicit
Save upgrades them; their original endpoint binding cannot be reconstructed.
Corrupt profile metadata now fails visibly instead of becoming an empty list.
QR/deep-link import, physical document providers, Keychain/Keystore, crash
durability and the remaining B4R controls retain separate gates.

The reviewed profile packet passes 25 focused Swift tests and the existing
159-test capture/control suite, with an iOS 16 typecheck and all three unsigned
app configurations built with Swift warnings treated as errors. The actual
Debug unit-test bundle compiles and contains a byte-identical copy of the
64-case shared profile corpus; the iOS bundle was not executed. Android passes
25 focused host and actual Gradle tests, then 410 tests in 54 suites in each of
Debug and Qualification, with zero failures, errors or skips. All three APKs,
qualification/release isolation, actual ARM64 native libraries, 16 KiB ZIP/ELF
alignment and required iOS privacy resources pass. Qualification/Release lint
has zero errors and the existing 14/13 warnings. Finite seeded mutation checks
cover 1,024 Swift and 2,048 Kotlin inputs; these are not long-fuzz acceptance.

The first full Android lint run correctly refused two calls requiring API 30.
The repaired implementation keeps API 29, uses a five-second logical timeout
and retains its IO slot until real read/close and any begun cancellation finish.
Independent review is clear on the final source. Receipts remain under
`target/manual-destination-ios/v2/` and
`target/manual-destination-profiles/android-v3/`; the original lint failure is
retained under `android-v2/`. No device installation, app launch, private media,
real provider, live service or signer action was exercised by these gates.

The branch subsequently integrates `origin/main` at `7a3435f` through merge
`422fd5b`, including receive-buffer hardening, default parser-log filtering and
the standalone listener example. The combined workspace passes 605
unit/integration/doc tests with four normally ignored, strict all-target Clippy
and formatting. The new upstream example needed formatting only. Regenerating
the 136-crate licence notice produces no tracked change. The generated loopback
smoke passes SRT/RTMP ingest, independent HLS decode, callbacks, reconnect with
one recording and encrypted passphrase acceptance/refusal. Its fixture servers
and generated media are cleaned by the harness. These remain local origin
gates, separate from physical phones or a deployed V4V show.

The current source adds the first background slice of the
[native microphone-only mode](audio-only-mobile.md) on iOS and Android. Its
immutable intent follows permissions, storage, grant admission, Start, reconnect,
private recording and exact-topology observation. Audio-only creates no camera,
video encoder or preview and waits for no video keyframe. Native output is AAC-LC
48 kHz mono at a 96 kbit/s target, with a separate 1 Mbit/s engineering grant
floor. Background continuation is off by default and is limited to an opted-in,
already committed audio-only show. Video and any audio-only show whose Start has
not committed stop on foreground departure. An eligible committed show may still
be establishing or re-establishing its authorised transport.

Android uses an application-owned, non-exported, `START_NOT_STICKY` microphone
foreground service and an exact per-show notification **Stop and save** action.
Notification and microphone authority are required before Start. iOS uses the
declared `audio` background mode and an exact in-app Stop action; SDK 18.2 has no
truthful locked-screen microphone Stop facility, so no playback/Now Playing state
is fabricated. Both platforms keep V4V End distinct from Stop and save, retain
exact journal/owner leases through real cleanup and recheck grant authority at
native transport submission. Physical lock/background, process/service kill,
network switch, notification/privacy toggles, external routes, recording and
sustained-show qualification remain open; this source/build packet does not
complete B4R.

The Android service now delegates its real command, foreground promotion,
notification, timer and teardown ordering to a production-used cold lifecycle
driver. Duplicate ARM or Stop cannot re-promote or retire a show whose native
cleanup is still retained. Failed platform promotion stops the unpromoted
service promptly while leaving exact application cleanup ownership intact;
successful promotion followed by rejected capture admission retains its
notification until actual closure. Thirteen driver regressions cover these
seams, stale/successor commands, task removal and unexpected destruction.

Foreground microphone selection now has native adapters and normal controls on
both platforms: System Default plus bounded built-in/wired/USB inventory, frozen
Prepare choice and actual-input verification before PCM admission. Default pins
its first verified input through rotation; mismatch/disconnect stops the current
capture without fallback or restart. Bluetooth/unqualified routes remain
unsupported. Physical USB, hot-unplug, background/lock and sustained performance
are open; source tests and app builds do not complete B4R. Routing
source tests and source-matched app builds are separate from those physical gates.

This source also adds explicit terminal TS input and actual recording drain.
A complete bounded AAC PES held in one start packet is no longer stranded at
connection EOF. Truncation, raw header stuffing, missing timestamps, codec epochs
and stale generations have focused regressions. Local finite tests remain
distinct from a complete native show and physical shutdown qualification.

TS ingest now checks 8 MiB of unread input before copying, limits each assembled
PES to 8 MiB and caps aggregate pending logical payload/reservation requests at
16 MiB. Bounded START preallocation is charged immediately; replacement on the
same PID includes both old and new ownership. A sticky `InputLimitExceeded`
releases pending parser input and assemblies, refuses further input and lets
`finish()` seal the accepted prefix with `media_loss` for incomplete recording.
These figures do not bound Vec capacity, allocator overhead, retained HLS data
or total process RSS.

Three original failing cases now pass: valid-continuity unbounded PES growth,
rotated-PMT reservation growth and oversized raw input. Independent V2 review
also confirms the malformed special-header accounting repair and its three
red-to-green regressions, preserving the old allocation's charge when upstream
rejects a replacement before insertion.

The earlier PES-limit snapshot passed 598 workspace tests with four normally ignored,
strict all-target Clippy and formatting. The initial sandboxed run could not
bind a loopback socket; the unrestricted loopback rerun passes, so that initial
failure is an infrastructure limitation. The current source's full origin
smoke passes SRT,
RTMP, reconnect, encryption, independent decoding and recording callbacks.
Its first run exposed a truncated final read in the black-box synthetic sender;
controlled wire capture isolated that loss before origin ingest. The sender
now validates complete TS packets and pads its final 1,316-byte block with
at most six null packets. The corrected wire bytes match the generated input;
production loss handling and smoke assertions remain unchanged. Five generated
`ts_ingest` seeds replay, and the fresh AddressSanitizer/overflow run completes
13,298 executions over 62 seconds. These finite local checks do not establish
a complete physical show, a total-memory bound or sustained performance.

App checks pass 347 Android unit tests in each of Debug and Qualification,
with three APKs, build isolation, 16 KiB ZIP/ARM64 ELF alignment and Release
lint. The current Qualification report has 14 warnings and the Release report
has 13, both with zero errors. All three unsigned iOS app
configurations and its actual unit-test bundle compile with zero warnings;
the iOS unit bundle was not executed. Focused Swift checks also pass
149 capture/control/routing, 26 catalogue/storage, 33 library/export and 33
creator-owner tests. Observation has 49 host tests and 77 packaged Rust publisher
tests; these suites overlap. An actual generated macOS decoder matrix accepts
48 kHz mono AAC without video, rejects wrong profiles/topologies and passes the
hardware A/V regression. Twelve provider and twelve loopback HTTP route tests
also pass. Fresh Apple, ARM64 Android and four-ABI Android SDK packages now build
from the PES-limit source and pass their installed ownership consumers. The 77
publisher and 33 creator host tests continue to pass against the Apple package.
Unsigned Debug, Qualification and Release iOS builds compile with warnings as
errors. Android Debug and Qualification each pass 347 unit tests; their app
assemblies, qualification isolation and lint pass, as do Release assembly and
lint. An initial Android compile used the stale four-ABI AAR after only the
separate ARM64 package had been refreshed; rebuilding the actual app dependency
resolved the missing generated bindings without a source change. These are
local package/build results, not CI or phone acceptance.

The current background-audio source has a source-matched app gate: all three
unsigned iOS configurations and the actual unit-test bundle compile/link with
zero warnings; the unit bundle was not executed. Android Debug and Qualification
each pass 385 tests in 53 suites with no failures, errors or skips. All three APKs assemble;
qualification/release isolation, native ARM64 and 16 KiB ZIP/ELF checks pass.
Qualification/Release lint has 14/13 warnings and zero errors. Debug and
Qualification use the existing debug certificate; Release is unsigned. These
builds reuse the existing native SDK packages unchanged. Focused routing checks
remain green; final Swift host gates pass 159 budget/lifecycle, 69 V4V, 50
observation, 86 publisher, 36 creator-owner and 14 configuration tests. The
production SRT Network.framework cancellation check also passes. No phone,
USB/hot-unplug, Bluetooth, physical background/lock, process/service kill or
sustained-performance qualification is claimed.

The 22 October 2026 generated native AAC checkpoint writes new audio-only
recordings as M4A, while video-bearing and historical container-less recordings
remain MOV. It uses the converter's observed priming and tail extents without
changing shared publishing payloads, timestamps or AudioSpecificConfig. All 143
native AAC access units from the 144000-frame case remain in their original
stored order and pass the packaged Rust ingress. Native playback returns the
exact 144000-, 143360-, 143123- and 511-frame input extents; the three
non-periodic presentation comparisons have zero-lag correlations above 0.99994.
FFmpeg may retain the declared dynamic trailing padding, so its decoded tail
length is not treated as a universal exact-presentation assertion. The source
checkpoint passed 38 catalogue/storage/timing and 35
library/admission/destination/owner host tests with warnings as errors,
typechecked 51 production Swift files against iOS 16 APIs, and preserved the
unchanged A/V MOV path through three strict independent decodes. The frozen
source and aggregate receipt are under
`target/ios-audio-recording-retention/v1/source-v1/`; the final generated M4A
receipt is
`target/ios-audio-only-fixture/20261022T122618Z-50ecbe54/receipt.local.json`.
This is generated macOS evidence only. Physical iPhone, background, lock-screen,
route-change, long-show, crash, power-loss and thermal acceptance remain open.
A fresh read-only Apple/CoreDevice inventory still reports the primary iPhone
unavailable. UI Automation was already enabled; no new physical runner,
installation or camera test is claimed. The Pixel's current capture remains
untouched.

Scoped origin recording completions now include non-empty immutable
`recordingTracks` booleans captured from each writer's own initialization.
Legacy callback bytes remain unchanged, codec epochs retain their distinct
topologies and synced incomplete prefixes stay labelled incomplete. This is the
producer half of replay classification. The V4V receiver still needs to persist
and fence the topology with the exact recording receipt before selecting
`audio/mp4` or `video/mp4`; current acceptance does not prove replay adoption,
decoding or complete-show aggregation.

The checkpoints below retain their original source/package context and counts.

The preceding origin prerequisite adds [explicit AAC-only ingest](audio-only-origin.md)
through our SRT/TS and RTMP paths, HLS and recording, without a video keyframe.
Track declarations are bounded and immutable per connection. Canonical AAC-LC
validation, rational 44.1 kHz timing, current-part track receipts and explicit
audio-only health are implemented. Recording loss survives epoch rollover and
protocol rejection; an ingest error cannot append a new codec under an old file
header. Main `8f9d7de` is integrated locally by merge `160e505`.

This source passes 552 workspace tests with four normally ignored, strict
all-target Clippy and formatting. Actual generated Swift and Kotlin host
consumers and eight status-lease checks pass against the current host library.
The existing mobile packages remain unchanged and represent their preceding
Rust snapshot. Five changed fuzz targets replay their historical regressions
and seeds, then each pass 61 seconds with AddressSanitizer and overflow checks:
1,569,357 mutation executions in total. These are local finite gates, not CI,
phone, sustained-load or competitive performance acceptance.

Generated loopback tests independently decode four AAC profiles through SRT or
our production RTMP publisher, HLS parts/segments and recordings; an A/V SRT
regression and nine negative cases also pass. Actual audio-only health carries
a null keyframe interval; A/V remains numeric. RTMP recordings preserve all
283/260 source frame counts. Initial SRT recordings contain 272/253. A separate
controlled test with an explicit 1,316-byte chunk and nine-second sender timer
captures the exact TS wire bytes: the unpadded test sender omits the final
1,128/188 bytes before the origin. Valid null-TS padding completes its last
block; captured bytes/hash then match the input and recordings contain 283/260
frames. Native shutdown, terminal PES cases and complete-show retention remain
open. The earlier default-chunk/EOF run remains separately labelled evidence.
No phone recording was accessed;
all test media were generated and all owned runtime children/scratch removed.
At that earlier checkpoint native mic-only mode and audio-only observation were
unfinished. Physical background/lock/routes and complete-show qualification
remain open in the current checkpoint too.

The latest reviewed source adds Capture elapsed, Broadcast elapsed and measured
Socket outbound to both native apps. Local write completion remains separate
from remote delivery or decoded-media proof. Reconnect gaps have no fresh rate;
incomplete historical totals remain labelled while a healthy successor warms
up its own rate. End freezes the broadcast clock without stopping independent
capture/recording. iOS telemetry is committed locally as `0a92e51`.

Android commit `7a50c0d` adds a private show/segment catalogue, cold profile-specific capacity
and storage checks, and explicit export through the system document picker.
Unfinished and orphaned files remain visibly unverified. Export revalidates its
original selected file, preserves the private original and retains capacity
until actual owners close. The picker is unavailable during capture. Late
permission/storage replies cannot revive a cancelled Prepare; healthy live
file ownership and uncertain cleanup are distinct. The catalogue currently
bounds scans to 256 segments; management and pagination remain open. There is
no automatic crash repair, deletion or replay publication.
This builds on `1e9b031`, which preserves recording across queued Start/End,
rotation and publisher-failure races with exact intent and actual cleanup fences.

The combined Android gate passes 305 tests across 46 suites in each of Debug
and Qualification, with zero failures/errors/skips. Three APKs assemble,
qualification/release isolation and all ARM64 libraries' 16 KiB alignment pass.
Lint has zero errors and seven/six warnings. Debug/Qualification use only the
existing debug certificate; Release is unsigned. The separate generated-media
macOS native mux/recorder test independently decodes finalised and clean-abort
segments. This does not qualify Android storage/providers or torn-file repair.

The preceding iOS telemetry snapshot passes 109 pure tests, 65 packaged control/authentication tests
and 32 creator-owner tests. These suites overlap. All three complete unsigned
app configurations compile and link with zero warnings, using identical source
and both existing native packages. The actual 94-input unit-test bundle also
compiles and links; it was not executed. Independent source and artefact reviews
are clear. No app was installed and the owner's Pixel capture was untouched.
A fresh read-only Apple device inventory still reports the primary iPhone
unavailable; the owner's confirmation that UI Automation was already enabled
does not establish a new runner or camera result.

The subsequent iOS implementation adds a private show/segment catalogue,
selected-profile storage admission, legacy unverified-file listing and explicit
folder export. Capture and export share a reservation gate; actual modal
dismissal and checked worker cleanup precede new capture. Unknown close retains
the file owners, permission scope and directory lock. Storage work runs on cold
workers; low space finalises an incomplete recording prefix while current
publishing continues. A later rotation can still stop publishing because restart
requires successful segment finalisation. This limitation remains open.

Focused checks pass 117 pure capture/budget tests, 26 catalogue/storage tests,
33 library/export tests and 17 generated privacy-resource tests. A generated
macOS check uses the production native recorder: a 3.02-second finalised file
and two 1.26-second incomplete prefixes independently decode with video and
audio; exported copies match their originals. These are generated fixtures,
not phone recordings, real disk-pressure or process-kill recovery evidence.
The required-reason privacy resource is a partial packaging checkpoint;
collection/tracking assessment and App Store readiness remain open.
All three complete unsigned iOS app configurations compile and link with zero
warnings, using identical 84-file source/package contexts. Each actual normal
app archive contains the verified root privacy resource and the expected
configuration. The actual 109-input iOS test bundle compiles and links with
zero warnings after restricting its subprocess lock test to macOS; no iOS
tests were executed. No app was installed, signed for distribution or published.

These are local source/build checkpoints. Physical profiles, current app
recording/export, real creator/signer consent, sustained performance, store and
owner acceptance remain open. The [OBS and library-quality work](library-quality-plan.md)
continues alongside these app gates; performance superiority is not established.

The preceding iOS quality commit `0710256` adds an explicit native 640×480 option beside
720p30, retaining the selected profile through Prepare, Start, rotation and
reconnects. Native camera output and compressed H.264 dimensions are checked;
1080p remains an engineering path excluded from the ordinary picker. V4V
quality admission precedes camera/publisher construction, accepts an explicit
480p choice with a 1 Mbit/s grant and refuses insufficient capacity without
changing quality. Acknowledged server operations cannot be retried merely
because local publisher construction failed. Settings reopen only after the
actual capture cleanup barrier, including rejected late candidates.

Its generated-fixture checks pass 88 pure capture/profile/destination tests,
65 control/authentication tests, 32 creator-owner tests, 39 observation tests
and 48 publisher/observation tests. These suites overlap. All three complete
unsigned iOS configurations and the actual unit-test bundle compile and link
with zero warnings or errors, using both existing native packages. The unit
bundle was not executed. This is source/build evidence; physical per-profile
capture, real service/signer acceptance, sustained performance and distribution
remain open. The iOS origin probe still confirms actual A/V decode for the
current attempt without comparing remote dimensions to the chosen profile.

Latest local checkpoint, 22 October: the requested origin-main integration is
complete as `1485ec0`; the later main documentation changes are merged as
`8116361`. SDK commit `1634a809` separates transport recovery from decoded
readiness. The full workspace gate at that checkpoint passes 475 tests, with
four normally ignored, plus strict all-target Clippy and formatting. Test-only commit
`8c2df4f` reads complete framed control responses without requiring graceful
TCP closure after early request rejection.

SDK commit `6235165` adds attempt-scoped decoded-media confirmation. An old
asynchronous decoder reply cannot confirm a successor connection, advance its
clock or clear its outage window. The client and binding crates pass 112 tests,
with three normally ignored, and strict all-target Clippy. Actual generated
Swift and Kotlin consumers, eight Kotlin status-lease checks, the refreshed
ARM64 AAR's packaged consumer and the refreshed Apple package's external
consumer pass. This is an additive source/package gate; it does not attach a
native origin decoder or qualify either phone. Earlier frozen app packages are
preserved separately from these refreshed SDK artefacts.

Android commit `aa8d5826` adds explicit pre-Prepare 480p and 720p profiles with
immutable profile admission and scoped SRT pacing. Its frozen package passes
183 JVM tests in 34 suites, lint with zero errors and ten warnings, actual
qualification/release isolation and native alignment checks. The signed
qualification APK has not replaced the owner's active Android capture. Actual
480p hardware, sustained grant-cap and orientation qualification remain open.

Android commit `b3b7398`, following attempt-correlation commit `97f0656`, adds
finite native hardware-video and PCM-audio observation. Fresh authenticated
status before and after decoding, captured attempt identity and current grant
authority guard SDK confirmation. Its reviewed qualification and unsigned
release APKs pass 218 JVM tests in 37 suites, lint with zero errors and ten
warnings, package isolation and ARM64 alignment checks. The new decoder has
not run on the Pixel; the owner's existing capture remains untouched.

iOS commit `fdc731fc` provides typed V4V contracts, bounded URLSession control,
durable session operations and encrypted journal storage. It passes 35 host
Swift tests and an iOS 16 compile check with warnings treated as errors.
Independent review is clear. At that foundation checkpoint creator sign-in,
application wiring and real Keychain/network acceptance remained open. The
normal application source below extends that work; the installed app remains
separate from these source gates.

Further iOS source adds a finite production origin-observation component with
36 passing host tests. The earlier component passes an iOS 16 compiler check.
It requires hardware H264
decode on iOS 17 or later, real PCM progress, fresh status on both sides of
decode and a final attempt-scoped SDK confirmation. The generated-media macOS
gate produces six hardware video outputs and 7,104 PCM frames, rejects
header-only input and confirms cleanup. Delayed observer replies cannot mutate
the creator's refreshed grant. This component is separate from application
wiring, physical iPhone decoding, real creator sign-in and sustained performance.
The new scoped publisher composes the existing reconnect owner, captured SDK
attempt, fresh nonce and origin observer. Its combined host gate passes 41
tests against the actual packaged Rust session, using simulated transport and
decode facts. A single app-owned observation slot retains uncertain cleanup
without polling or admitting another decoder. Scoped queue/pacing limits,
early codec-close fencing and an independent monotonic grant-expiry alarm are
implemented in source. The normal creator application wiring is described
below; the installed app has not been replaced by this work.

The separate maintained Signet authentication package now builds for ARM64
macOS, iOS and iOS Simulator. The actual combined static consumer, iOS 16
module/typecheck and combined iOS static link pass. The scoped worker and
dependency packet passes 512 workspace tests, strict Clippy, parser fuzz and
ten packaging regressions. The generated app project declares both packages;
the full unsigned iOS application builds and links both with zero warnings or
errors. Authentication headers have their own module directory, avoiding a
shared Xcode include destination. The normal application now includes creator
sign-in controls; real remote-signer consent and physical app qualification
remain open. No real account or relay was used by these build gates.

The creator-read packet passes 63 Foundation tests, including twelve new tests,
and real-package macOS/iOS 16 compiler checks. Its event list is bounded and
display-only. Fixed authenticated collection and exact-session reads preserve
uncertain operations and terminal End intent; reads never recover a grant.
The earlier capture-admission fix passes 59 host tests, including twenty ticket
tests. All 32 production Swift files at that checkpoint typecheck for iOS 16 with
both real native modules and no warnings. Prepare, permission replies, Start, Stop and rotation retain
their exact admitted intent. Stop waits for outstanding destination construction
and actual rejected-destination cleanup before allowing another intent. These
are source gates; physical capture remains open.
CI now includes source verification and the generated-only source and packaging
regressions. Those commands pass locally; hosted CI has not run for this branch.

The normal SwiftUI application now has explicit Record only, Manual destination
and V4V modes. A single app-lifetime creator owner composes the real Signet
adapter, separate encrypted publish and signer journals, typed event reads,
durable session operations and the scoped publisher. Connect signer, Prepare
and Start are explicit. An uncertain operation retains its original identity;
inactive mode cannot attach a late grant or start capture. Refresh requires an
acknowledged lease, and recovery acknowledgement and replacement are separate
actions. V4V remains free, unlimited and without a watermark.

End fences a delayed first Start while preserving a recording already in
progress, including its orientation epochs. Publishing starts before the Run
is exposed to initial samples; publishing startup failure closes only that
worker. Leaving a manual destination fences its exact publisher independently
of delayed display state. Closed creator owners retain the observation slot,
HTTP, signer and journal ownership until actual cleanup. The fixed production
journal lock prevents a replacement production owner overlapping that cleanup.

The current iOS source also gives an authenticated creator an explicit way to
end an active event broadcast that has no local journal. The destructive
confirmation carries the exact displayed event/session/generation/revision into
the owner. A fresh exact-session GET must complete within both its eight-second
permit and the original 60-second command window. Only an exact active receipt
is atomically persisted with terminal End intent; no grant, publisher, camera
preparation or capture authority is adopted. Identity changes, contradictory or
late replies and journal-save failure send no End. The resulting terminal scope
then uses the existing durable End request and restart/retry rules.

This packet passes 72 Foundation control/authentication tests and 40 creator-owner
tests against the actual Apple packages. The generic unsigned arm64 Debug app
also builds with warnings treated as errors. Independent review found and the
implementation closed both a stale confirmation identity race and a retry that
could otherwise adopt terminal ownership after the original command window.
These are host and compiler gates; no service request, signer consent, signed
installation or physical iPhone flow was exercised.

The follow-on creator-owner continuity packet retains an automatic V4V grant
refresh deadline while Start or another command owns admission, and reconciles
an overdue deadline when the scene returns to the foreground. Each scheduled
refresh is bound to the publisher binding's original generation and exact
capture ticket. Stop, End, expiry and retirement therefore fence both delayed
alarms and an already-admitted refresh whose queued action has not run. A
durably staged refresh still uses the existing exact-body and nonce retry path.
The focused creator-owner suite passes 47 tests, including deterministic Stop
races on both sides of automatic command admission, duplicate-alarm coalescing,
foreground reconciliation and expired-authority fencing. This is source and
host-test evidence. The generic unsigned arm64 Debug app also builds with Swift
warnings treated as errors. No service, signer, signed installation or physical
show was exercised by this packet.

V4V is unavailable by default in Debug and Release. A separate Qualification
configuration requires explicit approved routing and native authentication.
Qualification settings do not enable the ordinary configurations. These source
and build boundaries do not establish a deployed native-session backend, real
signer consent, Keychain acceptance or a physical V4V show. No installed app or
active Android capture has been replaced by this work.

The earlier creator-wiring checks passed 74 pure capture/destination tests,
64 control/authentication tests, 28 creator-owner tests using both real native
packages, 39 observation/cleanup tests, 46 publisher/observation tests using the
real Rust policy, and 14 configuration tests. These suites overlap; the counts
must not be added as unique coverage. The final creator source also passes
all three actual-package iOS 16 compiler checks without warnings. Independent
source review is clear, including the actual asynchronous Signet cleanup
notification after a prior failure consumed its result callback.

The complete unsigned app builds for Debug, Qualification and Release with zero
warnings or errors, using identical current inputs and both real native
packages. Actual built property lists confirm default Debug is disabled,
Qualification expands its explicit example profile, and ordinary Release remains
disabled even when qualification variables are supplied. All three binaries
contain the expected authentication and attempt-scoped media-confirmation
symbols. The actual iOS unit-test bundle also compiles and links without warnings;
it was not run. No signing, installation, device capture or real account action
was performed by these gates.

Test commit `d0643ff` adds a generated-media-only macOS native reader gate.
The current origin's exact init plus independent part passes independent FFmpeg
and Apple decoding; the latter produces six video outputs and 7,104 validated
PCM frames in one output buffer. Header-only input is refused. Five wrapper
regressions and independent source review pass. The harness removes generated
media and stops its children. This finite format check does not qualify a
production observer, phone decoder, hardware decoder or sustained performance.
See [native origin observation](native-origin-observation.md).

The refreshed ARM64 AAR passes its actual packaged consumer. Android commit
`4e2ff845` retains one logical destination across codec epochs and consumes the
new typed startup/progress events. Its exact 108-input source packet passes
174 tests in 33 suites, 21 APK-verifier tests, lint with zero errors and ten
warnings, actual qualification/release isolation and ARM64 native alignment
checks. Independent source and artefact review is clear. These are source and
package gates; they do not qualify the new APK on a physical network.

Separate commit `be965e5a` adds opt-in localhost TLS fixtures to the
qualification build only. Its 33 verifier tests and actual APK isolation checks
pass; removing the fixture properties restores the default APK exactly, and
the production APK stays byte-identical. No system trust installation or new
physical V4V authentication result is implied. The owner's current Android
capture remains running; neither new qualification APK has replaced it.

The iOS reconnect source at `a399d13` passes 30 pure Swift tests and a real
Network.framework cancellation probe without media. Its frozen application
and test bundle compile, and the development-signed app passes strict signature
verification, installation and launch on the primary iPhone. An initial UI
inspection stopped at the lock screen. A later fresh device query confirmed
the phone was unlocked and the UI runner launched, but XCTest timed out while
enabling automation mode, before a test method ran. The owner confirms that
UI Automation was already enabled. The cause remains unresolved; no camera
test ran. A metadata-only result audit found no attachments and extracted no
payloads. Installation does not establish physical capture, publishing,
recording or reconnect acceptance.
Later text-only commit `5d5133a` corrects reconnect guidance; it does not rebuild
that frozen app. Native iOS creator authentication and release acceptance remain
open. New SDK packages do not silently replace the signed application.

The device results below identify earlier APKs. In particular, the earlier
manual reconnect APK stopped at its original outage deadline despite recovering
the socket. The new transport policy addresses that source limitation; a new
physical qualification must establish its behaviour. No changes from this task
have been pushed or deployed publicly.

### Earlier integration and device evidence

Merge `1485ec0` integrates origin main
`10b7e40` into `feat/native-broadcaster-sdk`, including own SRT as the sole
server implementation and removal of srt-rs. Cargo regenerated both lockfiles;
the licence generator regenerated its output and reproduced it unchanged.
The merged Rust workspace passes 347 tests, format and strict Clippy. Both
explicitly run SDK decoder/reference tests and the origin's three-recording
epoch decoder gate pass. All 11 fuzz targets compile against the regenerated
lockfile. Four tests are ignored by the normal workspace command; the two SDK
tests and the recording decoder test were also run explicitly.

Before this merge, AddressSanitizer runs with overflow checks passed 61 seconds
each for RTMP client, mobile media and own SRT caller, with 162,047, 3,665,058
and 20,669 executions respectively. These finite local runs do not replace
longer fuzz, CI or physical acceptance.

The Pixel has published real hardware video and AAC through our AES256 SRT
sender to our encrypted own listener. A bounded origin sample independently
decodes with about 9 ms between track starts, and the unchanged native Safari
source presents frames. The owner confirms picture and sound with an estimated
delay under two seconds on this path. The earlier RTMP test also received an
under-two-second owner report, with automatic picture and sound recovery after
portrait and landscape changes in the same untouched Safari view.
This initial SRT run uses 120 ms negotiated SRT latency,
not the two-second reference network profile. Its independent phone recording
has not been accessed. A separate normal-Stop correction passes 112 Android
tests, lint and isolation. Its signed qualification APK also passes one physical
normal Stop and same-event restart: the origin received SRT shutdown about
381 ms after the Stop request, versus the previous build's five-second peer
timeout. The same app process restarted, a later origin segment independently
decoded as H264/AAC, and the unchanged Safari source resumed frame presentation
without another Play, seek or reload. This finite check does not qualify
repeated restart, orientation, carrier, recording or sustained behaviour on
that APK. That Stop-test origin preceded the main merge; the newer reconnect
qualification below uses a separate frozen APK and the integrated private origin.

V4V commit `711d1a8077c4bea9e1ed4bb155a9bcf6a8bcbb2b` on a separate local branch
prevents ended-event telemetry resurrection and rejects recording copies when
the prepared session changes. Live browser and generated-media ingest gates
pass. The follow-on scoped connector now has real creator prepare, refresh,
status and End, durable terminal-intent retries, and session/epoch/sequence
callback and recording fences. Its source at `28e8e313` passes 112 relevant
tests; the local branch retains the safe generated-media evidence and contract
at `182aef1b`. This remains separate from physical native creator integration.
No changes from this task have been pushed or deployed publicly.

### Scoped origin and connector checkpoint

The [scoped-session contract](scoped-publish-sessions.md) is implemented locally.
It provides a separate bounded loopback control listener, durable metadata with
no grant secrets, required per-session AES256 SRT, monotonic revisions and
connection epochs, terminal replay fences and active idle cancellation. Released
or cancelled leases cannot reopen admission or erase a refreshed grant's keys.
Restart restores admission disabled; retrying the old exact provision stays
inactive until a fresh higher-revision refresh. Retained session IDs cannot alias
another event's control route.

Core, store, manager, HTTP, callback and recorder checks pass, including 88 server
tests. The production parser/state fuzz target passes 12,332 executions over
61 seconds under AddressSanitizer with overflow checks. Its eleven original
synthetic seeds and the target participate in the existing dynamic CI fuzz list.
This finite local result is not a CI or sustained-network qualification.

A real isolated V4V/origin pair exercised signed creator login, provision,
encrypted black-box libsrt publishing, active refresh, independent H264/AAC HLS
decode, active End, callback delivery and independent HTTP replay decode. A saved
End request made while the origin was down recovered through the real retry
worker with its original session, revision and idempotency key. Wrong admission
and AES secrets, superseded generations and late terminal telemetry were refused
or ignored without reopening the event. An admitted idle encrypted caller also
reached native protocol shutdown on credential expiry. These tests used generated
colour/noise and sine media only; they did not exercise a phone or a real show.

The generated over-rate test exposed and then verified a recording correction:
rate rejection, ingest discard and SRT queue/transport loss now retain a synced
accepted prefix as `incomplete`. A normal clean End can still finalise. On the
corrected binary, V4V retained the incomplete recording receipt, kept End
incomplete and imported no replay. A finalised epoch file alone still does not
prove complete-show aggregation. The connector now has an explicit acknowledgement
to abandon automatic adoption of an incomplete/failed recording. It returns no
grant and preserves the failed receipt. A separate explicit Prepare can reserve
one acknowledged successor, including when the previous event ended. Ordinary
Prepare cannot reopen an ended event; cancelled or archived events stay closed.

The generated cross-runtime recovery check passed actual creator authentication,
fresh independent H264/AAC media after two connection epochs, End with a genuine
failed recording callback, acknowledgement and explicit replacement. A withheld
provision reply retried the exact successor request without minting another
session. Paused old recording copies and late callbacks could not attach to the
successor. Status polling and mutations now have separate rate budgets, so polling
cannot consume the End allowance. This evidence uses the frozen origin before
the newer attempt-ID and scoped-filename changes; their runtime gate is separate.

Both isolated listeners and every generated-media child were stopped, and their
temporary credentials, stores and media removed after safe receipts were retained.
The owner's separate camera lab was not changed. Scoped native creator login,
refresh and observed-media confirmation, production configuration and deployment,
complete-show recovery and physical iOS/Android acceptance remain open. Global
lab credentials are not a production connector.

The legacy generated-media smoke gate also passes SRT and RTMP ingest, health
and publish callbacks, independent segment/part decoding, grace reconnect with
one recording, encrypted ingest and missing/wrong-key refusal. Its master check
now permits codec-epoch dimensions without a fixed resolution attribute and
independently checks the decoded H264 dimensions and AAC profile instead.

### Native reconnect source and package checkpoint

Android now retains one shared destination session across orientation epochs.
Typed transport failures schedule a fresh attempt within the original grace;
the old transport must finish bounded cleanup first. Stop fences retries before
the codec handler queue. Failed cleanup retains its owner, and stale attempt,
destination or media-epoch callbacks cannot reopen it. Network loss leaves the
independent camera/audio capture and private recording running without adding a
disconnected media backlog. Each accepted attempt requests a fresh keyframe.

The SDK's new `replaceTransport` operation distinguishes healthy intentional
codec/socket replacement from failure. It starts no outage for a healthy
rotation and preserves the exact original deadline during an existing outage.
Its generated Swift and Kotlin consumers pass actual calls. The full Rust
workspace passes 443 tests, with four normally ignored, plus strict Clippy and
formatting. The ARM64 AAR builds and passes its packed-consumer check. This is
an ARM64 qualification package, not all-ABI or physical iOS acceptance.

The frozen Android source packet has 88 inputs. Its M4 build passes all 135 tests
in 29 suites, lint with zero errors and seven warnings, and actual APK checks for
qualification/release isolation with the new AAR and native library hashes.
The qualification APK retains its private test controls; the release APK has
neither those controls nor the benchmark entrypoint.

The signed qualification APK subsequently passed one physical unattended recovery
after a 12-second Pixel Wi-Fi cut. Fresh origin media was observed about 11.4
seconds after Wi-Fi was re-enabled. The same app process continued capture;
video/audio counters advanced and the private recording timer progressed from
68 to 122 seconds without Stop, Prepare or Start during recovery. The same native
Safari instance resumed presentation with its source assigned once. A recovered
origin segment independently decoded as H264/AAC; its temporary sample was deleted.
Phone recording files were never accessed, so their file continuity and decoding
remain unqualified. This one LAN test is not a sustained, carrier, rotation,
timed capture-to-screen latency or competitive-performance result.

Native V4V authentication and trusted current-media observations are still open.
Manual destinations never turn a handshake, ACK or local encoder into a playable
media confirmation. The earlier tested build reached its original outage deadline
and stopped publishing; it is not ready for a show. The new
[transport recovery policy](transport-recovery.md) keeps retry permission separate
from decoded readiness. Timely current-attempt peer media progress can re-arm
network recovery without a playback API. An accepted bounded sending attempt
with unknown ACKs can continue after its window expires, with further retries
disabled. New physical qualification remains required. Attaching the real V4V
decoded-observation contract is still necessary for verified playback status.
Later, unattributed re-admission and reset counters
are excluded from the finite recovery result. The test ended with normal Stop,
and its owned origin, viewer and proxy processes were stopped.

The origin's scoped `live` receipt now requires an independent completed part
from the current connection. Retained old HLS content and a changed-codec first
IDR alone cannot qualify it. Authenticated `freshMedia` identifies retained init
and part resources; optional `connectionAttemptId` correlates the accepted SRT
attempt. Neither field proves decoding. Scoped recording names now include their
callback recording identity, so same-second codec epochs cannot advance the
filename's start time or overwrite one another. The combined Rust source passes
456 workspace tests, four normally ignored, and strict all-target Clippy.

## Product constraints

- Standalone native SwiftUI iOS and Compose/Kotlin Android broadcaster, backed
  by a reusable Rust SDK and suitable existing ForgeSworn components.
- Our own SRT and RTMP/RTMPS protocol state machines. Platform hardware video
  and native AAC, maintained TLS/cryptographic primitives, no borrowed
  broadcaster SDK. Never read libsrt/srt-rs implementation source.
- Reliable publishing, independent recording and origin playback first;
  RelaySwarm follows its existing qualification gates.
- Open source, unrestricted free Android edition through Zapstore and no
  required Google Play runtime for that route.
- Always-free, unlimited, watermark-free V4V broadcasting in every build.
  Store pricing and the external-destination allowance remain undecided.
- Performance superiority needs matched physical measurements. Proposed
  percentage improvements are not achieved results.
- Real camera footage, extracted frames, phone recordings and archives that
  contain them must never be committed or published. Private lab evidence
  remains outside source control; only verified generated media may be a fixture.

## Licensing decision

The owner chose to retain MIT for the broadcaster and SDK, accepting permitted
closed commercial reuse and competing through price, performance, reliability
and the official product. The existing public Sapflow repository remains MIT;
no licence file or package licensing metadata needed changing. The valuable
client, protocol and native app work stays within that decision. Third-party
copyright and licence notices remain required.

Always-free unlimited V4V and the unrestricted Zapstore edition remain firm.
Store pricing, allowances and paid value are separate product decisions and
have not been implemented or silently frozen by this licensing choice.

## Current implementation and evidence

Source base at delivery start: 8af1d91 on the existing origin-fix branch.
The other origin session subsequently advanced the base through 37f428a.
The mobile work is isolated on feat/native-broadcaster-sdk.
Its changes were preserved; these are development bases, not release receipts.

| Component | Evidence so far | Remaining boundary |
| --- | --- | --- |
| sapflow-client | 37 regular tests passed, two external tests run separately; own SRT/MPEG-TS publisher and exact bytes into independent libsrt | Native journey, storage and platform qualification |
| Own RTMP publisher | Bounded Hard/Soft/Dynamic peer-window governor and real media-age deadlines; independent ffmpeg received 180 H264 and 283 AAC frames over 5.9 MB; an explicitly injected Hard control separately exercised the governor | Sustained mobile TLS/network and performance qualification |
| Own SRT caller/sender | 54 unit, 21 caller and 3 differential tests passed before encryption changes | Complete encrypted interoperability, negotiated late drop, adaptation, reconnection and allocation optimisation |
| Independent SRT receiver | libsrt 1.5.4, exact bytes in no-loss and six seeded impaired profiles | Physical carriers, sustained runs, congestion/energy comparison |
| Native ownership/sample/session bindings | 28 regular binding tests passed; expanded actual Swift and Kotlin consumers exercised shared samples, recording and SRT handshake/stop/stats | Physical hot-path profiling, complete session integration |
| Apple package | ARM64 macOS, iOS and Simulator slices refreshed with RTMP/session bindings; external Swift consumer and real unsigned network app/test bundle compiled | Signing/install and physical qualification |
| Android package/app | Refreshed four-ABI AAR and external Kotlin consumer passed; actual network APK built, 16 JVM tests passed, lint zero errors | Current-source package refresh, audio/file recording and sustained physical qualification |
| Physical Android capture and broadcast | Pixel 10 Pro XL, Android 17/API 37: hardware H264/native AAC through our RTMP and AES256 SRT to our origin; independent decode and Safari presentation; owner reports picture/sound under two seconds, and RTMP portrait/landscape automatic recovery | Earlier 10.5 s delay superseded for the tested LAN path; timed latency distribution, phone recording, sustained and comparative measurements remain open |
| Native iOS capture/recording | Hardware-only VideoToolbox, native AAC and independent fragmented AVAssetWriter source; actual app and test bundle compiled; 5 pure Swift tests passed | Physical capture, recording decode/recovery and complete publishing journey |
| MPEG-TS writer | Independent ffmpeg video/audio and audio-only decode passed; bounded 1,316-byte chunks and explicit copy accounting | Real native sender integration and sustained measurements |
| Encrypted SRT | Integrated tests and independent bidirectional AES-128/256 rotation/loss tests passed; wrong/missing keys delivered no media | Full hostile-input qualification, authenticated destination secret provisioning, mobile integration and performance |
| Fuzzing | RTMP-client, SDK-media and own-SRT-caller pass current ASAN/overflow runs for 61 seconds each, using generated seeds | Longer runs, CI and physical network acceptance |

The SRT receiver harness is
`python3 crates/sapflow-srt/tests/black_box_sender.py`. Its initial independent
receipt covered 67,492 bytes, SHA-256
`4b1b1b22fd4df8cb65c2ae31b5a98c7f3dd084d42d75eac8c1acfa60f63ebaf3`.
Impaired profiles used 10% data loss, 10% ACK loss and 0 to 20 ms reordering.
They required retransmissions; these are compatibility results, not efficiency
or Larix-parity evidence. Task-owned receivers, proxies and staging were removed.

The encrypted harness is
`python3 crates/sapflow-srt/tests/black_box_crypto.py`. Eight positive cases
cover AES-128 and AES-256 in both directions against libsrt 1.5.4, clean and
with seeded 10% data loss, 10% ACK loss and up to 20 ms reordering. Each
delivered the same 140,812 bytes, SHA-256
`59643299c6a6890d9a2c74cef4aaf3a02395db80a4d4a01d5af2d2221aa84339`,
with both key slots and three refresh requests observed. Four wrong/missing
secret cases delivered zero media. These finite executable checks do not
establish cryptographic authentication, broad SRT compatibility or physical
network acceptance. AES-CTR does not authenticate media. Control packets and
stream IDs remain visible, as specified in the public
[SRT protocol draft](https://datatracker.ietf.org/doc/html/draft-sharabayko-srt-01#section-6.1.1).
Those initial encrypted checks preceded the separate server/default integration
recorded below; they did not switch public credential policy.

Native build commands are `node scripts/build-native.mjs check`, `apple` and
`android`. An explicit `android --abis=arm64-v8a` produces a separately named
ARM64-only development package; the default Android command still builds all
four ABIs. They reuse this checkout's target directory and remove owned staging.
Packages are development artefacts under `target/native-packages`; they are
not published or signed mobile releases. Builders retain checksums and notices.
The refreshed Apple development archive SHA-256 is
`71f4d0c1d046336b21f68e207a788770f0854b85def0548a097e32540d6434a6`.
This newer snapshot includes recording, microsecond SRT scheduling and RTMP
wire-age deadline bindings; its expanded external Swift consumer passed.
The matching ARM64-only development Android archive is
`ca03f58dbe856082b1d57722d979eab93b122ffc8f22961013540871d71b96cf`.
Its expanded Kotlin consumer and archive readback passed.
The refreshed four-ABI Android archive SHA-256 is
`748744876af3e751c34b4c85df82ce52a4f08b462c594c7654a3403dee2ec673`.
Subsequent source changes require another package refresh; these are finite
snapshot receipts, not qualifications of later protocol revisions.

## Delivery gates still open

### Sustained-publishing investigation, 21 October

The frozen Android `drain-throughput-debug.apk`, SHA-256
`159643b2fab9988e0e896014e8e8f61dacbc04cb685c46eebf769a5981f96f6b`,
passed 37 JVM tests and lint with zero errors before its physical run. It
released publisher admission on the worker rather than waiting for a codec
handler continuation. The initial physical observations had zero publishing
resets: queue high-water 405 ms, five startup wait-for-IDR rejections and no
additional ingress rejection at about three minutes.

This candidate failed sustained acceptance. The origin logged its first
missing-media discontinuity about eight minutes after connection, followed by
16 discontinuities before the clean stop. At the ten-minute checkpoint the
phone reported 11 queue resets and 1,720 dropped samples. The publisher had
consumed 45,369 samples against 47,169 captured audio/video samples. Average
socket writes were about 220 microseconds; average per-sample consume was
8.255 ms and handler continuation delay was 6.157 ms. These measurements point
to insufficient handoff throughput on this device, without establishing a
general network or codec performance result. Increasing queue limits is not
the fix being qualified.

The bounded-batch candidate, SHA-256
`60819df1a94bfa964568724842c5c72c7ab01a6f88bf36435f6ffbe822308740`,
passed 42 JVM tests and lint with zero errors. It retained handler ownership of
native queue pops, existing queue limits and independent recording. At about
10 minutes 50 seconds its physical queue checkpoint had zero resets, zero IDR
requests and a 277 ms high-water mark. Only three startup wait-for-IDR samples
were rejected. The whole 12 minute 35 second origin session had no muxer breaks.

That run still failed playback acceptance: audio progressively fell behind
video and Safari stalled. Independently inspected live-part bytes contained
video PTS 493.333 to 500.767 seconds and AAC PTS 464.526 to 470.244 seconds.
Accepted AAC output also advanced slower than the required 46.875 units per
second. Zero queue resets therefore did not establish A/V continuity. The
frames-read counter did not validate microphone progress against its hardware
clock; AAC input availability shared the busy video/fanout callback handler.
The next source candidate isolates AAC callbacks and validates real microphone
position/time, read backlog and input/output cadence. Physical acceptance of
that correction remains open. No phone recording was accessed for diagnosis.

The origin in that run was SHA-256
`8639ee51056a01479bb37d1a7509f977ae5d95ae43daff28a20b42dd60272f68`.
The HLS test viewer could initially play around 1.5 to 2.6 seconds from its
estimated origin edge. That metric is not a capture-to-screen measurement.
Later viewer stalls coincided with real upstream gaps; earlier parsing
warnings still need their full fragment/timing evidence. The reusable
`tests/live-viewer.html` now captures error reasons, all buffered ranges,
fragment/part metadata, response metadata, bounded media bytes, init segments
and adjacent parts. Its diagnostic retention/export checks run with
`node tests/live-viewer-diagnostics.test.mjs`.

The viewer/performance fixtures are committed locally as `db346ef`. The
performance observer has 25 passing deterministic tests and a completed
lightweight idle-device smoke check. A short late-run diagnostic with repeated
memory-query timeouts was intrusive and is not a valid performance baseline;
use its explicit lightweight options for subsequent tick-only observations.
No matched Larix run or energy comparison has been completed.

The iOS orientation source now produces physical portrait encoder dimensions
and starts a new publisher/recording segment for live rotation. Fourteen Swift
tests, an unsigned arm64 app/test build and an independent portrait RTMP
fixture passed. Physical iPhone orientation and viewer recovery remain open.
The next Android portrait/audio candidate passed 68 JVM tests and lint with
zero errors. On the Pixel, its real H.264 bitstream decoded as 720×1280 with
autorotation disabled; 546 video and 849 AAC frames were independently decoded
from the short origin prefix. This proves encoded dimensions, not upright
four-pose acceptance: no asymmetric visual chart was available.

That candidate failed its hardware audio-continuity guard after about 20
seconds. A second source defect then reset an already-completed microphone
worker flag during Stop, preventing native teardown until its six-second
deadline. The origin eventually closed on idle timeout and finalised an
18.2-second prefix. The failed app was force-stopped; its private recordings
were not accessed. A bounded dedicated PCM-reader/pump path and monotonic
worker-completion fencing were then tested in a separately frozen candidate.
Its 75 JVM tests and lint passed, and physical failure cleanup subsequently
completed: all native owner flags were released, the publisher closed promptly
and the origin finalised a 53-second prefix. Throughput still failed after
about 55 seconds when all eight PCM blocks were occupied, seven waiting and
one held by the encoder pump. The microphone hardware position continued to
match frames read. Long encoded-callback delays and rising allocation counters
remain diagnostic evidence, not a completed causal or performance result.
The next implementation moves compressed AAC construction/release to its audio
owner and uses a bounded immutable-handle handoff to the single fanout owner.

An offline, synthetic binding benchmark on the Pixel measured about 0.251 ms
mean for one native sample getter, 1.063 ms for four getters, 0.474 ms for sample
creation/release and 2.132 ms for creation/fanout/both pops/releases. Each case
used 32 warm-up and 200 measured iterations through the actual installed AAR.
This finite debug benchmark identifies boundary overhead; it measures no
broadcast quality, whole-app energy or advantage over Larix. A combined native
RTMP operation now passes 13 targeted tests and real generated Swift/Kotlin
host consumers, including successful results, partial output, deadlines and
errors. Its refreshed ARM64 development AAR is SHA-256
`4d782d501ab8c171aaf83a7dcbe70675988c32f5551c3916e1149f530286fc1a`.
The previous `5f0822…` package is preserved under
`target/native-packages/android-arm64-v8a-before-fused`; the completed physical
runs above used that older package.

An installed Pixel debug benchmark then measured the previous RTMP call
sequence at 2.261 ms mean and the combined operation at 1.330 ms mean, with
200 measured iterations after 32 warm-up iterations per case. The combined
case had an 82.934 ms maximum outlier; its 95th percentile was 1.019 ms. This
is a sequential synthetic binding comparison, including sample creation and
release, without camera, microphone, sockets or decoding. It is not sustained
broadcast, battery or Larix evidence. The frozen offline-only APK is SHA-256
`444d473a9a7f29844862eae9ed08373dadff96d5d86ac000a1a63a2e066353f3`;
its receipt and displayed results are retained under `target/mobile-android`
and `target/physical-android`. Live activation remains to be qualified.

The combined-operation/AAC-owner candidate passed 85 tests and an exact-source
M4 build/lint check with zero errors and seven warnings. All 71 frozen source
hashes matched, as did the release-built native library. Its APK was re-signed
with the existing development certificate; all 149 non-signature payload
entries were unchanged. No uninstall or phone-recording access was needed.

Its physical run still failed after about 30.6 seconds: a compressed AAC
handoff reached 256.158 ms against its 250 ms bound. PCM stayed caught up with
a two-block high-water mark, while publishing had zero resets and a 99 ms
queue high-water mark. Cleanup released every owner and the origin promptly
finalised a 28.433-second prefix. The frozen APK is SHA-256
`d0a56e369418346b41fbec580d60947d11c5464d1f967563a5fafc581de686ff`.
The physical receipt is
`target/physical-android/aac-owner-fused-physical-receipt.json`.

The reported 4.96 ms AAC conversion average includes clock work, platform
buffer access, native sample construction and platform buffer release. It
does not isolate Rust or JNA cost. About 188 MB of reported allocations and
504 ms of cumulative GC time over this run do not prove a 256 ms GC pause.
The app was debuggable; performance attribution now requires a controlled
non-debuggable internal build with the same media code, AAR and queue bounds.

The controlled non-debuggable/profileable variant passed the same 85 tests,
lint with zero errors, release-source isolation and actual APK manifest/native
checks. Its 74-input archive changes only build/verification inputs from the
previous candidate. With explicit ART `speed` compilation in both controls,
the fanout benchmark fell from 2.133 ms to 0.100 ms and the combined RTMP case
from 1.284 ms to 0.062 ms. These are finite offline control measurements.

The same variant still failed live acceptance after about 63.7 seconds on a
212.213 ms PCM block age against its unchanged 200 ms bound. Publishing had
zero resets and a 127 ms high-water mark; all cleanup owners completed, and
the origin promptly finalised a 61.034-second prefix. The installed APK is
SHA-256 `71f9f9cd4796b58035e9197d7cb9bca087a079d164540abb5598452b81102e20`.
Its physical receipt is
`target/physical-android/qualification-physical-receipt.json`.

Own-process GC logs identify `NativeAlloc` collections and actual allocation
waits up to 827.105 ms around the failure. The final collection's reported
pauses were 0.719 ms and 1.227 ms, while total collection time was 1.117 s.
The long wait is established. Two subsequent process-scoped allocation traces
passed loss/error/heap validation before attribution. The native trace sampled
605.46 MB of cumulative allocation, ending with 3.60 MB of tracked live bytes;
actual JNA Memory/Structure allocation sites accounted for 3.43%. Generic JNI
frames around every Rust call are not evidence of JNA allocator dominance.
That instrumented run failed with all eight PCM blocks occupied and completed
cleanup. Profiling overhead prevents treating it as a performance baseline.

The separate 30-second ART trace sampled 199.50 MB of allocation. Explicit JNA
Structure, Memory, marshalling and cleanup contexts accounted for 128.51 MB
(64.42%); UI/text accounted for 2.63%, and app diagnostic formatting for 0.95%.
Many sampled stacks pass through generated status construction during receiver
clones, fanout pops and buffer frees. ART allocation records do not establish
retained bytes or leaks. The short run remained capturing until normal Stop,
returned to idle and finalised a 29.8-second origin prefix. It does not pass
sustained acceptance or prove the cause of a particular GC wait.

Validated trace receipts and complete attribution are retained in
`target/physical-android/native-alloc-trace/native-analysis-22333.md` and
`art-analysis-23473.md`. The controlled experiment now reproducibly generates
four lazy per-thread nested call-status slots, retaining legacy allocation on
cache overflow. Error-buffer ownership, nested frees, native handle consumption
and all media/queue bounds remain unchanged. Two generator-guard tests, eight
actual host helper suites, the Swift/Kotlin consumers and independent review
passed. The extracted AAR-class consumer also verifies compiled cache reuse.
The refreshed ARM64 AAR is SHA-256
`6102340fe9df6b9eaf674fa02afb3c9dd6d89dbd63998fbe0ede5816798fd4b2`;
its native ELF remains byte-for-byte identical to the preceding control.
The exact-source Android comparison build passed 85 tests, lint with zero
errors and release-source isolation. Its 76 frozen Android inputs change only
the verifier and add its tests/fixture. Physical allocation attribution and an
untraced sustained run remain required before calling this a performance fix.
Neither failed candidate established Safari playback: the viewer's first
manifest request followed unpublication, so the resulting 404 is not an
independent origin regression.

The installed status-cache comparison APK is SHA-256
`8d5205796febf19864e6dd1322974e9fbca6810220555fa83354a5bc8db6cb4c`.
Actual manifest/native checks passed; all 151 non-signature ZIP entries were
unchanged by re-signing with the existing development certificate. The same
explicit ART `speed` setup and offline benchmark measured a native getter at
2.28 microseconds mean versus the control's 22.15, and combined RTMP at 16.92
versus 61.96. Each case used 32 warm-up and 200 measured iterations. These are
sequential finite synthetic results, not sustained or comparative app evidence.

The subsequent live run exposed an owner-confirmed orientation failure:
portrait/landscape appeared swapped, movement distorted the image, and fixed
landscape put the grass on the right. Independent origin decoding with
autorotation disabled confirms sideways, stretched pixels inside a 1280×720
H.264 stream with no display-rotation metadata. The last epoch finalised with
83.4 seconds of video and 83.409 seconds of AAC after normal Stop. Multiple
orientation-triggered publisher restarts invalidate a steady throughput
comparison. This is not evidence that the prior audio stall was fixed.

The correction under review separates the Camera2/SurfaceTexture producer
transform from the app's encoded and preview rotations, keeps raw-buffer
dimensions in the sensor basis, and requires stable pose input before a new
epoch. Asymmetric labelled-corner/producer-matrix fixtures and a new physical
four-pose acceptance run are required; previous dimension-only checks did not
prove upright images. Private receipts, decoded origin frame and benchmark
results remain under `target/physical-android/status-leases-*`.

The first producer-transform correction passed 89 tests and independent source
review, but failed physical acceptance: fixed landscape became upside down.
The actual Pixel matrix confirmed that the producer already supplied the sensor
quarter-turn. A bounded live-origin segment independently decoded the same
upside-down 1280×720 pixels without rotation metadata. Normal Stop finalised a
163.8-second prefix; this finite run is not sustained acceptance. The initial
software oracle had used the wrong rear-camera physical rotation convention.

The revised correction uses the standard rear-camera sensor/pose convention,
with positive physical-pose vertex rotation for encoding and negative display
rotation for preview. Its independent world-labelled corner and ground-point
fixtures catch the prior upside-down result. These labels are test oracles;
runtime orientation uses camera metadata and the texture matrix, with no image
recognition or per-phone calibration. The 77 frozen inputs changed only the
orientation plan and tests from the previous correction. All 90 JVM tests,
lint with zero errors and independent source review passed. The APK is
SHA-256 `dc07e2330700ded6e43dd6e7023f552cf07a3044880e6a037c9f214cc016ef3c`;
the AAR/native library and all 151 non-signature entries survived verification
and re-signing unchanged. Independent inspection of the prepared landscape
preview and a bounded live-origin decode confirms upright, proportionate
1280×720 pixels. After physical rotation, the owner confirmed upright portrait
on the Pixel; an independent live-origin decode confirms upright 720×1280
pixels with autorotation disabled and AAC present. Both sampled live segments
decoded without errors. Four-pose qualification remains open. Evidence is under
`target/mobile-android/orientation-world-*` and
`target/physical-android/orientation-world-*`; the prior failure remains under
`target/physical-android/orientation-producer-*`.

Safari initially played the landscape stream with advancing media time and
decoded frames. It then stopped after the resolution changes and did not
recover. Its retained fatal error reports a media-sequence mismatch: the old
and new resolution both advertised sequence 7 with different segment URIs,
while resetting the media and discontinuity sequences to zero. Distinct file
prefixes do not make that timeline replacement legal. The frozen origin has
no reconnect grace. Current main adds grace for matching codec configuration,
but its changed-codec path still replaces the muxer. The correction in progress
backports the grace/health changes and preserves the HLS timeline through
resolution epochs, with immutable per-epoch init segments and recording rolls.
The viewer also gains bounded recovery and truthful stalled/error status;
source reloads must not conceal an invalid origin timeline.

The continuity candidate passed 147 focused Rust tests, strict Clippy and the
explicit independent recording-decode gate. Its release binary is SHA-256
`8643d87c47005c84a34ba03c56329c0e24b469c03c1615122fd4446a78e3bdd8`.
A private lab deployment retained presentation identity through three observed
codec epochs: 598 playlist observations over 20 minutes found no numbering,
URI, target or retained-init/media mutation. Bounded origin samples decoded
both actual portrait and landscape video with AAC. These results correct the
previous sequence-reuse defect; they do not qualify Safari recovery.

Physical native Safari acceptance still failed. One portrait change paused
the existing element and the owner pressed Play. A later change stalled it.
Both runs kept one source assignment. Rapid physical rotation subsequently
left the Android app in ERROR, with every camera, audio and publisher owner
released. Its first failure message had been overwritten by a normal rotation
message. Normal Prepare/Start recovered the rear landscape stream without
uninstalling the app or accessing its private recordings. The independent
diagnostic correction preserves the first failure cause and bounded lifecycle
codes; its frozen build passed 94 JVM tests, qualification isolation and lint
with zero errors and seven warnings. That APK has not yet been physically
qualified and is not a claimed rotation fix.

The viewer investigation now uses a separate automated synthetic event.
An unchanged native element accepted five aspect changes with advancing
decoded frames and no post-start pause during three publisher cycles. This
establishes that changing dimensions alone is not sufficient to reproduce the
Pixel failure. Subsequent replays used bounded excerpts from private
lab-origin recordings and phone-like reconnect gaps. No phone recording was read.
The arrival-clock PROGRAM-DATE-TIME defect now has a failing legacy regression
and a corrected media-clock anchor. All 151 focused unit tests and strict
Clippy pass. Candidate binary SHA-256
`d1344a4ac5659af03657f18b56fc415f1f228d4c433864051365be416ebaf6bc`
was tested on a separate loopback origin, retaining the Pixel origin. Both
old and corrected origins accepted the same mobile-media replays, six
20-second publishers with two-second reconnect gaps. Each native element
kept one source and automatically accepted four aspect changes. Expected
outages caused about 2.3 seconds of buffering before automatic recovery.
The corrected playlist's six sampled adjacent same-continuity date pairs had
zero timing mismatch; the old origin's pairs ranged from -6 to +2 ms.
This proves the metadata correction, not a cure for the owner's pause.
The temporary replay excerpts and their origin recordings were removed after
retaining metadata and hashes. The comparison origin, extra forward and three
synthetic browser windows were stopped; the original Pixel viewer was restored
without changing its source.

The qualification-only camera-pose control subsequently passed 102 JVM tests,
lint with zero errors and seven existing warnings, source review and actual
release-APK isolation checks. Its installed APK is SHA-256
`f77c0636cd39f2ff8ceba2d3e88a8bfa2645f9e1c2d22ec1e2bd5f1f98eacd7c`.
The control cannot start capture or choose a destination, is restricted to
the foreground qualification build and is absent from the release entrypoint.
It drives the production pose selector and real encoder restart. Six actual
camera/encoder changes and six further changes with Android window/surface
rotation completed without replacing the app process. The window test restored
all rotation settings. Safari accepted all six window-test aspect changes
with one source and no pause, but also had two sampled presentation stalls.
These are finite automated tests with a physically stationary phone; they do
not qualify upright physical rotation, rapid rotation, latency or the owner's
previous pause. The corrected date-anchor origin is now the private primary.

The local viewing proxy also logged upstream connection-address failures and
connection timeouts taking about 26 seconds. Its original configuration opened
a new upstream connection per HLS request. A private fixture change enabled
an upstream keepalive pool and added connect/header/response timing. Over
1,500 subsequent requests succeeded with zero measured connection delay;
the sampled forward sockets had no TIME_WAIT entries, versus 311 beforehand.
This establishes connection reuse, not the cause of every earlier stall.
Four additional native camera and window rotations completed on the same app
process with settings restored. Their Safari presentation result remains under
investigation: the presentation callback counter stopped before these changes
while the media clock and decoder counter continued. No Play, seek or source
replacement was used to conceal that result. All receipts are metadata in
ignored private lab storage; no real footage is part of the source commits.
Physical orientation and same-source Safari acceptance remain open.

The owner subsequently confirmed that the existing picture and sound were
working despite its stalled label, with a very large delay. The observer now
reports `unverified` when the clock advances without fresh selected frame
evidence, preserving the strict paired-evidence `playing` gate. It reserves
`stalled` for sustained clock nonprogress and never silently substitutes decoder
counts for a present but failing presentation API. Its deterministic regression
tests pass; this changes observation specificity, not playback behaviour.

A separately labelled fresh native session on the repaired proxy measured
about 0.4 seconds buffered ahead and roughly one second of arrival-derived
programme-clock offset, without a seek. The owner still reported a large real
delay with working picture and sound. That failed physical observation takes
precedence over any inference from those metadata estimates. Capture, encoder,
publishing and actual screen delay remained under investigation. The fresh
comparison page subsequently acquired a different browser instance and was
paused; no continuous observation is claimed across that replacement. After
retiring the old test tabs, a separately recorded single native viewer kept one
source with advancing presentation callbacks. The owner compared a physical hand
movement and reported under two seconds. This is one approximate physical
observation, not a latency percentile, sustained qualification or a competitor
comparison.

The owner then rotated the real Pixel to upright portrait and back to landscape
with the same Safari window untouched, confirming that picture and sound
recovered automatically after both changes. The retained native event log shows
720×1280 then 1280×720, automatic buffering recoveries of about 0.94 and 2.07
seconds, no pause and one source assignment in the unchanged browser instance.
Presentation callbacks continued afterwards and the app process was unchanged.
This is the first successful bounded physical rotation check on the repaired
viewing path. It does not establish all four poses, rapid rotation, sustained
latency after repeated changes, physical iPhone behaviour or a release gate.

The task's origin, local tunnel and viewing fixtures remain running for the
continuity investigation; their private process/port manifest and synthetic
evidence remain under `target/mobile-origin-av`. The primary Pixel stream is
landscape at the latest observation. No profiler or task emulator is running.
The native-HLS fixture observes one source without automatic Play, reload or
seek, so manual intervention cannot count as automatic recovery. The master's
omitted RESOLUTION is permitted by RFC 8216 but leaves Apple's authoring
requirement unresolved; no full Apple-authoring compliance is claimed.

Fetched `origin/main` at `8ee5704` now contains the separate M6b passphrase
commit `771c750d61006ab1c2bbfde97ee3f130f24cd828`, server integration `e608bf2`
and own-SRT-default change `681d458`, delivered by the other session. Mobile
development no longer blocks that integration. The task's M6b worktree was
verified clean with no unpushed commits and its HEAD an ancestor of
`origin/main`, then removed using `git worktree remove` and pruned. This mobile
branch and the frozen lab-origin binary have not yet incorporated the newer
server default; these observations do not establish mobile SRT or release
acceptance. This session did not push those changes.

Keep every gate in the source delivery plan open until its actual evidence
exists. In particular:

- B0: finish device/OS matrix, real reference measurements, interface and
  queue/thread/copy budgets, and destination authorisation contract.
- B1: build and install actual native consumers, capture on physical phones,
  verify single encode and seekable/recoverable recording, measure overhead.
- B2: retain origin fixes and prove authorised encrypted ingest, reconnect
  continuity in real players and the V4V event/replay contract.
- B3/B3R: complete our encrypted SRT and RTMPS publishing paths, independent
  interoperability, hostile-input fuzzing and impaired-network acceptance.
- B4/B4R: attach real publishers; complete manual/V4V creator flows, observed
  media status, audio-only/external audio, controls, standby and safe profiles;
  physically qualify elapsed/socket readouts, storage preflight and exact
  recording export and the implemented iOS recording catalogue/export UI.
- B5: sustained physical/network/thermal/storage tests, SDK consumption,
  route-specific signed packages, upgrade and free-V4V behaviour, owner
  acceptance, release readback and any production decision at its own gate.
- B6: native RelaySwarm/browser interoperability, verified delivery/fallback,
  consent, metered data and measured benefit, preserving existing peer gates.
- B7: separately qualified wider formats, advanced accessories, recording
  distribution, optional bonding and studio work as scoped by the plan.

A physical iPhone was reachable during discovery. The owner's physical Pixel
on a remote development host has now run the actual network debug app. Its
first origin run decoded video but omitted audio; the server's provisional
video-only init was then corrected for AAC configuration arriving before
servable content. The fresh session independently decoded H264 720p and AAC
48 kHz mono, and the owner confirmed both picture and sound in Safari.

The owner subsequently measured 10.5 seconds of glass-to-glass delay. Safari's
native player was about six seconds behind media it had already downloaded.
This is a tracked latency failure, not a passed performance gate. A local
live-part player and native capture-age measurements are being used to
separate player buffering from camera, encoder and transport delay. No claim
of beating Larix, sustained reliability or energy efficiency follows.

Private UI, frozen APK, decode receipts and local process/port manifests are
retained under `target/physical-android` and `target/mobile-origin-av`. The
Pixel later locked, ending its foreground-only session. A fresh recording
and deadline candidate was installed while locked; the next physical latency
test awaits unlock. An origin on the remote development host and a local
HTTP/2, gzip viewing fixture are prepared to remove the ADB ingest bridge
from the next measurement. The old ADB reverse and original origin were stopped.
The earlier video-only origin and its tunnel were stopped cleanly after its
recording finalised. No task-created emulator was started. No store submission,
paid subscription or production deployment is claimed here.

The last broad check, before encrypted SRT integration, passed 250 tests with
one external ffmpeg test ignored by default. That external MPEG-TS test was
also run explicitly and passed. Workspace strict clippy passed. Re-run the
appropriate checks after integration; these receipts are not release approval.

## Delegation and integration

- GPT-6.1 Sol, high effort: reusable SDK core, then native iOS implementation.
- GPT-6.1 Sol, high effort: RTMP client, then native Android implementation.
- GPT-6 Astra, high effort: own SRT sender and independent wire testing, then
  MPEG-TS integration.
- Parent: source/scope coordination, native bindings/packages, integration,
  consequential review, whole-workspace validation and release evidence.

Assignments do not establish a cost saving. Actual usage/cost is not recorded
as measured savings. Shared Cargo builds are serialised by agreement, with no
new CARGO_TARGET_DIR or CARGO_INCREMENTAL override. Only task-owned staging,
services, emulator resources and build output may be removed. Retain named
evidence and packages while their delivery/qualification work remains active.
