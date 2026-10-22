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

## Two machines over Wi-Fi, through Caddy (2026-10-21)

**This is not the wired Linux run.** The milestone item "measured on Linux
behind Caddy: the wired 250 and 750 full-viewer run" is still open. Neither
machine had a wired link or ran Linux, so the maintainer chose to run it now
over Wi-Fi with Caddy in Docker Desktop, and to label it as such. The Wi-Fi
carried 12 full viewers at 250 viewers and none at 750, so it cannot answer
the milestone's question; it does show how the pieces behave and what the
wired run must be built to measure. **Every run here was taken under high
local load** on the server Mac (one-minute load average 34 to 373 on 8 cores;
see Setup), so read the times as what a busy shared machine gives, not as
what sapflow or Caddy do on a quiet one.

### Setup

- **Server side:** this Mac (Apple M1, 8 cores, 16 GB, macOS 14.6). sapflow
  release build of main at 10b7e40, HTTP bound to the Wi-Fi address only
  (RTMP and SRT on loopback), fed by an ffmpeg RTMP test signal on the same
  Mac (720p30, H.264 2.5 Mbit/s CBR, 1 s keyframes, AAC 128 kbit/s; the
  multivariant playlist advertises 2.7 Mbit/s). Caddy 2.11.7 in Docker
  Desktop (its VM has 2 CPUs and 3.8 GB, shared with other containers) with
  `scripts/behind-caddy.Caddyfile`: `tls internal`, `h1 h2`, the Wi-Fi
  address as the site address, sapflow at the same address as upstream, and
  `keepalive_idle_conns_per_host` raised to 1024 (checked with `caddy adapt`).
- **Viewers:** a second Mac (Apple M4 Max, 16 cores, 64 GB, macOS 26.6),
  `sapflow-bench` release build of the same commit in a throwaway worktree
  (removed afterwards), Caddy's root certificate copied next to it for
  `--ca`, `ulimit -n 65536` (an ssh session gets 256).
- **Link:** both machines on one 5 GHz access point, 802.11ac, channel 100
  (80 MHz); the radios reported transmit rates of 520 to 585 Mbit/s (MCS 6
  and 7) at -51 to -60 dBm. A single TCP stream of 600 MiB from this Mac to
  the viewer host (Python sender and sink) ran at 131 and 122 Mbit/s. That is
  what to expect when every byte crosses the air twice, but the cause was
  not isolated. The earlier two-machine run (plain HTTP) had about 72
  Mbit/s.
- **Not quiet:** this Mac carried other sessions' builds, tests and a
  fuzzer, with 20 GB of swap in use and about 0% idle CPU in the checks.
  Its one-minute load average over the final runs was 40 to 373 on 8 cores
  (and 34 to 159 over the sweeps below). The load did not ease in the half hour the
  runs were held back for it, so they went ahead. The viewer host's one-minute
  load average was 2.6 to 10.5 over the final runs (16 cores); the runner
  waited up to five minutes for it to fall below 8 before each run, and read
  `uptime` on both machines before, during and after. The viewers used 7 to
  22% of one core in the 250-viewer runs and 8 to 40% in the 750-viewer runs, so
  the load generator was not the limit. The other
  session's lab origin, Ollama and the Android device on the viewer host
  were left alone.
- **Full viewers:** the bench makes every viewer full unless `--full` is
  given; `scripts/bench-remote.sh` defaults to 10, which is what "default"
  means here and what the earlier Wi-Fi run used. The second count is the
  most the link carried without saturating, found by the sweep below.

```
# server side (this Mac)
SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=<server-ip>:28888 \
  SAPFLOW_RTMP_ADDR=127.0.0.1:28935 SAPFLOW_SRT_ADDR=127.0.0.1:28890 \
  RUST_LOG=warn target/release/sapflow
docker run -d --name sapflow-wifi-caddy -p <server-ip>:28943:28943 \
  -e CADDY_HTTPS_PORT=28943 -e CADDY_SITE=<server-ip> -e CADDY_PROTOCOLS="h1 h2" \
  -e SAPFLOW_UPSTREAM=<server-ip>:28888 -e CADDY_SITE_EXTRA= \
  -e CADDY_PROXY_EXTRA=$'transport http {\n\t\t\tkeepalive_idle_conns_per_host 1024\n\t\t}' \
  -v "$PWD/scripts/behind-caddy.Caddyfile:/etc/caddy/Caddyfile:ro" caddy:2
docker cp sapflow-wifi-caddy:/data/caddy/pki/authorities/local/root.crt caddy-root.crt
ffmpeg -re -stream_loop -1 -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -keyint_min 30 -sc_threshold 0 \
  -b:v 2500k -maxrate 2500k -bufsize 2500k -c:a aac -b:a 128k \
  -f flv rtmp://127.0.0.1:28935/live/event-wifi-b

# viewer host, over ssh, each run 10 s ramp then 60 s
sapflow-bench --url https://<server-ip>:28943/live-hls/live/event-wifi-b/index.m3u8 \
  --ca caddy-root.crt --http2 --viewers 250 --full 10 --secs 60 --ramp 10
sapflow-bench --url http://<server-ip>:28888/live/event-wifi-b/index.m3u8 \
  --viewers 250 --full 10 --secs 60 --ramp 10        # the control, no Caddy
```

- **One Caddyfile line was added** for this: `default_sni {$CADDY_SITE}`.
  A client sends no SNI for an IP-literal site, and Docker's NAT hides the
  address it dialled, so Caddy had no certificate to pick and answered
  `tlsv1 alert internal error`. It changes nothing for a hostname.
- **Sampled on this Mac every second from 10 s in until the viewers exited:**
  sapflow's CPU (`ps` time) and RSS, the Caddy container's CPU and memory
  (its cgroup's `cpu.stat` and `memory.current`), the Docker Desktop backend
  process's CPU, and the bytes the Wi-Fi interface sent (`netstat -ib`: all
  traffic on it, headers and retransmissions included). The viewers ran under
  `/usr/bin/time -l`. The bench's counts and percentiles cover the whole
  70 s run, ramp included.
- **Rounds:** three interleaved rounds (Caddy and direct alternating, so each
  pair shares conditions), so every figure below is a range, lowest to
  highest across three runs. Times in ms: p50 / p95 / max.
- A request counts as failed if the bench saw an error status, a reset or a
  timeout. Caddy's log was read for each run too.

### 250 viewers

