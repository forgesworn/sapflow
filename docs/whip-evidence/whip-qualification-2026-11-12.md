# WHIP qualification evidence: A3.8 sync, A3.9 smoke, A4.2 cost

Status: evidence, 12 November 2026, on `test/whip-qualification-lab` from
main at `1d68397`. One change to Sapflow: the WHIP server now logs how each
session's start was aligned (`WHIP timeline aligned alignment=...`, at debug),
which A3.8 needed. The rest is measurement tools (`scripts/whip-sync.js`,
`scripts/whip-cost.py`), the WHIP part of `scripts/smoke.sh`, and a CI step.
This note reports against [the WHIP ingest design](../whip-ingest.md) (A3.8,
A3.9, A4.2).

## Summary

| Gate | Result | Status |
|---|---|---|
| A3.8 sync (EBU R37: audio at most 40 ms early, 60 ms late) | Chrome 157: 17 of 20 runs within (UDP 9 of 10, ICE-TCP 8 of 10); the three others had audio 43 to 49 ms early. Every Chrome run was aligned by arrival time, because Chrome's sender reports for both streams had arrived only 3.4 to 3.5 s after it connected, after Sapflow's 1.5 s start hold. FFmpeg 8.1.2 (sender reports at once): 10 of 10, 0 to 1.5 ms | **Not met.** The cause is Sapflow's start hold, not Chrome: with the hold lengthened to 6 s (an experiment, not committed) every Chrome run was aligned by sender reports and all five were within, at -2.3 to +15.7 ms. A decision point (below) |
| A3.9 smoke | `scripts/smoke.sh` publishes WHIP with FFmpeg 8's WHIP muxer and checks refusals, LL-HLS, decoding, callbacks, `DELETE` and the recording; CI installs a pinned static FFmpeg 8.1.3 for it | **Met locally**: the whole smoke passed on the test host with Homebrew's FFmpeg 8.1.2. CI has not run it yet (not pushed). The pinned build ran in an emulated x86_64 Linux container (Debian 12) on the test host: it lists the WHIP muxer and encodes H.264 baseline and Opus; it has not published WHIP there |
| A4.2 cost | Not yet measured to method: the host's load average stayed above the gate of 8 (other sessions). The run is under way | Pending |

## Host and versions

| Item | Value |
|---|---|
| Host | Apple M4, macOS 26.6.2, shared with other sessions. The one-minute load average is in every table |
| Browser | Google Chrome 157.0.8103.52, headless (`--headless=new`), a fresh profile each run |
| FFmpeg | 8.1.2 (Homebrew: libx264, libopus, OpenSSL) for the WHIP reference publisher, the sources and the measurements |
| SRT | srt-live-transmit 1.5.7 |
| Node | 24.14.0 |
| Sapflow | this branch, release build for `aarch64-apple-darwin` |

## A3.8 Audio and video sync

### Method

`scripts/whip-sync.js` publishes a source with a white flash on black every
2 s and a 10 ms 1 kHz click at the same instants, to a fresh Sapflow on
loopback. Just before the publisher stops it fetches the served HLS (the init
and every segment the media playlist lists); after the 2 s grace it reads the
recording. In each file `signalstats` finds each flash's first frame and
`astats`, over 1 ms windows, each click's first millisecond above -30 dBFS.
The offset is click minus flash, within 500 ms: positive is audio late. A run
is within R37 when every offset, served and recorded, is between -40 and
+60 ms.

```sh
node scripts/whip-sync.js --bin target/aarch64-apple-darwin/release/sapflow --out <dir> \
  --publisher chrome --transport udp|tcp --runs 10 --seconds 20
node scripts/whip-sync.js --bin target/aarch64-apple-darwin/release/sapflow --out <dir> \
  --publisher ffmpeg --runs 10 --seconds 20
```

**The source is made in the page, not read from a file.** The gate asks for
a test file, and the harness first used Chrome's fake devices
(`--use-file-for-fake-video-capture`, y4m and then Motion-JPEG;
`--use-file-for-fake-audio-capture`, a wav). Headless Chrome 157 on macOS
delivered 7 or 8 video frames from either file and then no more, with or
without the in-page watchers, while audio flowed (once `--no-sandbox` let the
audio device read its file). The two fake devices also start independently,
so nothing would align the two files. The sanctioned fallback is used
instead: a canvas and a WebAudio click, both timed by the AudioContext's
clock. A frame goes to the canvas track (`requestFrame`) when the colour
changes and otherwise every 33 ms, so about 30 fps (a recording measured
28.9 fps). The canvas turns white on the animation frame nearest each click,
up to 8.3 ms either side; the page records by how much (2.7 to 8 ms either
way in these runs).

