# TS resource admission clean-content control, 24 October 2026

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

Both current-only C8 controls passed on local M1 loopback. After adding AAC and video sample admission limits, each output retained exactly the same ordered compressed-media intervals as the earlier guarded startup control. This is cohort004; earlier cohorts and failed receipts remain separate.

| Order | Requested sender/proxy budget | Retained video | Retained coded AAC | Result |
| --- | ---: | --- | --- | --- |
| 1 | 2,000 ms | Source `[0:898)`, 898 access units | Source `[1:1405)`, 1,404 frames | Passed |
| 2 | 200 ms | Source `[0:898)`, 898 access units | Source `[1:1405)`, 1,404 frames | Passed |

The fixture was 30 seconds of generated FFmpeg testsrc2 and sine, sent in plaintext without injected loss, with a 30 ms one-way proxy-delay setting and one serial job. Its reference has 900 video access units and 1,408 AAC frames. Two video tail access units, one AAC prefix frame and three AAC tail frames remain excluded. Requested budgets are configuration settings, not measured latency. Exact retained intervals do not establish complete-source delivery, decoded PCM equality or A/V synchronisation.

The frozen native ARM64 debug receiver has SHA256 `c8af8dec493deab21f9dc5f6fe081b2b882038d1452f56a7500ab4b6a22aaa35`. Its successful Cargo build receipt, `2ca918fc8a1ad6680fe72ffc0cefd2b5a1f03e4c3bf455fce86cf6eb8d54ff6f`, binds 160 source/input files. A separate subsequent validation covers the same implementation through the full 304 regular checkout inputs and five explicit licence aliases: 868 workspace tests passed, five were ignored, and Clippy and formatting exited zero. That validation receipt is `fbd5a25f866f6b7203f3c1d00b18f3a0c29f35620ac4ed1d6675786ed1609c7f`. The two input scopes have distinct receipts.

The VCL and AAC checks compare each entire retained ordered payload vector at a unique source offset, after checking the full source vectors against frozen references. Both outputs also had positive decoded audio/video and no feeder, probe or decoder error lines. The content comparison receipt is `0a7da1047965bf3d4aeae6312ae72bc8eb9f583e02282359d37cf2359e015db4`; the frozen controller manifest is `73cdd4ef387adc428cfac31102b0252ae7c8a9849a43671785b8875bfecabc8e`.

The controller and all eight command leaders exited zero. Independent completion readback found all 90 recorded process identities and eight groups absent, all ten fixture ports free, and inputs unchanged. Both trials' actual work directories were checked in their owned process arguments. The aggregate work sidecar contains only the latest trial, so an initial reader that expected two entries failed. That failed reader receipt remains retained; the corrected independent reader checks both per-trial observations without rerunning media. It rehashed 103 retained files totalling 81,625,908 bytes before adding its proof, whose SHA256 is `4409759a7f0c292298a49d38aabe7cccd90ceb24da2fcc4881ddfc8ff4b8a4c8`.

Separate checks for C8 completed both 60-second TS/muxer fuzz runs without findings and passed own and Hyper end-to-end smokes. The smoke reader retained initial port-bind rejections and eventually verified exclusive availability within its declared deadline; it does not attribute those rejections to a particular cause. These are correctness checks. This cohort contains no CPU, RSS, energy, encrypted-network, loss-recovery, phone or comparative performance result. The global HLS residency budget described in [the resource contract](retained-media-contract.md) remains unimplemented.