| 250 viewers | Caddy, HTTP/2, 10 full | Direct, HTTP/1.1, 10 full | Caddy, HTTP/2, 12 full | Direct, HTTP/1.1, 12 full |
|---------|---------|---------|---------|---------|
| Failed requests | 0 of 52,934-64,868 | 0 of 93,529-101,609 | 0 of 55,482-66,775 | 0 of 100,817-102,250 |
| Caddy error lines (502s, refused dials) | 0 | n/a | 0 | n/a |
| Playlist | 179-265 / 539-676 / 1,008-1,583 | 32-46 / 117-217 / 230-2,346 | 195-231 / 538-716 / 1,458-4,564 | 36-40 / 119-150 / 434-722 |
| Segment (full viewers) | 192-330 / 711-798 / 1,054-1,558 | 74-151 / 234-846 / 459-4,913 | 252-320 / 562-860 / 1,339-4,775 | 79-189 / 291-466 / 826-1,060 |
| Part fan-out | 192-288 / 779-1,001 / 1,254-2,060 | 43-118 / 225-1,052 / 389-4,960 | 264-313 / 898-1,051 / 1,678-5,004 | 57-164 / 391-533 / 854-1,646 |
| Requests per second (mean over the run) | 756-927 | 1,336-1,452 | 793-954 | 1,440-1,461 |
| Wi-Fi out of the server, Mbit/s | 36-49 | 70-87 | 44-52 | 91-97 |
| sapflow CPU / peak memory | 3.4-3.9% / 15-19 MB | 7.0-8.1% / 19-23 MB | 3.5-3.9% / 14-22 MB | 7.7-8.0% / 20-22 MB |
| Caddy container CPU / peak memory | 92-104% / 87-194 MB | 3-5% (idle) | 93-101% / 101-184 MB | 3-5% (idle) |
| Docker Desktop backend CPU | 36-43% | 6-7% | 38-45% | 5-6% |

### 750 viewers

| 750 viewers, Caddy, HTTP/2 | 10 full | 0 full (playlists only) |
|---------|---------|---------|
| Failed requests | 0 of 70,114-110,489 | 0 of 40,256-55,868 |
| Caddy error lines | 0 | 0 |
| Playlist | 291-519 / 1,375-2,022 / 3,972-4,600 | 734-1,054 / 1,790-2,405 / 4,508-11,535 |
| Segment (full viewers) | 585-711 / 2,280-2,360 / 3,573-4,807 | none |
| Part fan-out | 396-572 / 1,670-2,631 / 4,573-6,627 | none |
| Requests per second (mean over the run) | 1,002-1,578 | 575-798 |
| Wi-Fi out of the server, Mbit/s | 31-51 | 11-17 |
| sapflow CPU / peak memory | 3.9-5.8% / 23-28 MB | 2.7-3.5% / 21-22 MB |
| Caddy container CPU / peak memory | 107-114% / 214-265 MB | 93-105% / 234-265 MB |
| Docker Desktop backend CPU | 40-56% | 29-37% |

### How many full viewers the Wi-Fi carries

Direct to sapflow over HTTP/1.1, 250 viewers, 30 s runs, one run each
(Wi-Fi out in Mbit/s; times in ms, p50 / p95 / max):

| Full viewers | Wi-Fi out | Segment | Part fan-out |
|---------|---------|---------|---------|
| 5 | 57 | 61 / 205 / 394 | 22 / 141 / 653 |
| 8 | 71 | 94 / 251 / 489 | 44 / 244 / 484 |
| 10 | 79 | 85 / 232 / 415 | 59 / 249 / 373 |
| 12 | 92 | 102 / 310 / 820 | 99 / 387 / 790 |
| 15 | 100 | 164 / 466 / 1,751 | 252 / 756 / 1,767 |
| 20 (busier period) | 76 | 191 / 524 / 2,140 | 222 / 714 / 2,056 |
| 30 (busier period) | 101 | 345 / 964 / 2,363 | 564 / 1,647 / 2,305 |

The output grew by about 5 Mbit/s per added full viewer up to 12, then
flattened at about 100 Mbit/s, which is the link (a single TCP stream gave
122 to 131). So 12 is the most it carried without saturating, and 15 is
past the knee: the fan-out median went from 99 to 252 ms. That is the
"12 full" column above.

At 750 viewers none can be added. Direct, 30 s, one run each: with no full
viewer at all the Wi-Fi already sent 102 Mbit/s, just for the blocking
playlist reloads (playlist 53 / 229 / 3,044), and with 4, 6 and 8 full it
sent 104, 88 and 96, with segment medians of 422, 333 and 427 ms and p95s of
1.6 to 3.8 s. No request failed in any of them. The "10 full" column in the
750 table is therefore already past what the link carries, and the Caddy
runs were not link-limited anyway (below).

### What it shows

- **Not one failed request, and no 502.** 45 runs, 2,855,441 requests, 0
  failed, 0 error lines in Caddy's log. That includes one run each at 250
  and 750 viewers with `keepalive_idle_conns_per_host` left at its default
  (0 of 29,978 and 0 of 23,686). Those are not a test of the earlier 502s: at
  these request rates the viewers never put the pool under that pressure.
  They were slower, though: 21 Mbit/s and 428 requests a second at 250
  viewers (playlist median 513 ms) and 10 Mbit/s and 338 a second at 750
  (median 2,055 ms), against 36-49 Mbit/s and 756-927 a second at 250 and
  31-51 Mbit/s and 1,002-1,578 a second at 750 with the pool at 1,024, with
  the server host's load average at 103 to 132 for the default-pool runs,
  inside the range of the others. One run each, so a hint that the pool
  size matters for throughput as well as for 502s, not a finding.
- **The Wi-Fi limits direct runs; Caddy limits the runs through it.** Direct,
  250 viewers use the link to its ceiling at 12 full (91 to 97 Mbit/s).
  Through Caddy the same viewers got 36 to 52 Mbit/s and about 60% of the
  requests they asked for (760 to 950 a second against 1,340 to 1,460
  direct), and the Caddy container sat at 92 to 104% of a core in every
  run. At 750 viewers with no full viewer, where the direct link carried 102
  Mbit/s and 3,750 requests a second (30 s probe), Caddy passed 11 to 17
  Mbit/s and 575 to 798 requests a second, at 93 to 105% of a core.
- **Cost of the front at 250 viewers, 10 full:** playlist median 179-265
  against 32-46 ms, segment median 192-330 against 74-151 ms, part fan-out
  median 192-288 against 43-118 ms. At 750 viewers the playlist median was
  290 to 1,050 ms. Part fan-out through Caddy sits near a part's length
  (about 234 ms), as it did on one machine.
- **Where the time goes:** into Caddy in Docker Desktop, not sapflow and not
  the radio. sapflow used 3 to 6% of a core through Caddy and 7 to 8% direct
  at 250 viewers (13 to 15% direct at 750), with 14 to 33 MB; Caddy's
  container used 20 to 40 times its through-Caddy figure, plus 29 to 56% of
  a core in Docker Desktop's backend process. Two caveats: the container's CPU is its guest
  kernel's accounting inside a 2-CPU VM on a host with no idle CPU, which can
  overstate it; and TLS with HTTP/2 is not the main part. A probe with Caddy
  proxying plain HTTP/1.1 (two interleaved runs each, 250 viewers, 10 full)
  gave Wi-Fi out of 31-36 Mbit/s, Caddy at 73-80% and playlist median
  213-272 ms, against 23-26 Mbit/s, 87-94% and 316-418 ms over TLS and
  HTTP/2 in the same rounds, so TLS and HTTP/2 may add some cost, but two
  runs under this load do not resolve it. At 750 viewers (10 full) plain
  HTTP gave 21-27 Mbit/s, Caddy at 90-96%, playlist median 775-901 ms.
