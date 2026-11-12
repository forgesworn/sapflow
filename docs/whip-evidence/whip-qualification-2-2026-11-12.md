# WHIP qualification evidence, second round: A3.10, A3.8 again, A4.1, A4.2 CPU

Status: evidence, 12 November 2026, on `test/whip-qualification-lab` after
the first round ([whip-qualification-2026-11-12.md](whip-qualification-2026-11-12.md)),
which this note follows. It reports against
[the WHIP ingest design](../whip-ingest.md) (A3.8, A3.9, A3.10, A4.1, A4.2).
Raw results are in `fixtures/whip/qualification-2-2026-11-12/`.

**Builds.** A3.8, A3.10 and A4.1 ran on builds made before main
(`b5a130a`, with the muxer changes of #79) was merged into this branch;
A4.2's Linux run and the final gate ran after. One A4.1 trial was rerun on
the merged build (see "A stream that never goes live").

## The owner's decisions

The owner's written decisions of 12 November 2026, as given:

1. "The Docker lock is stale, and the owner says to take it over."
2. "A4.1 uses the userspace loss proxy (docs/srt-loss-method.md) for both
   protocols, not netem. That is the owner's written acceptance of the
   change in method."
3. "A3.8: raise WHIP's start hold to about 4 s, then rerun A3.8 over UDP
   and TCP (10 runs each, as before)."
4. "A4.2: the owner accepts WHIP's memory cost (about 1 MB a stream over
   SRT) in writing." CPU is to be measured again in a quiet Linux container
   on the test host, "WHIP against SRT, with RTMP for reference, and the
   same ratio rule fixed in advance".

## Summary

| Gate | Result | Status |
|---|---|---|
| A3.9 smoke | CI's smoke job ran the WHIP part for the first time on PR #81 (merged as `9a6092c`): the pinned FFmpeg n8.1.3 passed its SHA-256 check, and the refusal, publish and `DELETE` checks passed | **Met**, locally and in CI |
| A3.10 OBS keyframes | OBS 32.0.4 does not answer PLI: in five runs its keyframes came every 10 s, or 7 s, as its own keyframe interval was set, whether Sapflow asked every 2 s or not at all (a control) | Recorded: **no** |
| A3.10 OBS rate | OBS's video stayed at 2.50 to 2.53 Mbit/s with receiver reports claiming 25% loss and with REMB at 500 kbit/s, as without either. The same stimuli took headless Chrome from about 1 Mbit/s to 0.03 and to 0.42 Mbit/s | Recorded: **no** change on either |
| A3.8 sync, 4 s start hold | Chrome 157: 20 of 20 runs within (UDP and ICE-TCP 10 each), audio -0.6 to +17.5 ms against the video, every run aligned by sender reports. FFmpeg: 10 of 10 at 0 to 0.5 ms, aligned at once | **Met** |
| A4.1 loss | Userspace proxy (the owner's decision), 1%, 5% and 10% random loss with 0 to 20 ms of jitter, five seeds; Chrome 157 for WHIP (FFmpeg's WHIP muxer does not retransmit). Frames unrecovered, WHIP against SRT: 9.4 against 2.0 at 1% (t 1.28), **83.0 against 6.8 at 5% (t 9.73)**, 50.8 against 130.8 at 10% (t -3.18), but 19.8% against 14.5% of the frames each sent | **Not met** at 5%. A decision point: the default `SAPFLOW_WHIP_JITTER` (200 ms) and the NACK schedule |
| A4.2 memory | accepted by the owner in writing (decision 4) | Accepted |
| A4.2 CPU, Linux container | 27 of 27 runs under the load gate, none skipped. WHIP's CPU per stream against SRT's: 0.74 to 0.84 at 1 stream, 0.83 to 0.96 at 50, and at 10 streams 0.75 and 0.83 with one round at 1.11. WHIP was below SRT in 8 of 9 rounds | **Met** at 1 and 50 streams by the rule. At 10 the rounds disagree (one of three above), so the call there is the owner's |

## Host and versions

| Item | Value |
|---|---|
| Host | Apple M4, macOS 26.6.2, shared with other sessions. The one-minute load average is given with each run |
| OBS | 32.0.4, the installed app, with a dedicated profile and scene collection, `A310-WHIP-test` |
| Browser | Google Chrome 157.0.8103.52, headless, a fresh profile each run |
| FFmpeg | 8.1.2 (Homebrew) on macOS |
| Sapflow | this branch, release builds for `aarch64-apple-darwin`; the A3.10 rate runs use a build with `--features test-feedback` (below) |

