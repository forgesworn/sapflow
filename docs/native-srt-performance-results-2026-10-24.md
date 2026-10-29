# Native SRT comparison, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

At **50 publishers**, cumulative candidate C3 reduced median server CPU by
**9.27% for plain SRT, 10.28% for AES-128 and 4.11% for AES-256**. Median
sampled RSS fell **42.69%, 43.00% and 42.97%**, respectively. All **24 timed
cases and four untimed encryption probes qualified**. These are process CPU
and memory results for nominal loopback ingest/HLS, with no timed viewers.

The sanitised scalar export
preserves every probe, case, stream-progress record and complete treatment
summary. Its closed schema
and SHA-256 manifest
identify the evidence. Media, raw logs, addresses, paths and process IDs
remain private.

## CPU and memory

Each value is the median of four fresh-process cases, with the full minimum
to maximum range in parentheses. CPU 100% means one occupied core. RSS is MiB.

| Mode and treatment | Server CPU, % | Sampled peak RSS, MiB |
| --- | ---: | ---: |
| Plain, before | 41.00 (40.6 to 41.5) | 1001.80 (998.3 to 1004.0) |
| Plain, C3 | 37.20 (37.1 to 37.2) | 574.10 (571.7 to 575.1) |
| AES-128, before | 42.30 (40.5 to 43.1) | 1003.05 (998.1 to 1005.1) |
| AES-128, C3 | 37.95 (37.1 to 38.9) | 571.75 (571.2 to 581.0) |
| AES-256, before | 40.10 (38.0 to 43.4) | 999.65 (996.4 to 1007.5) |
| AES-256, C3 | 38.45 (36.9 to 39.2) | 570.10 (568.0 to 576.7) |

Relative changes use the corresponding before median. AES-256 CPU ranges
overlap; four repetitions do not establish a confidence interval or every
host's behaviour. Comparisons are within each mode. The modes ran in sequence,
so the table does not isolate cipher overhead. The combined candidate also
does not isolate ACK estimation, fMP4 assembly, shared segment storage or
crypto transition costs. Allocation attribution remains separate.

## Delivered work and nominal encryption

Every timed case retained 24 completed HLS segments on all 50 streams. All
**1,200 stream checks advanced 62 segments**, above the predeclared minimum 55.
The snapshots bracket the sample plus outside-window inspection and settling
time; 62 is not a claim about exactly 60 seconds. The first and last streams in
each case decoded 87 or 88 video frames and 3.0 seconds of audio, with no decode
errors. Those 48 samples do not establish all 50 decoded playbacks.

| Mode and treatment | Full-session received packets |
| --- | ---: |
| Plain, before | 5,814,218 |
| Plain, C3 | 5,818,023 |
| AES-128, before | 5,816,009 |
| AES-128, C3 | 5,818,349 |
| AES-256, before | 5,817,415 |
| AES-256, C3 | 5,814,604 |

The 24 timed cases closed 1,200 SRT sessions with **34,898,618 received packets**
and zero observed retransmitted, lost or dropped packets. The four probes
closed with 35,660 received packets and the same zero counters. The gate
required zero dropped packets; the exporter retains actual loss and
retransmission counters rather than forcing them to zero. These counters
cover complete sessions, including setup, warm-up and decode/drain. Dividing
them by the 60-second CPU sample would give the wrong throughput or CPU-per-packet
denominator.

Each untimed AES-128/AES-256 before/after probe had at least 30 seconds of
sender exposure, decoded 88 video frames and 3.0 seconds of audio without errors,
and observed both data key slots: 4,818 packets with KK1 and 4,097 with KK2.
Each observed two standalone KM requests and two matching responses, plus
1,087 to 1,091 populated full ACKs. Actual CLI input had 9,231 UDP datagrams per
probe, each a whole number of 188-byte TS packets, from 188 through 1,316 bytes.
All four had zero bad lengths, sync faults, continuity gaps or forwarding
errors. These metadata observers ran only in untimed probes.

