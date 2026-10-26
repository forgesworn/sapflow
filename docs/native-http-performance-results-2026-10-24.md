# Native HTTP fan-out comparison, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

At 750 simulated full-media viewers, **own's median sampled RSS fell 36.24%
with median CPU unchanged**. Hyper's median sampled RSS fell 9.06%, with a
wide baseline range, while its median CPU **rose 3.06%**. Playlist and segment
p99 medians increased on both backends. This is a measured trade-off, not a
blanket performance win.

All **16 planned cases passed the fixed workload and clean-request gate**.
The sanitised scalar export
preserves every case and all four treatment groups unchanged. Its
closed schema and
SHA-256 manifest
identify the export. Private logs, media, binaries, addresses and process
identifiers are not published.

## Results

Each cell is the median of four fresh-process cases, with the full minimum to
maximum range in parentheses. CPU is server process CPU, where 100% is one
fully occupied core. RSS is MiB. The inherited scalar name `peak_rss_mb` uses
MiB, because the sampler divides process RSS KiB by 1024.

| Treatment | Server CPU, % | Sampled peak RSS, MiB | Playlist p99, ms | Segment p99, ms |
| --- | ---: | ---: | ---: | ---: |
| before-hyper | 62.05 (61.8 to 63.2) | 193.15 (114.0 to 212.2) | 35.8 (33.4 to 36.0) | 23.45 (21.3 to 24.0) |
| after-hyper | 63.95 (63.7 to 65.3) | 175.65 (173.3 to 180.7) | 36.6 (35.9 to 37.5) | 24.10 (23.8 to 24.9) |
| before-own | 36.05 (35.9 to 36.2) | 26.35 (26.1 to 26.6) | 29.6 (29.4 to 31.2) | 19.70 (18.9 to 20.2) |
| after-own | 36.05 (35.6 to 36.2) | 16.80 (16.8 to 16.9) | 31.0 (29.0 to 31.7) | 21.55 (20.2 to 22.5) |

Relative changes use each backend's before median as denominator. Own saves
9.55 MiB at the median; its playlist and segment p99 medians rise 4.73% and
9.39%. Hyper saves 17.50 MiB at the median, with CPU higher by 1.90 percentage
points; its playlist and segment p99 medians rise 2.23% and 2.77%. The low
114.0 MiB Hyper baseline remains included, alongside the other three cases.
The ranges are observed spread, not confidence intervals. CPU and response
times are printed to 0.1 units; medians of four can have a second decimal.

Part fan-out p95 medians are 60.25 to 59.75 ms for Hyper and 56.25 to 57.50 ms
for own. These observations do not reverse the playlist/segment p99 result.
The export retains all response percentiles, maxima, sample counts,
part/update fan-out, connection counts and ranges rather than selecting a
favourable latency measure.

## Delivered work and completion drain

Measured requests total **8,636,940**, with zero errors. A further **764,790**
excluded requests also have zero errors. All **19,812** admitted requests
finishing after the collection interval remain in the statistics.

| Treatment | Measured requests, total | Excluded requests, total | Drained requests, total | Maximum drain, s |
| --- | ---: | ---: | ---: | ---: |
| before-hyper | 2,160,099 | 189,859 | 4,857 | 0.214068 |
| after-hyper | 2,158,191 | 191,575 | 4,854 | 0.203844 |
| before-own | 2,158,615 | 192,731 | 5,197 | 0.222409 |
| after-own | 2,160,035 | 190,625 | 4,904 | 0.218379 |

Requests are admitted by start time inside the loader's fixed 60-second
post-ramp interval. Their complete responses remain included during drain,
under the original 15-second header and 15-second body timeouts. The
controller allows 35 seconds after its nominal window for completion. No
failure, shortened interval or clipped tail was accepted as a successful case.

Received bytes are response-body bytes in decimal MB. They include playlist
bodies, repeated parts and periodic whole segments, including admitted
completions during drain. They are not unique programme bytes, wire bandwidth
or a capacity result. Mean observed part reach ranges from 746 to 749 of the
750-viewer denominator. This describes observed receivers, not proof that
750 players decoded every part.

## Frozen workload and accounting

The Apple M4 Max has 16 logical CPUs. Four treatments ran in a balanced
four-round Williams order, each occupying every position once and every
ordered adjacent treatment pair once. Each case used a fresh server and one
RTMP publisher replaying the same pre-encoded generated clip with copy mode.
The two backends retained their default worker policy; own's expected default
is 16 workers, not a directly observed thread count. Recording, callbacks and
worker overrides were disabled.

