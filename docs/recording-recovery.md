# Recoverable mobile recordings

The B5 requirement is recovery through the last completed fragment after process
termination, with at most two seconds of trailing media loss. It remains open.
Normal Stop, a configured fragment interval and a file marked incomplete do not
establish recovery.

## Current generated evidence

The actual native iOS recorder's four-second closed control retains all 190 AAC
and 120 H.264 packets and passes native and independent decoding and seeking.
Actual SIGKILL at 1.51 and 2.51 seconds leaves no movie header or completed
fragment. Neither Apple nor FFmpeg can open those originals. A one-second
`movieFragmentInterval` is therefore insufficient on this writer path.
Source and scalar evidence remain under
`target/privacy-standby/interrupted-recording-spike/`.

A second generated experiment reuses the installed SDK's existing
`NativeRecordingMuxer` and `sapflow-hls` fragmented-MP4 implementation, with
500 ms fragments and the same native encoded media. Unchanged initial headers
and complete synchronised output chunks remain readable and seekable after
both kills. Stored prefixes preserve all 68 AAC/44 video and 114 AAC/73 video
packets respectively, with their original order and payloads.

That output fails recording presentation. The four-second SDK control decodes
192,448 native audio frames instead of the recorder's 192,000. Three-generation
cases preserve all 97 AAC and 63 video packets but have extra leading and final
padding, with incorrect source alignment in independent decoding. This is a
metadata prerequisite, not permission to drop access units, re-encode, change
source markers or relax the existing presentation checks. Evidence remains under
`target/privacy-standby/recovery-muxer-spike/`.

Readable coded prefixes do not prove the source-presentation loss bound, storage
durability or a production recovery path. Both experiments used generated media
on a host. Their children, media and compiler scratch were removed.

The subsequent native-header experiment preserves exact coded prefixes after
both kills, but Apple still presents final padding in all three closed cases.
Additional reader observations find no hidden trim attachment that would justify
changing the presentation oracle. A bounded diagnostic identifies two separate
container issues: non-zero media-header durations with empty sample tables
double the video duration, and the final AAC entry still declares its coded
padding as presentation time. Clearing only the media-header durations fixes the
first issue but leaves the audio failure intact.

A further four-second mono diagnostic clears those durations and shortens only
the final AAC container entry from 1,024 to 576 frames, using the actual checked
448-frame EOF padding. Apple then returns exactly 192,000 frames, byte-identical
to the native recorder. All 190 AAC and 120 video payloads and their original
timestamps remain unchanged. The independent decoder retains the existing
allowed-padding convention and exact valid prefix. This is a narrow causal
result, not production recovery or multi-generation qualification. Historical
failures and generated scalar evidence remain under
`target/privacy-standby/recovery-muxer-spike-v2/`,
`recording-tail-metadata-v1/`, `recording-mdhd-zero-v1/` and
`recording-final-trun-v1/`.

The opt-in SDK candidate therefore retains one possibly final AAC unit in its
existing buffer until a validated successor or checked final EOF establishes
its container duration. Publishing and the shared encoded sample retain their
original duration and payload. Checkpoints distinguish accepted media from
actually serialised coverage, and coded from container endpoints. Unknown EOF
retains the original final duration. Source review, focused Rust tests, strict
Clippy and generated Swift/Kotlin host consumers pass. The current package and
generated presentation results are recorded below; platform recorder adoption
remains separate.

## Native recording presentation qualification

The subsequent five-case generated A/V run passes its original strict checks,
including both actual SIGKILL prefixes. The ten-case audio-only and short-prefix
run completes, but its original strict aggregate fails on two closed controls:
their equal-length native Int16 arrays differ from the paired recorder by one
least significant bit at 28 and 22 sample positions. Payloads, coded timestamps,
presentation counts, valid independent-decoder PCM and applicable seeks pass.
Fresh reads of the same unchanged files also produce differing native Int16 and
Float32 results. The cause remains unresolved; this does not establish a
dithering mechanism or a container corruption fix.

For future generated qualification, `native-numeric-presentation-equivalence-v1`
is a separately registered numerical contract. Compare the full, originally
aligned, equal-length Int16 arrays: every sample must differ by at most one
least significant bit, using arithmetic that cannot wrap. There is no crop,
alignment search, normalisation or audio rewrite. A difference of two anywhere
fails. RMS, correlation and mismatch density are diagnostic only. This accepts
a difference of up to 1/32,768 of full scale at every position, including a
constant one-unit bias; it is an explicit numerical tolerance, not proof of
perceptual equivalence.

Every exact content and presentation requirement remains mandatory: original
encoded packet bytes, order, counts, coded durations and rational timestamps;
actual generation, leading and EOF-tail receipts; native output format, lossless
timestamp conversion, origin, endpoints and frame count; byte-exact independent
valid PCM; applicable positive seeks; paired native recorder controls; and the
required actual legacy-file negatives. Complete extents must satisfy the SDK's
checked bounds, including leading frames at most 8,191, final tail below 1,024
and positive generation count no greater than coded AAC-unit count. Unknown EOF
and prefixes containing no positive presentation retain separate classifications.

The original byte-exact result and original strict verdict remain reported
alongside the new numerical result. Historical runs are not reclassified. Before
adoption, a fresh generated A/V and audio-only matrix must extract the hard facts
from actual recordings and decoder receipts; synthetic oracle tests alone do
not prove file-corruption detection. The reviewed definition and 50 synthetic
boundary controls are retained under
`target/privacy-standby/native-presentation-acceptance-v1/source2/`.

This recording comparison does not replace privacy standby's synthetic-only
payload provenance, raw-admission or settled-silence requirements. It does not
qualify production recorder adoption, durable recovery, the aggregate loss bound,
power loss, physical devices or sustained shows.

## Fresh registered matrix result

