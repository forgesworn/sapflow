# HTTP and SRT efficiency, 11 November 2026

This document records the initial shared-Mac screening. The later
[Linux qualification and adoption decisions](performance-qualification-results-2026-11-11.md)
supersede its provisional conclusions where explicitly stated; all original
measurements below remain unchanged.

This work starts from `fa16f09112d92cf1db3fe04a9461e0feb0238bcd`.
It preserves the HTTP and SRT contracts, playback defaults and admission limits.
No deployed service, physical device, publishing route or media quality setting
was changed.

## Changes

HTTP workers subscribe only to presentations with parked requests. A stream
change adds one coalesced identity to each interested worker's pending set;
the worker examines that presentation's waiters. Previously every change woke
every worker and scanned every parked connection, even for unrelated streams.
The existing version check still decides which requests need another attempt.
Subscribe-before-recheck closes the registration race. Teardown synchronises
with notification and removes queued identities before releasing the stream.

The metadata is bounded by active connections and stream groups. Empty groups
are removed, surviving groups shrink geometrically after viewer migration, and
pending/woken scratch is reused. There is extra park/unpark bookkeeping, so a
single-stream workload needs its own control rather than assuming a benefit.

Conventional HLS playlists are rendered lazily once per state generation and
shared as immutable `Bytes`. Accepted changes, partially mutating ingest errors
and close invalidate the cache. Existing responses retain valid immutable bytes.
Low-latency-only presentations never populate this extra cache; conventional
mode can retain one additional playlist per presentation. Playlist memory is
outside the existing fragment-byte accounting, as before.

The SRT receiver retains the sequence identity of its first occupied slot.
Leading missing packets no longer require repeated linear searches on each
release/timer query. Accepted earlier retransmissions update the identity;
removing that packet finds its successor. Tests compare release, loss flags
and timer decisions with the former scan logic across gaps, wrap, reorder,
room-making, drop requests and shutdown.

## Evidence and limits

The measurements use native ARM64 release builds on an eight-core Apple M1 Mac
with 16 GiB RAM, Rust 1.94.1, and the repository's existing release profiles.
Other sessions remained active. At one preflight observation load averages were
81.86/76.41/58.35 and swap use was about 11.2 GiB. Serialising this task's jobs
does not make that an isolated measurement host.

- [Playlist cache model](performance-evidence/2026-11-11/playlist-cache.json):
  all eight alternating pairs preserved, with exact response bytes/headers and
  equal returned-byte checks. Median cached/control elapsed ratio was 0.041965
  (95.8% less response-construction time). Intervals were very short and the
  playlist was warm and unchanged. This is not a whole-server CPU, memory,
  socket-latency or live cache-invalidation result.
- [SRT receive-gap comparison](performance-evidence/2026-11-11/srt-gap.json):
  the longer, four-pair follow-up observed 91.8% less elapsed time in the
  synthetic burst-gap workload. Clean-stream median candidate/control ratio
  was 0.9997. One clean-stream pair and one pending/timer pair exceeded the
  predeclared 1.10 worst-pair guard, so nominal non-regression is unqualified.
  Both attempts and all 128 scalar rows are retained. Receiver layout grows
  from 776 to 784 bytes, excluding heap buffers. These are library timings,
  not network or whole-server measurements.
- [Live HTTP screening](performance-evidence/2026-11-11/http-efficiency.json):
  all 16 trials completed without measured or excluded request errors; every
  stream reported full viewer reach. Binary and harness hashes were unchanged
  throughout. Aggregate reported received media was 3,636.1 MB. This validates
  useful workload completion, not an efficiency improvement.

Median paired changes (candidate relative to parent):

| Workload | Server CPU | Sampled peak RSS | Received media | Requests |
| --- | ---: | ---: | ---: | ---: |
| One stream, 32 viewers | +11.9% | -1.0% | -1.5% | -1.6% |
| Eight streams, 16 viewers each | -1.8% | -0.1% | approximately unchanged | approximately unchanged |

