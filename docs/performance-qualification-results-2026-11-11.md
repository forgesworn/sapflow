# Linux qualification results

The [protocol](performance-qualification-2026-11-11.md) was committed before
measurement. Baseline: `aee2a526c26f8837266857e139b46d40591d5da2`.
Measured candidate: `1fa202bc705a577214bd865690fde22b98292126`.
Earlier shared-Mac evidence remains intact and is not substituted for these
results. This document records adoption decisions separately for each change.

## SRT receive-slot cache: accepted

[Compact receipt](performance-evidence/2026-11-11/srt-linux-qualification.json)
from [the hosted campaign](https://github.com/forgesworn/sapflow/actions/runs/42982388035).
Linux x86-64, Rust 1.94.1, unchanged release settings. Six fixed pilots selected
common counts before 24 measured rows in four balanced pairs. No row was
replaced or rerun. Source, manifest, executable and harness identities were
verified. All payload, delivery, loss, duration and host guards passed.

| Receiver workload | Median candidate/baseline elapsed time | Worst pair | Result |
| --- | ---: | ---: | --- |
| Clean stream | 1.00965 | 1.01035 | 0.97% more time; within the declared guard |
| Pending/timer, no gap | 0.88696 | 0.88854 | 11.30% less time |
| Synthetic burst with a 4,096-packet gap | 0.06952 | 0.07360 | 93.05% less time |

The shortest measured interval was 4.91 seconds. Steal time was zero throughout;
measured background work peaked at 0.0198 of one CPU (pilot maximum 0.0502).
Receiver layout grows from 776 to 784 bytes, excluding heap allocations.

This qualifies the receive-slot cache for the tested library workloads. It
does not establish WAN throughput, end-to-end stream latency, phone battery
life or behaviour under every possible loss pattern. The existing correctness
and independent old-scan differential tests remain required alongside timings.

## Targeted HTTP notifications: rejected and removed

[Compact rejected receipt](performance-evidence/2026-11-11/http-linux-qualification-rejected.json)
from the same hosted campaign. All 24 trials and 264 per-stream rows were valid;
no row was replaced or rerun. The complete campaign transferred 132,840,609,572
media bytes without measured or excluded request errors. Viewer reach was at
least 99.6875%, the smallest latency sample count was 119, and the maximum
completion drain was 205.5 ms. Host busy time ranged from 2.61% to 25.52%, with
zero steal time. Window spread, sampler boundary and clock drift guards passed.

| Live HTTP workload | Median CPU per received byte change | Worst pair change | Decision |
| --- | ---: | ---: | --- |
| One stream, 128 viewers | +2.18% | +6.25% | Control guard passed |
| 16 streams, two viewers each | +2.70% | +9.73% | Control guard passed |
| 16 streams, 64 viewers each | -0.51% | -0.22% | Benefit guard failed |

Target CPU-per-byte ratios were 0.997380, 0.979550, 0.992512 and 0.997820.
The median 0.994946 missed the predeclared 0.90 limit; none of four pairs met
0.95, where three were required. Memory and latency guards passed. This is a
valid rejection for insufficient benefit, not an invalid-host result or proof
of a large regression. The additional subscription, grouping and pending-set
complexity is removed. Existing broadcast notification behaviour is restored.
The failed receipt remains intact; thresholds were not relaxed and the same
candidate was not rerun to seek a favourable result.

## Conventional playlist cache: retained with narrow evidence

The lazy immutable playlist cache remains. The LL-HLS campaign above does not
exercise conventional playlist response construction, so it cannot qualify that
cache's benefit. The earlier [matched warm-cache model](performance-evidence/2026-11-11/playlist-cache.json)
observed 95.8% less response-construction time across eight alternating pairs
with identical response bytes and headers. Its intervals were short and the Mac
was shared, so this is only evidence for avoiding repeated construction of an
unchanged playlist, not a live-server CPU or latency percentage.

Cache correctness covers reuse, invalidation after accepted state changes,
partially mutating rejected ingest and close, plus the lifetime of previously
returned immutable bytes. Low-latency-only streams leave the lazy cache empty.
Conventional streams may retain one additional playlist outside fragment-byte
accounting. No broad HTTP throughput, memory or energy improvement is claimed.

## Final scope and verification

The accepted production changes are the conventional playlist cache and SRT
first-occupied-slot cache. HTTP targeted notification code and its exclusive
tests are removed. Reproduction tools and both accepted and rejected compact
receipts remain. The qualification workflow is manual-only after this campaign;
ordinary CI continues to test its measurement guards.

Independent review verified that all SRT runtime files, its benchmark and the
manifests are byte-identical to measured commit
`1fa202bc705a577214bd865690fde22b98292126`. Removing the HTTP experiment does
not alter the qualified SRT library workload. HTTP worker, notification and
socket handling code before `answer()`, plus deadline tests, match the baseline.

After removing the experiment, the server suite passed 287 tests with two
existing manual/environment tests ignored. All-target server Clippy, workspace
formatting and whitespace checks passed. Independent review found no blockers
in cache invalidation, response ownership or the restored notification path.
Final workspace, smoke, fuzz and platform CI must pass on this reduced diff
before merge; the pull request and main-branch checks record those outcomes.
No live deployment or new release version is part of this source change.
