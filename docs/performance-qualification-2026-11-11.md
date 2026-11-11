# Final HTTP and SRT qualification protocol

This protocol is frozen before the new campaign. Earlier shared-Mac receipts
remain in `docs/performance-evidence/2026-11-11`; they do not qualify these
comparisons. The baseline is current main
`aee2a526c26f8837266857e139b46d40591d5da2`. Each receipt identifies the exact
candidate commit, source/build inputs, executables and harness it measured.

## Environment and limits

HTTP and SRT run in separate fresh `ubuntu-24.04` GitHub-hosted jobs. Within a
job, builds are sequential in its existing `target/` and finish before timing.
Rust is pinned by `rust-toolchain.toml`. The same release settings and harness
apply to both variants. A fresh VM is not a dedicated physical machine: host
CPU, steal time and relevant load observations are retained, and failed host
guards invalidate qualification. This is Linux loopback or library evidence,
not WAN, device, production-capacity or energy acceptance.

Four pairs alternate baseline/candidate and candidate/baseline. All pilot,
calibration, successful and failed scalar rows are retained. No failed timing
row is replaced. There is one campaign per candidate/protocol revision;
follow-up requires a diagnosed source/harness defect or removal of a rejected
optimisation. Do not rerun unchanged inputs until the results happen to pass.
Workflow timeouts are 55 minutes for HTTP and 35 minutes for SRT. Raw media,
logs and temporary executables are removed after compact evidence is retained.

## HTTP

Use generated 360p30 H.264/AAC, two HTTP workers and two Tokio workers per
loader. Each trial has a five-second ramp and a 60-second measurement window:

| Profile | Publishers | Full-media viewers per publisher |
| --- | ---: | ---: |
| Single-stream control | 1 | 128 |
| Sparse multi-stream control | 16 | 2 |
| Multi-stream target | 16 | 64 |

The viewer reports exact received bytes, sample counts, unrounded mean part
reach and its real collection-window timestamps. The sampler records raw
Linux process CPU ticks, elapsed time and sampled RSS. Cross-loader window
spread and sampler boundary errors must each be at most 100 ms, and wall-clock
drift is checked. Mean host busy time must be at most 85%, and steal at most
0.5%. Every request must succeed, including excluded warm-up requests; part
reach must be at least 99%, each stream must have at least 100 playlist and
segment samples, and each paired media-byte/request ratio must be within 2%.
Completion drain is limited to one second per stream. Requests started in the
window include bounded completion drain; CPU sampling ends at the window end.
CPU per byte is therefore a bounded comparison, not exact per-request costing.

The targeted notification change is accepted only if all guards pass:

- Target CPU per received byte: paired median at most 0.90, at least three
  of four pairs at most 0.95, and every pair at most 1.05.
- Controls: CPU-per-byte median at most 1.05 and every pair at most 1.10.
  Relative CPU claims require at least 200 process ticks per arm; a low-CPU
  control instead permits at most 0.2 percentage points of one core overhead.
- RSS: paired median at most 1.05 and every pair at most 1.15.
- Latency: compare trial median and worst-stream playlist/segment p99 values;
  these are not pooled p99 values. Median paired increase must be at most the
  larger of 10% or 2 ms; every-pair increase at most the larger of 25% or 5 ms.

Failure does not qualify targeted notifications for adoption. Investigate a
specific defect or remove that complexity, preserving independently justified
playlist-cache and SRT changes. The existing conventional playlist response
model is separate from this LL-HLS workload.

## SRT

Use the public receiver API with generated plaintext packets and exact delivery
and loss-marker checks. Qualify clean-stream, pending/timer without gaps, and
bursty-stream with 4,096 missing leading sequence positions. The current example
is used against both receivers; the runner verifies all other runtime inputs
are identical before substituting only the baseline receiver source.

One fixed pilot per arm/profile selects a common fixed work count targeting
five seconds in the faster arm. Freeze all counts before the four measured
pairs. Every measured interval must be at least three seconds. Predicted slow
intervals above 120 seconds or counts above safety caps invalidate the campaign;
there is no adaptive timing retry. Isolated long-gap query timings are not merge
gates because making the cached query take seconds would make its old linear
scanner excessively expensive.

Clean-stream and pending/timer paired elapsed ratios must have median at most
1.05 and every pair at most 1.10. Burst-gap median must be at most 0.95, with
every pair at most 1.00. Steal must be at most 0.5%; measured background busy
work must be at most 10% of one CPU. Missing environmental evidence is unknown,
not a pass. All work counts, verified outputs, source/binary identities and
rows must match the declared campaign.

## Delivery

Retain the immutable protocol and compact receipts, record the final decision
in a separate results document, and review the exact accepted source. Run the
normal correctness, media and CI checks before merge. Rejecting an optimisation
is a valid outcome; no whole-server improvement is claimed solely from a
library or playlist-rendering result. Merge and source publication do not
represent deployment to an existing operator's service.