## A3.10 OBS keyframes and rate

### Method

`scripts/whip-obs.js` runs OBS against Sapflow on loopback, as W0's
`obs-run.js` did. It refuses to start if OBS is running, copies OBS's
whole config directory to a scratch backup, and writes a dedicated
profile and scene collection, `A310-WHIP-test`:

- Advanced output, x264 CBR at 2500 kbit/s, preset veryfast, baseline,
  no B-frames;
- OBS's own keyframe interval (`keyint_sec`) set to 10 s, far from
  Sapflow's 2 s request cadence, or 7 s for one run;
- 1280x720 at 30 fps, one media source looping a generated clip
  (testsrc2 and a tone), so the encoder has motion to spend its rate on;
- the WHIP custom service, pointing at Sapflow with a bearer token.

It then launches OBS with `--profile`, `--collection` and
`--startstreaming`, and lets it stream for 25 to 60 s after the session
connects. It ends OBS by its own PID: the first SIGTERM stops streaming,
and a second one closes it. Finally it restores the config directory from
the backup and checks it with `diff -r` and an `rsync -an --checksum
--delete` dry run. Every run printed `diff` exit 0 and an empty dry run.
The modified directories and the backups went to the Trash.

OBS reached the 127.0.0.1 candidate in every run. Its own log gives the
connect time as about 5.2 s.

