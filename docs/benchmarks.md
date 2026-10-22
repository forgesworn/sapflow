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

## Decision

2026-10-20, the user: **keep experimenting.** Before any production
switch: v4v's ingest gate against sapflow, Safari on an iPhone, and a test
on separate machines.
