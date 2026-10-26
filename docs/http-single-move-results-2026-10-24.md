# Hyper single-frame move comparison, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

The candidate2 `Bytes` move fast path **did not demonstrate a whole-server
performance benefit** in this eight-case fixture. Median sampled RSS was
unchanged at **170.75 MiB**. Median server CPU moved from 64.25% to 64.05%,
a 0.20 percentage-point decrease, while playlist and segment p99 medians rose
slightly. Four repetitions per arm and the observed spread do not establish
that the small CPU difference is a repeatable saving.

All **eight planned cases qualified**, with no measured or excluded request
errors. The sanitised export
preserves all eight case objects and both complete treatment summaries. Its
closed schema
and SHA-256 manifest
identify the evidence. Private logs, media, paths, addresses and process IDs
are omitted.

## Results

Values are medians of four fresh-process cases, with the full minimum to
maximum range in parentheses. CPU is server process CPU, where 100% is one
occupied core. RSS is MiB, including the inherited `peak_rss_mb` scalar name.

| Treatment | Server CPU, % | Sampled peak RSS, MiB | Playlist p99, ms | Segment p99, ms |
| --- | ---: | ---: | ---: | ---: |
| C1, `before-hyper` | 64.25 (63.2 to 64.8) | 170.75 (141.0 to 198.6) | 35.95 (35.5 to 36.2) | 23.50 (23.1 to 23.7) |
| C2, `after-hyper` | 64.05 (63.6 to 64.3) | 170.75 (166.8 to 198.5) | 36.40 (35.5 to 37.1) | 23.55 (23.1 to 24.3) |

Relative to C1's median, CPU is 0.31% lower, RSS is unchanged, and playlist
and segment p99 medians are 1.25% and 0.21% higher. Part fan-out p95 medians
are 60.45 and 61.10 ms. All values, including the low 141.0 MiB C1 case and
both high RSS cases, remain included. Ranges are observed spread, not
confidence intervals. The sampler prints CPU and RSS to 0.1 units; medians
of four may have a second decimal.

| Treatment | Measured requests | Excluded requests | Drained requests | Maximum drain, s |
| --- | ---: | ---: | ---: | ---: |
| C1 | 2,158,524 | 191,749 | 4,807 | 0.212686 |
| C2 | 2,159,095 | 189,701 | 4,660 | 0.207285 |

There were **4,317,619 measured requests** and **381,450 excluded requests**,
all error free. The **9,467** admitted requests completing after the interval
remain in the statistics. Requests are admitted by start time inside the
fixed 60-second post-ramp interval. Complete responses drain under the same
15-second header and 15-second body timeouts, with a 35-second controller
allowance. No failed case was replaced or excluded.

Received MB is decimal response-body bytes, including playlists, repeated
parts and periodic whole segments. It is not unique programme bandwidth or
maximum capacity. Mean observed part reach ranges from 746 to 750 of the
750-viewer denominator; this is not proof of 750 decoded players.

## Workload and accounting

This is a separate **C1 versus C2, Hyper-only** comparison on the Apple M4
Max with 16 logical CPUs. The four pairs ran AB, BA, AB, BA. Each case used
a fresh server, default Hyper worker policy, one RTMP publisher replaying
the same pre-encoded generated clip with copy mode, 25 seconds of cache
warm-up, a 10-second ramp and 60 seconds at 750 full-media simulated viewers.
Recording and callbacks were disabled. The fixture is 720p30 H.264 test
pattern with 48 kHz AAC sine audio, without camera input or live encoding.
Requests use plain HTTP/1.1 over IPv4 loopback; every viewer fetches parts
and periodic whole segments.

Every before/after playlist held 24 completed segments, with sequence advance
70. These probes bracket the ramp, collection and drain, so the sequence
advance does not describe only the 60-second CPU interval.

Actual sampler windows span **60.002 to 60.012 seconds**, inside the
predeclared 1.2-second tolerance. Recorded controller offsets after the
nominal ramp span 4.315 to 9.936 ms; loader-to-sampler child launch gaps span
10.004490 to 10.018602 seconds. These separate starts are retained, without
claiming exact synchronisation.

CPU and roughly one-second RSS samples cover the server process window.
They exclude the full drain tail, publisher, loader, controller, kernel and
whole-host costs. Sampled peak RSS may miss brief peaks and does not measure
lifetime peak memory or allocation rate. Existing services remained running;
per-case host load observations remain in the export.

## Identity and completion checks

C1 is the retained combined candidate from the
[earlier native HTTP comparison](native-http-performance-results-2026-10-24.md),
not that experiment's unmodified baseline. C2 adds only the move fast path
for a single `Bytes` body in `MediaBody::poll_frame`, plus its ownership
regression; the segmented path is unchanged. Both use source base
`b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e` with the C1 patch
`e30f81f9b5f2b069647a37829b09020a08348c1df91c787fa9b3f335e67e5c44`.
The C2 delta is
`afeccaf8933ef4fe659527ba825e8d856eabe70877ec75953aa2122438e719ca`.
Its observed M4 build used the matching Rust 1.94.1 toolchain, locked release
command and existing target, with source, environment, Cargo configuration
and fresh-artifact checks bound to the build receipt.

```text
C1      af8dd75d0942ca130efaba2358c8c4a6c39622e38d88b605caad8dbc115c24bc
C2      02c9fe3907e40304dc70bddc6491d7944e299ade7be724c3085dd6e72147ecaa
loader  7c56bdc0706ded332c89a515ddebea2cc2ba5156cca15470589611c2d30b6b1d
clip    e025fa9670e4d978dce7f67540f6e2d0d08b2bf27177cc05670a6ce05707120c
```

The common loader is the retained public `b2668893` executable. The frozen
eight-case entrypoint and bundle have separate identities from the earlier
16-case experiment. **No rows or summaries are pooled** with that experiment,
its own backend, the public b266 collection or SRT work.

Independent completion readback verified **32 child process groups**, the
controller group and **34 recorded PIDs**, with no owned live or zombie
processes and all three fixture ports unused. It checked **25 artefact
hashes**, **seven ARM64 input/copy identities**, the restored **21-file C1
checkout** and the archived **21-file C2 build source**. The controller's
exit 0 was observed by its supervisor; qualification also required the full
case, source, binary and cleanup checks.

Final closed case files, including each last result receipt, total at most
**15,031 bytes per case**, below the **16 MiB** limit. The retained clip
preserved its input inode and mode under private directories; no shared inode
was chmodded. Twenty-three offline regressions cover fixed grouping, actual
build and launch identities, failed and partial receipts, sampler consistency,
final log limits, cleanup, PID reuse and clip privacy. All eight
actual cases also passed the closed scalar schema.

At this readback, C1 source had been restored with fresh modification times;
the target binary remained C2 and was labelled stale for that source. Future
Cargo use must rebuild it. Source/target cleanup and remote main restoration
remain separate gates. These results do not establish own-backend, proxy,
phone, decoded-playback, energy, impaired-network, recording or SRT acceptance.
The [scorecard](performance-scorecard-2026-10-24.md) retains those wider gates.