- **Run-to-run spread is wide on this host,** for example direct playlist
  maximum 230 to 2,346 ms in the same cell. The medians and the Caddy-versus-
  direct gap held in every round; the maxima and the 5,000 ms tails are the
  host's and should not be read as sapflow's or Caddy's.
- **Per full viewer:** about 5 Mbit/s of Wi-Fi output each in the sweep.
  At that rate 250 full viewers need about 1.3 Gbit/s and 750 about 3.8
  Gbit/s (the one-machine run measured 3.8 Gbit/s over loopback at 750), so
  gigabit Ethernet cannot carry the 250 all-full milestone run either.

### What the wired Linux run still needs

- A Linux host for sapflow and Caddy, with Caddy native (or on the host
  network) rather than behind Docker Desktop's VM, and the viewers on a
  separate wired machine. The link has to be faster than gigabit: about 1.3
  Gbit/s for 250 full viewers and 3.8 for 750 by the figures above, so 10
  GbE, or the viewers split across several machines.
- `keepalive_idle_conns_per_host` raised, with Caddy's log read for 502s at
  the full request rate (this run could not reach it), and Caddy's own CPU
  read from the host (`pidstat` or its cgroup), not only sapflow's.
- A quiet server host with its load recorded, three or more runs per row, the
  viewer host's file-descriptor limit raised, and 60 s after a 10 s ramp as
  here so the rows compare.
- Not done here: the same run against mediamtx behind the same Caddy, and
  all 250 or 750 viewers full (the link could not carry it).

## One machine over loopback, all viewers full (2026-10-21)

The Wi-Fi run above could not carry more than 12 full viewers, so 250 and 750
viewers all fetching media were never measured across a network. This run
takes the network away: sapflow, the publisher and the viewers all ran on one
Mac over 127.0.0.1, direct HTTP/1.1, with no Caddy and no TLS. It is the
2026-10-20 one-machine test (every viewer full) repeated on a 16-core M4 with
the current main, three runs each. **It measures sapflow's own cost at full
load. It does not measure the network, Caddy, TLS or HTTP/2,** and it does not
close the milestone item for the wired Linux run behind Caddy.

### Setup

- **One machine:** Apple M4 Max, 16 cores, 64 GB, macOS 26.6, shared with
  other sessions' work (see Conditions). sapflow release build of main at
  b92e735 in a throwaway worktree (removed afterwards). The only 1.94.1 Rust
  toolchain installed on that Mac is the x86_64 build, which would have
  produced Rosetta binaries, so the build named the target and `file`
  confirmed both binaries as arm64.
- **Feed:** the same ffmpeg test signal as above (generated, 720p30, H.264
  2.5 Mbit/s CBR, 1 s keyframes, AAC 128 kbit/s) into RTMP on loopback.
- **Every run started its own sapflow and publisher,** waited 30 s (the
  playlist then held 24 segments and 12 parts, checked with a request before
  each run) and stopped both afterwards, so each peak memory is that run's
  own. Ports were free high ones, bound to 127.0.0.1 only.
- **Viewers:** `sapflow-bench` from the same build, every viewer full
  (`--full` equal to `--viewers`), 10 s ramp then 60 s, plain HTTP/1.1, one
  client per viewer (about three connections each). `ulimit -n 65536` for the
  bench (an ssh session gets 256). sapflow was started under that 256 limit
  and raised its own (`fdlimit.rs`): 2,267 to 2,271 `lsof` entries mid-run at
  750 viewers, no "too many open files" in its log.
- **Rounds:** three rounds, 250 then 750 in each, so each pair shares
  conditions. Every figure below is a range, lowest to highest across the
  three runs. Times in ms: p50 / p95 / max. The bench's counts and
  percentiles cover the whole 70 s run, ramp included; "reach" in its output
  (232 of 250, 696 or 697 of 750) is below the viewer count because viewers
  join over the first 10 s, so the average part reached about 93% of them.
- **Sampled on the Mac every second from 10 s in until the viewers exited:**
  sapflow's CPU (`ps` time) and RSS, and the bytes the loopback interface sent
  (`netstat -ib -I lo0`, one direction; it counts every process's loopback
  traffic, headers included, and the 2.7 Mbit/s feed). The viewers ran under
  `/usr/bin/time -l` for their CPU and peak memory over the whole 70 s.
- **Machine state,** read before the stack started, at the start of the
  bench, mid-run and after: `uptime`, and whole-machine CPU from `top` (a 2 s
  sample), plus a `top` snapshot of the busiest processes mid-run.
  Each run waited for the one-minute load average to fall below 8 (0 to 20 s
  was needed) and would have aborted at 12.

```
cargo build --release --target aarch64-apple-darwin -p sapflow-server -p sapflow-bench

# per run: a fresh sapflow, then the publisher, then 30 s
SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=127.0.0.1:38888 \
  SAPFLOW_RTMP_ADDR=127.0.0.1:38935 SAPFLOW_SRT_ADDR=127.0.0.1:38890 \
  RUST_LOG=warn target/aarch64-apple-darwin/release/sapflow
ffmpeg -re -stream_loop -1 -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -keyint_min 30 -sc_threshold 0 \
  -b:v 2500k -maxrate 2500k -bufsize 2500k -c:a aac -b:a 128k \
  -f flv rtmp://127.0.0.1:38935/live/event-loop

ulimit -n 65536
/usr/bin/time -l target/aarch64-apple-darwin/release/sapflow-bench \
  --url http://127.0.0.1:38888/live/event-loop/index.m3u8 \
  --viewers 250 --full 250 --secs 60 --ramp 10      # then --viewers 750 --full 750
```

### Results

| All viewers full, direct, HTTP/1.1, loopback | 250 viewers | 750 viewers |
|---------|---------|---------|
| Failed requests | 0 of 196,086-196,110 | 0 of 587,980-588,090 |
| Playlist | 4.2-4.5 / 8.6-9.3 / 19.0-22.7 | 9.6-9.8 / 19.8-21.2 / 49.6-51.7 |
| Segment | 3.1-3.5 / 6.9-7.1 / 13.6-17.3 | 5.3-5.5 / 9.3-9.7 / 36.7-50.3 |
| Part fan-out | 12.8-14.9 / 19.6-22.7 / 22.2-27.9 | 26.9-30.8 / 44.0-46.0 / 55.1-66.1 |
| Requests per second (mean over the run) | 2,801-2,802 | 8,400-8,401 |
| Loopback out, Mbit/s (60 s window) | 1,371-1,380 | 4,109-4,177 |
| Received by the viewers, MB (70 s) | 11,032-11,033 | 33,080-33,086 |
| Connections | 750-751 | 2,251-2,255 |
| sapflow CPU / peak memory | 11.5-18.6% / 42-73 MB | 54.7-62.6% / 183-234 MB |
| Viewers (bench) CPU / peak memory | 13.0-19.5% / 205-238 MB | 49.8-57.2% / 494-511 MB |

CPU is a percentage of one core; the Mac has 16.

### Conditions

