# Guarded TS startup clean-content control, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

Both final guarded clean controls passed. In each recorded MP4, all 898 retained video access units matched generated-source VCL access units `0:898)` exactly and in order; all 1,404 retained coded AAC payloads matched source `[1:1405)` exactly and in order. This is the separate current-only cohort003 on local M1 loopback. It contains no earlier C3, v3 or v4 receiver reruns or pooled results.

| Order | Requested sender/proxy budget | Ordered VCL match | Ordered coded AAC match | Result |
| --- | ---: | --- | --- | --- |
| 1 | 2,000 ms | Source `[0:898)`, 898 access units | Source `[1:1405)`, 1,404 frames | Passed |
| 2 | 200 ms | Source `[0:898)`, 898 access units | Source `[1:1405)`, 1,404 frames | Passed |

These are requested settings, not measured latency, negotiated delay or a delivery-deadline result. The fixed fixture was 30 seconds of generated FFmpeg testsrc2 and sine, sent in plaintext without an injected-loss profile, with a 30 ms one-way proxy-delay setting and one serial job. Its complete reference contains 900 video access units, eight slices per access unit, and 1,408 AAC frames. Two video tail access units, one AAC prefix frame and three AAC tail frames remain explicitly excluded. This does not establish complete-source delivery or assign an invented duration to terminal media.

The receiver was the frozen native ARM64 debug executable with SHA256 `cfea70feeed5b171f09ff675d5219d52e1d609629c15e097fb987704fa9e4ca8`. Its observed successful Rust/Cargo 1.94.1 build receipt has SHA256 `1efd38c619a02e4f83f518d8901eca313249d47392a168237a7825b7404a1a16`. The receipt binds 158 source/input files; the checkout commit alone does not identify the dirty candidate. Full ingest, muxer, startup-test, boxed-AES crypto and Cargo hashes are included in the [scalar export, together with the source-map digest and exact controller, lifecycle, proxy, helper, normaliser and tool identities.

The VCL check hashes each access unit's original type-1/type-5 NAL bytes with length framing, grouped using `first_mb_in_slice`. It excludes Annex-B framing, padding and non-VCL NALs; its scope is this pinned progressive H264 fixture. The AAC check copy-remuxes to ADTS, validates its headers and frame envelopes, strips the ADTS header and hashes each original coded payload. Both full source vectors first matched their frozen references. Each output then had to match its entire expected ordered vector at a unique source offset, with matching probe counts, positive decoded audio/video and zero recorded feeder, probe or decoder error lines. FFmpeg and FFprobe were 8.0.1, the black-box SRT tool used library 1.5.4, and Python was 3.14.2; their exact executable hashes are exported. This coded-content evidence does not establish decoded PCM equality, A/V synchronisation, arbitrary H264 normalisation or physical-device behaviour.

The controller's actual observed exit was zero. All eight command leaders exited zero, including both trial commands and the source/output extraction commands. Actual `WORK` was verified from owned process arguments for both trials and retained in the private evidence directory, rather than assumed from a requested environment value. Independent completion readback found all 91 recorded process identities and eight owned groups absent, all ten fixture ports free, and execution inputs unchanged. A separate offline pass re-read all 99 named retained files, totalling 81,557,616 bytes before the independent proof was added, and verified every recorded hash. Machine paths, process IDs, port numbers, raw logs, binaries, media and full payload-hash vectors remain private.

Separate correctness context for the same source is retained with exact receipt and log hashes in the export: 851 workspace tests passed with five ignored, including 23 startup cases; Clippy and formatting checks exited zero. Two finite fuzz runs, TS ingest and muxer timing, each requested 60 seconds, reported 61 seconds and exited zero. These are bounded checks, not comprehensive fuzz acceptance or media measurements. Both own and Hyper smoke commands also exited zero, but their outer driver exited one after two TCP bind-check failures. That failed outer receipt remains retained. A later independent cleanup readback found all 246 recorded process identities and two owned groups absent and all 21 ports exclusively bindable; it did not rerun or replace the failed outer gate.

This cohort makes no CPU, RSS, energy, resource-comparison, latency, A/V-sync, complete-source, device or general loss-recovery readiness claim. The new export is an explicit scalar whitelist with a closed schema and SHA256 manifest. Earlier reports, failed receipts and cohort001/002 evidence remain unchanged.