The current source-bound run completes all five A/V and ten audio-only/short-prefix
cases and passes the separately registered numerical contract. Nine complete
comparisons satisfy all 17 mandatory hard checks. Seven native PCM arrays are
byte-identical. The mono A/V control differs at 11 Int16 positions and the
96,192-frame audio-only control at 10 positions, each by at most one unit. The
96,193-frame control is byte-identical in this run. Exact packet retention,
rational timestamps, presentation counts, independent valid PCM and applicable
positive seeks pass.

Both original strict suite aggregates remain failed, with their original child
exit codes preserved. The 18-frame resampled recording passes its separate tiny
positive seek; the original fixed half-second seek remains failed. Earlier
failed runs are unchanged. The previous V3 run's two 22-position differences
remain recorded separately. These results do not establish the cause of native
read variation.

Both actual process-killed A/V prefixes retain only pre-death sealed coverage
and pass native and independent decoding and applicable seeks. Unknown-EOF and
head-only cases retain their distinct classifications without invented final
padding or positive presentation. Actual final tails observed across the two
suites are 0, 64, 448, 840, 860, 942 and 1,023 frames. Tails of 1,024 or more
remain unobserved.

Three actual generated-file extraction negatives detect a changed AAC payload,
a truncated box and a one-tick audio decode-time change. These are extraction
checks, not mutant-decoder or full-gate qualification. Independent review verifies
the retained 352 command-log hashes, current source/package bindings, results
and cleanup. All fixtures, cases, strict predicates and registered numerical
oracles remain unchanged from V3; only source/package evidence was rebound.
Generated media and owned compiler scratch were removed. Source and scalar
evidence remain under
`target/privacy-standby/generated-recording-qualification-v4/`; V3 is retained.

This refreshes the registered generated presentation prerequisite. App recorder
migration, durable orphan recovery, the aggregate two-second loss bound and
physical acceptance remain open. The host checkpoint and closed-header storage
composition below has separate evidence.

## Portable checkpoint SDK

The shared `sapflow_hls::recording_checkpoint` codec and typed Swift/Kotlin
bindings now describe an exact serialised fragment prefix. The native muxer
captures the original INITIAL header length/hash and stages the cumulative
fragment byte count with each flush. Only committing that same output advances
the byte frontier; held audio, rejected input and final header rewrites do not.
An older issued checkpoint retains its own snapshot after later output or finish.

One cold `encodeCheckpoint` call returns both the bounded slot bytes and typed
information for the platform's append-position check. Decoded persisted data is
a separate type and cannot act as a live muxer checkpoint. The portable V1 slot
has a 512-byte prefix, a 1 to 65,536-byte opaque header and a SHA256 trailer,
totalling at most 66,080 bytes. It checks exact supplied identity bindings,
lengths, reserved fields, scalar media bounds and successor order. Checksum-valid
contradictions are distinguished from corruption; a platform must not silently
discard a contradictory slot in favour of an older one.

These are metadata and serialisation guarantees. A checksum is not authenticity;
neither decode nor a native byte frontier proves a file was appended, synchronised,
closed, durably catalogued or independently decoded. The platform must retain
its file identity, hash the actual fragment region and complete its own storage
operations. No additional encoder, media queue or per-unit sample validation is
introduced by this SDK change.

That earlier checkpoint/extent source passed 708 workspace tests, strict Clippy
and formatting, generated-stream smoke checks and actual generated Swift/Kotlin
consumers. Four workspace tests remained ignored by that default command. The
unchanged bounded checkpoint parser previously passed four scalar seed replays
and 522,636 ASan/overflow fuzz executions. Host consumers covered frontier, hash,
successor, foreign-owner, retained-token and exact presentation-query checks.
Those results qualified the SDK API and bounded parser; they did not establish
installed recovery or the two-second loss bound. Evidence is retained under
`target/recording-checkpoint-gates-v1/`,
`target/privacy-standby/recording-checkpoint-fuzz-v1/` and
`target/privacy-standby/recording-checkpoint-native-check/`. The then-current
extent API and packages are recorded under
`target/privacy-standby/recording-native-extent-gates-v2/` and
`recording-native-extent-packages-v1/`.

The Apple SDK package at that milestone passed all three release slices and its
installed Swift host consumer. Its Android package passed all four ABIs, actual
16 KiB ELF load alignment and its extracted-AAR Kotlin host consumer. Both
retained unchanged source bindings and verified package contents. Those package
and host checks did not establish a rebuilt app, phone installation or physical
recording acceptance. The later timing-view package refresh is recorded below.

The immutable issued checkpoint also exposes a cold `presentationExtent()` query
in Swift and Kotlin. It uses the same shared Rust construction and movie-duration
calculation as native headers, returning exact ticks in the 720,000 Hz clock and
checked ceiling milliseconds. Old tokens remain valid after advance, finish and
cancel; the query takes no current muxer lock. Its AAC EOF bit does not certify
upstream admission, a written file or whole-show completion. Untrimmed diagnostic
`stats.durationMs` is not finalised presentation duration.

## iOS append-file owner

The additive `RecordingCatalogue.beginAppend` path now owns an exclusively
created, anchored 0600 media descriptor and the existing catalogue writer lease.
It binds the actual root/file device and inode plus file birth time; changing
size or modification time does not change that incarnation. Existing receipts
without this optional binding remain readable.

The owner writes one bounded INITIAL header and sequential complete chunks,
checking the actual position and expected end before writing. Partial writes
close admission and preserve their bytes. A confirmed incomplete close can
release the lease; uncertain closure or receipt installation retains the exact
owner and root flock. Dropping the caller's wrapper cannot release an open file.
The first append-only packet had no offset-zero rewrite or finalised-recording
operation. Its later submitted-input header installation is described below.

Admission charges six directory entries and an additional 256 KiB of metadata
headroom within the existing caps. Available bytes come from the retained root
descriptor. This is a conservative preflight, not a disk-space reservation.

That first host gate passed 71 recording lifecycle tests, 38 library tests and
the strict iOS 16 API check across all 67 app source files. The initial run
reported three failures across two replacement-file tests: their fixtures had
0644 permissions and reached the permission guard before the intended
incarnation checks. The reviewed repair sets and verifies 0600 replacements,
keeps the original identity assertions and separately tests same-inode 0644
refusal. Production code is unchanged between those two runs. Failed and passing
receipts remain under `target/privacy-standby/ios-recording-append-gates-v1/`
and `ios-recording-append-gates-v2/`.

