# Balanced M4 HTTP screening, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

All 25 planned trials and 50 measured intervals were collected. Sapflow's
20 trials recorded no measured or excluded request failures. MediaMTX's five
trials recorded three failed part requests at 750 viewers, in two trials.
The complete sweep therefore **fails the predeclared zero-request-failure
qualification**. Every failed result remains in the tables and evidence.

The scalar receipts and hashes
include all raw reports, the combined results and independent qualification.
These are same-host screening observations, with their stated quality limits.
They do not qualify new implementation candidates or a production switch.

## Method and provenance

- Apple M4 Max, 16 logical CPUs; native ARM64 Python, server, load generator
  and reference executable. Existing emulator/desktop workloads stayed
  running. The installed compiler host is x86_64; explicit
  `CARGO_BUILD_TARGET=aarch64-apple-darwin` produced thin ARM64 executables.
- Public source `b2668893d48e1e21e9c52c64f951f896d3856ef8`, including both
  post-ramp request-admission/drain fixes. Tracked source was clean. Server,
  loader and reference binary hashes are in `results.json`. MediaMTX is
  upstream `v1.21.2`, archive verified against its upstream checksum manifest
  and release-asset digest.
- Five rotating rounds of Hyper, own/default workers, own/one worker,
  own/four workers and MediaMTX. Every treatment occupied each order position
  once. Fresh servers; generated FFmpeg 720p30 H.264/AAC, 2.5 Mbit/s video,
  one-second keyframes. Recording off, loopback HTTP/1.1.
- Each trial warmed one publisher, then ran 250 and 750 simulated full-media
  viewers for 60 seconds after a ten-second ramp. Requests are admitted by
  start time; bounded completion drain preserves slow tails and late errors.
  Excluded warm-up requests/errors and drain counts remain separate.
- Independent validation checked the expected server, viewer count,
  measurement/ramp declarations, fan-out denominator, populated samples,
  positive bytes/RSS and finite measurements. The planned sampler tolerance
  was the larger of one second or 2%; the longest actual interval was
  **60.022 seconds**.

CPU is percentage of one core, so values over 100% are valid. RSS below is
**MiB**, obtained from `ps` KiB divided by 1024. The historical raw reports
label those values MB. Received MB in the loader is decimal and is a different
quantity. Process RSS excludes kernel socket memory.

## All five repetitions

Values are medians. The machine-readable results retain every minimum and
maximum, request count, error and drained completion.

| Configuration | CPU, 250 viewers | Peak RSS, 250 viewers, MiB | CPU, 750 viewers | Peak RSS, 750 viewers, MiB | Playlist p99, 750 viewers, ms | Measured request errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sapflow Hyper | 18.0% | 93.7 | 60.8% | 218.2 | 29.9 | 0 |
| Sapflow own, default 16 workers | 12.1% | 25.4 | 35.5% | 26.7 | 26.6 | 0 |
| Sapflow own, 1 worker | 9.6% | 24.9 | 20.1% | 25.7 | 32.4 | 0 |
| Sapflow own, 4 workers | 10.9% | 24.8 | 25.2% | 30.6 | 27.8 | 0 |
| MediaMTX v1.21.2 | 67.1% | 163.3 | 169.5% | 282.4 | 33.3 | 3 |

At 750 viewers, Hyper CPU ranged 59.7–62.2%, default-own 27.7–36.6%,
one-worker 20.0–21.0%, four-worker 20.4–28.0%, and MediaMTX 137.2–178.1%.
Peak RSS ranges were 200.7–278.3, 26.5–27.3, 25.2–25.8, 26.0–43.8 and
281.1–284.4 MiB respectively. Retain that four-worker memory variation when
choosing a configuration; its median alone is insufficient.

One worker used the least CPU, while its 750-viewer playlist p99 was higher
than the default own loop in every observed range: 29.5–35.0 ms versus
24.5–28.8 ms. These data expose a CPU/latency choice rather than establish
a universally best worker count. No backend or worker default was changed.

There were **18,002,316 measured requests**, three measured errors and zero
excluded-request errors. Received bytes include overlapping part and whole
segment downloads and must not be presented as unique programme throughput.

## Retained failure and continuation

The original fail-fast driver stopped after trial 13, round-three MediaMTX,
with one 750-viewer part request error. Its receipt and source remain
unchanged. The driver was verified terminal and the benchmark ports free.
The remaining twelve treatments then ran in their original order using the
same source, shell, sampler, server, loader and reference binaries. The
continuation retained the original failed qualification throughout. No failed
treatment was rerun or excluded.

Round-five MediaMTX added two errors of the same recorded kind:
`connection closed before message completed`. The available diagnostics do
not establish which side or timeout policy caused the closes. The retained
round-five server log was empty. A clean comparison requires diagnosing that
interaction; it cannot be obtained by deleting the failed trials.

The initial harness removed server/encoder diagnostic files during cleanup.
The continuation's first external watcher also missed some logs because this
host's native `mktemp -d` was verified to ignore TMPDIR. A supplemental
observer subsequently hard-linked only the task-owned Bash/FFmpeg log file
descriptors before cleanup. Missing diagnostics remain explicit in per-trial
metadata. Logs containing local endpoints/paths remain private. The observer
adds small host-side overhead during the latter collection phase; server CPU
accounting excludes its process. The collection pause and added observer
limit treatment of the whole series as an uninterrupted experiment.

## Interpretation and remaining gates

The four Sapflow configurations completed this generated HTTP workload with
no request failures. The own loop used less measured CPU and RSS than Hyper;
the worker counts show different CPU, tail-latency and RSS behaviour. The
MediaMTX request failures remain part of its observed result. This series
does not establish superiority under arbitrary networks or workloads.

The loader does not fetch MediaMTX's separate audio rendition, understating
its complete A/V viewer work. It fetches both parts and periodic complete
segments. It measures HTTP completion/fan-out among observed responses, not
per-viewer decoded quality. The shell and loader also retain a small start
offset, and the CPU window does not extend through the completion drain.

Current-source library and server candidates require their own paired M4
measurements. Recording, multiple publishers, slow consumers, SRT encryption
and impairment, RTMPS, HTTP/2/proxy, Linux, kernel resource accounting,
physical phones, energy and thermal qualification remain open. The
[scorecard](performance-scorecard-2026-10-24.md) and
[exercise](performance-exercise.md) retain those separate gates.
