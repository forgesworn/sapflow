# WHIP A4.1 retest: faster NACK repeats and a 400 ms wait

**Owner's decision, 15 November 2026: "Default 400 ms".** On this evidence
the default `SAPFLOW_WHIP_JITTER` is now 400 ms and A4.1 is accepted by the
owner (decision 9 in [the second qualification round](whip-qualification-2-2026-11-12.md));
the text below is as written before it.

Status: evidence, 15 November 2026, on `perf/whip-a41-retest`, cut from
`fix/dtls-round4-findings` at `5ef36b0`. It answers the owner's decision 5
of 15 November 2026 in
[the second qualification round](whip-qualification-2-2026-11-12.md),
against A4.1 in [the WHIP ingest design](../whip-ingest.md). Raw tables are
in `fixtures/whip/a41-retest-2026-11-15/` (`trials.tsv`, `summary.tsv`).

This note states the numbers. It does not declare A4.1 met and changes no
default: the wait stays 200 ms and the NACK repeat stays the RTT and a
quarter. The decision is the owner's.

## What was asked

Decision 5: "Park, test both later". Two changes not yet tried were to be
measured at 5% loss with five seeds: NACK repeats sooner than every 1.25
round trips at the same 200 ms wait, and a 400 ms wait. Four arms ran in
the same session on the same host:

| Arm | Protocol | Wait | NACK repeat |
|---|---|---|---|
| a, `base` | WHIP | 200 ms (default) | RTT and a quarter, at least 5 ms (default) |
| b, `srt` | SRT | latency 200 ms | libsrt's own |
| c, `nack30` | WHIP | 200 ms (default) | fixed 30 ms (`SAPFLOW_WHIP_NACK_REPEAT=30`) |
| d, `wait400` | WHIP | 400 ms (`SAPFLOW_WHIP_JITTER=400`) | RTT and a quarter (default) |

SRT's latency stayed 200 ms in every arm, so arm d against arm b is not a
comparison at the same wait: WHIP had twice SRT's wait there.

## Method

The method of the A4.1 section of the qualification note, unchanged except
for the arms: `srt-loss-proxy` for both protocols (`--protocol whip`
impairs only SRTP and SRTCP), 30 ms delay each way, random loss on both
directions, seeds 1 to 5, a fresh Sapflow, proxy and publisher per trial.
SRT sends the 900-frame 30 s clip through srt-live-transmit (libsrt 1.5.7);
WHIP's publisher is headless Chrome 157.0.8103.52 with a 1280x720 canvas at
30 fps and at most 2.5 Mbit/s, answering NACKs over RTX. Frames unrecovered
are frames sent less the video frames in Sapflow's recording: 900 for SRT,
Chrome's own `framesSent` for WHIP.

- **Settings.** The wait and the repeat are chosen at start by two
  experimental settings added for this retest (`SAPFLOW_WHIP_JITTER`, 20 to
  1000 ms, and `SAPFLOW_WHIP_NACK_REPEAT`, 5 to 1000 ms; see the design's
  Configuration section). Unset, they leave the defaults; unit tests check
  that and that bad values are refused. Every trial's log records the
  effective values (`WHIP listening jitter_ms=... nack_repeat=...`), and
  each arm ran what it names.
- **Profiles.** The 5% headline profile of the qualification note (0 to 20
  ms of reordering jitter), then the two after-review profiles: 5% with no
  jitter, and 5% with 20 ms of jitter that keeps order. Three clean runs per
  arm (no loss, no jitter) came first.
- **Order.** For each seed the four arms ran back to back in the order a,
  b, c, d, so drift in the host's load reached every arm alike.
- **Load.** The host is shared. A trial started only when the one-minute
  load average was below 8 (`--max-load 8`), and the load at the start and
  at the end of each trial is kept. Trials waited 1410 s in all for the
  load to fall, at most 525 s for one.
- **Build.** A release build (`aarch64-apple-darwin`) of this branch with
  the two settings (`b0f1b51`, and the driver at `f3e2142`), on the test
  host, an Apple silicon Mac.