The app recorder still uses its existing writer. That append-only packet did
not install checkpoint slots or publish storage progress. The later composition
is described below; crash recovery and the aggregate two-second loss bound
remain open.

## iOS checkpoint installation

The initial SDK/file composition installed two alternating, bounded
checkpoint slots. One operation owns fragment append, media sync, exclusive
temporary-slot write/sync/checked close, anchored rename, directory sync and
exact bounded readback. It checks actual file incarnations throughout. Unexpected
stable slots, replacements and occupied temporary names refuse further writes
and preserve the originals.

A private compound owner creates the real SDK muxer and consumes its outputs.
It hashes each actual fragment buffer once, checks the issued snapshot against
the original INITIAL and file bindings, and uses shared Rust decode/successor
validation before installation and on installed bytes. Only a successful
readback close can produce the filesystem receipt used for scalar publication;
cancellation is checked again before publication. No Swift checkpoint parser,
additional encoder or media queue is introduced.

Its first host gate passed 80 filesystem tests, 38 library tests, nine real-SDK
filesystem tests and the strict iOS 16 API check across all 68 app source files.
The SDK tests use synthetic audio bytes and scalar receipts as API controls;
they do not establish native AAC timing or decoder presentation. Their driver
consumes explicit source-bound host-runtime and generated-binding receipts.
Integration of this local qualification driver into CI remains open.

Review added explicit unused/replaced stable-slot cases before execution.
The harness's forced-cleanup control terminates an uncooperative Python child,
removes its three new scratch directories and preserves pre-existing files.
That control is separate from the six successful compiler/test commands.
Source and scalar evidence remain under
`target/privacy-standby/ios-recording-checkpoint-owner-source-v2/` and
`ios-recording-checkpoint-owner-gates-v2/`.

At that stage FINAL_HEADER remained deferred and every file closed as incomplete.
The following operation adds checked header installation. Neither app recorder
has adopted this composition by default. Crash recovery, physical storage
qualification and the aggregate two-second loss bound remain open.

## iOS submitted-input finalisation

The `finishStoredInputs()` entry point finishes only the SDK inputs it actually
accepted.
It retains the latest successfully installed real issued token, captures actual
finish/drain/statistics before native cleanup, checks exact checkpoint/header
and accepted-sample coverage, then queries shared presentation duration once.
There is no caller-supplied upstream-complete flag or fabricated producer proof.
Every result from this entry point is scoped to submitted SDK inputs, with
application admission unknown and an explicitly incomplete catalogue receipt,
even at complete AAC EOF.

The same append owner, catalogue lease and root flock remain held through actual
append descriptor close, anchored reopen of the same file, fixed-length offset-zero
header replacement, media sync/readback/checked close and canonical receipt
installation/readback/checked close. Only then is the original lease released.
Fragment bytes and offsets stay fixed. An uncertain descriptor close or partial
terminal transaction retains the exact owner and original files; it cannot
silently retry a recycled descriptor or report completion. Cancellation before
the checked-header-close commit decision is recorded; later cancellation allows
that already-authorised storage commit to finish.

An interrupted header overwrite can invalidate the current INITIAL hash without
changing the committed fragment region. Future explicit recovery must use the
saved checkpoint header, concrete root/file birth binding and exact fragment
hash, treating the current header as advisory. This operation does not implement
recovery or adopt orphan slots.

The earlier submitted-input host gate passes 86 filesystem tests, 38 library
tests, 16 real-SDK filesystem/API tests and the strict iOS 16 check of all
68 app source files.
Synthetic SDK payloads and scalar EOF controls establish storage/API behaviour;
they are separate from native AAC and decoding evidence. The first run's SDK
test compile failure is retained: an XCTest assertion inferred a throwing syscall
callback. The reviewed test-only nonthrowing helper preserves all three refusal
assertions; production code is unchanged. Evidence is under
`target/privacy-standby/ios-recording-normal-finish-gates-v1/` and
`ios-recording-normal-finish-gates-v2/`.

## Native AAC generation receipts

`NativeAudioEncoder` now has an explicit full-generation receipt mode; legacy
callbacks remain the default. Full mode forwards its existing measured generation
and file receipts, including raw and silence counts within the same converter
generation. It never reconstructs those counts from the legacy total-input event.
No extra encoder, per-unit validation, payload copy or queue is introduced.

Fourteen actual generated-PCM/native-AAC cases pass strict compilation and shared
SDK acceptance, covering raw 48 kHz/44.1 kHz, initial silence, same-generation
Resume, multiple generations, empty input, sticky failures and default legacy
controls. The mixed-source case measures 20,017 silence and 16,039 raw frames in
one generation. These tests do not decode audio or inject a native converter EOF
fault. The unchanged existing audio-only and standby decoder/recording gates also
pass against this source and current package, with all 82 command logs verified.
Their declared 1,024-frame decoder settling guard and original strict assertions
remain unchanged. Evidence is under
`target/privacy-standby/ios-native-aac-full-receipts-gates-v1/` and the source-bound
`target/ios-audio-only-fixture/` and `target/ios-audio-standby-fixture/` runs.

At the full-receipt milestone, the real app producer/ingress/recording closure
chain and synchronous known-loss accounting remained open. The later concrete
producer closure and its completed generated gates are described below.
Physical capture/Stop qualification remains open.
The submitted-input result cannot substitute for those barriers or the aggregate
two-second loss requirement.

## iOS first recorder adoption composition

The first bounded app composition is locally committed as `928a616`. Its twelve
source paths preserve the current product default. The concrete `CaptureRecording`
facade retains legacy recording as the default. An internal cold selection may
choose the SDK adapter, which owns the existing `RecordingCheckpointOwner` on
the existing `recordingIO` queue and `EncodedWorkBudget`. Ingress supplies the
same immutable sample constructed by its sole existing validation, with the
original validated payload byte count. There is no extra sample query, encoder,
media queue, encoded payload copy or per-unit validation.

