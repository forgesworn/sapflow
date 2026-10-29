# Retained HLS resource contract

This follows the 24 October performance audit. PR #30 (`348bdf8`, merged to
main as `39b3180` on 28 October) implements resident presentation admission,
prompt close-time cache release, shared encoded-backing admission and absolute
response deadlines. The two dated receipts below were written on its local
branch before that merge and are kept as recorded.
Matched nominal/adverse measurements and Linux audience qualification remain
open. Recorder admission and muxer input limits cover different owners.

| Current resource | Limit and scope |
| --- | --- |
| SRT publishers | `N`, default 16, including pending admissions |
| RTMP connections and publishers | `4N` connections; C9 separately admits `N` current publication owners |
| Resident presentations | `SAPFLOW_MAX_PRESENTATIONS`, default `4 * N` capped at 4,096 (64 at default publisher admission); includes removed streams through their final owner |
| Shared encoded init/fragment backings | `SAPFLOW_MAX_FRAGMENT_BYTES`, default 536,870,912 bytes (512 MiB); whole encoded Vec capacity, charged once through the final payload owner |
| HLS window | Default 24 entries per path, configurable from 7 through 1,000, plus open media |
| Reconnect hold | Default 90 seconds, configurable up to 3,600 seconds per useful path |
| HTTP connections | 20,000; this is not a media-byte limit |
| Ready HTTP response lifetime | `SAPFLOW_HTTP_BODY_TIMEOUT`, default 120 seconds, configurable from 1 through 3,600; independent of progress-reset idle time and parked waits |

The relevant owners are in [the registry and stream](../crates/sapflow-server/src/stream.rs),
[publication retirement](../crates/sapflow-server/src/publish.rs),
[HTTP bodies](../crates/sapflow-server/src/media_body.rs) and
[the own HTTP connection](../crates/sapflow-server/src/own_http.rs).

One active publisher can populate successive paths and leave useful caches in
reconnect hold. Publications, grace tasks and parked requests can retain a
stream after registry removal. Each stream now holds one non-cloneable resident
permit until its final owner drops. Closing releases its cached ingest state
and rendered playlists after terminal drain and recorder feed; accepted body
and recording aliases keep their immutable bytes valid.

An incomplete segment response retains its shared chunk array, including
consumed prefix chunks. Evicting that segment from the playlist does not free
payloads retained by responses. The own backend's 60-second idle timeout
resets on successful progress; the independent absolute response deadline does
not. Recording queues are another owner. Their aliases keep the original shared
backing charge as well as the recorder's separate queue/writer reservations.

The contract uses one App-owned registry admission domain, with these limits:

- Implemented: count resident presentations through their final stream owner.
  The selected default is `4 * N`, capped at 4,096, giving 64 at the default
  publisher limit. `SAPFLOW_MAX_PRESENTATIONS` or `--max-presentations` selects
  an explicit 1 through 4,096. These are admission settings, not qualified
  operating guarantees. The domain is shared by both ingest protocols.
- Implemented: charge each immutable init/fragment backing once, before
  publishing it. The selected default is 512 MiB for whole encoded Vec
  capacities, including encoded headers and spare capacity.
  `SAPFLOW_MAX_FRAGMENT_BYTES` or `--max-fragment-bytes` selects a positive
  byte count representable on the platform.
  All cache, segment, response, emitted-frame and recorder aliases retain that
  same charge. Release follows the final payload owner, including after cache
  eviction, codec replacement, reconnect or registry removal.

The selected defaults still need operating qualification. Neither setting
establishes a measured operating guarantee. Input/PES buffers, pending samples,
encoding temporaries, recorder output copies, allocator overhead, metadata and
kernel socket buffers remain separate. A fragment charge is not
a process RSS measurement. Counting every alias's length would overcount
shared storage.

At resident capacity, RTMP receives `NetStream.Publish.Denied` with
`presentation limit`; SRT receives the existing resource-refusal status 402.
Admission runs before recorder setup. Failure rolls back any temporarily
reserved RTMP publication credit. Same-path reconnect and takeover reuse the
resident reservation, without silently evicting another presentation. If an
expired presentation is removed while an old owner retains it, a fresh object
must acquire another reservation and may be refused until that owner drops.