```sh
scripts/whip-loss.py --bin <sapflow> --proxy target/<triple>/release/srt-loss-proxy --out <dir> \
  --arms base=whip,srt=srt,nack30=whip+SAPFLOW_WHIP_NACK_REPEAT=30,wait400=whip+SAPFLOW_WHIP_JITTER=400 \
  --profiles 5:20,5:0,5:20:keep --seeds 5 --clean 3 --max-load 8
```

**Choosing arm c's interval.** At the round trips Chrome reported in these
runs (62 to 65 ms without jitter, 70 to 99 ms with it), the default repeat
of 1.25 RTT is 78 to 124 ms. A packet seen missing gets its first NACK at
10 ms, and at most one repeat whose answer can return inside the 200 ms
wait. A fixed 30 ms spacing sends the cap of four NACKs at 10, 40, 70 and
100 ms, so the last answer is due at about 100 ms plus one round trip,
inside the wait for round trips up to about 100 ms. The cap of four NACKs a
packet and the 64 kbit/s NACK ceiling were left as they are, so a lost
packet still draws at most four NACKs; they come sooner. A repeat sent
below one round trip can reach Chrome while its retransmission is in
flight. In a one-seed pilot (5%, 20 ms reordering, not part of the results)
Chrome's NACK count rose from 418 to 658 while its retransmissions stayed
at 624 against 627, so the spacing did not multiply retransmissions. A
spacing scaled to the RTT (RTT/2) was not chosen: Sapflow samples the RTT
only from a packet NACKed once, and with repeats below one round trip
almost every recovered packet has been NACKed twice, so the estimate would
stay at its assumed 100 ms and RTT/2 would be a fixed 50 ms in effect. The
interval was fixed after that pilot and before the five-seed run. It suits
this test's round trip only; on a path with a round trip much over 100 ms
all four NACKs would go before the first answer could return.

## Results

No trial failed to become ready, no publisher reported an error, and no
clean recording had a decoder error.

### Frames unrecovered

| Profile | Arm | Frames sent | Frames unrecovered, by seed | Mean (sd) | Share of frames sent | Load, start and end |
|---|---|---|---|---|---|---|
| clean | base | 884 to 886 | 0 0 0 | 0.0 (0) | 0.0% | 2.6 to 7.9 |
| clean | srt | 900 | 1 1 1 | 1.0 (0) | 0.1% | 1.9 to 7.9 |
| clean | nack30 | 881 to 884 | 0 0 0 | 0.0 (0) | 0.0% | 1.9 to 4.7 |
| clean | wait400 | 883 to 885 | 0 0 0 | 0.0 (0) | 0.0% | 2.1 to 4.7 |
| 5%, 20 ms reordering | base | 457 to 735 | 61 107 110 52 81 | 82.2 (26.2) | 13.2% | 3.0 to 12.6 |
| 5%, 20 ms reordering | srt | 900 | 16 53 22 1 43 | 27.0 (20.9) | 3.0% | 3.0 to 22.9 |
| 5%, 20 ms reordering | nack30 | 460 to 828 | 0 76 31 5 59 | 34.2 (33.1) | 5.5% | 3.0 to 7.6 |
| 5%, 20 ms reordering | wait400 | 536 to 671 | 0 2 1 0 4 | 1.4 (1.7) | 0.2% | 3.4 to 7.3 |
| 5%, no jitter | base | 873 to 886 | 17 52 26 17 0 | 22.4 (19.0) | 2.5% | 4.0 to 16.7 |
| 5%, no jitter | srt | 900 | 1 8 1 8 22 | 8.0 (8.6) | 0.9% | 5.1 to 9.6 |
| 5%, no jitter | nack30 | 872 to 885 | 27 70 11 10 19 | 27.4 (24.8) | 3.1% | 4.4 to 8.0 |
| 5%, no jitter | wait400 | 882 to 886 | 16 18 0 0 16 | 10.0 (9.2) | 1.1% | 4.4 to 14.8 |
| 5%, 20 ms keeping order | base | 731 to 884 | 154 205 104 150 200 | 162.6 (41.4) | 19.7% | 3.9 to 6.5 |
| 5%, 20 ms keeping order | srt | 900 | 40 61 39 58 49 | 49.4 (10.1) | 5.5% | 3.6 to 7.0 |
| 5%, 20 ms keeping order | nack30 | 772 to 884 | 82 75 50 90 89 | 77.2 (16.4) | 9.3% | 3.5 to 7.0 |
| 5%, 20 ms keeping order | wait400 | 749 to 883 | 115 17 16 15 44 | 41.4 (42.9) | 5.1% | 2.7 to 6.6 |