The adapter uses the real SDK's relative epoch-zero policy in the existing
capture media clock, with 4 MiB fragments and a 500 ms fragment interval. These
settings do not establish the aggregate two-second loss bound. Actual native
full-generation AAC receipts are forwarded verbatim. At that first composition,
every SDK completion, including a successful complete SDK outcome and checked
header rewrite, returned a nil completion file URL, an incomplete app receipt and
application admission unknown. After checked closure, the recording remains discoverable
through the existing catalogue as an incomplete entry. Only actual
compound-owner cleanup releases its retained storage
ownership. That first composition does not implement producer closure authority
or finalised app admission. Its original Stop/cancel and recorder/transport
cleanup joins remain the compatibility baseline.

The cold capacity query uses the same append owner's child/root lease and
compound busy guard, checking actual media/end state, owned slots and absent
fixed temporary names. Its private scan reserves still-missing names within
the same six-name allowance and existing 1,024-entry/256-segment caps. Available
bytes are queried from the actual retained root descriptor. Three new
capacity/failure tests cover descriptor identity, namespace limits and unknown
metadata-close handling. The strict 200 MiB plus
256 KiB floor remains. This check is not a second reserve owner or a fabricated
storage reservation.

At that milestone, all six platform commands passed: 89 filesystem tests,
38 library tests, 16 real-SDK checkpoint-owner tests, both generated modules and the strict iOS 16
API check of all 69 production source files. Compiler warnings are errors.
Independent outcome review verifies the source/package bindings, six logs and
retained cleanup receipts. Evidence is under
`target/privacy-standby/ios-recorder-adoption-platform-gates-v1/`.

The actual generated-input adapter gate passes five finite native cases through
seven commands, including four independent exact-original-AAC payload probes:

| Case | Actual retained result |
|---|---|
| Healthy | All 18 of 18 native AAC units stored. |
| Mixed zero/raw | All 38 of 38 units stored; one generation contains 20,017 intentional-zero and 16,039 raw frames. |
| Publishing-only failure | All 18 of 18 units stored independently of the refused publisher. |
| Missing file EOF ordering | All 15 of 15 pre-close units stored; three late native output units refused. No complete EOF is invented. |
| Recording queue exhaustion | A positive bounded prefix of 127 of 159 native units is stored, with recording refusal retained. |

The four payload probes compare stored packet sizes and hashes against the
original native AAC bytes for the first four cases. The queue-refusal control
checks its positive bounded prefix separately. This establishes actual adapter
storage and payload behaviour, not new SDK decoder presentation or real capture
producer closure. Source and scalar evidence are under
`target/privacy-standby/ios-recorder-adoption-native-gates-v1/actual-v1/`.
Independent platform and native outcome review is clear, including all current
input, witness and command-log hashes.

Both legacy regression commands at that milestone passed: 86 observed-publisher
tests and seven original native stored-payload/presentation controls, retaining both
expected historical negatives and their original assertions. Evidence is under
`target/privacy-standby/ios-recorder-adoption-legacy-gates-v1/`. Independent
outcome review is clear. Full Debug, Qualification and Release app builds also
pass through eleven commands with all current source/package bindings unchanged.
All three built privacy resources, creator configuration modes and manual-link
schemes pass. The Debug test bundle contains both exact corpora and all three
new capacity test symbols, without test execution. Separate actual product
readback verifies each app code object is unsigned and records the Debug
implementation dylib separately from its launcher. Evidence is under
`target/privacy-standby/ios-recorder-adoption-app-gates-v1/run-20261023T064820Z-dbedb444/`.
Independent app outcome review is clear. No XCTest device bundle
execution, signing, publication, phone installation or physical capture is
established.

The earlier numerical presentation contract, strict historical failures and
all numerical thresholds are unchanged. That first composition did not supply
producer-finalised app receipts; the later closure below has separate evidence.
SDK default adoption, aggregate two-second loss, durable orphan recovery,
power-loss storage behaviour and physical acceptance remain open. No Rust SDK,
dependency or CI qualification change is part of that composition.

## iOS producer and ingress finalisation

The producer closure source is locally committed as `135b3cb`. It changes eight
production Swift paths, including the new `CaptureRecordingProducer` in the
actual `CaptureRun` composition. On the control owner, its concrete `stopInput()`
detaches the real capture-output delegates and waits for the actual session to
stop. The existing samples queue then joins already queued callbacks. The same
concrete owner calls the actual `NativeAudioEncoder.finish()` and
`HardwareVideoEncoder.finishChecked()` methods, including native EOF and video
callback quiescence. An unclosed native input, failed codec close, uncommitted
Start or held startup/Resume media cannot grant a clean capture closure.

The privately constructed terminal closure binds the exact media run, committed
Start, actual recording-admission owner and ticket, recorder, ingress worker and
native producer objects. It advances once through the existing ingress FIFO,
where callback attempts recorded before filtering must match successful
recording forwards. Audio timing events share that conservation check. A native
output or timing event arriving after this finalisation starts synchronously
faults that exact run rather than silently becoming ordinary Stop cancellation.
The original legacy cancellation behaviour is retained without this capability.

Reported native `didDrop` loss is synchronously attributed before deferred UI
work. A callback queued while live may arrive after Stop, privacy revision or
camera replacement; its interval cannot be proved outside admitted capture, so
it conservatively keeps that segment incomplete. Deliberate privacy discard
delegates never report these drops. An old callback cannot fault a successor.
Recording-only failure has a separate synchronous sticky latch. At the actual
fixed-header commit decision, that latch and the run's native-loss state are
checked under their scalar locks, independently of publishing success.

The SDK/file compound owner still verifies actual complete native AAC EOF,
installed checkpoint/header bytes, drained native stats, exact accepted-unit
counts and current file ownership. Only the concrete capture closure can
upgrade application admission to `completeCapture` at the same installed
frontier. The trusted catalogue outcome hook classifies storage; it does not
prove native capture itself. Actual receipt/metadata closure and lease release
remain necessary before reporting completion. A finalised file URL is available
only after that checked complete closure. The old `finishStoredInputs()` entry
point remains explicitly incomplete with admission unknown.