Sapflow now logs at debug each keyframe request its receiver queues (`WHIP
keyframe requested`) and each keyframe's arrival (`WHIP keyframe
received`). Those are new in this round. The harness reads both, and the
recording's keyframes from ffprobe.

```sh
node scripts/whip-obs.js --bin <sapflow> --out <dir> --seconds 40 --keyframe-interval 2 --obs-keyint 10
node scripts/whip-obs.js --bin <sapflow> --out <dir> --seconds 40 --keyframe-interval 0 --obs-keyint 10   # control
```

### Keyframes

No loss was applied, so the only keyframe requests were the scheduled ones
and the retries before the first keyframe.

| Run | Seconds | `SAPFLOW_WHIP_KEYFRAME_INTERVAL` | OBS keyframe interval | Load | PLIs sent | Keyframes received | Gaps between keyframes, s | Recording's keyframes, s |
|---|---|---|---|---|---|---|---|---|
| 1 | 25 | 2 | 10 | 20.6 | 23 | 4 | 9.95 to 10.04 | 0, 10.0, 20.0, 30.0 |
| 2 | 40 | 2 | 10 | 26.3 | 31 | 5 | 9.99 to 10.04 | 0, 10.0, 20.0, 30.0, 40.0 |
| 3 | 40 | 2 | 10 | 11.9 | 31 | 5 | 9.99 to 10.04 | 0, 10.0, 20.0, 30.0, 40.0 |
| control | 40 | 0 (none scheduled) | 10 | 8.0 | 7 (the retries before the first keyframe) | 5 | 9.97 to 10.04 | 0, 10.0, 20.0, 30.0, 40.0 |
| 7 s | 40 | 2 | 7 | 9.5 | 29 | 7 | 7.00 to 7.01 | 0, 7.0, 14.0, 21.0, 28.0, 35.0, 42.0 |

The four rate runs below also asked every 2 s, 40 or 41 times each. Their
keyframes came 9.96 to 10.04 s apart.

So OBS's keyframes follow its own interval and nothing Sapflow sends. In
the first run, OBS's first keyframe reached Sapflow 4.97 s after the
session connected. That fits OBS's own schedule from when its encoder
started, about 10 s earlier, before the 5.2 s connect. Sapflow sent 6 PLIs
before it. OBS's offer has no `ccm fir`, so PLI is the only request it can
be sent. That OBS ignores PLI is consistent with not reading Sapflow's RTCP
at all; the NACK run below separates the two.

**OBS reads Sapflow's RTCP: it answers NACKs.** One more 40 s run put the
loss proxy between OBS and Sapflow: `--protocol whip`, 5% random loss on
SRTP and SRTCP, 30 ms each way, seed 1. The answer's only candidate was
the proxy.

- **Retransmissions.** The proxy dropped 1613 of OBS's 33 052 datagrams.
  1495 RTP packets arrived again with the same SSRC and sequence number:
  OBS resends a NACKed packet on its original SSRC, as the design expects.
  Sapflow's counters agree: 1581 sequence numbers NACKed, 1408 recovered.
- **Keyframes.** Its keyframes still came every 10 s.
- **What it cost.** The cost is what ignoring PLI means:
  - 21 video packets stayed lost, and 20 frames were incomplete;
  - while video waited for OBS's next keyframe, up to 10 s away, 1320
    frames were skipped and 162 delivered;
  - the muxer restarted its first segment three times, and no stream or
  recording was made.

So OBS ignores PLI, REMB and the loss in receiver reports, while acting on
the NACKs in the same RTCP. A publisher that ignores PLI depends wholly on
its own keyframe interval for recovery from a loss NACKs cannot repair.

### Rate

**Stimuli.** Sapflow sends no REMB. Its receiver reports give the loss it
measured, and for OBS, which resends on the original SSRC, that is loss
after repair. So both stimuli need a test-only hook, which is new in this
round:

- **Build.** A server built with `--features test-feedback`, which pulls in
  the `testing` features of `sapflow-rtp` and `sapflow-whip`.
- **Arming.** The hook is armed at run time by `SAPFLOW_TEST_WHIP_FEEDBACK`,
  and the server warns when it is armed. A default release build has
  neither the feature nor the variable: `strings` finds no
  `SAPFLOW_TEST_WHIP` in it, and finds it once in the test build.
- **`loss:64:20`.** From 20 s after SRTP is up, every receiver report block
  claims 64/256 (25%) lost, and its cumulative loss grows to match. The
  answer leaves out `transport-cc`, so the reports are the only congestion
  signal. `loss:0:0` is the same answer with nothing claimed.
- **`remb:500000:20`.** The answer echoes `goog-remb` for video where the
  offer lists it (OBS's does), and leaves out `transport-cc`. From 20 s
  every report also carries a REMB of 500 000 bit/s for every stream. The
  REMB is built from draft-alvestrand-rmcat-remb-03.

**Rate series.** For OBS, the series is the recording's video bytes per
second, from ffprobe's packet sizes. The health callback's bitrate was not
used: it averages the last six segments. For Chrome, the series is its own
`targetBitrate` and `availableOutgoingBitrate`, a second apart, from
`getStats`.

**The control.** `scripts/whip-rate.js` publishes a 1280x720 canvas of
moving noise from headless Chrome. It shows the hooks move a sender that
does respond. It ran each stimulus beside a run of the same length without
it, 60 s each.

| Publisher | Stimulus | Answer | Load | Video, kbit/s, 5 to 20 s | 25 to 40 s | 40 to 60 s |
|---|---|---|---|---|---|---|
| OBS | none | REMB not echoed, no transport-cc (OBS offers none) | 5.4 | 2523 | 2519 | 2500 |
| OBS | `loss:0:0` | reports only | 19.0 | 2523 | 2519 | 2514 |
| OBS | `loss:64:20` (25% claimed from 20 s) | reports only | 10.0 | 2524 | 2507 | 2526 |
| OBS | `remb:500000:20` | `goog-remb` (see below) | 7.6 | 2531 | 2520 | 2512 |
| Chrome | none | transport-cc | 18.6 | target 2083 | 2180 | 2251 |
| Chrome | `loss:0:0` | reports only | 17.1 | target 631 (ramping) | 2023 | 2295 |
| Chrome | `loss:64:20` | reports only | 11.0 | target 631 | **107** | **30** |
| Chrome | `remb:20000000:0` (no limit) | `goog-remb` | 11.2 | target 2083 | 2382 | 2305 |
| Chrome | `remb:500000:20` | `goog-remb` | 7.9 | target 631; available 757 | **416**; available **500** | **416**; available **500** |

No OBS answer was captured. That the REMB run's answer to OBS carried
`goog-remb` rests on two things. One is the unit test on OBS 32.0.4's W0
offer (`the_remb_hook_answers_goog_remb_without_transport_cc`). The other
is Chrome's run with the same build and variable, whose answer did
(`answer_remb: true`).

Chrome's target fell within 1 to 2 s of each stimulus. With the claimed
loss it went 1113 kbit/s at 21 s, then 860, 664, 513, 395, and on down to
30. With the REMB it went from 1113 to 416 at 22 s, with the available
bitrate at exactly 500. OBS's video stayed between 2.39 and 2.76 Mbit/s
every second, in every run, before and after 20 s. Its own log shows the
x264 bitrate at 2500 and nothing about congestion.

**Found on the way.** In the Chrome runs at a low rate, Sapflow never made
the stream ready. That was the REMB at 500 kbit/s, and the claimed loss,
on a noise source that needs far more. Chrome sent about one frame a
second; the timeline re-anchored three times; the muxer logged
"timestamps broke before the first segment was complete; starting over"
12 and 18 times. No segment and no recording were made. Chrome's own
statistics are the rate series there. The same muxer message comes up
under A4.1 below. Nothing was changed for it here; see What is left open.

## A3.8 with the 4 s start hold

**The change.** `TimelineConfig::start_hold` now defaults to 4 s, up from
1.5 s.

- **Tests.** A new timeline test checks three things: reports for both
  streams at 3.5 s are waited for; reports with the first packets end the
  hold at once; with no reports the hold ends at 4.0 s, not before.
- **Tests that keep 1.5 s.** Some tests were written around the 1.5 s hold,
  and they now set it explicitly. These are the server's in-process mark
  runs, whose publisher sends no sender reports, and the `sr_timeline` fuzz
  seeds.
- **Tests moved past 4 s.** The receiver's fixture runs now drive their
  timers past 4 s.
- **Unchanged.** `max_hold_bytes` stays 4 MiB, which is 4 s at about
  8 Mbit/s.

**The rerun.** The same harness and source as the first round
(`scripts/whip-sync.js`, the canvas and WebAudio page), with the release
build of this change. It ran 10 runs over UDP, 10 over ICE-TCP, and 10
FFmpeg runs.

### Chrome 157 over UDP

| Run | Load | Alignment | Aligned after connect, ms | Ready, ms | HLS offsets, ms (min to max) | Recording offsets, ms | Source, ms (median) | Net, ms | Within |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 9.21 | SenderReports | 3555 | 5644 | -0.3 to 0.1 | -0.6 to 0.1 | -6 | 6 | yes |
| 2 | 7.68 | SenderReports | 3506 | 5604 | 13.1 to 15.4 | 13.1 to 17.5 | 7.6 | 5.8 | yes |
| 3 | 5.85 | SenderReports | 3458 | 5592 | 1.9 to 7.9 | 1.9 to 8.1 | -1 | 8.1 | yes |
| 4 | 7.71 | SenderReports | 4015 | 7617 | 6.2 to 7.8 | 6.2 to 9 | -0.3 | 7 | yes |
| 5 | 19.79 | SenderReports | 4017 | 6683 | 12.1 to 14.1 | 12.1 to 14.1 | 5.6 | 8 | yes |
| 6 | 43.11 | SenderReports | 4018 | 6607 | 5.1 to 11.9 | 5.1 to 12.3 | 2 | 9.3 | yes |
| 7 | 31.35 | SenderReports | 3634 | 5712 | 8 to 11.8 | 8 to 11.8 | 3 | 7.6 | yes |
| 8 | 24.35 | SenderReports | 3655 | 5741 | 1.8 to 4.7 | 1.8 to 4.7 | -3.7 | 6.2 | yes |
| 9 | 18.28 | SenderReports | 3571 | 5666 | 9.9 to 13.4 | 9.9 to 13.4 | 3.9 | 9.2 | yes |
| 10 | 13.03 | SenderReports | 3648 | 5735 | 7.9 to 9.9 | 7.9 to 12.2 | 3.8 | 5.9 | yes |

All 59 served offsets were -0.3 to 15.4 ms (median 9.0), and all 79 recorded
offsets -0.6 to 17.5 ms (median 9.7). Net was 5.8 to 9.3 ms (median 7.6).

### Chrome 157 over ICE-TCP

| Run | Load | Alignment | Aligned after connect, ms | Ready, ms | HLS offsets, ms | Recording offsets, ms | Source, ms (median) | Net, ms | Within |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 8.84 | SenderReports | 3667 | 5747 | 3.9 to 6.7 | 3.9 to 7.7 | -1.5 | 7.1 | yes |
| 2 | 13.78 | SenderReports | 3797 | 5883 | 4 to 6.8 | 4 to 8.1 | -1.9 | 8.4 | yes |
| 3 | 11.28 | SenderReports | 3671 | 5761 | 6.3 to 11.7 | 6.3 to 11.7 | 2.4 | 8.2 | yes |
| 4 | 9.06 | SenderReports | 3485 | 5603 | 0.6 to 1.2 | 0.6 to 2.7 | -6.9 | 7.6 | yes |
| 5 | 8.59 | SenderReports | 3529 | 5626 | 4.6 to 8.6 | 4.6 to 8.8 | -2.4 | 8.6 | yes |
| 6 | 7.41 | SenderReports | 3466 | 5617 | 10.5 to 13.5 | 10.5 to 14.5 | 2.6 | 10.6 | yes |
| 7 | 7.26 | SenderReports | 3657 | 5734 | 8.1 to 10.7 | 8 to 10.7 | -0.6 | 10.2 | yes |
| 8 | 17.81 | SenderReports | 3635 | 5735 | 10.8 to 13.7 | 10.8 to 14.9 | 2.7 | 9.7 | yes |
| 9 | 27.66 | SenderReports | 3789 | 5903 | 13.6 to 16.6 | 13.6 to 16.6 | 6.9 | 9.1 | yes |
| 10 | 28.65 | SenderReports | 3611 | 5719 | 3.3 to 4 | 3.3 to 4.4 | -4.8 | 8.8 | yes |

All 60 served offsets were 0.6 to 16.6 ms, and all 80 recorded offsets 0.6
to 16.6 ms (median 8.1 for both). Net was 7.1 to 10.6 ms (median 8.8).

### FFmpeg 8.1.2 over UDP

All 10 runs were aligned by sender reports 0 to 1 ms after the session
connected, as before the change. They were ready 1590 to 1603 ms after the
publisher connected. All 70 served offsets and all 90 recorded offsets were
0 to 0.5 ms. Load averages were 9.1 to 34.3.

### Time to ready, before and after

| Publisher | 1.5 s hold (first round), ms | 4 s hold, ms |
|---|---|---|
| Chrome, UDP | 5594 to 5946 (median 5677) | 5592 to 7617 (median 5724) |
| Chrome, ICE-TCP | 5631 to 6785 (median 6592) | 5603 to 5903 (median 5735) |
| FFmpeg, UDP | 1590 to 2613 (median 1606) | 1590 to 1603 (median 1597) |

Chrome's time to ready barely moved, because its first segment waits on its
first keyframe either way. The exceptions are UDP runs 4 to 6. There, the
start was aligned 4015 to 4018 ms after the session connected: that is the
hold's end, with reports for both streams already in. Those runs were ready
6.6 to 7.6 s after connecting. FFmpeg, whose reports come with its first
packets, is not delayed at all.

**What the longer hold costs.**

- A publisher that sends no sender reports now waits 4 s for its first
  media, up from 1.5 s.
- Above about 8 Mbit/s, the 4 MiB bound ends the hold before 4 s, by
  arrival time as before.
- Live latency was not measured in either round; the first round listed
  that as a limit too.

**Margin.** Three of the 20 Chrome runs were aligned by reports only at the
hold's end. The other 17 had their reports by 3.46 to 3.80 s. A Chrome
whose reports come later than 4 s would fall back to arrival time, as every
run did before the change. One host, headless and synthetic, gives no
bound on that.

## A4.1 Loss

### Method

The owner's decision 2: the userspace loss proxy of
[srt-loss-method.md](../srt-loss-method.md) for both protocols, in place of
netem. `srt-loss-proxy` now has two options:

- **`--jitter-ms`.** Each impaired packet waits a further time drawn
  uniformly from 0 to J ms. The draw comes from its own seeded process, so
  the loss pattern is the same with and without jitter. Packets leave as
  they fall due, so they can be reordered, as netem's jitter does.
- **`--protocol whip`.** Only SRTP and SRTCP are impaired. They are sorted
  by first byte (RFC 7983); STUN and DTLS pass with the fixed delay alone.
  SRT is impaired as before: everything but the handshake.

`scripts/whip-loss.py` drives the trials.

- **Profiles.** Random loss of 1%, 5% and 10% on both directions, with 0 to
  20 ms of jitter, a 30 ms delay each way, and seeds 1 to 5. Three clean
  runs per protocol had no loss and no jitter.
- **Order.** Each seed ran both protocols back to back, the order flipping
  each seed.
- **Each trial.** A fresh Sapflow, a fresh proxy and one publisher.
- **SRT.** A 30 s clip of 900 frames: 1280x720 at 30 fps, constrained
  baseline H.264 at 2.5 Mbit/s, 1 s keyframes, AAC. It goes `ffmpeg -re`
  (copy) into UDP, then srt-live-transmit (libsrt 1.5.7) through the proxy
  to Sapflow's SRT listener. Latency is 200 ms on both ends.
- **WHIP.** Sapflow's jitter wait is its compiled default, 200 ms.
  `SAPFLOW_WHIP_JITTER` is a proposal, not a setting.
- **WHIP publisher.** FFmpeg's WHIP muxer was checked first, and it does
  not retransmit. Its clean and 5% runs sent the same 12 448 RTP packets,
  and none was repeated. That is no fair match for SRT, so headless Chrome
  157 is the WHIP publisher. It sends a 1280x720 canvas at 30 fps (moving
  bars and a frame counter) at up to 2.5 Mbit/s, and it answers NACKs over
  RTX and keyframe requests with keyframes.
- **Frames unrecovered.** Frames sent less the video frames in Sapflow's
  recording (ffprobe `-count_packets`). Frames sent are the clip's 900 for
  SRT, and Chrome's own `framesSent` for WHIP.

```sh
scripts/whip-loss.py --bin <sapflow> --proxy target/<triple>/release/srt-loss-proxy --out <dir> \
  --protocols srt,whip --losses 1,5,10 --seeds 5 --clean 3
