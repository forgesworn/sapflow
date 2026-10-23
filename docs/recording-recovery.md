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
Clippy and generated Swift/Kotlin host consumers pass. Refreshed packages and
the unchanged native presentation checks remain separate gates before a
platform recorder adopts it.

## Implementation order

1. Extend the existing SDK muxer with an explicitly selected native-AAC
   recording mode. Preserve legacy recording/HLS bytes and the same validated
   immutable sample handles. Check measured first-file leading, completed native
   generation facts and proven final EOF tail. Interior generation slots remain
   intentional media. Missing EOF produces an incomplete prefix with no invented
   final trim.
2. Qualify exact native and independent audio presentation, packet retention,
   timestamps and seeks against the unchanged generated experiment. Fixed-size
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