All seven current platform commands pass: 261 budget/admission tests, including
six new ownership/loss controls, 89 filesystem tests, 38 library tests, 16
real-SDK checkpoint-owner tests, both generated modules and the strict iOS 16
API check across all 70 production Swift sources. All 275 recorded inputs remain
unchanged. The source-only review and independent completed outcome audit are
clear. The module log retains Xcode filesystem-event/cache fallback diagnostics;
the module command exits successfully with no Swift warning or error.

The actual native gate passes its five original incomplete stored-input controls
and all ten producer controls through thirteen commands. The four original AAC
payload probes still compare exact stored packet sizes and hashes with the native
encoder's originals. The positive producer cases cover generated audio-only,
ten-frame A/V and independent publishing failure. Each produces an actual
finalised file, passes stream probing and independently decodes without errors;
the A/V probe reads all ten submitted video frames. Seven negative controls
remain incomplete: header-held native drop, recording-only loss, late encoded
output, late timing, input not closed, wrong worker and a stale same-Start ticket.
Normal Stop cancellation, repeated closure, unchanged bytes after duplicate
finish and recorder closure before transport completion are checked.

The first native attempt fails during AAC/H.264 codec discovery, including the
original five controls. It is retained as `actual-v1`, with source and witness
hashes unchanged and checked cleanup. The separately authorised `actual-v2` run
uses macOS codec-service access and passes with the same source, SDK witnesses
and oracle predicates. Its thirteen owned command groups stop and its generated
media and scratch are removed. The earlier failure is not relabelled as a pass.

Both legacy regression commands also pass: 86 observed-publisher tests and all
seven original native stored-payload/presentation controls. Both expected
historical bad-order/false-publishing-receipt failures and all original assertions
remain intact. The two task-owned groups stop, new scratch is removed and the
pre-existing ownership scratch is preserved. These legacy presentation results
do not qualify the new SDK's presentation equivalence.

Current Debug, Qualification and Release app builds pass through eleven commands
with all 313 source/package inputs unchanged and zero compiler warnings or
errors in each configuration. The three built privacy resources, creator
configuration modes and manual-link schemes pass. The compiled Debug test bundle
contains both exact corpora and all six new ownership/loss method symbols; it is
not executed by this app gate. Actual unsigned-code-object readback passes for
all three apps, with no `_CodeSignature` directory. The Debug loader is stable;
its implementation dylib is separately bound to SHA-256
`f11ba2d6a7e45013a37b8a35e943ab0a0988d2ba0d5668e8714274531f808813`.
Task-owned working scratch is removed; shared DerivedData/caches are retained.

Exact retained evidence is:

- Source freeze, source commit and independent source review:
  `target/privacy-standby/ios-recording-producer-closure-v1/author/manifest.json`,
  `target/privacy-standby/ios-recording-producer-closure-v1/source-commit.local.json`
  and
  `target/privacy-standby/ios-recording-producer-closure-v1/review/independent-frozen-v1-review-receipt.local.json`.
- Seven platform commands:
  `target/privacy-standby/ios-recording-producer-closure-platform-v1/receipt.local.json`
  and its sibling `independent-outcome-audit.local.json`.
- Retained native discovery failure:
  `target/privacy-standby/ios-recording-producer-closure-native-v1/actual-v1/receipt.local.json`
  and its sibling `independent-failure-audit.local.json`.
- Completed generated native gate:
  `target/privacy-standby/ios-recording-producer-closure-native-v1/actual-v2/receipt.local.json`,
  with `exact-payload-proofs.local.json`, `producer-finalisation-proofs.local.json`
  and `independent-outcome-audit.local.json` in the same directory.
- Legacy regressions:
  `target/privacy-standby/ios-recording-producer-closure-legacy-v1/receipt.local.json`
  and its sibling `independent-outcome-audit.local.json`; the actual ownership
  child receipt is
  `target/ios-output-ownership-fixture/20261023T095855Z-df5b56e6/receipt.local.json`.
- Three app configurations:
  `target/privacy-standby/ios-recording-producer-closure-app-v1/run-20261023T100048Z-de33b3d5/receipt.local.json`,
  with `actual-product-signature-readback.local.json`,
  `new-admission-test-symbol-readback.local.json` and
  `independent-outcome-audit.local.json` in the same directory.

All independent completed outcome reviews are clear. The source commit contains
exactly the fifteen reviewed source/test/driver paths; the pre-commit gates keep
their original `aa6fc765` HEAD, while the app gate binds the clean `135b3cb` commit.
Earlier presentation, submitted-input and first-adoption evidence remains
historical. The 2,875-byte numerical presentation contract and every original
threshold are unchanged.

The generated producer path uses actual native encoders and the same concrete
close method as `CaptureEngine`, with an empty stopped capture session. It
cannot establish camera permissions, real microphone/camera callback closure,
physical Stop, power/thermal behaviour or creator acceptance. Legacy remains
the product default. No extra queue, encoder, encoded payload copy or per-unit
validation is introduced. Product/default adoption, orphan recovery, aggregate
two-second source-presentation loss and new presentation equivalence remain
open. No phone access, signing, installation, push or publication is part of
this packet.

## Shared AAC timing views

The shared routing seam is now locally committed as `c886419`. An explicit
opt-in API derives two immutable timing choices from one existing validated
AAC-LC 48 kHz mono sample, with the exact `[0x11, 0x88]` configuration and
1,024-frame duration. Recording retains that supplied original coded PTS/DTS;
publishing optionally receives a supplied normalised PTS/DTS. Integer bounds,
profile and branch continuity are checked before either queue changes. The SDK
does not infer a coded clock from the signed native PTS observation.