Fragment refusal returns `MuxError::FragmentLimit` or
`IngestError::FragmentLimit`, reaches the publication policy and marks any
accepted recording prefix incomplete. A refused part gets no published ID.
Terminal drain seals only admitted parts, without extending their duration to
cover the refused suffix. A codec replacement that cannot seal its old tail
keeps the old epoch and releases its unused successor init. Further media on
that failed muxer is refused; a new ingest resumes the accepted presentation
after terminal cleanup, with the same quota. Recovery still needs free byte
capacity. No active presentation is silently evicted to make room.

The absolute response timer starts when `respond` owns a ready framed
response, including headers. It excludes blocking playlist/preload waiting,
whose existing deadline remains three target durations. HEAD releases its
unused media immediately. A completed response releases its payload and ends
that timer; a subsequent keep-alive response gets a new timer.

Every write checks the monotonic deadline, even after successful progress.
A turn yields after 16 write attempts so other connections and deadline sweeps
can run. Sweeps normally run every 250 ms; actual scheduling under load remains
part of qualification. Expiry closes the connection and drops the incomplete
response. It appends no error response to an already-started body. A client
can detect the incomplete Content-Length and retry normally. Bytes already
accepted by the socket remain an exact prefix. Kernel send/receive queues and
client playback buffers are separate from application backing ownership.

Qualification uses generated media and small injected limits:

1. With one active publisher, populate eight distinct useful paths before
   grace expires. First reproduce current retention, then prove admission at
   the configured resident limit and release after the final owner drops.
2. Retain segment bodies, rotate beyond their windows, and verify lookup
   eviction while the retained bodies still supply exact ordered bytes.
   Drop counters must distinguish consumed-prefix retention from payload
   release.
3. Exercise exact byte capacity, one-over refusal, arithmetic overflow, cached
   aliases, codec changes, reconnects and stalled own responses. Charges must survive every alias and
   return exactly once.
4. Prove recording status, publisher admission and recovery under exhaustion.
   Repeat matched nominal and adverse-load measurements with all configured
   limits declared. Keep logical accounting, RSS and kernel-memory evidence
   separate.

C9 supplies the separate RTMP publication gate. Its permit belongs to the
current session generation. Same-path takeover transfers that permit; retirement
returns it even while a presentation remains in reconnect hold. A detached
reconnect must acquire available capacity again. Cross-protocol refusal leaves
the existing owner intact. The separate `4N` connection cap remains in place.

At the C9 admission baseline, the previous code accepted two distinct RTMP
publishers with `N = 1` in an executed regression. Its correction passed 16
new quota, ownership, wire and recording tests within an 884-test workspace
run, plus Clippy and formatting. That admission receipt did not implement the
global HLS budget or qualify throughput, RSS or sustained availability.

## Earlier local presentation step, 28 October 2026

This preserves the narrower receipt before the fragment/deadline extension
below. Source: local branch `fix/hls-presentation-residency`, based on `30dedca`.
The sorted SHA-256 manifest of the twelve changed Rust files has digest
`7d15f798c373a7b441d04ebc261a992ccb62bbfba71d00e1e9d4f554b8f5d14e`
(each line is `<file SHA-256>  <relative path>\n`).
This is a local implementation receipt; it has not been pushed, merged or
deployed. It changes resource admission without changing the operator role,
network destinations or media disclosure.

Three regressions were executed before adding enforcement and close-time
release. They failed on the fifth useful path exceeding a four-presentation
limit, admission while a removed stream still had owners, and cached muxer
retention after close. All three pass after the fix.

Fifteen added tests cover presentation counts and overflow, concurrent permit
acquisition/release, configuration bounds, serial paths admitted into a
90-second reconnect hold,
refusal before recorder setup, RTMP credit rollback, same-path takeover,
reconnect and codec changes, expired replacement, final stream-owner release,
actual window eviction and exact ordered response bytes. A payload drop counter
shows consumed-prefix retention until the final body drains, with each backing
released once. The RTMP test checks the actual protocol refusal and subsequent
admission. SRT status mapping is unit-tested. A real recording test checks
that a refused presentation starts no recording and leaves the current one
complete, then admits a fresh recording after final release. Existing terminal
loss tests still require incomplete status and exact accepted recording bytes.