| Conditions on the Mac | 250 viewers | 750 viewers |
|---------|---------|---------|
| One-minute load average: before / mid-run / after | 3.6-7.8 / 5.2-9.3 / 4.2-7.6 | 3.8-7.2 / 3.3-6.2 / 4.1-9.95 |
| Whole-machine CPU idle (2 s samples): before / mid-run / after | 74-89% / 70-86% / 76-87% | 73-93% / 79-81% / 85-87% |

- **Not a quiet machine,** but not a starved one either. The highest load
  average in any recorded reading was 9.95, so the abort threshold of 12 was
  never reached, and the machine was at least 70% idle in every sample. The
  `top` snapshots show what else was running: the window server (up to 43% of
  a core), a browser (one helper at 68% in one 250-viewer run), an agent
  app, an Android emulator, Docker's backend (20% in one 750-viewer run) and
  `kernel_task` at 18 to 30% of a core, which is not in the per-process
  figures above and which was not separated into loopback and other work.
  Swap in use was unchanged at about 3.6 GB.
- **Two shorter trial runs** (250 viewers for 15 s, 750 for 35 s) came first
  to check the driver and are not included. The 750 trial read a one-minute
  load average of 13.8 mid-run with the CPU 77% idle, which was mostly the
  trial's own load; no reported run came near it.

### What it shows

- **Not one failed request.** Six runs, 2,352,424 requests, 0 failed, and no
  error of any kind in the bench's output.
- **All full is about 5.5 Mbit/s per viewer,** 1.37 Gbit/s for 250 and
  4.1 Gbit/s for 750 over the window (3.78 Gbit/s averaged over the 70 s run,
  ramp included, close to the 3.8 Gbit/s the earlier one-machine run
  reported).
  That matches the "about 5 Mbit/s per added full viewer" from the Wi-Fi
  sweep, and it is what a network would have to carry: gigabit Ethernet is
  not enough for either count.
- **Response times stayed small.** At 750 viewers the median playlist and
  segment times were under 10 ms, the part fan-out median 27 to 31 ms and its
  maximum under 70 ms; at 250 viewers the fan-out median was 13 to 15 ms. The
  hundreds of milliseconds in the Wi-Fi tables are therefore not sapflow's
  own cost at this load, which agrees with that section's reading. What the
  link and Caddy add at full load is still unmeasured.
- **sapflow used 0.55 to 0.63 of one core at 750 full viewers** and 0.12 to
  0.19 at 250, with 183 to 234 MB and 42 to 73 MB. For three times the
  viewers CPU rose 3.4 to 4.8 times and peak memory 3.2 to 4.4 times (per round).
  The viewers used 0.50 to 0.57 of a core at 750, so the load generator
  did not saturate and neither did the Mac.
- **The CPU spread at 250 viewers is wider than the latency spread:** 11.5%
  in the first round against 18.0 and 18.6%, and the viewers' CPU moved the
  same way (13.0% against 19.5 and 19.4%). The first round also had the
  busiest machine (load 7.8 before, a browser at 68%). The cause was not
  established.
- **Against the 2026-10-20 one-machine table (the 8-core M1 Mac, macOS 14, an
  earlier commit):** the response times are of the same order (at 750 viewers:
  playlist median 10.3-13.5 then, 9.6-9.8 now; part fan-out median 34-42
  then, 27-31 now; at 250 the fan-out median was 5.7-6.3 then, 12.8-14.9
  now). **CPU and memory are higher here:** 250 viewers 10.2-11.5% and
  45-56 MB then, 11.5-18.6% and 42-73 MB now; 750 viewers 26.0-29.7% and
  66-77 MB then, 54.7-62.6% and 183-234 MB now. The measurement method is
  the same (`ps` time and RSS) and the bench's plain HTTP client has not
  changed since its first commit, so this is the Mac, macOS 26, the commits
  in between, or some mix of them; this run cannot say which. Running the
  earlier commit on this Mac would separate the code from the machine. Read
  the two tables as separate machines, not as a trend.

### What it does not show

- **The network.** Loopback has no link limit, no loss and no network card.
  Moving 1.4 and 4.1 Gbit/s needed a link faster than gigabit, as the Wi-Fi
  section worked out, and a real card's cost is not in these numbers.
- **Caddy, TLS or HTTP/2.** The production path has Caddy in front; this one
  does not, and the Wi-Fi section's measurements of Caddy's cost stand
  alone.
- **A separate viewer machine.** The viewers, the publisher and sapflow
  shared the cores and the kernel's loopback path. Nobody was starved, but
  part of the loopback work is charged to the kernel and not to either
  process, so sapflow's CPU here is the process's, not the whole cost of
  serving.
- **A quiet machine, Linux, or mediamtx.** The Mac was shared (see
  Conditions), and there is no mediamtx run of this shape.
- **A ceiling.** At 750 full viewers sapflow and the bench each used about
  half a core on 16, with the Mac mostly idle, so the most this machine could
  serve was neither reached nor looked for.
- **Real players.** Viewers are the bench's model of hls.js; one rendition,
  one stream.

## sapflow against mediamtx 1.21.2, one machine (2026-10-22)

The 2026-10-20 comparison ran on an 8-core M1 against mediamtx 1.19.1, and
the 2026-10-21 loopback run measured sapflow alone on a 16-core M4 and found
its CPU and memory at about double the M1 figures, with no mediamtx control.
This run puts both servers on the same M4 in the same hour, over loopback,
fed and watched the same way, so the two can be compared without the machine
in between. **It measures the servers' own cost on one shared Mac. It does
not measure a network, Caddy, TLS, HTTP/2, Linux or real players.**

Headline, medians of three runs, every viewer fetching media (the default of
`scripts/bench.sh`, and what the 2026-10-20 table measured):

| 250 and 750 viewers, all full | sapflow | mediamtx 1.21.2 | mediamtx / sapflow |
|---------|---------|-----------------|--------------------|
| Server CPU, 250 / 750 viewers | 17.6 / 62.0% of a core | 59.5 / 154.7% | 3.4x / 2.5x |
| Server peak memory, 250 / 750 viewers | 69 / 216 MB | 161 / 274 MB | 2.3x / 1.3x |
| Part fan-out p50, 250 / 750 viewers | 12.8 / 28.2 ms | 21.4 / 34.0 ms | 1.7x / 1.2x |
| Binary size / idle memory | 3.3 MB / 3.4 MB | 52.2 MB / 32.4 MB | 15.8x / 9.5x |

Failed requests: 0 of 3,554,083 for sapflow, 1 of 3,560,436 for mediamtx,
over twelve loaded runs each. The full tables, ranges and conditions follow.

### Setup

- **Machine:** Apple M4 Max, 16 cores, 64 GB, macOS 26.6.2 (25G83), the same
  Mac as the 2026-10-21 loopback run. Not dedicated: other sessions' builds,
  Spotlight indexing and an Android emulator ran throughout (see Machine
  state). Everything (server, publisher, viewers) over 127.0.0.1, plain
  HTTP/1.1, no Caddy and no TLS; the test traffic never left the machine.