A missing publishing view deliberately withholds that input. It does not close
the branch or count a fabricated rejected input; existing accepted publishing
backlog still ages under its unchanged queue policy. Both views retain the same
original payload and codec Arcs. Provenance has constant depth one and cannot
be built from another view. The producer generation and native PTS are explicit
caller-reported, unattested context, not proof of one AU per callback, leading
trim, generation completeness, admission or EOF. Cold provenance remains
available after queue pop without requiring a new FFI query per input.

There is no additional encoder, media queue, payload-byte copy from that
validated sample or compressed-payload validation in view construction/fanout.
There are new metadata allocations: one recording sample Arc, an optional
publishing sample Arc and the FFI paired-object Arc, plus payload/config/original
Arc references. The optional provenance field is also present in ordinary legacy sample
metadata; exact target layout and cost remain unmeasured. Cold inspection can allocate handle wrappers, and
transactional continuity previews clone bounded metadata references. Safe
pointer-identity controls qualify sharing from the validated handle only.
Complete encoder-to-UniFFI ingress copy cost, binding/SDK allocation and handle
cost, sustained memory/CPU and phone power remain separate benchmark gates.

All five actual serial SDK gates pass: formatting, 720 workspace tests with four
ignored, strict workspace/all-target Clippy, unchanged default-HTTP generated
smoke and freshly generated Swift/Kotlin consumers. All twelve added Rust tests
are individually present as passed in the actual log. Both consumer helpers and
all eight existing Kotlin status controls pass. The current host witness binds
the completed build, its current exact 118-source input set and actual host
library without another build. The new Apple package passes all three release
slices and its installed Swift consumer; Android passes all four ABIs, actual
16 KiB ELF load alignment and its extracted-AAR Kotlin consumer. Exact package,
archive, source and installed-marker readback and independent reviews are clear.
Previous same-version archives and host runtime remain retained separately.

The unchanged generated iOS adapter/native gate passes again against these new
witnesses: five original stored-input controls remain incomplete and all ten
producer controls pass through thirteen commands. Four original AAC packet
probes remain exact. Three generated finalised files independently decode,
including all ten submitted A/V video frames; seven negative producer controls
remain incomplete. These are compatibility controls for the existing adapter.
Neither app adopts the new timing-view API, and this run establishes no new
presentation-equivalence, camera/microphone or physical capture acceptance.
The seven iOS platform commands pass against these new witnesses: 261 budget
controls, 89 filesystem tests, 38 library tests, 16 real-SDK checkpoint tests,
both generated modules and strict iOS16 API checking across all 70 production
Swift sources. All 309 inputs remain unchanged. Both legacy commands pass 86
publisher tests and seven original native ownership controls, retaining both
expected historical failures and all 318 input hashes. Independent completed
outcome reviews are clear for both scopes; owned groups stop, new scratch is
removed and pre-existing scratch is preserved.

All eleven full Debug, Qualification and Release app commands pass with all
327 inputs unchanged. Built privacy resources, creator configuration modes and
manual-link schemes pass, and the compiled Debug test bundle retains both exact
profile corpora. That test bundle is not executed by the app gate. Separate
actual code-object readback verifies all three apps are unsigned, with no
`_CodeSignature` directory, and binds both the Debug loader and implementation
dylib. The implementation dylib is SHA-256
`5c2d9ea46ca99925526a899701fb2fb78444ef24531bdbf9cabac8f772e959ba`.
Root-owned groups stop and working scratch is removed; shared DerivedData and
caches remain. The full-app independent audit is clear, including absence of Mach-O signature
load commands in all four actual code objects. Earlier completed app results
above remain bound to their historical witnesses.

Android Debug and Qualification each genuinely execute all 636 JVM tests in
70 suites, including twenty storage and nineteen AAC observation controls, with
zero failures, errors or skips. Both actual Gradle task states are executed in
this run; no cached XML is presented as fresh execution. Independent readback
verifies all 345 current inputs and 140 XML reports. All three APK assemblies,
lint and qualification isolation pass. Lint retains zero errors/fatal findings
and 11/16/15 warnings for Debug, Qualification and Release respectively.

All APKs contain the exact current AAR ARM64 SDK bytes. Four-ABI AAR ELF and all
APK native ELF/ZIP 16 KiB alignment, source notices, QR decoder DEX, manual-link
scheme/singleTask and qualification/release isolation checks pass. Independent
build and artefact review is clear. Separate direct Release readback checks the
actual complete single-disk ZIP directory and its 150 entries, with no V1
signature entries or APK signing-block footer before the central directory.
It establishes an unsigned Release APK; the inherited artefact verifier alone
does not. Debug and Qualification retain the standard debug configuration;
the independent review records actual signing-block footer presence without
claiming cryptographic signer verification. No release signing, installation,
phone, native descriptor/SELinux or recorder/default adoption is established.

Retained current evidence is under
`target/privacy-standby/shared-sample-timing-view-v1/`,
`shared-sample-timing-view-gates-v1/run-20261023T111300Z-219a651e/`,
`shared-sample-timing-view-packages-v1/` and
`shared-sample-timing-view-native-v1/actual-v1/`. The current app refresh is
separately scoped under `shared-sample-timing-view-app-gates-v1/`: current
platform and legacy receipts are `ios-platform/receipt.local.json` and
`ios-legacy/receipt.local.json`, each with its sibling
`independent-outcome-review.local.json`. The full-app receipt and unsigned
readback are in `ios-app/run-20261023T114707Z-276df5fa/` as
`receipt.local.json`, `actual-product-signature-readback.local.json` and
`independent-outcome-review.local.json`. The current Android evidence is in
`android/run-20261023T115051Z-de1e7f46/` as `receipt.local.json`,
`artifact-verification.local.json`, `release-signature-readback.local.json` and
`independent-outcome-review.local.json`.
The original numerical contract, every original strict failure and all previous
source/package receipts retain their original classifications.

