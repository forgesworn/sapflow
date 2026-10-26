# M1 SRT loss collection, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

All **84 planned trials were attempted**: **61 passed the frozen harness gate and 23 failed**. C3 passed 42 of 42; libsrt passed 19 of 42. All 23 failures emitted decoder diagnostics, even though the decoder exited zero. This does **not establish an overall winner or source-equivalent quality**. C3 already omitted source video in the clean condition, and the receiver output formats differ.

The sanitised export retains all 84 rows, their exits, counters and failures, plus every condition cell including failed attempts. The closed schema and SHA-256 manifest identify the export. Raw generated media, logs, local paths and process identities remain private.

Both requested latency settings produced these clean packet counts:

| Output | Video packets | AAC packets | Proxy originals | Retransmissions |
| --- | ---: | ---: | ---: | ---: |
| Generated source | 900 | 1408 | Not applicable | Not applicable |
| C3 recorded fMP4 | 868 | 1356 | 8460 | 0 |
| libsrt raw TS | 900 | 1408 | 8460 | 0 |

Normalised compressed VCL analysis of the clean C3 recording proved that its **868 retained access units match source indices 30 through 897**, using zero-based inclusive indices. It omitted **30 prefix access units and two tail access units**. The comparison grouped normalised VCL payloads by `first_mb_in_slice`, excluding AUD/SPS/PPS/SEI and Annex B padding; all 868 retained hashes matched. This is a specific compressed-video content result, not a decoded pixel comparison, A/V synchronisation measurement or proof for every impaired output. An earlier plain VCL hash comparison did not establish a contiguous C3 match.

The AAC result remains **counts only**: 1408 source packets versus 1356 C3 packets, a difference of **52**. The AAC content check did not establish contiguous equality or locate that difference. Do not assign it to the video prefix or tail omission without further evidence.

The ranges below include **all five seeds per cell**, including failed reference trials. They are absolute probe packet counts. Each clean baseline above is one trial per receiver and requested latency, rather than five repeated clean trials.

| Requested latency, ms | Loss | C3 harness pass | C3 video packets | libsrt harness pass | libsrt video packets | libsrt decoder lines |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 2000 | random:3 | 5/5 | 868 | 5/5 | 900 | 0 |
| 2000 | random:10 | 5/5 | 868 | 3/5 | 900 | 0 to 2 |
| 2000 | random:20 | 5/5 | 864 to 868 | 4/5 | 900 | 0 to 2 |
| 2000 | burst:8,8 | 5/5 | 867 to 868 | 4/5 | 899 to 900 | 0 to 1 |
| 200 | random:3 | 5/5 | 735 to 862 | 1/5 | 898 to 900 | 0 to 11 |
| 200 | random:10 | 5/5 | 296 to 837 | 0/5 | 900 to 901 | 11 to 105 |
| 200 | random:20 | 5/5 | 189 to 721 | 0/5 | 900 to 901 | 104 to 174 |
| 200 | burst:8,8 | 5/5 | 739 to 816 | 0/5 | 895 to 900 | 4 to 25 |

C3 emitted zero decoder lines in every row, but at requested 200 ms with 20% random loss it retained only **189 to 721 video packets**, against a 900-packet source. Clean-relative normalisation would hide its existing clean omission and must not replace these absolute counts. Some damaged reference outputs probed as 901 packets; packet counts under corruption are not a count of verified correct frames. The export retains audio ranges and all individual counters as well.

The frozen trial gate required a completed source feeder, positive video/audio and proxy-original counts, successful probes/decoder exits, zero decoder diagnostic lines, valid receiver/proxy/sender exit outcomes and no source timeout. Every source, sender, receiver, proxy, probe and decoder exit was zero here; failed trial exits were one because decoder output was non-empty. That gate detects unusable tool outcomes and decoder diagnostics. It does not require complete source duration, cadence, content equality or recording integrity beyond those checks.

