# Profiling: where the CPU goes

Measured 2026-10-20, before RTMP moved from `rml_rtmp`
to our own code (`sapflow_hls::rtmp`); the RTMP rows describe `rml_rtmp`. The question: before replacing
`srt-protocol`/`srt-tokio` (vendored `srt-protocol` 0.4.4) or the vendored
`rml_rtmp` 0.8.0 with our own implementations, how much of the server's CPU do
they account for, compared with what each viewer costs?

The working hypothesis was that ingest is one encoder at a few Mbit/s, a whole
stream with no viewers costs 0.3 to 0.8% of one core, the protocol libraries
are a small part of that, and the per-viewer cost lives in our own HTTP and
playlist code. The short answer: true for RTMP, not true for SRT, and the
per-viewer cost is mostly kernel socket I/O under hyper, not our own code.
Details and limits below.

## Summary

CPU is process CPU time over wall time as a percentage of one core (100% = one
core busy). "Unsampled" windows ran with no profiler attached; "recording"
windows are the ones the profile rests on. Idle means a running server with no
stream and no viewers (90 s window, `ps` resolution 0.01 s, not profiled).

| Scenario | Encoder rate | CPU, unsampled windows | CPU while recording | On-CPU samples |
|----------|-------------:|-----------------------:|--------------------:|---------------:|
| Idle server | none | 0.04% | not profiled | none |
| a. RTMP, 720p, no viewers | 2.7 Mbit/s | 0.35%, 0.42% | 0.44% | 1,223 |
| a. RTMP, 1080p, no viewers | 8.3 Mbit/s | 0.53%, 0.71% | 0.56% | 1,312 |
| b. SRT, 720p, no viewers | 2.8 Mbit/s | 0.90%, 1.07% | 1.14% | 4,948 |
| b. SRT, 1080p, no viewers | 8.5 Mbit/s | 1.90% | 1.73% | 4,179 |
| c. RTMP 720p, 250 viewers | 2.7 Mbit/s | 9.98%, 10.14% | 10.45% | 10,401 |

Rates are measured from the encoder output including audio (RTMP: FLV; SRT:
MPEG-TS, which adds container overhead). Where two unsampled values are given
they are two separate runs.

| Question | Answer |
|----------|--------|
| RTMP stream, no viewers | 0.35 to 0.71% of a core (2.7 and 8.3 Mbit/s). Inside the hypothesised range. |
| SRT stream, no viewers | 0.90 to 1.9% of a core (2.8 and 8.5 Mbit/s). Above the hypothesised range, 2.6 times the RTMP stream at the same video rate, and it rose somewhat faster with bitrate (within the noise of these windows). |
| `rml_rtmp` + `rml_amf0` | 11% of an RTMP stream's CPU, 0.047% of a core at 720p (0.061% at 1080p). |
| `srt-protocol` + `srt-tokio` | About half of an SRT stream's CPU: 0.57% of a core at 720p, 0.84% at 1080p. About half of that again is `recvfrom`/`sendto`/`getsockname`/`kevent` calls, which a rewrite still pays. |
| One viewer | (10.45% - 0.44%) / 250 = 0.040% of a core, on loopback. |
| Where viewer CPU goes | 83% hyper/axum/HTTP serving, of which two thirds is kernel `writev`/`recv`. Our own handlers are 3.5% of the total. Playlist rendering is not per viewer. |

## Method

- **Build:** `cargo build --profile profiling -p sapflow-server -p sapflow-bench`.
  The `profiling` profile inherits `release` (thin LTO, one codegen unit,
  `panic = "unwind"`) and keeps symbols with line tables.
- **Machine:** Apple silicon, 8 cores, macOS 14. Server, encoder, load
  generator and profiler all ran on it. It was not idle: other jobs (builds,
  fuzzers, a container VM) kept the load average between 10 and 50 throughout.
  Process CPU time is mostly insensitive to that, but treat absolute figures as
  good to roughly 20%. Recording windows read higher than unsampled windows in
  four scenarios (by 0.03 to 0.47 points) and lower in one (SRT 1080p, by 0.17);
  That is taken as profiler overhead plus run-to-run noise, and the per-stream
  totals as the range across all windows.