The 180.021-second fixture is generated 720p30 H.264 test pattern and mono
48 kHz AAC sine audio, targeting 2.5 Mbit/s video and 128 kbit/s audio. It is
the same verified clip as the [native ingest/cache A/B](native-server-performance-results-2026-10-24.md),
with no camera input or live encoding. After readiness, each case warms its
cache for 25 seconds, ramps all 750 viewers for 10 seconds and collects for
60 seconds. All viewers fetch parts and periodic whole segments; none is
configured as playlist-only. This is HTTP/1.1 over loopback, without a TLS
proxy or HTTP/2.

Before/after playlists retain 24 completed segments in every case, with
sequence advance 70. These probes bracket the ramp, measured window and
completion drain, so 70 is not a claim about just the 60-second CPU interval.

The sampler's actual intervals span **60.003 to 60.016 seconds**, inside the
predeclared 1.2-second tolerance. Recorded controller offsets after its nominal
ramp span 1.568 to 9.787 ms; the recorded loader-to-sampler child launch gaps
span 10.002225 to 10.015837 seconds. Loader startup and the sampler's first
reading leave an offset between their clocks. The receipt retains these
values and does not claim exact synchronisation.

CPU and roughly one-second RSS samples cover the server process's sampling
window, excluding the complete drain tail, loader, publisher, controller,
kernel memory and whole-host costs. Sampled peak RSS can miss brief peaks and
is not a lifetime maximum or allocation-rate measurement. Existing services
remain running, with load samples retained. Four repetitions do not establish
every host or future workload.

## Source, identities and independent checks

These are the same retained native ARM64 server binaries as the native A/B,
compiled with Rust 1.94.1. Before source is
`b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e`; the measured candidate patch is
`e30f81f9b5f2b069647a37829b09020a08348c1df91c787fa9b3f335e67e5c44`.
This is the combined candidate's result; it does not isolate individual code
changes. Later source changes receive no credit from this receipt.

```text
before  5feff2f114ae557f335a04b67ea87c65127af0e459277a8d45c692f82f4640b0
after   af8dd75d0942ca130efaba2358c8c4a6c39622e38d88b605caad8dbc115c24bc
loader  7c56bdc0706ded332c89a515ddebea2cc2ba5156cca15470589611c2d30b6b1d
clip    e025fa9670e4d978dce7f67540f6e2d0d08b2bf27177cc05670a6ce05707120c
```

The loader is the retained public `b2668893` executable, source SHA
`2946569e4014d07212f09be692d75b65c846effb22ca87c978473849c625cce8`.
The native branch's different loader was not rebuilt or substituted. This
pre-encoded fixture and harness have their own identity. Do not pool these
timings with the [public b266 collection](performance-results-2026-10-24.md),
which measured the unchanged public server and had its failed gate retained.
No MediaMTX comparison is made here.

Independent readback checks all **64 child process groups**, the controller
group and 66 recorded process IDs in total, with no live owned or zombie
processes and all three fixture ports unused. Seven native ARM64 checks cover
four original executable inputs and three retained executable copies, not
seven different builds. It also checks 18 artefact hashes and 17 candidate or
configuration source identities. No PIDs, addresses or port numbers enter the
export.

Final closed case sizes include each last result receipt. Their maximum is
**14,941 bytes**, below the **16 MiB per-case limit**; the offline audit checks
this after cleanup, closing the frozen runner's final-log growth gap. Clip
privacy also follows the approved storage rule: its input and retained hard
link share the same inode and original mode under a private directory. The
reader does not chmod a shared inode; a separate copy must be private.

All 16 cases passed the offline audit and closed-schema check. Thirteen
offline validator regressions cover grouping, failed/excluded requests,
partial schedules, sampler consistency, final log bounds, cleanup, full-viewer
commands, ARM64 identity and privacy; five reader regressions cover clip
hard-link/copy and parent-directory privacy. A successful controller exit
alone was not used as qualification. Source/target cleanup and remote main
restoration remain separate later gates.

This result does not establish proxy, phone, decoded-playback, WAN, energy,
recording, slow-consumer, overload, Linux or current SRT performance acceptance.
Those phases remain separate from these HTTP results. The
[scorecard](performance-scorecard-2026-10-24.md) and
[exercise](performance-exercise.md) retain the wider gates and the requirement
to assess CPU, memory, delivery and p99 together.