C3 is the frozen **debug** server on an Apple M1. Its output passes through Sapflow ingestion, demux, packaging and recording to fragmented MP4. The reference writes received TS through the black-box SRT CLI. Lower decoder diagnostics across these paths cannot isolate SRT recovery from media handling or omission. No earlier Sapflow version was included, so this cohort does not identify which change introduced the omission. The three concurrent trials share a host, and equal seeds describe the same loss process rather than identical packet faults when control traffic differs. No CPU, RSS, p99 latency, energy, handset or production-network result comes from this cohort. It remains separate from nominal M4 resource measurements.

The method used a generated 30-second 1280 by 720, 30 fps H.264/AAC clip, with 2.5 Mbit/s video and 128 kbit/s, 48 kHz audio targets. A common pre-encoded source was paced through FFmpeg copy/remux into a bound CLI UDP input configured for at most 1316 bytes per datagram. The generated TS source passed the whole, sync-aligned 188-byte packet check. All 84 sender UDP ownership checks preceded feeder admission. Loss was random 3%, 10%, 20%, or Gilbert-Elliott burst 8% with mean burst length eight packets, in both directions. Handshakes were exempt; fixed delay was 30 ms each way. This was **plaintext only**.

Latencies of 2000 and 200 ms are **requested settings**. The own listener source negotiates the larger of its 120 ms minimum and the caller's advertised delay, but this run did not independently retain the accepted latency value. The proxy's `in_time` count is survival/admission to its delay queue before actual delayed forwarding. It is **not** the receiver's delivery deadline, TSBPD loss or viewer stall rate.

A reproduction must use the frozen hashes and the [collection method](srt-loss-method.md), with these explicit parameters and fresh private output:

```sh
SEEDS=5 JOBS=3 CLIP_SECS=30 DELAY_MS=30 KEEP=1 \
LOSSES="random:3 random:10 random:20 burst:8,8" LATENCIES="2000 200" \
RECEIVERS="ownC3=<PINNED_C3_DEBUG_BINARY> libsrt" \
SAPFLOW_BIN="<PINNED_C3_DEBUG_BINARY>" PROXY_BIN="<PINNED_PROXY_BINARY>" \
OUT="<NEW_PRIVATE_OUTPUT>" bash "<FROZEN_HARNESS>/scripts/srt-loss.sh"
```

Clear inherited server/test/encryption settings, pin tools and inputs before and after, and retain an owned-process-group supervisor with a 60-minute deadline. `KEEP=1` is essential for attribution: default cleanup removes source/output media and successful plaintext diagnostics. Record the actual generated-work location; macOS `mktemp` need not honour the requested `TMPDIR` location. Preserve failures and unattempted rows without selective reruns. This cohort took **1368.158 seconds**, completed naturally and returned one for its failed qualification.

| Frozen identity | SHA-256 |
| --- | --- |
| C3 debug server | `c5fdfca5418c40e9e691685fab2a7b255c049870fd11689295d48b8499b72678` |
| Copied loss proxy | `9e3d100426a580274afe9f1628962b16636ac9c0ec4f35b4a2df10620b8c0113` |
| Qualification receipt | `1ce895255ca903c87842c0bcbf1fd623ea57c454611819efec11e2d9746dddf3` |
| Normalised VCL comparison | `94cdaee356ab376e41c9facbb44ae47cc23b8cc0be8746420547d8e594763784` |

FFmpeg/FFprobe were **8.0.1** and the SRT CLI reported **1.5.4**. Complete tool, original-controller and copied shell/UDP/result-helper hashes are in the scalar export. No library source was used for the reference.

Independent completion readback found the owned group empty and all **420 fixture port checks free** (336 UDP and 84 TCP). After terminal verification, **1095 files totalling 801737820 bytes** were retained with all file hashes verified after relocation; the original work location was absent. Lifecycle/evidence retention passed, while media qualification failed. The clean video omission, unresolved AAC difference and failed reference cells remain visible; no comparative winner summary is issued.