The load column is the one-minute average at each trial's start (all below
8, by the gate) and end; other sessions' builds raised it during some
trials, to 22.9 at the end of one SRT trial.

### Against SRT and against the baseline

Each difference is in frames; the reading rule is the qualification note's,
fixed before those runs: with five seeds, a |t| below about 2.3 is no
difference worth claiming.

| Profile | Arm | Less SRT | Welch t | Less baseline | Welch t |
|---|---|---|---|---|---|
| 5%, 20 ms reordering | base | +55.2 | 3.68 | | |
| 5%, 20 ms reordering | nack30 | +7.2 | 0.41 | -48.0 | -2.54 |
| 5%, 20 ms reordering | wait400 | -25.6 | -2.72 | -80.8 | -6.88 |
| 5%, no jitter | base | +14.4 | 1.54 | | |
| 5%, no jitter | nack30 | +19.4 | 1.65 | +5.0 | 0.36 |
| 5%, no jitter | wait400 | +2.0 | 0.36 | -12.4 | -1.31 |
| 5%, 20 ms keeping order | base | +113.2 | 5.94 | | |
| 5%, 20 ms keeping order | nack30 | +27.8 | 3.24 | -85.4 | -4.29 |
| 5%, 20 ms keeping order | wait400 | -8.0 | -0.41 | -121.2 | -4.54 |

### Counters

| Profile | Arm | Chrome RTT, ms | NACKed (Sapflow) | NACKs Chrome received | Chrome retransmissions | Residual packets lost (Sapflow) | Frames skipped after loss | Proxy drops, client to server |
|---|---|---|---|---|---|---|---|---|
| 5%, 20 ms reordering | base | 73 to 90 | 488 to 1179 | 306 to 830 | 440 to 810 | 8 to 12 | 44 to 72 | 292 to 407 |
| 5%, 20 ms reordering | nack30 | 80 to 90 | 641 to 1403 | 454 to 917 | 452 to 940 | 0 to 5 | 0 to 53 | 252 to 436 |
| 5%, 20 ms reordering | wait400 | 70 to 93 | 641 to 826 | 423 to 522 | 524 to 720 | 0 | 0 to 1 | 304 to 339 |
| 5%, no jitter | base | 63 to 64 | 273 to 361 | 223 to 295 | 258 to 340 | 0 to 5 | 0 to 29 | 397 to 597 |
| 5%, no jitter | nack30 | 62 to 63 | 639 to 921 | 495 to 741 | 279 to 356 | 1 to 3 | 7 to 41 | 411 to 683 |
| 5%, no jitter | wait400 | 62 to 64 | 320 to 360 | 260 to 306 | 311 to 343 | 0 | 0 | 407 to 626 |
| 5%, 20 ms keeping order | base | 74 to 93 | 231 to 344 | 201 to 283 | 221 to 329 | 9 to 28 | 81 to 177 | 332 to 630 |
| 5%, 20 ms keeping order | nack30 | 70 to 90 | 649 to 1014 | 541 to 793 | 239 to 375 | 6 to 10 | 34 to 78 | 330 to 648 |
| 5%, 20 ms keeping order | wait400 | 74 to 99 | 230 to 383 | 200 to 336 | 216 to 355 | 0 to 1 | 0 to 17 | 307 to 443 |

SRT, for the same profiles: the proxy counted 6668 to 6781 retransmitted
data packets from libsrt with reordering jitter, 469 to 518 without jitter
and 741 to 812 with jitter that keeps order (Sapflow's SRT receiver counted
6342 to 6434, 454 to 491 and 692 to 767); Sapflow's SRT receiver dropped 0 to 2, 0 to 1 and 2 to 6
packets. As before, the reordering jitter, not the loss, makes most of
libsrt's retransmissions under the headline profile.

