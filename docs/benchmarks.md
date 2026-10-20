# Benchmarks: sapflow against mediamtx 1.19.1

M4 in `GOALS.md`. Same machine, same encoder input, same simulated viewers.
Reproduce with `scripts/bench.sh sapflow` and
`MEDIAMTX=<path> scripts/bench.sh mediamtx`.

## Method

- **Machine:** Apple silicon Mac, 8 cores, 16 GB, macOS 14. Server, encoder
  and simulated viewers all run on it, so the viewer numbers include
  contention from the load generator (about 3.8 Gbit/s over loopback at 750
  viewers). Treat the tail latencies at 750 viewers as an upper bound for
  both servers.
- **Feed:** ffmpeg test signal over RTMP: 1280x720, 30 fps, H.264 2.5 Mbit/s
  CBR, 1 s keyframes, AAC 128 kbit/s (v4v's recommended encoder settings).
- **Servers:** sapflow release build; mediamtx v1.19.1 darwin/arm64 release
  binary (checksum-verified). Both with v4v's reference LL-HLS settings:
  1 s segments, 200 ms parts, 24 segments, remux from publish. mediamtx in
  its default session mode (cookie check, then a per-viewer session in every
  URI), which is how v4v's reference config runs it.
- **Viewers** (`crates/sapflow-bench`): each behaves like hls.js in
  low-latency mode on its own connection: blocking playlist reloads
  (`_HLS_msn`/`_HLS_part`) plus the preload-hinted part, and every fifth
  reload a plain playlist request and a fetch of the newest whole segment.
  10 s ramp, then 60 s measured.
- **Memory:** resident set size. **CPU:** process CPU time over wall time,
  as a percentage of one core (100% = one core busy).
- **Part fan-out:** for each part, the time from the first viewer receiving
  it to the last; how evenly a new part reaches everyone.
- **Runs:** sapflow 3 or more runs per row (failures: the runs after the
  keep-alive fix), mediamtx 3 (fan-out: 1, after a harness fix). Ranges are lowest to highest across runs.

## Results (2026-10-20)

| Measure | sapflow | mediamtx 1.19.1 |
|---------|---------|-----------------|
| Binary size | 3.5 MB | 50.5 MB |
| Container image (arm64) | 38.5 MB | 62.2 MB |
| Idle memory | 3.5 to 3.6 MB | 16.0 to 31.4 MB |
| One stream, no viewers: memory | 16.9 to 24.1 MB | 31.5 to 52.8 MB |
| One stream, no viewers: CPU | 0.3 to 0.4% | 0.7 to 0.9% |
| **250 viewers** | | |
| CPU | 10.2 to 11.5% | 34.6 to 37.5% |
| Peak memory | 44.6 to 55.6 MB | 132.6 to 142.1 MB |
| Playlist p50 / p95 / p99 | 4.2-4.8 / 6.1-6.9 / 7.5-11.4 ms | 8.2 / 14.8-21.1 / 22.7-35.1 ms |
| Segment p50 / p95 / p99 | 3.0-3.4 / 4.7-5.3 / 6.4-9.1 ms | 5.1-5.2 / 8.9-11.9 / 13.4-25.1 ms |
| Part fan-out p50 / p95 / max | 5.7-6.3 / 10.4-12.8 / 21.0-29.2 ms | 18.2 / 42.2 / 94.9 ms |
| Failed requests | 0 (2 to 8 per run before the keep-alive fix) | 0 |
| **750 viewers** | | |
| CPU | 26.0 to 29.7% | 103.1 to 107.0% |
| Peak memory | 66.2 to 77.3 MB | 263.3 to 266.5 MB |
| Playlist p50 / p95 / p99 | 10.3-13.5 / 30.2-42.4 / 53.6-81.8 ms | 23.9-29.3 / 58.7-103.4 / 101.1-190.5 ms |
| Segment p50 / p95 / p99 | 6.8-8.2 / 21.0-37.3 / 35.8-88.5 ms | 8.0-13.5 / 37.4-74.8 / 72.4-207.0 ms |
| Part fan-out p50 / p95 / max | 34.2-42.1 / 92.8-130.7 / 126.9-460.8 ms | 55.5 / 116.4 / 393.5 ms |
| Failed requests | 0 (up to 3 per run before the fix) | 0 |
| Glass-to-glass delay | not measured | not measured |

## Reading it

- Where the CPU goes, by profiler: see [profiling.md](profiling.md).
- sapflow used about a third of mediamtx's CPU at both loads, and about a
  third of its memory under load.
- Median playlist times were about half of mediamtx's at both loads; median
  segment times were about 60 to 85% of mediamtx's. At 750 viewers the tails
  overlap more (the load generator shares the machine), though sapflow's
  were still lower in these runs.
- sapflow's failed requests came from the server closing idle keep-alive
  connections after 10 s (hyper applies its header timeout to them), which
  raced with viewers reusing those connections. With 60 s, seven runs since
  (five at 250 viewers, two at 750) had no failures.
- Worst cases at 750 viewers: sapflow's maximum segment time and fan-out
  (up to 405 and 461 ms) can exceed mediamtx's (348 and 394 ms), although
  its medians and p95s are lower. With the load generator on the same
  machine these maxima are noisy; a test on separate machines would settle
  it.
- Not measured: glass-to-glass delay, which needs a camera filming a clock
  and a phone playing the stream. Time from encoder connect to a servable
  stream was measured instead: about 4 s over SRT with a 2 s SRT latency,
  about 1 s over RTMP.
- One machine, short runs. A deployment-shaped test (separate machines,
  Caddy in front, real encoder) would be the next step if the decision is to
  keep going.

## Across two machines (2026-10-20)

The user asked for a separate-machine test before any production use. Server
on a second Apple silicon Mac (M4, 16 cores, 64 GB, macOS 26); encoder and
simulated viewers on the first; `scripts/bench-remote.sh`. The link between
them was Wi-Fi at about 72 Mbit/s, which carries about ten full viewers, so
250 viewers followed the playlist with blocking reloads and only 10 fetched
media. Both runs saturated the link (about 52 Mbit/s received), so response
times here are mostly the network's. sapflow: two runs after the open-file
fix below (ranges); mediamtx: one run.

| 250 viewers (10 full), server on the M4 | sapflow | mediamtx 1.19.1 |
|---------|---------|-----------------|
| Idle memory | 3.5 MB | 34.3 MB |
| One stream, no viewers: memory / CPU | 23.4-23.5 MB / 0.6-0.8% | 69.1 MB / 0.4% |
| Server CPU / peak memory | 10.3-11.0% / 30.5-31.2 MB | 25.1% / 132.4 MB |
| Playlist p50 / p95 / p99 / max | 59-67 / 178-502 / 545-691 / 2,067-2,125 ms | 69 / 1,244 / 2,262 / 14,403 ms |
| Segment p50 / p95 / p99 (full viewers) | 322-394 / 822-998 / 1,115-1,465 ms | 1,009 / 3,289 / 5,076 ms |
| Playlist update reaching every viewer, p50 / p95 / max | 536-656 / 1,307-1,555 / 2,346-2,538 ms | 2,105 / 7,479 / 14,291 ms |
| Failed requests | 0 of 88,956 and 0 of 91,995 | 1 of 64,028 (timeout) |

- The CPU and memory ratio matches the single-machine runs: about a third.
- On a congested link sapflow's tails were a quarter to a half of mediamtx's.
- **Bug found by this test:** before the fix, sapflow failed 17 to 18
  requests per run here (connection resets) and mediamtx none. The server
  log showed "Too many open files": a process started over SSH on macOS gets
  a soft limit of 256 files, and 250 viewers need more. Go's runtime raises
  that limit for mediamtx automatically; sapflow now does the same at
  startup (`fdlimit.rs`) and warns if it cannot. systemd's usual default of
  1,024 would have hit the same wall in production.
- A first attempt measured a sapflow left running from the previous run in
  place of mediamtx (sapflow held the IPv4 port, mediamtx only IPv6); those
  numbers were discarded, and the runner now checks which process holds the
  port.
- Still wanted: wired gigabit or faster between the machines, so 250 and 750
  full viewers can be measured across the network.

## Decision

2026-10-20, the user: **keep experimenting.** Before any production
switch: v4v's ingest gate against sapflow, Safari on an iPhone, and a test
on separate machines.