- **Server:** `SAPFLOW_OPEN_PUBLISH=1`, HTTP on 127.0.0.1:8888, SRT on
  127.0.0.1:8890, RTMP on 127.0.0.1:1935. A fresh server for every scenario.
- **Feeds:** ffmpeg test signal, `testsrc2` 30 fps plus a sine tone, x264
  `veryfast` `zerolatency`, 1 s keyframes, CBR to the stated rate, AAC 128
  kbit/s. 1280x720 at 2500k, and 1920x1080 at 8000k to see whether ingest cost
  scales. RTMP: ffmpeg straight to the server. SRT: ffmpeg MPEG-TS piped into
  `srt-live-transmit` with `latency=2000`.
- **Viewers:** `sapflow-bench --viewers 250 --secs 60 --ramp 10`, as in
  `docs/benchmarks.md`. Recordings started about 14 s into the run, after the
  ramp. The bench reported no failed requests in any of the three runs.
- **Timing:** each scenario waited 25 s after the playlist first returned 200.
  CPU was read with `ps -o time=` before and after: one 60 s unsampled window,
  then around each profiler recording.
- **Profiler:** `xctrace` Time Profiler attached to the server pid, which
  samples only threads that are running on a CPU, 1 ms each. Two recordings per
  scenario (150 s, 120 s for the 1080p runs, 50 s with viewers); the SRT 720p
  scenario has three good ones because one recording failed inside `xctrace`
  and was repeated. Each sample is 1 ms of CPU, so 1,000 samples is about one
  second of server CPU.
- **Why not `sample`:** it was tried first (30 s windows, 1 ms interval). It
  lists every thread at every tick, including runnable threads that are not on
  a CPU, and with the load above it reported 2.6 to 3.6 times the CPU time that
  `ps` measured (RTMP 720p: 1.5% against 0.42%; 250 viewers: 27% against
  10.3%). The cause was not verified. Time Profiler's total sample time
  matches `ps` to within 3 to 8% in every scenario (92% to 97%), so shares and
  absolute figures here come from it. The `sample` shares pointed the same way
  (for RTMP 720p `rml_rtmp` came out at 8% against 11% here) and are not used.