**The source's own offset is measured in the page.** Watchers
(`MediaStreamTrackProcessor`) on clones of both tracks note, on the page's
clock, when each flash's first frame and each click's first loud sample
reach them. That is the source column; net is the served median less the
source median, so what Chrome's sending and Sapflow add between them. The
tracks' own timestamps do not compare: Chrome's canvas track starts its
timestamps at 0 at its first frame, the audio track's are near the page
clock (first video frame 0 µs at page time 237.6 ms; first audio data
229,699 µs at 240.0 ms), and their difference moved from 225 to 260 ms between
runs, so they were dropped.

**Sapflow's start alignment is read from its log** (`sapflow::whip` at
debug). Before this branch the log said only `aligned`.

The early trials, before the source was settled, are kept because they
moved the result: drawing the flash by the output timestamp
(`getOutputTimestamp`) gave -66 ms, and drawing it on the first animation
frame after the click (always late, up to 16.7 ms) gave -43 and -47 ms with
net -30 ms. Neither is in the tables below.

### Chrome 157 over UDP, stock Sapflow

| Run | Load | Alignment | HLS offsets, ms (n, min to max) | Recording offsets, ms (n, min to max) | Source, ms (median) | Net, ms | Within |
|---|---|---|---|---|---|---|---|
| 1 | 4.72 | Arrival(Timeout) | 6, -43.2 to -40.4 | 8, -43.2 to -40.4 | -0.4 | -42.2 | **no** |
| 2 | 4.11 | Arrival(Timeout) | 6, -16.3 to -13.6 | 8, -16.7 to -13.6 | 3.7 | -17.6 | yes |
| 3 | 4.72 | Arrival(Timeout) | 6, -15.3 to -10.5 | 8, -15.3 to -10.5 | 1.5 | -14.1 | yes |
| 4 | 4.67 | Arrival(Timeout) | 6, -31.3 to -27.3 | 8, -35.1 to -27.3 | 1.2 | -28.6 | yes |
| 5 | 5.74 | Arrival(Timeout) | 6, -17.1 to -15.5 | 8, -18.2 to -15.5 | 0 | -16.4 | yes |
| 6 | 5.82 | Arrival(Timeout) | 6, -21.8 to -17.5 | 8, -21.8 to -17.5 | -5.6 | -15.5 | yes |
| 7 | 5.7 | Arrival(Timeout) | 6, -11.8 to -7.4 | 8, -11.8 to -7.4 | -3.4 | -4.6 | yes |
| 8 | 5.54 | Arrival(Timeout) | 6, -14 to -10.5 | 8, -14 to -10.5 | -3.1 | -9.8 | yes |
| 9 | 7.19 | Arrival(Timeout) | 6, -4.7 to -2.4 | 8, -4.7 to -2.4 | 2.5 | -6.6 | yes |
| 10 | 6.21 | Arrival(Timeout) | 6, -39 to -35.3 | 8, -39 to -35.3 | -4 | -33.9 | yes |

All 60 served offsets: -43.2 to -2.4 ms, median -15.6; all 80 recorded:
-43.2 to -2.4, median -16.3. Net -42.2 to -4.6 ms (median -15.5, spread
37.6 ms); within a run the served offsets spread 4.8 ms at most.

### Chrome 157 over ICE-TCP, stock Sapflow

