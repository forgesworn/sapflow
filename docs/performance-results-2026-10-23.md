# M4 HTTP worker screening, 23 October 2026

Twelve trials, three per treatment, with 750 simulated viewers all fetching
media.  Every trial completed and the client reported zero errors across
7,055,374 requests.  These are same-host engineering screening results.

## Measurements

CPU is a percentage of one core.  RSS is MiB as reported by the existing
runner; its historical output calls this MB.  Values are medians, with the
lowest and highest run in parentheses.  Client latency statistics include
ramp-up, as explained below.

| Treatment | CPU % | Peak RSS MiB | Playlist p99 ms | Part fan-out p50 ms |
| --- | --- | --- | --- | --- |
| MediaMTX 1.21.2 | 167.8 (160.1 to 169.5) | 276.7 (274.0 to 278.2) | 33.2 (32.8 to 35.3) | 36.5 (36.0 to 37.8) |
| Sapflow, Hyper | 60.9 (60.6 to 61.1) | 250.2 (246.5 to 255.5) | 29.3 (27.8 to 30.7) | 34.8 (34.6 to 35.2) |
| Sapflow, own loop, 16 workers | 35.0 (34.9 to 36.1) | 26.4 (26.3 to 26.5) | 28.0 (26.0 to 29.1) | 26.1 (24.4 to 29.2) |
| Sapflow, own loop, 1 worker | 20.3 (19.9 to 20.5) | 25.0 (24.8 to 25.2) | 33.7 (32.8 to 34.7) | 35.8 (35.6 to 36.1) |

## Interpretation

One own-loop worker used 42% less process CPU than sixteen in this
workload.  Its CPU range did not overlap the default own loop or Hyper.
Its median playlist p99 was 33.7 ms against 28.0 ms, and median part fan-out
was 35.8 ms against 26.1 ms.  Segment p99 and part fan-out p95 were lower
with one worker.  This is a resource/latency trade-off to investigate; it does
not establish a universally preferable worker count.

The next experiment should separate the client collection window, compare
1/4/16 workers in a balanced order and validate both 250 and 750 viewers.
The proxy, recording, multi-publisher and sustained-load gates follow in
[the exercise plan](performance-exercise.md).  Defaults remain an operational
decision after those gates.

## Frozen method and limits

The measured SDK development snapshot is ahead of public main.  Its eventual
source publication is a separate delivery; running the harness on public main
produces a different source/binary receipt.

- Source: `2d20835ef246740ab42ca810df7c51fd43bf9ff6`, the native development branch, rather than the published 0.1.0 package.  The receipt retains the source diff hash.
- The fresh checkout reported a Windows Gradle wrapper line-ending difference before measurement.  It touched no media/benchmark code; the same tracked diff and binary hashes were checked at the end.
- Platform: Apple M4 Max, 16 logical CPUs, 64 GiB RAM, macOS 26.6.2.  Existing unrelated workloads, including an emulator, were left running.
- Rust 1.94.1 with the explicit aarch64-apple-darwin target; all three measured executables were verified as thin ARM64 binaries.  Server and client were built separately to avoid feature unification.
- [MediaMTX 1.21.2](https://github.com/bluenviron/mediamtx/releases/tag/v1.21.2) was the latest release returned by its upstream release API during preparation.  Its arm64 archive was verified against the upstream checksums before extraction; binary hashes are in the receipt.
- Feed: ffmpeg version 8.1.2 Copyright (c) 2000-2026 the FFmpeg developers.  Generated testsrc2 video at 1280x720/30 fps, x264 veryfast/zerolatency at 2.5 Mbit/s, one-second keyframes, and AAC at 128 kbit/s over RTMP.  No camera/phone media or input files.
- One fresh server per trial, 25-second playlist warm-up, twenty-second ingest-only CPU window, then ten-second viewer ramp and a sixty-second full-load target.  Four treatments rotated over three rounds; not all treatment positions are balanced.
- Plain loopback HTTP/1.1, no proxy/TLS, recording off.  The shell samples CPU with ps and includes its polling overhead and any short idle tail in the sampling window.  CPU percentages are useful for these paired screenings, not an isolated throughput ceiling.
- The existing Rust client collects counters and latency samples during the ramp as well as the subsequent full-load window.  The reported playlist p99 is therefore not a strictly steady-state percentile.  Its collection boundary needs fixing before percentile acceptance.
- MediaMTX emits audio as a separate rendition that these viewers do not fetch, understating its full audio/video player cost.  Part and periodic whole-segment fetches overlap, so received MB is not unique programme throughput or ordinary player bandwidth.
- Fan-out is among observed receivers; mean reach was about 696 of 750 because the aggregate includes ramp-up.  This is not proof that all viewers decoded every part.  RSS excludes kernel socket memory.  No Linux, WAN, thermal, energy or physical-phone claim follows.

## Retained evidence

[Structured receipt and every scalar run](performance-evidence/2026-10-23/results.json)
and [file hashes](performance-evidence/2026-10-23/SHA256SUMS) accompany the twelve
raw round outputs in the same directory.  No media, credentials, real endpoints
or machine paths are retained.  Task-owned scratch, downloaded tools and build
output were removed after readback.  The existing M4 checkout was returned to
its original branch; unrelated services and local Android work were preserved.
