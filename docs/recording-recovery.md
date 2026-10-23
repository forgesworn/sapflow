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

The current source passes 708 workspace tests, strict Clippy and formatting,
generated-stream smoke checks and actual generated Swift/Kotlin consumers. Four
workspace tests remain ignored by that default command. The unchanged bounded
checkpoint parser previously passed four scalar seed replays and 522,636
ASan/overflow fuzz executions. Host consumers cover frontier, hash, successor,
foreign-owner, retained-token and exact presentation-query checks. These results qualify
the SDK API and bounded parser; they do not establish installed recovery or the
two-second loss bound. Evidence is retained under
`target/recording-checkpoint-gates-v1/`,
`target/privacy-standby/recording-checkpoint-fuzz-v1/` and
`target/privacy-standby/recording-checkpoint-native-check/`. The current extent
API and refreshed packages are recorded under
`target/privacy-standby/recording-native-extent-gates-v2/` and
`recording-native-extent-packages-v1/`.

The refreshed Apple SDK package passes all three release slices and its
installed Swift host consumer. The Android SDK package passes all four ABIs,
actual 16 KiB ELF load alignment and its extracted-AAR Kotlin host consumer.
Both retain unchanged source bindings and verified package contents. Package
and host checks do not establish a rebuilt app, phone installation or physical
recording acceptance.

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

The concrete owner can now finish only the SDK inputs it actually accepted.
It retains the latest successfully installed real issued token, captures actual
finish/drain/statistics before native cleanup, checks exact checkpoint/header
and accepted-sample coverage, then queries shared presentation duration once.
There is no caller-supplied upstream-complete flag or fabricated producer proof.
Every current result is scoped to submitted SDK inputs, with application admission
unknown and an explicitly incomplete catalogue receipt, even at complete AAC EOF.

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

The current host gate passes 86 filesystem tests, 38 library tests, 16 real-SDK
filesystem/API tests and the strict iOS 16 check of all 68 app source files.
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

The real app producer/ingress/recording closure chain, synchronous known-loss
accounting, SDK recorder migration and physical Stop qualification remain open.
The submitted-input result cannot substitute for those barriers or the aggregate
two-second loss requirement.

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