- **sapflow:** current main, `eee669c`, release build in a throwaway
  worktree (removed afterwards), arm64 (the only 1.94.1 Rust toolchain on that
  Mac is x86_64, so the build named the target and `file` confirmed arm64).
  Built as `scripts/bench.sh` does, in two separate cargo invocations, so the
  viewers' HTTP/2 support is not unified into the server: `sapflow` is
  3,480,704 bytes, sha256 `7d4974ff71f0...5df0a`.
- **mediamtx:** v1.21.2 darwin/arm64 release binary, 54,724,098 bytes, sha256
  `72f67f718cfe5f75f081db5b2f31d1664008b6b8ff8627cb2a8acf5d7e5e61be`
  (matches the checksum recorded when it was downloaded; re-checked with
  `shasum -a 256` after copying it to the M4). Run from that run's own output
  directory, in its default session mode (cookie check, then a per-viewer session in
  every URI), as v4v's reference config runs it.
- **mediamtx configuration:** the file `scripts/bench.sh` writes, with local
  ports. mediamtx 1.21.2 rejects unknown keys (checked: with an extra line it
  exits at once with `json: unknown field`) and started on this file with
  every key unchanged, so none that `scripts/bench.sh` uses has been renamed or
  removed since 1.19.1. **One key was
  added:** `moq: no`. MoQ is new, on by default, listened on `*:8892` (TCP and
  UDP) and UDP 8893 and wrote `auto.key` and `auto.crt` into the working
  directory. With it off, `lsof` showed only the two 127.0.0.1 listeners for
  every run (recorded in each run's conditions). `scripts/bench.sh` itself
  does not set it; its mediamtx config needs the line for 1.21.
  ```
  logLevel: warn
  rtmp: yes
  rtmpAddress: 127.0.0.1:38935
  hls: yes
  hlsAddress: 127.0.0.1:38888
  hlsAlwaysRemux: yes
  hlsVariant: lowLatency
  hlsSegmentDuration: 1s
  hlsPartDuration: 200ms
  hlsSegmentCount: 24
  hlsAllowOrigins: ["*"]
  rtsp: no
  webrtc: no
  srt: no
  moq: no
  api: no
  authInternalUsers:
    - user: any
      permissions:
        - action: publish
        - action: read
  paths:
    all_others:
  ```
- **Feed:** generated test signal only (ffmpeg 8.1.2, `lavfi`), exactly the
  2026-10-20 Method: RTMP, 1280x720 at 30 fps, H.264 2.5 Mbit/s CBR, 1 s
  keyframes (`-g 30 -keyint_min 30 -sc_threshold 0`), AAC 128 kbit/s.
- **Viewers:** `sapflow-bench` from the same build as sapflow, HTTP/1.1, 10 s
  ramp then 60 s, `ulimit -n 65536` for the viewers. Both servers were started
  under the ssh session's 256 file limit and raised their own (no "too many
  open files" in either log).
- **Runs:** a fresh server and publisher for every run, 30 s of warm-up (every
  run then checked that the media playlist held 24 segments and some parts),
  both stopped afterwards, so each peak memory is that run's own. Before each
  run the driver checked that the new process held the HTTP and RTMP ports.
  The one-minute load average had to fall below 8 (up to 5 minutes' wait; it
  never needed more than 40 s) and the run would have been abandoned at 12.
  No run was abandoned or repeated: 30 of 30 matrix runs took their first
  attempt.
- **Order:** three rounds. In each round, for each row, sapflow and mediamtx
  ran back to back, 10 s apart, and the order flipped each round (sapflow
  first, mediamtx first, sapflow first), so a drift in the machine falls on
  both. Rows in a round: footprint, 250 viewers with 10 full, 750 with 10
  full, 250 all full, 750 all full. 08:38 to 09:39 local time.
- **Sampled** as in the 2026-10-21 section: server CPU from `ps` time and
  peak RSS each second over the window from 10 s into the viewers' run until
  they exited (about 60 s), the viewers under `/usr/bin/time -l`, and
  `uptime` and whole-machine CPU idle (`top`, 2 s samples) before, mid-run and
  after, plus a `top` snapshot of the busiest processes mid-run. The bench's
  counts and percentiles cover the whole 70 s, ramp included.
- **Ratio column:** the median of mediamtx's three runs divided by the median
  of sapflow's three, for each figure separately (so a ratio is not the ratio
  of the two ranges beside it). Shown only where both servers have three runs.
  Everything else is a range, lowest to highest across the three runs.
- **What "10 full" and "all full" mean.** `scripts/bench.sh` does not pass
  `--full`, and the bench makes every viewer full unless told otherwise, so
  the 2026-10-20 table was all full. 10 full is the default of
  `scripts/bench-remote.sh` (a Wi-Fi link cannot carry more). Both were run;
  compare the all-full rows with 2026-10-20.

```
# build, in a throwaway worktree of origin/main on the M4
cargo build --release --target aarch64-apple-darwin -p sapflow-server
cargo build --release --target aarch64-apple-darwin -p sapflow-bench
file target/aarch64-apple-darwin/release/sapflow     # Mach-O 64-bit executable arm64

# per run: one server (the other is not running)
SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=127.0.0.1:38888 \
  SAPFLOW_RTMP_ADDR=127.0.0.1:38935 SAPFLOW_SRT_ADDR=127.0.0.1:38890 \
  RUST_LOG=warn target/aarch64-apple-darwin/release/sapflow
mediamtx mediamtx.yml                                 # the file above

# then the publisher, the same for both
ffmpeg -re -stream_loop -1 -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -keyint_min 30 -sc_threshold 0 \
  -b:v 2500k -maxrate 2500k -bufsize 2500k -c:a aac -b:a 128k \
  -f flv rtmp://127.0.0.1:38935/live/event-loop

# footprint: sample the server's RSS and CPU for 60 s, 30 s after the publisher starts
# loaded: 10 s ramp then 60 s; --viewers 250 --full 250, 750 --full 750, 250 --full 10, 750 --full 10
ulimit -n 65536
/usr/bin/time -l sapflow-bench --url http://127.0.0.1:38888/live/event-loop/index.m3u8 \
  --viewers 250 --full 250 --secs 60 --ramp 10
```