Generated fixtures are the documented sine-audio and test-pattern transport
streams already in the repository. Injected resident limits are one and four;
the rotating window has eight entries. No camera or microphone media is used.
No fragment budget is enforced by this step. Selected server defaults are 64
presentations, 24 playlist entries and 90 seconds of reconnect hold.

Local format and workspace Clippy checks pass. `cargo test --workspace` passes
972 tests, with six tests ignored by the standard command. The codec-epoch
decode check below was also executed explicitly; other ignored gates remain
separate.
Commands: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`. No dependencies or parser formats were added.

`bash -n scripts/smoke.sh` and `python3 -B tests/test_smoke_media.py` pass
(11 helper tests). The full smoke suite passes with
`SAPFLOW_BIN=./target/debug/sapflow SAPFLOW_HTTP_THREADS=1
SAPFLOW_MAX_PRESENTATIONS=4 scripts/smoke.sh`. This covers generated SRT/RTMP
media, LL-HLS, callbacks, independently decoded segments/parts/recordings,
reconnect, takeover, encrypted/optional-encryption refusal and clean shutdown.
Two earlier attempts failed during live HTTP assertions after successful
decode checks. Tracing the second showed the finite publisher's reconnect
hold expired during those checks. The script now checks live responses before
decoding its downloaded files, retaining all original assertions.

The normally ignored
`epoch_tests::each_resolution_epoch_recording_decodes_independently_with_audio`
passes explicitly using `cargo test -p sapflow <test-name> -- --ignored --exact`.
Its earlier runs exposed two fixture assumptions: three offline epochs could
exceed the two-writer path bound while predecessors drained, and asynchronous
file creation could differ from codec arrival order. The fixture now waits
for each predecessor's real synced recording-completion callback and checks
the expected track configurations independently of filenames. All three
files decode with audio. HTTP epoch order remains checked separately.
This is generated host media, not physical capture or native recovery proof.

All eight recorded Rust fuzz inputs pass: one FLV ingest, one SRT listener,
three SRT packet, two RTMP session and one small-parser regression.
Seeded campaigns also pass without findings:

| Target | Inputs executed | Reported duration |
| --- | ---: | ---: |
| `ts_ingest` | 18,173 | 61 seconds |
| `flv_ingest` | 608,434 | 61 seconds |
| `muxer_timing` | 8,395 | 61 seconds |

Commands use `RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 cargo fuzz run -O -a
<target>` from `fuzz/`, recorded regressions as file arguments, then task-owned
copies of the target's seeds with `-- -max_total_time=60 -max_len=65536`.
The initial sanitiser-instrumented build received SIGTERM before executing its
regression; its cause was not established. Restarting in the same worktree
target completed successfully. A build interruption is not a fuzz verdict.
These finite campaigns do not replace full CI or long source-specific fuzzing.

Raw local logs and the source manifest are retained as ignored
`target/presentation-residency-*` evidence. Smoke children have stopped, and
task-owned temporary corpora, generated-media scratch and test directories
were removed. Existing services and private operational evidence were left
untouched. `scripts/tidy.sh` reports 2.4 GiB in this worktree's target.

At that earlier step, still required for task 2.6: whole-allocation fragment charges through every
alias, fragment-exhaustion/incomplete-recording recovery, an absolute body
deadline for stalled and trickling responses, and matched nominal/adverse load
measurements. Logical charges, RSS and kernel memory remain separate. Neither
these tests nor the selected count default qualifies Linux audience capacity.

The small-window setup also exposed an existing separate readiness problem:
`has_content()` requires more than seven entries, so the permitted seven-entry
configuration never becomes servable. These regressions use eight entries.
Fixed on 29 October: `has_content()` now tests for a real newest entry, and a
muxer regression covers the seven-entry window; the full smoke test also
passes with `SAPFLOW_SEGMENT_COUNT=7`.

## Shared backing and response deadlines, 28 October 2026

Source: the same local `fix/hls-presentation-residency` branch on `30dedca`,
extending the earlier presentation step. This receipt was prepared before
commit and hosted CI; it records local qualification rather than deployment
or production acceptance. The sorted SHA-256 manifest of the 31 changed Rust, fuzz-seed and
smoke-script files has digest
`998e942da9f7b79e9225cedacf4aae17c9bccc12a56b9a49b7eb8564b01e84da`
(lines are `<file SHA-256>  <relative path>\n`).

The first ownership regressions failed before enforcement: spare capacity was
uncharged, one-over capacity was accepted, and TS published beyond a one-byte
limit. Three response regressions likewise failed without deadline enforcement:
stalled output, trickling progress and a second keep-alive response retained
their bodies past expiry. These pass with the implementation.

Core checks cover exact capacity, one-over refusal, arithmetic overflow,
concurrent reservations and final release. HLS checks cover real encoded Vec
capacity including headers/spare space, clone/slice ownership, cache eviction,
codec replacement and reconnect recovery. A separate failing regression found
that room for a successor init could hide refusal while sealing the old tail;
the fix returns that refusal before installing the successor epoch.

Server checks cover both TS and FLV initialization, a shared exact-capacity
limit across paths, ordered retained responses after eviction/close, consumed
prefix retention, queued/active recorder ownership through success, error and
cancellation, single-batch and terminal exhaustion, exact incomplete recording
bytes, and fresh publication recovery. Deadline checks cover stalled/trickling
output, ready-time start after parked waiting, HEAD, keep-alive reuse, checks
between successful writes, bounded write turns and real TCP backpressure with
an exact framed-prefix readback. Deadline clocks are advanced in those tests;
this is not a sustained deployment timing measurement.

Selected server defaults are 64 resident presentations at 16 publishers,
512 MiB of encoded backing and a 120-second ready-response deadline. Unit
budgets include one-byte refusal, measured exact first-presentation capacity,
16 KiB/1 MiB/8 MiB cases and small clock-driven deadlines. Fuzz budgets use a
u16 byte maximum, with generated 8 KiB seeds. No test publishes camera or microphone material; existing
generated transport fixtures and original structural placeholders are used.

Final local verification:

- `cargo fmt --all -- --check` and workspace Clippy with `--all-targets -- -D
  warnings` pass. `cargo test --workspace` passes 1,000 tests with six ignored
  by that command; the codec-epoch decode test also passes explicitly.
- All eight recorded Rust fuzz regressions pass. The extended seeded ASan
  campaigns, using `RUSTC_BOOTSTRAP=1 CARGO_BUILD_JOBS=2 cargo fuzz run -O -a
  <target> <owned-corpus> -- -max_total_time=60 -max_len=65536`, pass without
  findings: TS ingest 32,213 inputs, FLV ingest 500,121 and muxer timing 8,579,
  each in 61 reported seconds. Their `FBUD` mode exercises shared backing
  limits and final release; the muxer mode also checks aliases and refused
  successor epochs. This does not replace long fuzzing or full hosted CI.
- The full `scripts/smoke.sh` passes with `SAPFLOW_BIN=./target/debug/sapflow`,
  one HTTP thread, `SAPFLOW_MAX_PRESENTATIONS=4`,
  `SAPFLOW_MAX_FRAGMENT_BYTES=16777216` (16 MiB) and
  `SAPFLOW_HTTP_BODY_TIMEOUT=120`. All generated ingest, HTTP, recording,
  reconnect, takeover, encryption/refusal and shutdown checks pass.

Raw logs and the current source manifest remain ignored under
`target/retained-media-*`; earlier failing and intermediate `target/fragment-*`
and `target/body-deadline-*` logs are retained locally. Task-created corpora,
fixture directories and smoke children are removed/stopped. The task's
instrumented release build is cleaned separately; ordinary worktree builds
and evidence remain. Existing services and private operational files were
preserved.

Task 2.6 implementation and local correctness are now covered. Its matched
nominal/adverse measurements remain open, including multiple publishers,
slow consumers, sustained memory/descriptors, recording integrity and deadline
scheduling under load. Wired Linux/Caddy/HTTP2 audience qualification, long
fuzz campaigns and the production decision remain separate gates. Logical
charges, process RSS and kernel memory must be reported separately. The
seven-entry readiness problem above was fixed separately on 29 October.