Android's next boundary still requires an authoritative generation receipt from
successful native input/output ownership, actual eligible original AAC-unit
packing/order/count and measured first leading/final EOF-tail facts. Capture
must retain original coded AAC before the current publishing clock/priming
filters, select publishing timing separately and reuse these exact shared
handles and two queues. Missing or unsupported trim, ambiguous output packing,
codec/queue/release failure or unknown EOF cannot be replaced by a zero, inferred
unit count or fabricated full receipt. F0 remains diagnostic. The optional
checked storage below still needs a real SDK recorder adapter on the existing
recording worker/lease, exact generation conservation and concrete producer,
ingress and checked file closure before complete app admission. The private
runtime file/run bindings also need a checked persisted or versioned
reconstruction contract before cold recovery can use them. None of these native,
app-adoption or recovery prerequisites is completed by timing views. Legacy
remains the default; the aggregate two-second loss and physical gates stay open.

## Android checked checkpoint storage

The four-path storage source is locally committed as `d2483b2`:
`RecordingCatalogue.kt`, `RecordingCheckpointStorage.kt`,
`AndroidRecordingStorageIO.kt` and `RecordingCheckpointStorageTest.kt`. The
optional storage factory is a cold child of the exact existing catalogue
`Writer`; no current recorder calls it. `PrivateRecordingSink`, `RecordingFile`,
`QueueDrainer`, `CaptureController`, native codecs/F0 observation, Rust/SDK
bindings and packages, and product defaults are unchanged.

The child retains the same writer lease and single worker. Existing finalised
or `ownersClosed` calls cannot release an active compound child. Cancellation
fences new mutation without waiting for file IO. Metadata, media and root owners
attempt checked close once; an uncertain close retains the actual resources and
catalogue reservation without retrying a possibly recycled descriptor. A checked
incomplete close releases the lease only after actual resource closure. Neither
that close nor the optional fixed-length header rewrite grants finalised app
admission.

Original fragment byte arrays are written and hashed directly, without another
encoded-payload clone, staging queue, encoder or per-unit validation. Container
header/checkpoint metadata is bounded and may be copied. Two alternating slots
and one fixed checkpoint temporary name retain exact owned-file/readback checks.
The storage bounds are 65,536 header bytes, 16 MiB per fragment, 545 to 66,080
checkpoint bytes, 1,024 entries and 256 segments, with a strict free-space floor
above 200 MiB plus 256 KiB. Missing names remain reserved within the existing
six-name allowance. These limits do not establish the aggregate loss bound.

Checkpoint installation requires a cold validator before mutation and after
readback. Its expected sequence, committed offset, initial-header length/hash,
fragment-region length/hash and recording/source-file bindings come from the
actual storage frontier. The current source does not supply an SDK token owner:
a later adapter must compare the real encoded/decoded checkpoint and successor
against every expected field. A sidecar, storage byte count or checked header
alone is not an AAC receipt, producer EOF, finalised app admission or recovery
certificate. A failed partial append may leave uncommitted file bytes; reported
closure bytes cover accepted whole chunks only.

The optional production backend uses Android public `Os` positional IO,
no-follow/close-on-exec flags, retained descriptor identity and bounded names
under a verified `/proc/self/fd` anchor. Root provisioning requests mode 0700 on
the retained directory descriptor under the existing writer lease; media and
metadata creation request 0600. The ParcelFileDescriptor bridge keeps ownership
reachable through duplication/alias/detach checks and uses checked raw close;
quiet wrapper close is not a cleanup receipt. Bounded directory enumeration
uses the verified descriptor anchor. There is no original-path or NIO media-IO
fallback. API 30+ flag inspection is a runtime prerequisite; the optional backend
refuses API 29 before opening, while the existing app minimum and recorder path
are unchanged. Actual bridge, proc, fdsan/SELinux, filesystem and provider
semantics, including digest-copy support, are not established by host tests or
source review.

The existing catalogue now requests owner-read/owner-write permissions atomically
when creating its receipt temporary file. This removes reliance on a process
umask for that request. Unsupported POSIX attributes fail closed; there is no
post-creation privacy window or permissive retry. The source change does not
claim an observed Android umask or provider result. Existing receipt scan/read
behaviour remains separate from the optional strict storage backend.

The first actual V3 app attempt executed 635 Debug tests across 70 suites with
27 failures, zero errors and zero skips; Qualification did not execute. Sixteen
new filesystem tests failed root-identity verification. The retained host
observation shows stable device/inode while adding an entry changed directory
link count from two to three. Eleven later capacity failures, three new and
eight existing library cases, followed retained owners filling the unchanged
16-owner registry. The failure, XML and observation remain retained.

Reviewed V4 removes only directory link count from directory identity;
device/inode/type checks and all regular-file one-link safeguards remain.
Its added real-filesystem regression creates a root entry, checks capacity and
append, then creates a media hardlink and requires the next append to fail
without changing earlier bytes. It does not reset the registry, weaken capacity
or treat successful cleanup as permission to adopt that unsafe file. The other
nineteen test bodies and assertions remain. All twenty storage tests now pass in
both genuinely executed host variants.

The corrected V4 gate genuinely executes both Debug and Qualification unit-test
tasks: each runs 636 tests in 70 suites, including all twenty
`RecordingCheckpointStorageTest` cases, with zero failures, errors or skips.
The three APK assemblies, lint and qualification isolation pass with all 337
recorded inputs unchanged. Lint reports zero errors and 11/16/15 warnings for
Debug, Qualification and Release respectively; no issue is located on the four
changed paths. The Gradle parent is terminal. This evidence does not claim
independent descendant process-group cleanup.

Actual artefact checks pass byte identity with the unchanged SDK AAR, all four
AAR native ABI ELF alignments, and ARM64 native ELF/ZIP 16 KiB alignment in each
of the three actual APKs. Source notices, QR decoder DEX, manual-link scheme and
singleTask manifest entries pass. Qualification/release DEX and manifest
isolation are independently read back; installed routing is not exercised.
These ARM64 APK results do not qualify a four-ABI application release.