The driver (a shell script that starts and stops the above, samples, and
writes each run's conditions) is not in the repository.

### Footprint

| Footprint | sapflow | mediamtx 1.21.2 | mediamtx / sapflow |
|---------|---------|-----------------|--------------------|
| Binary size (MB) | 3.3 | 52.2 | 15.8x |
| Idle memory (MB) | 3.3 to 3.4 | 32.3 to 32.5 | 9.5x |
| One stream, no viewers: memory at the start of the 60 s window (MB) | 23.2 to 23.3 | 61.7 to 62.2 | 2.7x |
| One stream, no viewers: memory at the end of the window (MB) | 24.0 to 24.5 | 79.6 to 88.3 | 3.5x |
| One stream, no viewers: CPU over the window (% of one core) | 0.53 to 0.56 | 1.08 to 1.34 | 2.2x |

Idle is 3 s after start. The 60 s window starts about 30 s after the
publisher connects, when the playlist window is full. mediamtx's memory was
still rising through the window; sapflow's was nearly flat. Binary size is
bytes divided by 1,048,576, as `scripts/bench.sh` computes it.

### All viewers full

| All viewers full | sapflow | mediamtx 1.21.2 | mediamtx / sapflow |
|---------|---------|-----------------|--------------------|
| **250 viewers** | | | |
| Server CPU (% of one core) | 17.4 to 18.0 | 50.4 to 60.2 | 3.4x |
| Server peak memory (MB) | 44 to 92 | 160 to 164 | 2.3x |
| Playlist p50 / p95 / p99 (ms) | 4.5-4.8 / 8.6-9.4 / 12.0-12.9 | 6.8-7.5 / 15.9-18.1 / 20.7-23.2 | 1.6x / 2.0x / 1.8x |
| Segment p50 / p95 / p99 (ms) | 3.4-3.5 / 6.6-6.7 / 8.7-9.4 | 4.6-4.8 / 7.5-7.9 / 11.1-11.6 | 1.4x / 1.1x / 1.2x |
| Part fan-out p50 / p95 / max (ms) | 12.7-12.8 / 19.7-20.6 / 23.9-25.3 | 19.8-22.0 / 27.0-29.3 / 32.9-33.9 | 1.7x / 1.4x / 1.3x |
| Playlist max / segment max (ms) | 20.2-21.5 / 14.9-34.2 | 30.1-31.3 / 21.0-23.1 | 1.5x / 1.4x |
| Failed requests, three runs together | 0 of 195,988-195,990 per run | 0 of 196,236-196,240 per run | |
| Requests per second / received Mbit/s (mean over 70 s) | 2,800 / 1,275 | 2,803 / 1,218 | |
| Connections | 752 to 753 | 1,000 | |
| Viewers (bench) CPU % / peak memory MB | 18.6 to 19.4 / 206 to 235 | 19.3 to 23.3 / 176 to 205 | |
| **750 viewers** | | | |
| Server CPU (% of one core) | 43.8 to 62.0 | 129.0 to 156.2 | 2.5x |
| Server peak memory (MB) | 89 to 230 | 272 to 276 | 1.3x |
| Playlist p50 / p95 / p99 (ms) | 8.7-9.7 / 17.6-21.0 / 21.0-27.6 | 13.5-14.3 / 26.2-26.6 / 32.3-33.5 | 1.4x / 1.3x / 1.3x |
| Segment p50 / p95 / p99 (ms) | 5.0-5.4 / 9.3-9.4 / 12.8-15.5 | 7.7-7.8 / 11.1-11.8 / 15.2-16.5 | 1.5x / 1.2x / 1.1x |
| Part fan-out p50 / p95 / max (ms) | 22.1-30.7 / 29.4-46.8 / 39.1-64.8 | 33.5-34.1 / 44.5-46.0 / 52.3-89.9 | 1.2x / 1.1x / 1.1x |
| Playlist max / segment max (ms) | 29.0-48.3 / 23.4-46.0 | 44.4-69.9 / 30.8-53.3 | 1.1x / 1.0x |
| Failed requests, three runs together | 0 of 587,842-587,860 per run | 1 of 588,618-588,798 per run | |
| Requests per second / received Mbit/s (mean over 70 s) | 8,398 / 3,824 | 8,409 to 8,411 / 3,654 to 3,655 | |
| Connections | 2,252 to 2,255 | 3,000 to 3,028 | |
| Viewers (bench) CPU % / peak memory MB | 42.9 to 57.7 / 485 to 550 | 44.4 to 53.6 / 465 to 520 | |

### 10 viewers full

Same stream and viewers, but only 10 of them fetch parts and segments; the
rest follow the playlist with blocking reloads.

| 10 viewers full, the rest follow the playlist only | sapflow | mediamtx 1.21.2 | mediamtx / sapflow |
|---------|---------|-----------------|--------------------|
| **250 viewers** | | | |
| Server CPU (% of one core) | 6.2 to 6.4 | 30.8 to 31.6 | 4.9x |
| Server peak memory (MB) | 33 to 34 | 138 to 144 | 4.3x |
| Playlist p50 / p95 / p99 (ms) | 2.4-2.6 / 5.8-6.7 / 7.4-9.1 | 6.9-7.0 / 17.5-18.0 / 21.0-21.2 | 2.8x / 2.7x / 2.5x |
| Segment p50 / p95 / p99 (ms) | 0.9-1.0 / 2.7-3.1 / 3.7-5.0 | 0.9-1.0 / 2.7-2.8 / 3.9-4.6 | 1.0x / 0.9x / 0.8x |
| Part fan-out p50 / p95 / max (ms) | 3.7-4.0 / 7.0-8.5 / 9.2-13.3 | 10.3-10.4 / 18.6-19.6 / 23.2-30.8 | 2.7x / 2.6x / 2.2x |
| Playlist max / segment max (ms) | 11.6-14.8 / 4.4-11.4 | 26.8-33.0 / 7.0-11.4 | 1.9x / 1.1x |
| Failed requests, three runs together | 0 of 102,321 per run | 0 of 102,571-102,572 per run | |
| Requests per second / received Mbit/s (mean over 70 s) | 1,462 / 92 | 1,465 / 96 | |
| Connections | 270 to 271 | 520 to 521 | |
| Viewers (bench) CPU % / peak memory MB | 7.5 to 7.8 / 33 to 34 | 8.9 to 9.1 / 44 | |
| **750 viewers** | | | |
| Server CPU (% of one core) | 12.7 to 18.9 | 64.2 to 82.4 | 4.4x |
| Server peak memory (MB) | 51 to 77 | 206 to 214 | 3.7x |
| Playlist p50 / p95 / p99 (ms) | 3.9-5.7 / 10.9-12.5 / 14.1-16.6 | 11.2-13.1 / 24.0-27.7 / 28.2-33.1 | 2.5x / 2.2x / 2.0x |
| Segment p50 / p95 / p99 (ms) | 1.3-1.8 / 4.0-5.6 / 5.5-7.1 | 0.6-1.2 / 4.7-5.3 / 5.8-6.7 | 0.6x / 0.9x / 1.0x |
| Part fan-out p50 / p95 / max (ms) | 7.3-8.0 / 13.6-15.8 / 16.6-21.4 | 15.3-16.9 / 26.7-30.6 / 30.2-90.5 | 2.2x / 1.9x / 2.0x |
| Playlist max / segment max (ms) | 22.0-24.5 / 9.1-11.9 | 35.0-94.1 / 7.5-13.8 | 2.0x / 1.2x |
| Failed requests, three runs together | 0 of 298,511-298,563 per run | 0 of 299,255-299,352 per run | |
| Requests per second / received Mbit/s (mean over 70 s) | 4,264 to 4,265 / 169 | 4,275 to 4,276 / 188 | |
| Connections | 770 | 1,520 to 1,523 | |
| Viewers (bench) CPU % / peak memory MB | 16.7 to 22.9 / 65 to 70 | 18.8 to 24.0 / 90 to 92 | |

### Machine state

| Machine state, three runs each | sapflow | mediamtx 1.21.2 |
|---------|---------|-----------------|
| **250 viewers, all full** | | |
| One-minute load average: before / mid-run / after | 3.7 to 7.5 / 4.2 to 7.2 / 5.9 to 6.5 | 4.8 to 5.9 / 2.9 to 5.8 / 4.0 to 5.7 |
| Whole-machine CPU idle %: before / mid-run / after | 88 to 91 / 76 to 86 / 78 to 88 | 91 to 92 / 66 to 82 / 76 to 89 |
| Server open files mid-run / threads | 768 to 769 / 17 | 1,030 / 45 to 47 |
| **750 viewers, all full** | | |
| One-minute load average: before / mid-run / after | 4.2 to 5.9 / 6.2 to 9.6 / 5.0 to 7.9 | 4.8 to 7.5 / 4.3 to 7.5 / 3.7 to 8.0 |
| Whole-machine CPU idle %: before / mid-run / after | 81 to 93 / 75 to 82 / 79 to 88 | 69 to 94 / 67 to 68 / 75 to 82 |
| Server open files mid-run / threads | 2,268 to 2,270 / 17 | 3,022 to 3,030 / 76 to 96 |
| **250 viewers, 10 full** | | |
| One-minute load average: before / mid-run / after | 3.3 to 7.8 / 5.0 to 6.5 / 4.9 to 7.4 | 4.6 to 6.8 / 5.9 to 9.5 / 4.7 to 11.2 |
| Whole-machine CPU idle %: before / mid-run / after | 80 to 85 / 77 to 87 / 81 to 89 | 84 to 90 / 82 to 87 / 75 to 89 |
| Server open files mid-run / threads | 287 / 17 | 550 to 551 / 24 to 29 |
| **750 viewers, 10 full** | | |
| One-minute load average: before / mid-run / after | 4.4 to 7.1 / 5.5 to 12.1 / 4.9 to 8.4 | 5.3 to 7.5 / 6.7 to 8.8 / 5.1 to 10.2 |
| Whole-machine CPU idle %: before / mid-run / after | 85 to 92 / 74 to 86 / 69 to 88 | 82 to 90 / 73 to 80 / 76 to 83 |
| Server open files mid-run / threads | 787 / 17 | 1,550 / 34 to 37 |

- **Not a quiet Mac, not a starved one.** Whole-machine CPU idle was at
  least 65% in every sample, and the one-minute load average at the start of
  every run was at most 7.8. Seen in the `top` snapshots at over 25% of a core
  and spread over both servers' runs: Spotlight (`mds`, up to 105%), another
  session's `rustc` and `clippy-driver` (about 65% each) and desktop apps (up
  to 62%); `kernel_task` was at 18 to 36% in every snapshot and is not in the
  per-process figures. The Android emulator and Docker's backend were left
  alone. Swap in use was unchanged at 3.6 GB.