**4096/1024 are requested URI settings, not verified effective settings.**
All 804 encrypted sender commands requested those values. Each emitted a CLI
startup notification setting `KMPREANNOUNCE` to `0x7ff` (2047) while setting
`KMREFRESHRATE` to `0x1000` (4096). The final effective preannounce value was
not independently queried or established. The observed key/control/media
gate passed without changing the requested settings. The separate
[rapid-refresh controls and earlier failures](srt-preflight-results-2026-10-24.md)
remain separate evidence.

## Frozen workload and resource scope

The Apple M4 Max has 16 logical CPUs. Each mode ran four pairs in AB, BA, AB,
BA order. Every case used a fresh Hyper server, 50 FFmpeg copy feeders and 50
libsrt CLI callers, with generated 720p30 H.264/AAC media. Each caller's UDP
input had to be bound before its feeder started. FFmpeg sent whole TS
datagrams with `pkt_size=1316`, avoiding the earlier stdin short-read fixture.
There were no per-stream Python padders, live encoders, recording, callbacks
or HTTP requests inside the CPU window. Negotiated SRT latency was 2000 ms.

Warm-up was at least 35 seconds from last readiness; actual durations span
36.455 to 36.485 seconds. Actual sampler intervals span **60.004 to 60.018 seconds**,
inside the fixed 1.2-second tolerance. Controller spans were 60.117 to 60.338 seconds,
including launch/reap overhead; these are not identical clocks. The complete
cohort took 2669.687 seconds within the 2700-second collection budget, with no
selective retry, replacement or omitted case.

CPU covers the server process; roughly one-second RSS samples may miss brief
peaks. Sender, feeder, controller, kernel and whole-host costs are outside
those figures. The retained observer-accounting deltas span 2.740 to 3.167 CPU
seconds per sample. Existing services remained running and host-load
observations remain visible. This experiment measures healthy ingest/HLS
resource cost, not viewer p99, capacity, loss recovery, whole-host memory or
energy.

## Identity and independent checks

Before is the retained native baseline at
`b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e`. C3 includes the combined C1
candidate, single-body move and KM transition correction. Both builds use
the matching Rust 1.94.1 release configuration; the C3 archive/build receipt
binds its actual source and binary. No results are pooled with earlier HTTP,
RTMP, failed SRT fixtures or library models.

```text
before  5feff2f114ae557f335a04b67ea87c65127af0e459277a8d45c692f82f4640b0
C3      24b7fc7be864705696b5881f832a0bd7ed0d33e8343f6a4cde02c5942581ed68
SRTCLI  f23423041f219a73a1b4a9168eb2d473b0a78c5407d71c10199b212e433e5cee
clip    e025fa9670e4d978dce7f67540f6e2d0d08b2bf27177cc05670a6ce05707120c
TS      e39da6332f74c1f2d9988d4f14ac963075ed7d96abd4e99f7c4fdef7ff298b40
```

Independent completion readback checked 2,512 child process groups plus the
controller group and 2,514 recorded PIDs, with no live owned or zombie work.
All declared server/diagnostic and 50 input ports were unused. It verified 30
artefact hashes, eight ARM64 input/archive/target identities, the 24-file
restored C1 checkout and the 24-file C3 source archive. The supervisor observed
controller exit 0; the cohort also passed the independent offline audit and
closed schemas. Final case sizes include their last result receipt: the
maximum 250,086 bytes is below the 16 MiB per-case cap.

Twenty-eight offline regressions cover complete and failed cohorts, retained
counters, short samples, unattempted suffixes, source/reader identity drift,
unknown exits, process reuse, closed-file caps and public-field privacy. At
measurement readback, C1 source was restored and the target remained C3,
labelled stale for that source. Subsequent source/target cleanup and remote
main restoration have separate receipts.

Allocation, adverse-network/overload, slow-consumer, native Linux, recording,
physical-phone and energy gates remain open in the
[scorecard](performance-scorecard-2026-10-24.md).
