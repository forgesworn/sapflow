# Native server comparison, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

At 50 RTMP publishers, the candidate used **20.73% less server CPU** and
**41.78% less sampled peak process RSS**, comparing the medians of four runs
per variant. All 16 trials passed this ingest/cache qualification. These are
whole-server measurements with Hyper in both variants, generated media,
**no timed viewers, recording or callbacks**.

The scalar evidence
retains every trial and range, delivery checks, source/binary identities and
private-evidence hashes. Its SHA-256 manifest
covers the export. The export was reconstructed through an explicit field
whitelist and is byte-identical to the validated scalar receipt. Logs, source
bodies, executables and generated media remain private.

## Results

Each cell is the median of four trials, with the complete minimum-to-maximum
range in parentheses. CPU is the server's average process CPU over the sampled
interval; **100% means one fully occupied CPU core**. RSS is in MiB.

| Publishers | Before CPU, % | After CPU, % | Before sampled peak RSS, MiB | After sampled peak RSS, MiB |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0.60 (0.5 to 0.6) | 0.45 (0.4 to 0.5) | 24.20 (24.0 to 25.1) | 14.85 (14.8 to 15.3) |
| 50 | 20.50 (17.8 to 20.9) | 16.25 (13.9 to 16.4) | 964.55 (955.1 to 1745.8) | 561.55 (526.5 to 836.1) |

At 50 publishers, the differences between variant medians are 4.25 CPU
percentage points and 403.00 MiB. The relative reductions use the before
median as denominator. The first 50-publisher pair has the highest RSS in
both variants, 1745.8 and 836.1 MiB. It remains included in the ranges and
medians; it was not discarded as warm-up.

At one publisher, the CPU difference is only 0.15 percentage points. The
sampler prints CPU to 0.1 percentage point, so this low-load CPU result needs
that quantisation caveat. Four pairs provide the observed ranges, rather than
a claim about every future load or machine.

## Workload and measurement

Four balanced pairs ran at each publisher count, reversing before/after order
each round with fresh server processes. The machine was an Apple M4 Max with
16 logical CPUs, 64 GiB RAM and macOS 26.6.2. Rust 1.94.1 produced thin ARM64
executables. Its compiler host was x86_64; the measured server, FFmpeg and
FFprobe executables were native ARM64.

Every publisher replayed the same clip, pre-encoded once outside measurement:
180.021 seconds of generated 1280 x 720, 30 fps H.264 test pattern and mono
48 kHz AAC sine audio. Targets were 2.5 Mbit/s video and 128 kbit/s audio,
with a 30-frame keyframe interval. Its content SHA-256 is in the receipt.
This is generated media, with no camera or phone recording input.

Both variants use RTMP over TCP and the Hyper HTTP backend. Each stream
retains 24 completed HLS segments. After readiness and at least 35 seconds of
warm-up, the sampler targets 60 seconds; actual intervals are 60.005 to
60.016 seconds, within the predeclared 1.2-second tolerance. Publisher process
and established RTMP-connection counts are checked before, during and after
sampling. The receipt retains observer CPU/wall costs separately.

Peak RSS here means the maximum of roughly one-second process RSS samples
**inside that 60-second interval**. It excludes startup/warm-up peaks and can
miss brief intervening peaks. It is not the lifetime maximum reported by
`time -l` in the separate [library/cache models](library-performance-results-2026-10-24.md).
Process RSS also excludes kernel socket memory and whole-host/encoder costs;
this experiment does not measure allocation rate or energy.

All 408 publisher checks across the 16 trials have full 24-segment caches
before and after sampling and sequence advances of 61 or 62. Those playlist
observations bracket the quiet interval and post-sampler connection check;
their span is slightly longer than the timed 60 seconds. Sampled process
and connection counts retain the requested one or 50 publishers. The first
and last publisher receive three-second audio/video decode checks after the
CPU window; those checks produce 89 or 90 video frames with required audio.
For one publisher these are the same stream. These samples qualify only those
selected streams, not every frame from every publisher.

## Source and qualification scope

The baseline is `b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e`. The candidate patch is
`e30f81f9b5f2b069647a37829b09020a08348c1df91c787fa9b3f335e67e5c44`.
The scalar export contains all 16 candidate file identities and their existing
baseline counterparts, along with harness, sampler and plan hashes. Independent
readback checks match those files and both measured executables:

```text
before  5feff2f114ae557f335a04b67ea87c65127af0e459277a8d45c692f82f4640b0
after   af8dd75d0942ca130efaba2358c8c4a6c39622e38d88b605caad8dbc115c24bc
```

All 522 retained evidence hashes were independently checked, alongside the
individual sampler outputs, source identities, clip and binaries. Final
cleanup readback checks 599 task process IDs, finds none live or reused by
unrelated work, confirms the benchmark ports are free and records zero forced
cleanup kills in every trial. No process identifiers or port numbers are in
the sanitised scalar export.

The saving belongs to this combined server candidate under RTMP ingest and
cache retention. It is not an isolated attribution to the SRT ACK estimator,
which this RTMP workload does not exercise. Recording is disabled, so the
recording byte budget receives no measurement credit here. This source is
also distinct from the unchanged public server in the [HTTP screen](performance-results-2026-10-24.md).

750-viewer fan-out, recording, current SRT loss/encryption, slow consumers,
sustained overload, phone delivery and whole-device energy retain separate
gates. This result does not establish them or a comparison against MediaMTX,
Larix or another phone broadcaster. The [scorecard](performance-scorecard-2026-10-24.md)
and [exercise](performance-exercise.md) retain that wider qualification scope.