- **One load reading above 12:** 12.1, mid-run, in the third sapflow run at
  750 viewers with 10 full. The same snapshot shows nothing but the stack
  itself busy (ffmpeg 28%, the viewers 15%, sapflow 12%) and the Mac 85% idle,
  so it was the stack's own run queue and not other work; the run was kept.
- **Drift between rounds is real.** At 750 viewers with 10 full, both servers
  used 20 to 30% less CPU in round 1 (sapflow 12.7% against 18.2 and 18.9;
  mediamtx 64.2% against 80.5 and 82.4) than in rounds 2 and 3. Per round,
  mediamtx / sapflow CPU there was 5.1, 4.4 and 4.4. Compare the servers
  within a round, which the interleaving allows; the absolute CPU figures move
  by that much over an hour on this Mac.
- **Memory when sapflow is busy.** At 750 all full, sapflow's first run of the
  session peaked at 89 MB and 43.8% CPU; its other eight runs at that row
  (two more of this commit here, and in the control below three of this commit
  and three of `b92e735`) were 216 to 256 MB and 60.0 to 62.0%. The 2026-10-21 run on this Mac saw 183 to 234 MB and 54.7 to 62.6%.
  So 216 to 256 MB is the usual figure here, the first run was the exception,
  and the range in the table is wide for that reason. The cause of the
  difference was not established.

### Delay through the server

Over RTMP only: no `srt-live-transmit` on the M4, and mediamtx's SRT was left
disabled. The method is `docs/interop.md`'s: `scripts/ingest-lag.py rtmp`
publishes ffmpeg's test signal in real time (720p30, 2 Mbit/s, 2 s keyframes),
polls the media playlist every 20 ms, and for each new part compares the
moment it appeared with the moment ffmpeg sent its last frame. 40 s per run,
the first 8 parts skipped, three runs each, interleaved.

| Over RTMP | Median | p90 | Parts per run |
|---------|---------|---------|---------|
| sapflow | -0.44 to -0.45 s | -0.43 s | 191-192 |
| mediamtx 1.21.2 | -0.47 s | -0.45 to -0.46 s | 181 |

- The script was copied and generalised for this, because it assumed
  sapflow's playlist name and sends no cookies: it takes the master's first
  variant as the media playlist, follows mediamtx's session redirect with a
  cookie jar, and finds the init segment by its name with mediamtx's
  `?session=` suffix. It reads mediamtx's video rendition only.
- **mediamtx's media timeline starts at 10 s** (gohlslib's fixed
  `fmp4StartDTS`, noted in `docs/reference.md`; the first part listed was at
  12.02 s in all three runs), so 10 s was subtracted from its part times. With
  that, the two servers agree to 0.02 to 0.03 s. Without it mediamtx reads
  -10.5 s. The correction rests on that constant, not on a second clock.
- Both are negative because ffmpeg's read burst at the start shifts every
  sample early by the same amount (about 0.4 s here; `docs/interop.md` saw
  about 0.3 s on another day). Read only the difference. At this method's
  resolution there is **no measurable difference in delay through the server
  over RTMP**: parts appear at about the same moment relative to the encoder
  on both. It says nothing about SRT, the player, or glass to glass.

### Does the rise from the M1 hold for mediamtx?

CPU and peak memory, all viewers full. The M1 column is the 2026-10-20 table
(8-core M1, macOS 14, mediamtx 1.19.1, sapflow of that day). The M4 columns
are today's. "Earlier M4" is the 2026-10-21 sapflow-only run at `b92e735`.

| All full | M1, 2026-10-20 | Earlier M4 run, sapflow `b92e735` | M4, today | M4 median / M1 range midpoint |
|---------|---------|---------|---------|---------|
| sapflow CPU, 250 / 750 | 10.2-11.5 / 26.0-29.7% | 11.5-18.6 / 54.7-62.6% | 17.4-18.0 / 43.8-62.0% | 1.6x / 2.2x |
| mediamtx CPU, 250 / 750 | 34.6-37.5 / 103.1-107.0% | not run | 50.4-60.2 / 129.0-156.2% | 1.7x / 1.5x |
| sapflow peak memory, 250 / 750 | 44.6-55.6 / 66.2-77.3 MB | 42-73 / 183-234 MB | 44-92 / 89-230 MB | 1.4x / 3.0x |
| mediamtx peak memory, 250 / 750 | 132.6-142.1 / 263.3-266.5 MB | not run | 160-164 / 272-276 MB | 1.2x / 1.0x |