## What it shows

- **The baseline.** At the headline profile WHIP's baseline left 82.2
  frames unrecovered against SRT's 27.0 (t 3.68), so the qualification
  note's result (83.0 against 6.8) holds for WHIP. SRT's mean is higher
  than in that note (27.0 against 6.8; two seeds at 53 and 43), on a
  different day and host load; this run's SRT figure is the comparison
  used here. With jitter that keeps order the gap is again clear (162.6
  against 49.4, t 5.94). Without jitter it is not one worth claiming this
  time (22.4 against 8.0, t 1.54), where the note had 48.0 against 6.0
  (t 7.35) on three seeds.
- **A 400 ms wait (arm d).** It left the fewest frames unrecovered of the
  WHIP arms on all three profiles. Against the baseline: -80.8 frames with
  reordering jitter (t -6.88), -121.2 keeping order (t -4.54) and -12.4
  without jitter (t -1.31, no difference worth claiming). Against SRT at
  200 ms: fewer at the headline profile (1.4 against 27.0, t -2.72; the
  run's two highest end loads, 16.3 and 22.9, were SRT's seeds 1 and 2 of
  that profile, so load may have raised SRT's mean there), and no
  difference worth claiming on the other two (t 0.36 and -0.41). Sapflow's
  receiver was left with 0 or 1 unrepaired packets per trial, against 0 to
  28 for the baseline, and NACK traffic was about the baseline's.
- **Faster NACK repeats (arm c).** Against the baseline it left fewer
  frames with jitter (-48.0, t -2.54, reordering; -85.4, t -4.29, keeping
  order), and no difference worth claiming without jitter (+5.0, t 0.36).
  Against SRT: no difference worth claiming at the headline profile (34.2
  against 27.0, t 0.41) or without jitter (t 1.65), and more keeping order
  (77.2 against 49.4, t 3.24). Without jitter and keeping order its NACKs
  were two to three times the baseline's (639 to 921 against 273 to 361
  without jitter), while Chrome's retransmissions stayed close to the
  baseline's (279 to 356 against 258 to 340); with reordering jitter the
  ranges overlap. That fits Chrome declining most repeats that come within its RTT of
  a resend; it is inferred from the counters, not read from Chrome.
- **What is left after a 400 ms wait.** Without jitter and keeping order,
  most of arm d's remaining frames were lost after the receiver, not by
  it. At the headline profile the few frames left (0 to 4) were not. The
  split is approximate: the receiver's frame count comes from a debug line
  logged at most once a second, and some recordings held more frames than
  its last value. With that caveat, Sapflow's frame counter was within
  about 24 frames of Chrome's `framesSent` in every arm d trial, and on
  those two profiles the recording had up to about 91 fewer frames than
  the receiver produced. Those trials logged the muxer's "media broke off (stall, late
  keyframe or jump in time); new segment from the next keyframe" with a gap
  of 31 to 85 ms, and video then waited for the next keyframe. The same
  message appears in some baseline and arm c trials. Why a gap that short
  breaks off the segment after a longer wait was not investigated here.
- **Latency.** A 400 ms wait can hold a frame up to 200 ms longer behind a
  gap than the default. End-to-end latency was not measured in this run.

## Limits

- Five seeds per arm and profile, one host, one Chrome version, the proxy's
  random loss model and a 60 to 100 ms round trip. A |t| near the 2.3 line
  (arm c against the baseline at the headline profile, -2.54; arm d against
  SRT, -2.72) is close to the rule's threshold.
- The load gate held each start below 8, but loads rose during some trials
  (to 22.9 at one SRT trial's end, 16.7 at one baseline trial's end).
  Interleaving spreads that across arms; it does not remove it.
- Arm c's 30 ms spacing is fixed, not scaled to the round trip, and was
  chosen for this test's round trip (above).
- Frames unrecovered counts what reached the recording, so it includes the
  muxer's drops as well as the receiver's.
- Audio is outside the gate: Chrome offers no NACK for Opus, and Sapflow
  filled 58 to 95 Opus packets with silence per lossy trial in every WHIP
  arm.
