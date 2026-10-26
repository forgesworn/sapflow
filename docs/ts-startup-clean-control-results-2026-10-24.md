# TS startup clean controls, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Both separate four-case clean-control cohorts passed their frozen gates. The
combined binaries containing startup candidates **v3 and v4 each retained 898
compressed video access units matching source `0:898)`**. Old C3 retained
868 matching `[30:898)`. A subsequent, separate AAC payload analysis also
proved the retained compressed-audio ranges below. These are narrow content
results for generated media. **They do not establish current C6 readiness.**

The [scalar export
retains all eight rows and both build identities separately. Its
closed schema
and SHA-256 manifest
identify the evidence. Generated media, raw logs, commands, local paths and
process identities remain private.

Each cohort used fresh servers in ABBA order: old/new at requested 2000 ms,
then new/old at requested 200 ms. The generated source contains **900 video
AUs and 1408 AAC frames**. All ranges below use zero-based indices, with the
start included and the end excluded.

| Cohort | Case | Server | Requested sender/proxy budget, ms | Video AUs and exact source range | AAC frames and exact source range |
| --- | ---: | --- | ---: | --- | --- |
| v3 control | 1 | Old C3 | 2000 | 868, `[30:898)` | 1356, `[49:1405)` |
| v3 control | 2 | Combined v3 | 2000 | 898, `[0:898)` | 1404, `[1:1405)` |
| v3 control | 3 | Combined v3 | 200 | 898, `[0:898)` | 1404, `[1:1405)` |
| v3 control | 4 | Old C3 | 200 | 868, `[30:898)` | 1356, `[49:1405)` |
| v4 control | 1 | Old C3 | 2000 | 868, `[30:898)` | 1356, `[49:1405)` |
| v4 control | 2 | Combined v4 | 2000 | 898, `[0:898)` | 1404, `[1:1405)` |
| v4 control | 3 | Combined v4 | 200 | 898, `[0:898)` | 1404, `[1:1405)` |
| v4 control | 4 | Old C3 | 200 | 868, `[30:898)` | 1356, `[49:1405)` |

All retained ranges matched uniquely and in order. The new outputs still
omit **two source video AUs, one prefix AAC frame and three tail AAC frames**.
The cause of the video tail omission and the timing reasons for the AAC
omissions remain unresolved. Do not describe either newer output as complete
900-frame or complete-source A/V delivery.

The video check groups type-1/type-5 VCL NALs by `first_mb_in_slice`, with
eight slices per AU. It excludes Annex-B prefixes/padding and non-VCL metadata
such as AUD, SPS, PPS and SEI. Each AU hash includes every original NAL,
preceded by its four-byte big-endian length. Emulation-prevention bytes remain
in the hashed NALs; RBSP unescaping is used only to parse the slice position.
Before either cohort ran, this normaliser reproduced the original source
900-AU and retained old-output 868-AU hash vectors byte-for-byte. This method
is scoped to the pinned generated progressive H.264 fixture.

The later AAC check used the same pinned FFmpeg executable to copy compressed
audio into ADTS without decoding or encoding. It verified frame boundaries,
seven-byte headers and one raw AAC block per frame, removed the headers, then
compared the entire ordered payload-hash vector at every possible source
offset. All eight matches were unique. The earlier framehash check did not
establish equality; this method explicitly normalises transport framing.
No specific cause for the earlier result is asserted. Compressed-payload
equality does **not** establish decoded PCM equality, A/V synchronisation,
audio placement in parts, or adverse-startup acceptance.

The clean trial gate required completed feeders, successful probes and decoder
exits, positive A/V counts, and zero feeder, probe and decoder error-log lines.
All eight cases met those checks and the exact VCL range gate; the subsequent
AAC proof is separate from that original gate. Each case observed 8460 proxy
original packets and zero retransmissions, NAKs or reordered originals.
Proxy `in_time` remains an admission diagnostic, not a receiver delivery deadline.
Positive counts or clean decoding alone are not complete-quality evidence.

Subsequent review of **v4** identified valid-input regressions in delayed-AAC
budget accounting and audio ordering. These clean controls do not clear those
regressions. Separate v5 work is not represented by either cohort. No current
C6 readiness, AAC ordering acceptance, CPU, RSS, measured latency, energy,
handset or production result is claimed here.

The workload was a pre-encoded, generated 30-second 1280 by 720, 30 fps
H.264/AAC source, with 2.5 Mbit/s video and 128 kbit/s, 48 kHz audio targets.
FFmpeg copied/remuxed that exact source into the black-box SRT CLI's verified
UDP input, configured for at most 1316 bytes per datagram. The trials were
plaintext, with no loss and a fixed proxy setting of 30 ms each way. The
2000/200 ms values are requested sender/proxy settings, not measured latency
or independently retained negotiated-delay readbacks. The server's native
ingest, packaging and recording path produced fragmented MP4. These controls
do not isolate a transport-only or C6-only effect.

Both newer native ARM64 **debug** binaries combine boxed crypto keys, the
recorder quiescence/resource-limit changes, their respective startup candidate
and the own-HTTP default. The build receipts identify dirty source overlays;
the common checkout revision alone is not their source identity. The export
retains build/log hashes and selected source digests, with the differing
receipt coverage counts. FFmpeg/FFprobe were **8.0.1**, the SRT CLI **1.5.4**
and Python **3.14.2**; exact tool and helper hashes are retained.

| Frozen identity | SHA-256 |
| --- | --- |
| Old C3 debug binary | `c5fdfca5418c40e9e691685fab2a7b255c049870fd11689295d48b8499b72678` |
| Combined v3 debug binary | `8f1497470089830b6c4e9a96b661a7e44cb2495c4589cad63f53be6a30fab7c3` |
| Combined v4 debug binary | `0bf48db287a1181281a4a4ae78a0862594e4f4ecf1e6b9cc80fcf3f9ff438954` |
| Generated source TS | `d81d9a2f481b38d4d6628ad10b29068b8eb52c64e2930677e1ab026531714c9d` |
| Common loss proxy | `9e3d100426a580274afe9f1628962b16636ac9c0ec4f35b4a2df10620b8c0113` |

Independent completion checks recorded both controller exits as zero and all
ten subprocess leader exits per cohort as zero. The cohort-specific readers
checked 184 recorded PIDs/11 groups for v3 and 180/10 for v4, finding no live
recorded or group processes; all 20 fixture ports per cohort were free.
Artefact verification covered 116 files totalling **155763554 bytes** for v3
and 116 totalling **155774325 bytes** for v4, with unchanged execution inputs.
These inventory counts precede later proof/handover files.

The cohorts remain separate from each other, the
[earlier impaired M1 collection](srt-loss-results-2026-10-24.md), and completed
M4 measurements. Neither the earlier failed cells nor any historical binary
or receipt was replaced by these controls or the later AAC analysis.