| Run | Load | Alignment | HLS offsets, ms (n, min to max) | Recording offsets, ms (n, min to max) | Source, ms (median) | Net, ms | Within |
|---|---|---|---|---|---|---|---|
| 1 | 5.1 | Arrival(Timeout) | 6, -19.6 to -17.9 | 8, -20.5 to -17.9 | -2.8 | -15.9 | yes |
| 2 | 5.28 | Arrival(Timeout) | 6, -9.7 to -8.3 | 8, -9.7 to -6.6 | 2.6 | -11.1 | yes |
| 3 | 5.27 | Arrival(Timeout) | 6, -22.9 to -18.7 | 8, -22.9 to -18.7 | 0.1 | -20.8 | yes |
| 4 | 12.89 | Arrival(Timeout) | 6, -27.4 to -26.3 | 8, -27.5 to -26.3 | -1.7 | -25.3 | yes |
| 5 | 28.53 | Arrival(Timeout) | 6, -27.5 to -25.1 | 8, -27.5 to -25.1 | 0.5 | -25.8 | yes |
| 6 | 27.44 | Arrival(Timeout) | 6, -13.1 to -11.6 | 8, -13.1 to -11.6 | 7.4 | -19.4 | yes |
| 7 | 30.75 | Arrival(Timeout) | 6, -13.7 to -10.7 | 8, -13.7 to -10.7 | 6.6 | -18.4 | yes |
| 8 | 38.69 | Arrival(Timeout) | 5, -28.4 to -26.7 | 8, -28.4 to -24.8 | 1.8 | -29.7 | yes |
| 9 | 47.55 | Arrival(Timeout) | 6, -41.6 to -34.6 | 8, -41.6 to -34.6 | -3 | -32.7 | **no** |
| 10 | 54.79 | Arrival(Timeout) | 6, -49.3 to -45.9 | 8, -49.3 to -45.9 | 5 | -51 | **no** |

All 59 served offsets: -49.3 to -8.3 ms, median -22.9; all 80 recorded:
-49.3 to -6.6, median -22.9. Net -51 to -11.1 ms (median -20.8, spread
39.9 ms); within a run the served offsets spread 7.0 ms at most. Every run's
nominated pair was ICE-TCP (UDP in the table above), from Chrome's own
statistics.

### FFmpeg 8.1.2 over UDP, stock Sapflow (the reference)

FFmpeg sends its sender reports with its first packets, so every run was
aligned by them, 1 ms after the session connected. Its timestamps are the
file's, so this is what Sapflow adds.

| Run | Load | Alignment | HLS offsets, ms (n, min to max) | Recording offsets, ms (n, min to max) | Within |
|---|---|---|---|---|---|
| 1 | 43.4 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 2 | 31.34 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 3 | 21.58 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 4 | 15.8 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 5 | 19.97 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 6 | 31.95 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 7 | 43.06 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 8 | 41.76 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |
| 9 | 30.86 | SenderReports | 7, 1 to 1.5 | 9, 1 to 1.5 | yes |
| 10 | 24.46 | SenderReports | 7, 0 to 0.5 | 9, 0 to 0.5 | yes |

All 70 served offsets 0 to 1.5 ms (median 0.4), all 90 recorded 0 to 1.5 ms
(median 0.5), under load averages of 16 to 43.

### Chrome 157 over UDP, start hold lengthened to 6 s (experiment)

A build with `TimelineConfig::start_hold` at 6 s instead of 1.5 s, made for
this run and not committed. Every run was aligned by sender reports, 3.38 to
3.52 s after the session connected, which is when Chrome's reports for both
streams had arrived.

| Run | Load | Alignment | HLS offsets, ms (n, min to max) | Recording offsets, ms (n, min to max) | Source, ms (median) | Net, ms | Within |
|---|---|---|---|---|---|---|---|
| 1 | 17.71 | SenderReports | 6, 10.2 to 14 | 8, 10.2 to 14.3 | 3.7 | 7.2 | yes |
| 2 | 11.86 | SenderReports | 6, -0.7 to 15.7 | 8, -0.7 to 15.7 | -5.4 | 20 | yes |
| 3 | 8.72 | SenderReports | 6, -2.3 to 14.7 | 8, -2.3 to 14.7 | 7.1 | 7.4 | yes |
| 4 | 7.43 | SenderReports | 6, 7.8 to 12.1 | 8, 7.8 to 12.1 | 2.2 | 8.7 | yes |
| 5 | 18.98 | SenderReports | 6, 0.4 to 8.4 | 8, 0.4 to 8.7 | -2.9 | 4.8 | yes |

All 30 served offsets -2.3 to 15.7 ms (median 10.7); net 4.8 to 20 ms
(median 7.4). Within a run the offsets moved by up to 17 ms, more than with
arrival alignment (7 ms), as later reports were followed. The streams were
ready (`stream ready`) 5.6 to 5.9 s after the publisher connected, with
either hold.

### Findings

1. **Chrome's first sender reports come after Sapflow's start hold.** The
   hold waits 1.5 s from the first media packet for a report on every
   stream. Chrome's reports for both streams had arrived 3.4 to 3.5 s after
   the session connected (the 6 s runs), and every one of the 20 stock
   Chrome runs logged `Arrival(Timeout)`, about 1.52 s after it connected.
   So with Chrome the timeline is never aligned by sender reports.
