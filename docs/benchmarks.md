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
  10 s ramp, then 60 s measured. Plain HTTP/1.1 unless asked otherwise:
  `--http2` runs them over TLS and HTTP/2 (see
  [Viewers over HTTP/2](#viewers-over-http2-through-caddy-2026-10-21)).
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
| Glass-to-glass delay | not measured here; 3.0 to 3.2 s later, Larix to Safari over HTTP/2 (`docs/interop.md`) | not measured |

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
- Not measured in this run: glass-to-glass delay (measured later: 3.0 to
  3.2 s, Larix to Safari over HTTP/2, `docs/interop.md`), which needs a camera filming a clock
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

## Viewers over HTTP/2 through Caddy (2026-10-21)

Production puts Caddy (TLS, HTTP/2) in front of sapflow, and Safari only uses
low-latency positioning over HTTP/2, so `sapflow-bench` can now speak both:

```
sapflow-bench --url https://<front>/live-hls/live/<event>/index.m3u8 \
    --ca <root.crt> --http2 --viewers 250 --secs 60
```

- `https://` needs `--ca <pem>`: the CA that signed the front's certificate
  (Caddy's `tls internal` root, or a system bundle such as
  `/etc/ssl/cert.pem`). Nothing else is trusted; there is no insecure switch.
  TLS 1.3 only.
- `--http2`: ALPN offers only `h2`, and each viewer opens one HTTP/2
  connection that carries all its requests (blocking reload, part, playlist,
  segment) as streams, as a browser does. A front that cannot speak it is an
  error, not a fallback. Without it, `https://` speaks HTTP/1.1 over TLS, a
  control that separates what TLS and Caddy cost from what HTTP/2 changes.
  `http://` is unchanged; `--http2` and `--ca` are refused with it.
- Each viewer makes its own full TLS handshake (no resumption between them)
  and advertises HTTP/2 windows of 1 MiB per stream and 4 MiB per connection,
  as browsers do in megabytes; hyper's 64 KiB default would add a round trip
  per 64 KiB of a segment and show up as the front's delay.
- The report gains a line: responses by HTTP version and connections used
  (over HTTP/2 a run should say `HTTP/2 ...` and `1.00 per viewer`).
- A failed response body (for example a reset stream) now counts as a failed
  request; before, it was dropped without being counted.
- `scripts/bench-remote.sh` passes `BENCH_URL` (the front's URL) and
  `BENCH_ARGS` (for example `--http2 --ca root.crt`) to the viewers. Its
  server CPU and memory are still sapflow's alone, not Caddy's.
- `cargo test -p sapflow-bench` runs the viewers against an in-process TLS
  front (throwaway CA, HTTP/2 and HTTP/1.1 by ALPN): one connection per
  viewer over HTTP/2, a reload and a part in flight at once on it, HTTP/2
  refused when the front lacks it, an untrusted CA refused.

**Smoke comparison, one Mac, shared.** sapflow (a prebuilt release build) fed
by `scripts/bench.sh`'s ffmpeg RTMP signal (720p30, 2.5 Mbit/s, 1 s
keyframes); Caddy 2.11.7 in Docker Desktop with
`scripts/behind-caddy.Caddyfile` (`tls internal`, `h1 h2 h3`) as in
`scripts/behind-caddy.sh`; viewers `--secs 60 --ramp 10`, direct
`http://127.0.0.1:8888/...` against `https://localhost:8943/live-hls/...
--ca <root.crt> --http2`. Times in ms: p50 / p95 / max.

| Path | Connections per viewer | Failed requests | Playlist | Segment | Part fan-out |
|------|------|------|------|------|------|
| **50 viewers, all full** | | | | | |
| Direct, HTTP/1.1 | 3.06 | 0 of 39,270 | 0.8 / 2.4 / 14.9 | 0.7 / 2.6 / 22.5 | 1.1 / 4.1 / 35.7 |
| Caddy, HTTP/2 | 1.00 | 0 of 36,638 | 18.3 / 76.5 / 359.3 | 39.2 / 307.7 / 592.8 | 212.5 / 492.1 / 780.6 |
| Caddy, HTTP/1.1 over TLS | 2.92 | 0 of 38,302 | 12.4 / 38.0 / 194.0 | 26.1 / 70.7 / 210.7 | 189.6 / 271.3 / 650.6 |
| **250 viewers, 50 full** | | | | | |
| Direct, HTTP/1.1 | 1.40 | 0 of 119,042 | 2.1 / 6.0 / 14.7 | 1.2 / 2.9 / 7.5 | 3.9 / 10.4 / 14.8 |
| Caddy, HTTP/2, Caddy defaults | 1.00 | 6,375 of 79,554 (502) | 166.6 / 508.4 / 892.3 | 197.7 / 644.1 / 1,565.4 | 414.4 / 1,158.8 / 1,665.9 |
| Caddy, HTTP/2, upstream keepalive 1024 | 1.00 | 0 of 113,550 | 43.9 / 109.8 / 368.4 | 93.5 / 370.4 / 1,165.0 | 235.8 / 503.2 / 1,132.5 |
| **250 viewers, all full** | | | | | |
| Direct, HTTP/1.1 | 3.00 | 0 of 195,982 | 4.3 / 7.7 / 61.1 | 3.4 / 5.4 / 32.5 | 7.8 / 19.6 / 27.0 |
| Caddy, HTTP/2, Caddy defaults | 1.00 | 479 of 73,014 (502) | 231.9 / 593.1 / 1,160.7 | 557.8 / 1,024.7 / 1,916.5 | 949.8 / 1,755.9 / 2,507.9 |
| Caddy, HTTP/2, upstream keepalive 1024 | 1.00 | 0 of 56,406 | 186.6 / 545.4 / 1,104.9 | 1,093.3 / 2,056.6 / 3,017.2 | 1,244.1 / 2,282.2 / 3,160.1 |

- **Connections:** counted by the viewers and checked with `lsof` 40 s in:
  50 and 250 established connections over HTTP/2 for 50 and 250 viewers;
  152 and 750 over HTTP/1.1 direct.
- **The 502s are Caddy failing to reach sapflow, not HTTP/2.** Every one in
  Caddy's log is `dial tcp <Docker host address>:8888: connect: connection
  refused`. HTTP/1.1 over TLS failed the same way at 250 viewers (436 of
  66,947 all full; 732 of 65,173 with 50 full). With Caddy's
  `keepalive_idle_conns_per_host` raised from its documented default of 32
  to 1024 (`CADDY_PROXY_EXTRA` in `scripts/behind-caddy.sh`), one run of
  each 250 row had none. Every blocking reload holds an upstream
  connection, which suggests a Caddy in front of sapflow needs that pool
  sized for its viewers; one run each is not proof, and why the dials were
  refused (Docker Desktop's forwarding, or the macOS listen backlog of 128)
  was not isolated.
- **Not Caddy's cost.** The machine was shared: one-minute load average 9 to
  26 before the runs on 8 cores, above 30 after two of them, and Caddy ran
  behind Docker Desktop's VM and network hop. With every viewer full, the
  last row moved 3.2 GB in the run against 11 GB direct, so it is bound by
  that hop (about 0.36 Gbit/s), not by Caddy or sapflow. Part fan-out through
  Caddy sits near a part's length (200 ms) even at 50 viewers; this run does
  not explain it. Take the table as showing the client works and what a
  Docker-on-a-Mac setup cannot answer.
- **The real run needs two wired machines:** sapflow and a native Caddy on
  one, the viewers on the other, 250 and 750 viewers all full, through
  `BENCH_URL` and `BENCH_ARGS` in `scripts/bench-remote.sh`, with
  `keepalive_idle_conns_per_host` raised, a look at 502s in Caddy's log, and
  Caddy's own CPU measured (the script reports sapflow's only).

## Decision

2026-10-20, the user: **keep experimenting.** Before any production
switch: v4v's ingest gate against sapflow, Safari on an iPhone, and a test
on separate machines.