```

**The reading.** The driver's summary columns were set before the runs:
each protocol's mean and standard deviation, the difference and a Welch t.
The pass and fail reading was written after the numbers. It is the
method's rule: with five seeds, a |t| below about 2.3 is not a difference
worth claiming. So the gate fails at a profile where WHIP's mean is higher
with |t| at or above that. The 5% result fails under any reading of the
numbers.

### Results

| Loss | Protocol | Frames sent | Frames unrecovered, by seed | Mean (sd) | Share of frames sent | Residual packets lost (Sapflow) | Proxy drops, client to server |
|---|---|---|---|---|---|---|---|
| none | SRT | 900 | 2 2 2 | 2.0 (0) | 0.2% | 0 | 0 |
| none | WHIP | 783 to 798 | 1 1 1 | 1.0 (0) | 0.1% | 0 | 0 |
| 1% | SRT | 900 | 2 2 2 2 2 | 2.0 (0) | 0.2% | 0 (dropped) | 144 to 176 |
| 1% | WHIP | 558 to 786 | 8 1 4 32 2 | 9.4 (12.9) | 0.1 to 5.7% | 0 to 2 | 56 to 88 |
| 5% | SRT | 900 | 22 2 2 6 2 | 6.8 (8.7) | 0.2 to 2.4% | 0 to 1 (dropped) | 762 to 835 |
| 5% | WHIP | 466 to 697 | 84 101 77 61 92 | 83.0 (15.2) | 9.7 to 18.0% | 3 to 9 | 280 to 392 |
| 10% | SRT | 900 | 43 115 144 171 181 | 130.8 (55.4) | 4.8 to 20.1% | 5 to 13 (dropped) | 1582 to 1657 |
| 10% | WHIP | 246 to 272 | 56 40 48 66 44 | 50.8 (10.4) | 14.7 to 26.3% | 6 to 12 | 348 to 380 |

| Loss | WHIP less SRT, frames | Welch t | Reading |
|---|---|---|---|
| 1% | +7.4 | 1.28 | no difference worth claiming |
| 5% | **+76.2** | **9.73** | **WHIP leaves more** |
| 10% | -80.0 | -3.18 | WHIP leaves fewer by count; as a share of what was sent, more (19.8% against 14.5%) |

No trial failed to become ready, and no recording had a decoder error. Load
averages were 3.3 to 62.3; the 1% WHIP run with 32 frames unrecovered ran
at 62.3.

**A4.1 is not met at 5%.** It is close at 1%, and at 10% it depends on the
measure. What the counters show:

- **Repair.** WHIP is left with 3 to 9 video packets that its NACKs did not
  repair in 200 ms, against SRT's 0 or 1. Within that wait, at a 60 ms
  round trip and up to 20 ms of jitter each way, a packet gets about two
  NACKs. They repeat every round trip and a quarter, at most 4 in all; how
  many go is inferred, not counted. Under every lossy profile, libsrt
  retransmitted about 6700 packets in 30 s, far more than the 760 to 1660
  the proxy dropped, so it repairs more aggressively.
- **Each unrepaired packet.** It costs the frames until Chrome's next
  keyframe: one incomplete frame, then 4 to 9 skipped while the PLI's
  answer comes. Each SRT loss costs the rest of the 1 s GOP, which is why
  SRT's count rises steeply at 10%.
- **Chrome's rate.** Chrome lowers its rate and frame rate under loss. It
  sent 466 to 697 frames at 5% and 246 to 272 at 10%, against 900 for the
  stream-copied SRT clip. So a count of frames unrecovered and a share of
  frames sent rank the two differently at 10%.
- **Audio.** It is outside the gate but differs too. Chrome offers no NACK
  for Opus, so WHIP's audio loss is not repaired: at 5% (seed 2) Sapflow
  filled 84 Opus packets with silence. SRT repairs its AAC with everything
  else.

This is a decision point for the default `SAPFLOW_WHIP_JITTER`, which A4.1
was to settle. A longer wait or more NACK attempts would repair more, at the
cost of latency. Neither was tried: the wait is compiled in.

**FFmpeg as the WHIP publisher** (one seed each, to show the limit, not part
of the gate):

| Loss | Frames unrecovered of 900 | Ready |
|---|---|---|
| none | 32 (the first GOP: one incomplete frame at the start and 29 skipped to the next keyframe) | yes |
| 1% | 716 | yes |
| 5% | 900 | **no**: no stream, no recording. On the merged build: ready after 20.95 s, 7 frames recorded (893 unrecovered) |
| 10% | 900 | **no** |

**Found on the way.** At 5% and 10% with FFmpeg, Sapflow (before the
merge) never made the stream ready. Its muxer logged "timestamps broke before the first segment
was complete; starting over" as video skipped from loss to keyframe. The
same happened with Chrome at a low rate (A3.10) and with OBS at 5% (above).
A publisher whose first segment keeps breaking never goes live. Nothing was
changed for it; see What is left open.

## A4.2 CPU in a Linux container

### Method

The same driver and rule as the first round's A4.2 (`scripts/whip-cost.py`,
benchmarks.md section B), now inside a Linux container on the test host.
`scripts/whip-cost-linux.sh` builds it:

- **Sapflow.** Built for Linux inside the container (Rust 1.94.1 on Debian
  bookworm, `cargo build --locked --release -p sapflow`) from this branch
  after merging main (`b5a130a`). The build went into a scratch target
  directory of its own, since a Linux build cannot share the host's.
- **Tools.** FFmpeg is the pinned FFmpeg-Builds n8.1.3 static build for
  arm64, checked by its SHA-256 (`a08c0ee6...fcb525`), since Debian's has
  no WHIP muxer. srt-live-transmit 1.5.1 and procps come from Debian.
- **Container.** The repository is mounted read-only, and the container
  is removed when the run ends. Docker Desktop's VM gives 16 CPUs and
  16 GB, running Linux 6.12.
- **CPU time.** The driver now reads it from `/proc/<pid>/stat`, in clock
  ticks, because procps prints CPU time in whole seconds.
- **Load gate.** It is the container's own one-minute load average, below
  8. The macOS host's own load was 21.6 at the start and 7.9 at the end,
  from other sessions.

The Docker lock was taken over as the owner directed: the stale lock went
to the Trash, and a new one named this session.

```sh
scripts/whip-cost-linux.sh <scratch> --streams 1,10,50 --rounds 3 --gate-wait 600
```

**The rule, unchanged.** For each N, in each round where both runs are
results, take the WHIP/SRT ratio of CPU per stream. WHIP passes at that N
if every ratio is at most 1, and fails if every ratio is above 1.
Otherwise the rounds disagree, and the call is the owner's.

### Results

Three rounds, 12:15 to 12:52 UTC. All 27 runs were results. Only one waited
for the gate (25 s), and no round was skipped. The container's load
averages were 1.1 to 7.8 at the start of each run.

| Protocol | Streams | Server CPU, % of a core (three rounds) | CPU per stream, % | Peak RSS, MB | RSS per stream, MB | Publishers' CPU, % |
|---|---|---|---|---|---|---|
| SRT | 1 | 2.33, 1.82, 1.26 | 1.26 to 2.33 | 13.4 to 13.8 | 13.4 to 13.8 | 2.7 to 5.1 |
| RTMP | 1 | 1.37, 0.93, 0.82 | 0.82 to 1.37 | 13.3 to 13.6 | 13.3 to 13.6 | 0.7 to 1.3 |
| WHIP | 1 | 1.86, 1.35, 1.06 | 1.06 to 1.86 | 13.5 to 13.9 | 13.5 to 13.9 | 1.8 to 3.2 |
| SRT | 10 | 11.85, 8.63, 8.97 | 0.86 to 1.19 | 124.7 to 135.4 | 12.5 to 13.5 | 22.8 to 30.8 |
| RTMP | 10 | 7.21, 5.99, 5.64 | 0.56 to 0.72 | 121.4 to 138.1 | 12.1 to 13.8 | 6.4 to 7.5 |
| WHIP | 10 | 13.18, 6.50, 7.41 | 0.65 to 1.32 | 118.8 to 126.5 | 11.9 to 12.7 | 14.3 to 27.5 |
| SRT | 50 | 31.59, 33.84, 32.06 | 0.63 to 0.68 | 618 to 717 | 12.4 to 14.3 | 88.7 to 99.1 |
| RTMP | 50 | 30.30, 23.28, 27.12 | 0.47 to 0.61 | 667 to 736 | 13.3 to 14.7 | 24.8 to 30.7 |
| WHIP | 50 | 26.13, 30.93, 30.73 | 0.52 to 0.62 | 531 to 573 | 10.6 to 11.5 | 64.2 to 78.6 |

| Streams | Round | WHIP/SRT CPU | WHIP/SRT RSS | WHIP/RTMP CPU |
|---|---|---|---|---|
| 1 | 1 | 0.80 | 0.98 | 1.36 |
| 1 | 2 | 0.74 | 1.04 | 1.45 |
| 1 | 3 | 0.84 | 1.00 | 1.30 |
| 10 | 1 | **1.11** | 0.92 | 1.83 |
| 10 | 2 | 0.75 | 0.93 | 1.09 |
| 10 | 3 | 0.83 | 0.95 | 1.31 |
| 50 | 1 | 0.83 | 0.93 | 0.86 |
| 50 | 2 | 0.91 | 0.74 | 1.33 |
| 50 | 3 | 0.96 | 0.81 | 1.13 |

**Verdict.** By the rule, WHIP's CPU is no higher than SRT's at 1 stream
and at 50. At 10 the rounds disagree: two at 0.75 and 0.83, and one at 1.11,
where WHIP's own CPU per stream (1.32%) was twice its other two rounds'.
That call is the owner's. WHIP was below SRT in 8 of the 9 rounds, and
above RTMP in 8 of 9.

The container's spread is much smaller than the macOS host's. SRT at 50
streams was 31.6 to 33.8% here, against 17.2 to 31.4% in the first round.
On Linux, WHIP's memory per stream was at or below SRT's in 8 of 9 rounds,
not above as on macOS; the owner has accepted it either way.

## What is left open

- **A4.1's decision.** At its 200 ms wait, WHIP leaves more frames
  unrecovered than SRT at 5% loss. The default `SAPFLOW_WHIP_JITTER` and
  the NACK schedule (how many attempts, how soon) are the owner's to
  choose. The wait is compiled in (`SAPFLOW_WHIP_JITTER` is not a setting
  yet), so no other value was measured.
- **A stream that never goes live, or goes live late.** Under loss that is
  not repaired, video skips from a loss to the next keyframe. Sapflow's
  muxer then logs "timestamps broke before the first segment was complete;
  starting over", and can keep starting over.
  - **Before the merge.** The stream was never ready and nothing was
    recorded. That was seen with FFmpeg's WHIP muxer at 5% and 10% loss,
    with OBS at 5% (10 s keyframes, PLI ignored), and with Chrome at a very
    low rate on a noise source.
  - **On the merged build.** FFmpeg at 5% (seed 1) restarted three times
    and was ready only after 20.95 s, with 7 of 900 frames recorded.
  - **Not investigated.** It belongs with the muxer and the timeline's
    re-anchoring.
- **WHIP audio is not repaired.** Browsers offer no NACK for Opus, so loss
  is filled with silence: 84 Opus packets at 5% in one run. Opus in-band
  FEC is negotiated, but Sapflow does not decode, so it cannot use it.
- **The hold's margin.** 3 of 20 Chrome runs were aligned by reports only
  at the 4 s hold's end, so a slower Chrome would fall back to arrival
  time. Other browsers' report timing is not known. OBS and FFmpeg were
  aligned by reports in every run.
- **FFmpeg's WHIP muxer** does not retransmit. In a clean run it lost the
  first GOP (32 frames): one incomplete frame at the start, then 29
  skipped to the next keyframe. Why that frame was incomplete was not
  looked into.
- **A3.10's scope.** One OBS version, x264 only, and one level of each
  stimulus (25% claimed loss, REMB at 500 kbit/s) on loopback. Hardware
  encoders and OBS's own dynamic bitrate setting were not tried.
- **A4.2 at 10 streams** is the owner's call (above). The macOS figures
  from the first round stand beside these.
- **Docker, left on the test host.** The images pulled or built for this
  work were removed: `rust:1.94.1-slim-bookworm`,
  `sapflow-cost-linux:local`, and `ubuntu:24.04` from the first round. The
  amd64 variant of `debian:bookworm-slim` pulled in the first round is
  still there. That tag predates this work, and removing one variant needs
  `--force`, which acts on every image that refers to it. The lock this
  session took went to the Trash at the end.
- **The VM was shared.** When the A4.2 run ended, another session's
  containers were running in the same Docker VM. Whether they overlapped
  the measurement windows is not known. The container's load gate was below
  8 at the start of every run, and its load was at most 11.8 at the end of
  one.