- **Attribution:** each sample goes to the innermost frame on its stack that
  belongs to one of the areas in the tables, skipping `std`, `core`, `alloc`,
  `bytes`, `tokio` and `mio` frames. Areas are matched by source path where the
  frame has one, otherwise by symbol. Where the compiler has inlined the whole
  async state machine into tokio's task poll frame, the task's future type
  (named in the frame's generic arguments) decides. Samples left with only
  runtime frames are "tokio runtime and unattributed". Syscalls and allocator
  calls count under the code that made them and appear in their own columns.
  Inlining across crates (thin LTO) is resolved from debug line tables, but
  some samples will still land in the wrong neighbour; the size of that error
  is not measured.
- **Sampling error:** shares come from the sample counts above. For a share
  near 30% on 1,200 samples the 95% interval is about plus or minus 2.6
  percentage points; near 10% about plus or minus 1.7. Rows under 5 samples are
  not quoted. Absolute figures are share times the `ps` CPU measured during the
  same recordings.

## a. One RTMP stream, no viewers

720p, 2.7 Mbit/s, 0.44% of a core while recording (2 recordings, 1,223 samples).

| Area | Samples | Share | CPU, % of one core | of which kernel syscalls | of which allocator |
|------|--------:|------:|-------------------:|-------------------------:|-------------------:|
| srt-protocol | 3 | under 5 samples, noise | - | - | - |
| srt-tokio | 35 | 2.9% | 0.013 | 0.001 | 0.000 |
| rml_rtmp, rml_amf0 | 130 | 10.6% | 0.047 | 0.000 | 0.020 |
| TS demux (mpeg2ts, `hls::ingest`) | 11 | 0.9% | 0.004 | 0.000 | 0.001 |
| H.264/AAC/FLV (h264-reader, adts-reader, `hls::codec`, `hls::flv`) | 57 | 4.7% | 0.020 | 0.000 | 0.014 |
| fMP4, muxing, playlist (mp4-atom, `hls::fmp4/muxer/playlist`) | 306 | 25.0% | 0.110 | 0.002 | 0.017 |
| HTTP serving (hyper, axum, tower, `server::http`) | 0 | 0 | 0 | 0 | 0 |
| server glue (`rtmp.rs`, `srt.rs`, `stream.rs`, `publish.rs`, other) | 339 | 27.7% | 0.122 | 0.074 | 0.011 |
| tokio runtime and unattributed | 342 | 28.0% | 0.123 | 0.059 | 0.000 |

- `rml_rtmp` is 130 samples (95% interval 9.0 to 12.5% of samples) and 0.047% of
  a core. 56% of it is allocator (43%) or memcpy (13%).
- The biggest blocks are the tokio runtime and unattributed (`kevent` polling
  and wake-ups, 28%), fMP4 muxing (25%, mostly copying and buffer growth in
  `Muxer::rotate_part`) and the RTMP socket reads (17% of all samples are
  `recvfrom`).
- The 2.9% under `srt-tokio` is the idle SRT listener, which wakes every 100 ms
  with no connection. It costs about 0.013% of a core.

1080p, 8.3 Mbit/s, 0.56% of a core (2 recordings, 1,312 samples).

| Area | Samples | Share | CPU, % of one core | of which kernel syscalls | of which allocator |
|------|--------:|------:|-------------------:|-------------------------:|-------------------:|
| srt-protocol | 0 | 0 | 0 | 0 | 0 |
| srt-tokio | 19 | 1.4% | 0.008 | 0.003 | 0.000 |
| rml_rtmp, rml_amf0 | 144 | 11.0% | 0.061 | 0.000 | 0.017 |
| TS demux (mpeg2ts, `hls::ingest`) | 9 | 0.7% | 0.004 | 0.000 | 0.003 |
| H.264/AAC/FLV (h264-reader, adts-reader, `hls::codec`, `hls::flv`) | 66 | 5.0% | 0.028 | 0.000 | 0.007 |
| fMP4, muxing, playlist (mp4-atom, `hls::fmp4/muxer/playlist`) | 429 | 32.7% | 0.183 | 0.005 | 0.007 |
| HTTP serving (hyper, axum, tower, `server::http`) | 0 | 0 | 0 | 0 | 0 |
| server glue (`rtmp.rs`, `srt.rs`, `stream.rs`, `publish.rs`, other) | 391 | 29.8% | 0.167 | 0.109 | 0.012 |
| tokio runtime and unattributed | 254 | 19.4% | 0.108 | 0.048 | 0.000 |

- Three times the bitrate cost 1.3 times the CPU in the recording windows (1.5
  to 1.7 times in the unsampled ones). A straight line through the two points
  puts three quarters or more of the 720p cost in a fixed part (runtime,
  timers, per-message work). `rml_rtmp` stays near 11% (0.061% of a core). The
  part that grows is fMP4 muxing, now a third of the total, with 20% of all
  samples in memcpy.

## b. One SRT stream, no viewers

720p, 2.8 Mbit/s, 1.14% of a core while recording (3 recordings, 4,948 samples).

| Area | Samples | Share | CPU, % of one core | of which kernel syscalls | of which allocator |
|------|--------:|------:|-------------------:|-------------------------:|-------------------:|
| srt-protocol | 398 | 8.0% | 0.092 | 0.000 | 0.000 |
| srt-tokio | 2095 | 42.3% | 0.482 | 0.293 | 0.029 |
| rml_rtmp, rml_amf0 | 0 | 0 | 0 | 0 | 0 |
| TS demux (mpeg2ts, `hls::ingest`) | 438 | 8.9% | 0.101 | 0.000 | 0.012 |
| H.264/AAC/FLV (h264-reader, adts-reader, `hls::codec`, `hls::flv`) | 226 | 4.6% | 0.052 | 0.000 | 0.031 |
| fMP4, muxing, playlist (mp4-atom, `hls::fmp4/muxer/playlist`) | 441 | 8.9% | 0.102 | 0.003 | 0.013 |
| HTTP serving (hyper, axum, tower, `server::http`) | 0 | 0 | 0 | 0 | 0 |
| server glue (`rtmp.rs`, `srt.rs`, `stream.rs`, `publish.rs`, other) | 321 | 6.5% | 0.074 | 0.001 | 0.021 |
| tokio runtime and unattributed | 1029 | 20.8% | 0.237 | 0.108 | 0.000 |

- `srt-protocol` is 8.0% and `srt-tokio` 42.3%: together 0.57% of a core, half
  the stream. 0.29% of that is kernel calls (`recvfrom` for incoming packets,
  `sendto` for the ACKs back to the encoder, `getsockname`, `kevent`), which any
  replacement still needs. What a zero-cost library could remove is the
  remaining 0.28% of a core.
- The SRT path also runs the TS demuxer (8.9%), which RTMP does not.
- `srt-tokio` calls `UdpSocket::local_addr()` on every received packet to learn
  the IP version (`net.rs`, `parse`). That is a `getsockname` syscall per
  packet: 118 of 4,948 samples (2.4% of the stream) here, 3.1% at 1080p.

1080p, 8.5 Mbit/s, 1.73% of a core (2 recordings, 4,179 samples).

| Area | Samples | Share | CPU, % of one core | of which kernel syscalls | of which allocator |
|------|--------:|------:|-------------------:|-------------------------:|-------------------:|
| srt-protocol | 250 | 6.0% | 0.104 | 0.000 | 0.000 |
| srt-tokio | 1769 | 42.3% | 0.734 | 0.453 | 0.044 |
| rml_rtmp, rml_amf0 | 0 | 0 | 0 | 0 | 0 |
| TS demux (mpeg2ts, `hls::ingest`) | 446 | 10.7% | 0.185 | 0.000 | 0.019 |
| H.264/AAC/FLV (h264-reader, adts-reader, `hls::codec`, `hls::flv`) | 185 | 4.4% | 0.077 | 0.000 | 0.035 |
| fMP4, muxing, playlist (mp4-atom, `hls::fmp4/muxer/playlist`) | 422 | 10.1% | 0.175 | 0.004 | 0.004 |
| HTTP serving (hyper, axum, tower, `server::http`) | 0 | 0 | 0 | 0 | 0 |
| server glue (`rtmp.rs`, `srt.rs`, `stream.rs`, `publish.rs`, other) | 359 | 8.6% | 0.149 | 0.015 | 0.036 |
| tokio runtime and unattributed | 748 | 17.9% | 0.310 | 0.153 | 0.000 |

- The libraries are 48% (0.84% of a core), of which 0.45% is kernel calls and
  0.38% userland. The per-packet `getsockname` is 130 of 4,179 samples (3.1% of
  the stream).
- Three times the bitrate cost 1.5 times the CPU in the recording windows (1.8
  to 2.1 times in the unsampled ones), against 1.3 and 1.5 to 1.7 for RTMP.
  Both rise; SRT somewhat more, presumably because the per-packet work
  (receive, ACK, timers) grows with the packet rate. The windows are too noisy
  to call the difference firmly.

## c. One RTMP stream and 250 viewers

720p, 2.7 Mbit/s, 10.45% of a core while recording (2 recordings of 50 s, 10,401
samples).

| Area | Samples | Share | CPU, % of one core | of which kernel syscalls | of which allocator |
|------|--------:|------:|-------------------:|-------------------------:|-------------------:|
| srt-protocol | 0 | 0 | 0 | 0 | 0 |
| srt-tokio | 8 | 0.1% | 0.008 | 0.002 | 0.000 |
| rml_rtmp, rml_amf0 | 42 | 0.4% | 0.042 | 0.000 | 0.014 |
| TS demux (mpeg2ts, `hls::ingest`) | 2 | under 5 samples, noise | - | - | - |
| H.264/AAC/FLV (h264-reader, adts-reader, `hls::codec`, `hls::flv`) | 26 | 0.2% | 0.026 | 0.000 | 0.016 |
| fMP4, muxing, playlist (mp4-atom, `hls::fmp4/muxer/playlist`) | 103 | 1.0% | 0.103 | 0.003 | 0.010 |
| HTTP serving (hyper, axum, tower, `server::http`) | 8671 | 83.4% | 8.709 | 5.916 | 0.837 |
| server glue (`rtmp.rs`, `srt.rs`, `stream.rs`, `publish.rs`, other) | 182 | 1.7% | 0.183 | 0.063 | 0.009 |
| tokio runtime and unattributed | 1367 | 13.1% | 1.373 | 0.990 | 0.000 |

HTTP serving, by innermost frame (whole-stack ownership is the HTTP row above):

| Innermost frame in | Samples | Share of HTTP | CPU, % of one core | of which socket syscalls |
|---|--:|--:|--:|--:|
| hyper, hyper-util | 5231 | 60.3% | 5.254 | 4.284 |
| axum, tower, http crates | 1644 | 19.0% | 1.651 | 0.000 |
| connection task, outside hyper frames (request reads) | 1430 | 16.5% | 1.436 | 1.166 |
| `server::http` handlers | 366 | 4.2% | 0.368 | 0.000 |

- Socket I/O is 53% of everything: `writev` of responses alone is 41% of all
  samples and `recvfrom` of requests about 11%. Loopback probably overstates
  this: the kernel delivers to the receiving socket within the same call, work
  a real network card would offload or leave to the receiver. That was not
  measured. It is the largest single item either way.
- Userland HTTP (hyper, axum, tower, our handlers) outside syscalls is about
  2.8% of a core, 0.84% of it in the allocator. Our own handlers
  (`server::http`) are 0.37% of a core, 3.5% of the total.
- The fMP4/playlist row is 0.10%, the same as with no viewers. Media playlists
  are rendered once per update and shared; nothing about playlist rendering
  scales with viewers.
- Everything on the ingest path together (RTMP glue, `rml_rtmp`, codec, muxer,
  stream) is about 0.35% of a core, the same as without viewers within noise.
- The tokio runtime is 13% (1.37% of a core), mostly `kevent` and thread
  wake-ups.

## By kind of work

Percent of one core, with the share of samples in brackets. "Other compute and
libc" is everything that is not a syscall, allocator call or memcpy, including
protocol, codec and HTTP logic and lock operations.

| Kind of work | RTMP 720p | RTMP 1080p | SRT 720p | SRT 1080p | RTMP 720p, 250 viewers |
|---|--:|--:|--:|--:|--:|
| Socket I/O syscalls (recv, send, write, writev) | 0.073 (16.6%) | 0.106 (18.9%) | 0.235 (20.7%) | 0.354 (20.4%) | 5.513 (52.8%) |
| kqueue polling (`kevent`) | 0.054 (12.3%) | 0.047 (8.4%) | 0.116 (10.2%) | 0.156 (9.0%) | 0.597 (5.7%) |
| Other syscalls (locks, clock, `getsockname`, thread wake) | 0.010 (2.4%) | 0.012 (2.1%) | 0.054 (4.7%) | 0.114 (6.6%) | 0.866 (8.3%) |
| Memory allocator (malloc, free) | 0.062 (14.2%) | 0.046 (8.2%) | 0.107 (9.4%) | 0.139 (8.0%) | 0.887 (8.5%) |
| memcpy, memset | 0.053 (12.2%) | 0.112 (20.1%) | 0.084 (7.4%) | 0.182 (10.5%) | 0.149 (1.4%) |
| Other compute and libc | 0.186 (42.4%) | 0.236 (42.2%) | 0.543 (47.7%) | 0.788 (45.4%) | 2.435 (23.3%) |

## What this says about replacing the libraries

- **A zero-cost RTMP library** (`rml_rtmp`, `rml_amf0`) would save at most
  0.047% of a core per 720p stream, 0.061% at 1080p. One viewer costs 0.040%,
  so the ceiling is about one to one and a half viewers' worth. With 250
  viewers it is 0.5% of the server's CPU. The recv syscalls that feed it
  (17% of a no-viewer RTMP stream) stay.
- **A zero-cost SRT library** (`srt-protocol` plus `srt-tokio`) would save at
  most 0.57% of a core at 720p and 0.84% at 1080p, about 14 and 21 viewers'
  worth. Roughly half of that is syscalls, so the userland saving is nearer
  0.28% and 0.38%, about 7 and 10 viewers' worth, or 2.5 to 3.2% of the server's
  CPU with 250 viewers watching. This is where the hypothesis fails: SRT's
  libraries are half of an SRT stream's cost, not a small fraction, though the
  absolute amount is still small.
- **Per viewer**, 0.040% of a core. About half is kernel socket I/O, a quarter
  is hyper, axum and tower code outside syscalls (including the allocator), and
  13% is the tokio runtime. Rewriting our own HTTP handlers and playlist code
  would touch 3.5% of it.
- Both libraries are small in absolute terms: a whole stream plus 250 viewers is
  about 11% of one of eight cores. On CPU alone the case for replacing either
  library is weak: the ceiling is a fraction of one core per stream. This
  profile does not speak to correctness, memory use, latency under loss,
  features, or maintenance, which are other reasons one might.
- If CPU per stream ever mattered, the cheaper wins in this profile are
  elsewhere: the fMP4 muxer (25 to 33% of an RTMP stream, mostly copying), the
  per-packet `getsockname`, tokio wake-ups, and the allocator (8 to 14% of
  everything).

## Limits

- One machine, one encoder content (`testsrc2`), short windows, no packet loss.
  SRT under loss runs retransmission and NAK handling that this does not
  exercise, so its protocol share would be larger there.
- The server will probably run on Linux. Syscall costs, `epoll` against
  `kqueue`, and batched UDP calls (`recvmmsg`) differ, and could move SRT's
  syscall share in particular.
- Viewers ran on loopback with the load generator on the same machine.
- The machine was busy throughout. The two SRT 720p unsampled windows differ by
  0.17 percentage points between runs, so differences under about 0.2 points
  between scenarios are within noise.
- Attribution depends on debug line tables and symbol names (see Method).
  "Tokio runtime and unattributed" is a residue, not a measured runtime cost.
- Sample counts are in every section. Anything under about 5 samples is noise.
- The profiled binary came from the working tree at the commit above, with
  the vendored libraries as they are there. A separate RTMP rewrite on another
  branch was not profiled.

## Own RTMP against `rml_rtmp` (2026-10-20)

Same method and scenarios, after RTMP moved to `sapflow_hls::rtmp`, on a
less loaded machine (load average 10 to 24).

| Scenario | `rml_rtmp` | Own RTMP | Samples |
|---|---|---|---|
| 720p, no viewers | 0.047% of a core (10.6% of the stream) | 0.031% (8.9%) | 130 vs 91 |
| 1080p, no viewers | 0.061% (11.0%) | 0.045% (8.6%) | 144 vs 106 |
| 720p, 250 viewers | 0.042% | 0.037% | 42 vs 38 |

- A third less, 0.015% of a core per stream: under half of one viewer
  (0.040%). The no-viewer differences are about 2.5 standard deviations on
  Poisson counts; the 250-viewer one is within noise.
- Allocator time inside RTMP roughly halved; most of what is left is
  `Session::poll`.
- Whole-stream totals also came out lower (0.37% at 720p, 0.47% at 1080p,
  9.2% at 250 viewers), but by more than the RTMP saving, so most of that is
  the quieter machine, not the rewrite.