The last column divides today's median by the middle of the M1 range, so it is
approximate; the M1 runs have no per-run values here.

**Control: is it the code or the machine?** Sapflow at `b92e735` (the commit
of the 2026-10-21 run on this Mac; its `sapflow` built the same way is
2,878,624 bytes, sha256 `7dadce180a54...cfffd`) and at `eee669c`, three runs
each at 250 and 750 viewers all full, interleaved with the order flipping each
round, the same current `sapflow-bench` for both, 09:55 to 10:20 on the same
day. Same driver, same conditions checks (no run abandoned or repeated).

| Control: sapflow at two commits, same machine, built the same way | b92e735 (the earlier loopback run) | eee669c (current main) |
|---------|---------|---------|
| **250 viewers, all full** | | |
| Server CPU (% of one core) / peak memory (MB) | 17.7 to 18.0 / 82 to 100 | 17.1 to 18.1 / 43 to 82 |
| Playlist p50 / p95 (ms) | 4.2-4.3 / 8.5-8.6 | 4.3-4.6 / 8.6-8.8 |
| Part fan-out p50 / p95 / max (ms) | 12.9-13.6 / 20.1-20.9 / 22.5-25.0 | 13.0-13.1 / 19.8-21.3 / 23.8-26.1 |
| Failed requests | 0 of 195,990 per run | 0 of 195,990 per run |
| **750 viewers, all full** | | |
| Server CPU (% of one core) / peak memory (MB) | 60.6 to 61.8 / 225 to 232 | 60.0 to 61.2 / 242 to 256 |
| Playlist p50 / p95 (ms) | 9.4-10.1 / 19.4-20.3 | 9.5-9.8 / 19.1-19.9 |
| Part fan-out p50 / p95 / max (ms) | 26.8-29.3 / 39.9-44.7 / 47.4-71.3 | 27.1-28.4 / 42.4-44.5 / 51.6-63.4 |
| Failed requests | 0 of 587,766-587,876 per run | 0 of 587,504-587,980 per run |

- **CPU: yes, mediamtx rose too.** At 250 viewers both servers needed about
  1.6x the CPU on the M4 that they did on the M1, so most of that rise is the
  machine, its OS, or (for mediamtx, which also moved from 1.19.1 to 1.21.2)
  the version, and not something about sapflow. At 750 viewers sapflow rose
  more (about 2.2x against 1.5x), so the gap to mediamtx narrowed: 3.8x on the
  M1 (105 against 28, range midpoints), 2.5x here.
- **It is not the commits since the earlier M4 run.** `b92e735`, the commit of the earlier M4 run,
  built the same way and run interleaved with current main on the same Mac,
  gave the same CPU and latency at both loads, and similar memory at 750
  viewers (225 to 232 MB against 242 to 256) but a wide spread at 250 (82 to
  100 against 43 to 82). The 42 commits between them did not change sapflow's
  CPU or latency at these loads.
- **Memory: no, only sapflow's rose.** mediamtx's peak memory is about 17%
  above its M1 figure at 250 viewers and within 3% at 750; sapflow's is about
  1.4x and 3x. Why sapflow holds more here was not established (the M4 has
  twice the cores; nothing was tried to test whether that matters).
- **What would separate machine from mediamtx version:** mediamtx 1.19.1 on
  this M4. It was not run: the only mediamtx binary available and verified was
  1.21.2. What would separate machine from OS: the same run on the M1 on
  macOS 26, or on this M4 on macOS 14. Neither was done.

### What it shows

- On the same machine, feed and viewers, sapflow used less CPU and memory
  than mediamtx at every load: in the all-full rows 3.4x and 2.5x less CPU,
  2.3x and 1.3x less peak memory (250 and 750 viewers; at 750 sapflow's first
  run peaked at 89 MB and the rest at 216 to 256, see Machine state), and a
  15.8x smaller binary, 9.5x less idle memory, 3.5x less memory and 2.2x less
  CPU with one stream and no viewers. The 2026-10-20 CPU ratio of about three
  held at 250 viewers (3.3x then, 3.4x now) and narrowed at 750 (3.8x then,
  2.5x now), from range midpoints then and medians now.
- Playlist and part fan-out medians were lower for sapflow in every row (all
  full: playlist p50 1.6x and 1.4x, part fan-out p50 1.7x and 1.2x); segment
  medians 1.4x and 1.5x lower with all viewers full, but **equal or slightly
  worse with 10 full** (segment p50 1.0x at 250, 0.6x at 750, meaning mediamtx
  was faster; only about 700 segment requests per run there). At 750 all full
  the tails were close: part fan-out p95 29 to 47 ms for sapflow against 45 to
  46 for mediamtx, and maximum part fan-out 39 to 65 ms against 52 to 90.
  sapflow's maxima were not above mediamtx's, as they were on the M1, but the
  ranges overlap.
- No failed request in 3,554,083 for sapflow; one in 3,560,436 for mediamtx
  (a part request in its first 750 all-full run: "connection closed before
  message completed"). Neither is evidence of reliability, only that nothing
  broke at these loads in these runs.
- mediamtx opened 4 connections per viewer (the cookie redirect and session)
  against sapflow's 3, and at 750 viewers ran 76 to 96 threads against
  sapflow's 17.

### What it does not show

- **The production path.** No network card, no Caddy, no TLS, no HTTP/2, no
  Linux. Safari only takes low-latency positioning over HTTP/2, and the Wi-Fi
  section found most of the time going into Caddy in Docker Desktop; none of
  that is in these numbers.
- **A fair per-viewer load on mediamtx.** The bench follows the master
  playlist's first variant. mediamtx 1.21.2 lists audio as a separate
  rendition (`audio2_stream.m3u8` and its own segments), which the bench does
  not fetch; a real player would poll both. The viewers received 4 to 5% fewer
  bytes from mediamtx (about the audio share, 128 of 2,628 kbit/s) and made no
  audio playlist requests. mediamtx's playlist work per real viewer is
  therefore understated here, and sapflow's, which muxes audio and video into
  one playlist, is not. Whether 1.19.1 did the same in 2026-10-20 was not
  checked.
- **An impartial viewer.** `sapflow-bench` is this project's own tool and its
  model of hls.js. It was the same for both servers, but it was written
  against sapflow's behaviour first.
- **A ceiling.** At 750 viewers mediamtx used up to 1.6 cores and sapflow 0.6
  of 16, with the Mac mostly idle. Neither server's limit was reached or
  looked for.
- **The kernel's share.** `kernel_task` carried the loopback traffic; CPU here
  is the server process's, not the whole cost of serving.
- **Anything about SRT ingest, recording, glass-to-glass delay or the
  callbacks.** Only RTMP in, one stream, one rendition.
- **Small differences.** The run-to-run and round-to-round spread on this Mac
  (sapflow's 750-viewer CPU 43.8% in its first run and 62.0% in the next two;
  both servers' 10-full CPU 20 to 30% lower in round 1) is bigger than some of
  the differences above, which is why they are given as ranges.

## Decision

2026-10-20, the user: **keep experimenting.** Before any production
switch: v4v's ingest gate against sapflow, Safari on an iPhone, and a test
on separate machines.