Server CPU was only 0.92-1.25% of one core in the single-stream trials and
4.50-6.24% in the eight-stream trials. Paired CPU ratios ranged from 0.928 to
1.273 and 0.800 to 1.055 respectively. The single-stream control is adverse;
the multi-stream reduction is small and inconsistent. These results do not
establish a live-server performance benefit. Peak RSS also varied widely in
the final eight-stream pair (candidate 16.1 MiB, parent 12.0 MiB); the favourable
median must not hide that row. Media totals differ slightly because the windows
start independently against live publishers, so they are not byte-identical
fixed-work trials. Every stream's reported mean part reach was its full viewer
count. This workload uses LL-HLS; conventional-playlist cache benefit remains
limited to the separate response-construction model above.

**Default adoption remains unqualified.** Preserve the single-stream overhead
and failed SRT worst-pair guards. A controlled host, longer CPU intervals and
production-shaped load are needed before claiming an overall improvement.

## Verification

The HTTP implementation passed 296 server tests, with two ignored tests (the
manual cache benchmark and a physical-media check), and all-target server
Clippy. The cache model was then run explicitly in release mode. New tests
cover subscribed workers only, coalescing, pending-set drain/rearm, concurrent
unsubscribe, final-waiter and worker cleanup, presentation identity, cache
invalidation and waiter-storage release after migration.

The SRT suite passed 77 unit, 29 caller integration and three recorded-trace
differential tests, with one existing manual crypto benchmark ignored. Its
all-target Clippy check passed. The new independent old-scan model checks
24,000 mixed events and separately covers sparse nonmonotonic timestamps.

The final workspace format and all-target Clippy checks passed. Workspace tests
passed 1,062 tests across 25 suites, with seven ignored manual or environment
checks and no failures. Performance receipt, process sampler and smoke helper
regressions passed another 52 Python tests. Gitleaks found no secrets in the
implementation commits and retained receipts. The release-binary smoke passed:
SRT/RTMP publication and refusals, blocking reload, decoded segments and parts,
callbacks, recording, reconnect continuity, publisher takeover, required and
optional SRT encryption, and clean shutdown. The two decode checks verified
150 and 180 video frames at 30 fps. This is generated local media, not physical
device acceptance. Historical performance receipts do not
qualify this revision. Native Linux, production-shaped proxy/load, sustained
loss/recovery, physical playback and energy remain separate evidence gates.

## Reproduction

Build each server revision sequentially in one worktree's `target/`, preserve
the parent executable, then restore and build the candidate. Build the viewer
separately to avoid unifying its HTTP/2 features into the server:

```sh
cargo build --locked --release -p sapflow -j 2
cargo build --locked --release -p sapflow-bench -j 2
python3 -B scripts/compare-http-efficiency.py \
  --baseline target/perf-parent --candidate target/release/sapflow \
  --bench target/release/sapflow-bench \
  --baseline-source fa16f09112d92cf1db3fe04a9461e0feb0238bcd \
  --candidate-source 44bbd091f5b15960939164ac631b9bcd9824e519 \
  --pairs 4 --seconds 12 --threads 4 --output target/http-efficiency.json
```

The runner uses generated 360p30 H.264/AAC, one stream with 32 full-media
viewers and eight streams with 16 viewers each. Publishers remux the same
generated fixture; alternating AB/BA order, executable/harness hashes,
nonzero useful media, all errors and failed trials are retained. Server CPU
sampling is nominally aligned to the post-ramp viewer windows; process/runtime
startup adds unmeasured skew. There is no decoder, proxy, TLS or recording in
this screening. The separate end-to-end smoke supplies decoding/recording
checks, not comparative timings.

Only compact scalar receipts and this method are retained. Generated media,
raw logs, temporary binaries and task-owned fixture processes are removed
after their results are recorded.