Separate actual ZIP/EOCD/central-directory readback finds neither V1 signature
entries nor an APK signing-block footer in the Release APK. Its SHA-256 is
`5d98459866ac22886ebfd6517d88a641a20543b0e81ba81d1fe56bd68a4dbd6b`.
This establishes the retained Release artefact's unsigned structure only;
Debug and Qualification keep the standard debug signing configuration. There
is no Release signing, installation or distribution in this packet. Independent
build and artefact outcome reviews are clear. The host filesystem model remains
a test backend only and never a production fallback.

The recording and source-file bindings currently include a private run UUID
which the existing writer receipt does not persist. After cold restart these
bindings cannot be reconstructed from show/segment and device/inode alone.
Future recovery requires sufficient checked, versioned persisted context or a
reconstructible versioned binding scheme. This source packet does not implement
that prerequisite, migrate existing recordings or adopt orphan sidecars.

Exact retained source, failure and completed gate evidence is:

- Four-path source freeze and independent source review:
  `target/privacy-standby/android-native-recorder-adoption-v1/root-v4/manifest.local.json`
  and
  `target/privacy-standby/android-native-recorder-adoption-v1/independent-review-v4/receipt.local.json`.
- Exact local source commit:
  `target/privacy-standby/android-native-recorder-adoption-v1/root-v4/source-commit.local.json`.
- First failed actual run:
  `target/privacy-standby/android-checkpoint-storage-app-gates-v1/run-20261023T102302Z-305408b3/receipt.local.json`,
  with retained failure details, XML, `root-link-observation.local.json` and
  `independent-failure-review.local.json` in the same directory.
- Completed V4 build and tests:
  `target/privacy-standby/android-checkpoint-storage-app-gates-v2/run-20261023T103516Z-5327ef24/receipt.local.json`
  and its sibling `independent-build-outcome-review.local.json`.
- Actual artefact, isolation and Release signature readbacks:
  `target/privacy-standby/android-checkpoint-storage-app-gates-v2/run-20261023T103516Z-5327ef24/artifact-verification.local.json`,
  with `independent-isolation-readback.local.json`,
  `release-signature-readback.local.json` and
  `independent-artifact-outcome-review.local.json` in the same directory.
- Final combined outcome and source-commit lineage review:
  `target/privacy-standby/android-checkpoint-storage-app-gates-v2/run-20261023T103516Z-5327ef24/independent-final-outcome-review.local.json`.

The completed gates keep their original `0dd4384` HEAD and exact four-path dirty
source binding. The clean local `d2483b2` commit contains the same reviewed and
tested bytes; historical receipts are not relabelled. The first V3 failure and
all earlier iOS/Android evidence remain retained. The 2,875-byte numerical
presentation contract and its thresholds are unchanged.

Independent source, build and artefact reviews are clear. Actual Android
descriptor/API-provider and physical storage qualification, native AAC/SDK
adapter composition, producer closure, default adoption, decoder/presentation
acceptance, recovery and the aggregate two-second loss bound remain open.
No phone/device/camera/microphone access, installation, Release signing, push or
publication is part of this storage packet.

## Android AAC observation packet

The locally committed Android F0 packet (`64e9521`) adds scalar diagnostics only.
Raw and intentional-zero counts commit after successful native input queuing;
missing, explicit zero and unavailable API 29 trim metadata retain distinct
states. Output callback counts, handoff events and a cold snapshot do not prove
access-unit extents, recorder admission, native EOF or durable progress. There
is no hot admission ledger or changed priming/timing policy. It does not supply
the producer authority required for recorder migration or recovery.

Debug and Qualification each execute 616 JVM tests in 69 suites with zero
failures, errors or skips. The three APK/lint/isolation and actual native-byte,
16 KiB ELF/ZIP, routing, QR DEX and notice checks pass; lint retains 11/18/17
warnings and zero errors. Independent source/outcome reviews are clear, and
separate direct ZIP/signing-block readback verifies this Release APK is unsigned.
Evidence is under
`target/privacy-standby/android-aac-observation-app-gates-v1/run-20261023T062739Z-deeb42c4/`.
No codec, capture, decoder, physical-device or recovery qualification is implied.

## Implementation order

1. Extend the existing SDK muxer with an explicitly selected native-AAC
   recording mode. Preserve legacy recording/HLS bytes and the same validated
   immutable sample handles. Check measured first-file leading, completed native
   generation facts and proven final EOF tail. Interior generation slots remain
   intentional media. Missing EOF produces an incomplete prefix with no invented
   final trim.
2. Qualify native audio against the registered numerical contract while retaining
   original strict verdicts, exact independent audio presentation, packet
   retention, timestamps and seeks. Fixed-size
   recording headers and exact track/movie clocks must not shift fragment bytes.
   Candidate edit/roll metadata and the final container duration are subject to
   those decoder checks.
3. Add anchored append-only media and bounded checkpoint ownership to the
   platform recorder. A checkpoint covers only complete written and synchronised
   fragments. Keep recoverable configuration/header facts independent of any
   interrupted final-header overwrite. File and checkpoint installation failures
   retain their actual ownership and visible incomplete state.
4. Recover an eligible owned original into a new exclusive file using checked
   header facts and a bounded streaming copy of its committed fragment prefix.
   Preserve the original until the result is closed, checked and durably
   catalogued. Recovery is explicit, idempotent and subject to the existing
   capture/library gate; it never starts capture or uploads media.
5. Qualify both platform adapters, installed recovery/export workflow and
   sustained resource use. Keep SIGKILL and physical power-loss/storage behaviour
   distinct.

The loss bound covers producer/native pending work, ingress, recording queue,
muxer buffering and actual durable common audio/video presentation. The current
two-second queue bound plus a fragment interval cannot satisfy it by addition.
Enforce the aggregate bound against real durable progress at exact-owner media
admission, independently of network cleanup. A stopped or overdue recording is
a failed sustained recording run, even if publishing continues successfully.

No additional encoder, per-unit validation, raw-frame bridge, unbounded index or
hot duplicate recording journal is authorised by this design. Account for the
existing muxer's assembly/binding copies and synchronisation costs; measure
their complete recording path before making a performance claim. The broader
[delivery plan](mobile-streaming-plan.md) and
[native performance contract](native-performance-contract.md) remain binding.