2. **After an arrival start the offset stays where the start put it.** The
   timeline takes a stream's next agreeing report as its reference after an
   arrival alignment (`sapflow-rtp`'s `timeline.rs`), and the offsets held
   steady within each run, so the offset is set by which packets arrived
   first: Chrome's video against its audio (presumably its encoding and
   pacing; that was not measured). That is -2.4 to -49.3 ms here (audio early), the
   same within a run (spread at most 7 ms) and different between runs
   (spread 38 to 40 ms), including at a load average under 5 (UDP run 1,
   -42.2 ms net). The failures were UDP run 1 (load 4.7) and ICE-TCP runs 9
   and 10 (load 48 and 55).
3. **Sapflow's own arithmetic adds nothing measurable** with FFmpeg's
   reports: 0 to 1.5 ms. With Chrome's (the 6 s runs), net 4.8 to 20 ms
   (audio late), well inside R37.
4. **The source is not the cause.** Its own offset was within 10 ms either
   way (median -0.5 ms UDP, 1.7 ms ICE-TCP), and net, which takes it out,
   fails the same way.
5. **ICE-TCP did not differ in kind from UDP.** Its runs came as the load
   rose; its two failures were at the two highest load averages of the
   campaign.
6. **The publisher's share.** Taking FFmpeg's 0 to 1.5 ms as Sapflow's, what
   Chrome's sending adds is, with its reports used, 4.8 to 20 ms net (five
   runs, UDP), and with arrival alignment -4.6 to -51 ms net, which is
   Chrome's video against audio delay at the start as Sapflow then takes it.
   Chrome's skew between its RTP timestamps and its sender reports was not
   measured directly: SRTCP encrypts the reports on the wire, Sapflow does
   not log their contents, and in the page the two tracks' timestamps do
   not share an origin.

So A3.8 is not met as built, and the fault is Sapflow's start hold, not
Chrome. Nothing in Sapflow was changed for it. It is a decision point:

- **Lengthen the start hold for WHIP**, to about 4 s (Chrome's reports came
  by 3.5 s here). Measured with 6 s: every run inside R37. The cost is
  memory held at the start (bounded by `max_hold_bytes`, 4 MiB, and half the
  event queue) and, in principle, later first media; here readiness did not
  move, since it was 5.6 s either way. A publisher whose reports come later
  still would fall back as now. Its evidence is thin: five runs, UDP only,
  live latency not measured, and other browsers' report timing unknown.
- **Or keep 1.5 s and correct to the first agreeing report pair** after an
  arrival start, instead of taking it as the reference: slewing video at up
  to 5% closes 50 ms in about a second, with no discontinuity. That changes
  `sapflow-rtp`'s timeline, its harness model and its fuzz oracles.
- **Or accept the arrival offset** in writing, since 17 of 20 runs were
  inside the range and the worst was 9 ms past it.


## A3.9 Smoke

`scripts/smoke.sh` now ends with a WHIP part (`whip_smoke`), on a fourth
server with only WHIP and HTTP for it (SRT on an unused port, RTMP off),
sharing the callback receiver:

- a `POST` with a wrong token, and one with none, get `401`, and the wrong
  token is not in the log;
- FFmpeg's WHIP muxer publishes 12 s of 1280x720 30 fps H.264 (constrained
  baseline, no B-frames) and 48 kHz stereo Opus over ICE-lite UDP on
  loopback (`-whip_flags dtls_active`, since FFmpeg's default
  `setup:passive` is refused);
- the multivariant playlist's `CODECS` is `avc1.<profile>,Opus`, the media
  playlist has parts, and the segments decode (H.264 1280x720, Opus 48 kHz
  stereo) with no error;
- the `publish` callback arrives;
- FFmpeg's `DELETE` at the end ends the session (`reason="deleted"`), the
  `unpublish` callback arrives, and after the grace the
  `recordSegmentComplete` callback names a recording that decodes.

**Publisher: FFmpeg 8's WHIP muxer.** Of the three choices, it is the
simplest that is honest: a real, independent WHIP client (Sapflow's own code
plays no part on the sending side), one process, no browser, and the same
tool the smoke already uses. Headless Chrome would need Chrome or Chromium
and a driver in the smoke job, and `scripts/whip-e2e.js` already covers it.
An in-repo client would test Sapflow against itself. Ubuntu 24.04's ffmpeg
(6.1) has no WHIP muxer, so CI downloads a pinned static build for this part
only, checks its SHA-256 and sets `SMOKE_WHIP_FFMPEG`:

| Item | Value |
|---|---|
| Build | FFmpeg-Builds `autobuild-2026-10-31-12-59`, `ffmpeg-n8.1.3-27-g8dca1586f4-linux64-gpl-8.1.tar.xz` (a month-end build, which that project keeps for two years) |
| SHA-256 | `41e9c565594c390622cf968d82fa4135d9e9a11d06c1515644288cbc844e3380` |
| Configuration | `--enable-openssl`, libx264, libopus; the WHIP muxer is present |
| Checked | in an x86_64 Debian 12 container (emulated, on the test host): `ffmpeg version n8.1.3-27-g8dca1586f4-20261031`, `E whip` in `-muxers`, and 1 s of 1280x720 H.264 baseline with no B-frames and stereo Opus encoded with no error. Not a WHIP publish: that is CI's first run |

Locally, `SMOKE_WHIP_FFMPEG` is not needed when `ffmpeg` is 8 or later; the
part fails with that message otherwise. It adds about 15 s to the smoke
(93.8 s in all on the test host, with a debug build).

Local run (debug build for `aarch64-apple-darwin`, `SAPFLOW_HTTP_IMPL=own`,
`SAPFLOW_HTTP_THREADS=1`, as CI sets them; load average 14.9):

```text
ok   WHIP refusals (401 for a missing and a wrong token, the token not logged)
ok   WHIP publish (FFmpeg): LL-HLS with H.264 and Opus decodes, publish callback
ok   WHIP DELETE: session ended, unpublish callback, recording (<n>.mp4)
ok   SIGTERM stops the server cleanly
PASS
```

The server's log for the part: the wrong token refused as
`reason=Request(Unauthorized)`, the session ended `reason="deleted"` with
`lost=0`, and the recording finished `complete=true` at 11.9 s. The
multivariant playlist read `CODECS="avc1.42c01f,Opus"`.

A first version sent the wrong token with a placeholder body (`v=0`) and got
`422`: the offer is parsed before admission checks the token, as the design
lists. A missing token is refused before that, so it can stay a bare `curl`;
the wrong token now comes from FFmpeg with a real offer.

## A4.2 Cost

`scripts/whip-cost.py` follows [benchmarks.md](../benchmarks.md) section B
for SRT, RTMP and WHIP on the same host: 1, 10 and 50 publishers into a fresh
server (`SAPFLOW_MAX_PUBLISHERS=100`, no viewers, no recording), the v4v
settings (1280x720, 30 fps, H.264 2.5 Mbit/s, 1 s keyframes) encoded once
into a 150 s clip and stream-copied (`ffmpeg -re -stream_loop -1 -c copy`).
SRT and RTMP carry AAC 128 kbit/s as before; WHIP carries the same video with
Opus 128 kbit/s through FFmpeg's WHIP muxer, since WHIP needs Opus and
constrained baseline H.264 (the clip is baseline for all three). Publishers
start 0.1 s apart (WHIP 0.3 s, under the per-address pending cap of 4). The
window starts 25 s after every playlist has a segment and a part and lasts
45 s; server CPU is its CPU time over the window, memory the peak of one
sample a second; every playlist must have advanced by at least 30 segments
and every publisher must still run. Three rounds, the protocol order rotating
each round. Only the sapflow process is measured; the publishers' CPU is
listed beside it. A run starts once the one-minute load average is below 8;
one whose 600 s wait ran out is recorded but is not a result.

```sh
scripts/whip-cost.py --bin target/aarch64-apple-darwin/release/sapflow --out <dir> \
  --streams 1,10,50 --rounds 3 --gate-wait 600
```

**The verdict rule, set before the numbers.** For each N, in each round where
both the SRT and the WHIP run are results, the WHIP/SRT ratios of CPU per
stream and of RSS per stream. At that N, WHIP passes if every ratio is at
most 1, fails if every ratio is above 1, and otherwise the rounds disagree
and the call is the owner's. If the gate never opens, A4.2 is not measured to
method, and runs above the gate are shown only as labelled indicative
figures.

Results: not yet measured to method. The host's load average stayed above the gate (other sessions), and the run is under way.
