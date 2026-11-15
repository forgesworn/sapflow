# WHIP client retest and loss at the 400 ms default

Status: evidence, 15 November 2026, on `test/whip-client-retest-20261115`,
cut from main at `705220f`. Main now has the DTLS fixes for A4.4 findings 7
to 12, the cookie exchange behind the off-by-default `hello-verify` feature,
and the WHIP wait raised to 400 ms by the owner's decision 9 in
[the second qualification round](whip-qualification-2-2026-11-12.md). This
note reruns the publishers against that build and repeats A4.1's loss runs
at the new default, against SRT at both 200 and 400 ms latency. Raw tables
are in `fixtures/whip/client-retest-2026-11-15/` (`clients.tsv`,
`trials.tsv`, `summary.tsv`).

This note states the numbers. It declares no gate met and changes no
default.

## Host, build and tools

| Item | Value |
|---|---|
| Host | The test host, an Apple silicon Mac, shared with other sessions. Its default route is a VPN tunnel, as in W0. Its screen was locked throughout (`CGSSessionScreenIsLocked`) |
| Build | Release build of `705220f` for `aarch64-apple-darwin` (`sapflow`, `srt-loss-proxy`), built once on the host |
| Scripts | `scripts/whip-e2e.js` with a new `--browser chrome\|firefox\|safari` (`cef24e6`), `scripts/whip-obs.js` unchanged, `scripts/whip-loss.py` with per-arm SRT latency (`c120d28`) |
| Chrome | 157.0.8103.52, headless, temporary profile |
| Firefox | 160.0, from Mozilla's release download, installed unprivileged in the user's Applications folder. `codesign --verify --deep --strict` passed; `spctl -a -vv` gave "accepted, source=Notarized Developer ID", authority "Developer ID Application: Mozilla Corporation (43AQ936H96)". The image's SHA-256 was `08e2a87f6eb4c33163e4d8ccb0cf3c0336dae99747683760d154dc7d409d3813`. Headless, temporary profile, fake media prefs from `scripts/whip-w0/browser.js` |
| Safari | 26.6.2 through `safaridriver`, run as a LaunchAgent in the GUI session (`W0_SAFARI_AGENT=1`), default `webkit:WebRTC` capabilities (`DisableICECandidateFiltering` false), synthetic media (a 640x360 canvas and a WebAudio tone) |
| OBS | 32.0.4, through `scripts/whip-obs.js` (config backed up, dedicated profile, config restored and diffed) |
| hls.js | The copy in another local checkout's `node_modules`, outside this repository |
| Others | Node 24.14.0, FFmpeg 8.1.2, libsrt's `srt-live-transmit` 1.5.7 |

Every listener was on 127.0.0.1, at ports 47300 and up. Before the runs the
RFC 5769 STUN vectors passed (6 checks).

## Clients

The real Sapflow binary on loopback, one run each unless stated. `whip-e2e.js`
advertises one candidate, UDP or a passive ICE-TCP one, so the client has one
way in; it then plays the stream in hls.js, DELETEs the session, decodes
every recording whole with FFmpeg and checks the trace log for leaks. Every
run's server log showed `WHIP listening ... jitter_ms=400
nack_repeat="rtt*1.25"`, the new default with no setting.

| Client | Transport | Result | SRTP profile | Transport used | Playback | Recording | Session end |
|---|---|---|---|---|---|---|---|
| Chrome 157 | UDP | all 22 checks passed | `AeadAes128Gcm` | UDP host to UDP host | hls.js 10.1 s at 640x360, 206 frames decoded, 0 dropped, no stall | 1 file, complete; 247 H.264 and 612 Opus frames, 0 decode errors | deleted, nothing lost |
| Chrome 157 | ICE-TCP | all 24 checks passed | `AeadAes128Gcm` | TCP active (prflx) to TCP passive; 3 TCP connections | hls.js 10.1 s at 640x360, 206 frames, 0 dropped, no stall | 1 file, complete; 244 H.264 and 606 Opus frames, 0 decode errors | deleted, nothing lost |
| Firefox 160 | UDP, twice | not connected | none | none | none | none | handshake timeout |
| Firefox 160 | ICE-TCP | not connected | none | none | none | none | handshake timeout |
| Safari 26.6.2 | UDP, twice | ICE and DTLS connected; media not obtained | `AeadAes128Gcm` | UDP | none | none | consent expired; media timeout |
| Safari 26.6.2 | ICE-TCP | ICE and DTLS connected; media not obtained | `AeadAes128Gcm` | ICE-TCP, 1 TCP connection to the passive listener | none | none | media timeout |
| OBS 32.0.4 | UDP | connected, media received and recorded | `Aes128CmHmacSha1_80` | UDP | not played; playlist served | 1 file; 1067 H.264 frames at 1280x720 over 35.6 s and 1776 Opus frames, 0 decode errors | not ended by OBS (below) |

`clients.tsv` has the loads and the counts for every run.

### Chrome 157

Both runs passed every check, as before the DTLS changes: POST 201 with an
absolute `Location`, the A3.6 checks (nothing bound with WHIP off,
`SAPFLOW_WHIP_UDP_ADDR` alone refused), the stream ready 4607 ms (UDP) and
4623 ms (TCP) after the publication began (`ready_ms`), aligned by sender
reports, one
recording per served codec epoch, decoded whole, the session ended by
`DELETE` with nothing lost, and no token, offer address, port or publisher
address in the trace log (20 and 26 literals checked). Loads were 18.8 at
the start and 25.4 at the end of the pair.

### Firefox 160

Firefox did not connect, as in W0, and for the same host reason. Two
attempts, then stop:

1. Default prefs (fake media only): the offer had 4 UDP and 4 TCP host
   candidates, all mDNS names. Firefox formed no candidate pair at all with
   the one loopback candidate in the answer, sent no STUN check (Sapflow's
   trace log has none), and reported `iceConnectionState` `failed`. Its
   encoders sent 0 packets. Sapflow ended the session with
   `reason="handshake-timeout"` about 11 s after the POST.
2. `media.peerconnection.ice.loopback` true and
   `media.peerconnection.ice.obfuscate_host_addresses` false, over UDP and
   then ICE-TCP: the host candidates were now real addresses, but only the
   VPN tunnel's private address, not loopback. Again no pair, no check, no
   TCP connection to the ICE-TCP listener, and a handshake timeout each
   time.

This matches W0's blocked runs: on this host Firefox gathers candidates only
on the tunnel interface, and `ice.loopback` does not change that. Nothing on
the host's network was changed. A Firefox result against Sapflow needs the
tunnel down or another host. It says nothing about Sapflow's DTLS with
Firefox, whose ClientHello has still never reached a Sapflow endpoint.

### Safari 26.6.2

`safaridriver` created a session each time (remote automation is enabled),
and each session was ended with `DELETE /session`, the LaunchAgent booted
out and its plist moved to the Trash. The automation Safari outlived the
session delete each time; the first time it was ended by its PID by hand,
and after that by `whip-e2e.js`, which now ends the automation Safari a run
started, by PID.

- **Connection.** In all three runs Safari completed ICE and DTLS with
  Sapflow: `WHIP session connected profile=AeadAes128Gcm`, over UDP twice
  and over ICE-TCP once (one TCP connection to the passive listener on
  loopback). W0 saw Safari's ICE and ClientHello against a fake endpoint
  only; this is the first completed DTLS handshake from Safari with Sapflow.
- **Media.** None that could be used. The host's screen was locked, and the
  page reported `document.visibilityState` `hidden`. In the first run
  (canvas driven by `requestAnimationFrame`) Sapflow received 349 Opus
  packets in about 8 s, then nothing, and no video packet; its 35 keyframe
  requests went unanswered and the session ended on consent expiry. In the
  second UDP run and the TCP run (canvas on a 33 ms timer, and a progress
  beacon), the canvas drew 7 frames, Safari's outbound RTP counters stayed
  at 0 packets, the beacon stopped after 4 to 7 s, and the sessions ended
  on the 30 s media timeout. The page never got as far as playback, so the
  stream was never ready and nothing was recorded.

Reading: with the screen locked, Safari throttles and then suspends the
hidden page, so these runs show the connection, not media. A media run needs
the host's screen unlocked (the owner's step), or another host. The
`sudo safaridriver --enable` step was not needed.

### OBS 32.0.4

`scripts/whip-obs.js` with its defaults (x264 CBR at 2500 kbit/s, 10 s
keyframes, Sapflow asking for a keyframe every 2 s), for 30 s of media. OBS
was not running at the start; its config was backed up and restored, and
`diff -r` against the backup exited 0 with an empty `rsync --checksum`
dry run.

- **Connection.** Connected with `Aes128CmHmacSha1_80`, the only SRTP
  profile OBS offers (OBS's log: connect time 5523 ms). Aligned by sender
  reports.
- **Keyframes.** As in A3.10: 26 keyframe requests, and OBS's keyframes
  followed its own 10 s interval (4 received, 9.96 to 10.05 s apart). Its
  first keyframe came 4.6 s after the connection; the 136 frames before it
  were skipped. The stream was ready 14.7 s after the connection
  (`ready_ms=20184`, counted from the publication's start), when the next
  keyframe closed the first segment.
- **Playback.** Not played. A poll of the media playlist every 2 s got 404
  or 503 before the stream was ready and 200 after, in 15 polls (177 parts
  and 7 segments at the last).
- **Recording.** One file, 1067 H.264 frames at 1280x720 over 35.6 s and
  1776 Opus frames, which FFmpeg decoded whole with no error. No NACK was
  needed (no loss on loopback).
- **End.** `whip-obs.js` ends OBS with SIGTERM; OBS needed the second one.
  No `DELETE` reached Sapflow, so the session had not ended when the script
  stopped Sapflow 4 s later, and the recording finished `complete=false
  reason="server-stopped"`. That is the script's shutdown order, not a loss:
  the file decodes whole. Whether OBS sends `DELETE` when it is stopped from
  its own interface was not tested.

## Loss at the 400 ms default

### What was asked

The qualification note's open items for A4.1 after decision 9: SRT at a
400 ms latency had not been measured, so there was no comparison at the
same wait, and 1% and 10% loss had not been rerun at 400 ms. Three arms ran
in one session, back to back per seed in this order:

| Arm | Protocol | Wait or latency |
|---|---|---|
| `whip` | WHIP, Chrome 157 publishing | 400 ms, the default (no setting; every trial logged `jitter_ms=400`) |
| `srt200` | SRT, srt-live-transmit | latency 200 ms, as in the earlier runs |
| `srt400` | SRT, srt-live-transmit | latency 400 ms (`latency=400`; Sapflow's listener accepted 400 in every trial) |

### Method

As in the A4.1 retest: `srt-loss-proxy` for both protocols (`--protocol
whip` impairs only SRTP and SRTCP), 30 ms delay each way, random loss on
both directions with 0 to 20 ms of reordering jitter, seeds 1 to 5, a fresh
Sapflow, proxy and publisher per trial, three clean runs per arm first.
Frames unrecovered are frames sent less the video frames in Sapflow's
recording: 900 for SRT, Chrome's own `framesSent` for WHIP. Each trial
started only below a one-minute load of 8; trials waited 120 s in all for
that, at most 60 s for one.

```sh
scripts/whip-loss.py --bin <sapflow> --proxy <srt-loss-proxy> --out <dir> \
  --arms whip=whip,srt200=srt,srt400=srt+latency=400 \
  --profiles 5:20,1:20,10:20 --seeds 5 --clean 3 --max-load 8 --base 47340
```

### Results

All 54 trials became ready, no publisher reported an error, and no
recording had a decoder error, clean or lossy.

| Profile | Arm | Frames sent | Frames unrecovered, by seed | Mean (sd) | Share of frames sent | Load, start and end |
|---|---|---|---|---|---|---|
| clean | whip | 840 to 841 | 0 0 0 | 0.0 (0) | 0.0% | 1.2 to 5.1 |
| clean | srt200 | 900 | 1 1 1 | 1.0 (0) | 0.1% | 1.6 to 5.1 |
| clean | srt400 | 900 | 1 1 1 | 1.0 (0) | 0.1% | 1.2 to 4.1 |
| 5%, 20 ms reordering | whip | 500 to 726 | 2 8 0 0 3 | 2.6 (3.3) | 0.4% | 2.8 to 10.4 |
| 5%, 20 ms reordering | srt200 | 900 | 1 30 1 1 18 | 10.2 (13.3) | 1.1% | 2.2 to 9.5 |
| 5%, 20 ms reordering | srt400 | 900 | 1 1 1 1 1 | 1.0 (0) | 0.1% | 2.2 to 17.6 |
| 1%, 20 ms reordering | whip | 648 to 759 | 2 16 6 23 3 | 10.0 (9.1) | 1.4% | 3.0 to 6.8 |
| 1%, 20 ms reordering | srt200 | 900 | 1 1 1 1 1 | 1.0 (0) | 0.1% | 2.9 to 6.8 |
| 1%, 20 ms reordering | srt400 | 900 | 1 1 1 1 1 | 1.0 (0) | 0.1% | 2.3 to 6.8 |
| 10%, 20 ms reordering | whip | 185 to 264 | 39 12 20 37 48 | 31.2 (14.8) | 13.6% | 2.3 to 4.5 |
| 10%, 20 ms reordering | srt200 | 900 | 187 260 118 128 163 | 171.2 (56.8) | 19.0% | 2.3 to 4.5 |
| 10%, 20 ms reordering | srt400 | 900 | 58 1 29 1 16 | 21.0 (23.8) | 2.3% | 2.2 to 4.5 |

Every SRT trial, clean ones included, left 1 frame unrecovered at least, as
in the earlier runs.

### WHIP against each SRT arm

Differences in frames, WHIP less SRT, with the reading rule of the
qualification note (five seeds: a |t| below about 2.3 is no difference
worth claiming). `summary.tsv` has the same figures (the `srt400` pair in
its last two columns).

| Profile | Against SRT at 200 ms | Welch t | Against SRT at 400 ms | Welch t | SRT 400 less SRT 200 | Welch t |
|---|---|---|---|---|---|---|
| 5%, 20 ms reordering | -7.6 | -1.24 | +1.6 | 1.09 | -9.2 | -1.55 |
| 1%, 20 ms reordering | +9.0 | 2.20 | +9.0 | 2.20 | 0 | (no variance) |
| 10%, 20 ms reordering | -140.0 | -5.34 | +10.2 | 0.82 | -150.2 | -5.46 |

### Counters

| Profile | Chrome RTT, ms | NACKed (Sapflow) | NACKs Chrome received | Chrome retransmissions | Residual packets lost (Sapflow) | Frames skipped after loss | Proxy drops, client to server |
|---|---|---|---|---|---|---|---|
| 5%, 20 ms reordering | 76 to 97 | 490 to 731 | 326 to 488 | 486 to 709 | 0 to 1 | 0 to 7 | 283 to 348 |
| 1%, 20 ms reordering | 70 to 90 | 611 to 740 | 386 to 475 | 593 to 714 | 0 | 0 to 4 | 64 to 93 |
| 10%, 20 ms reordering | 77 to 97 | 238 to 423 | 162 to 301 | 184 to 304 | 0 to 3 | 0 to 17 | 323 to 376 |

SRT, retransmitted data packets counted by the proxy: at 200 ms latency
6529 to 6727 (5%), 6424 to 6532 (1%) and 6642 to 6913 (10%); at 400 ms 613
to 647, 198 to 239 and 1171 to 1247. Sapflow's SRT receiver dropped 0 to 2,
0 and 8 to 15 packets at 200 ms, and 0, 0 and 0 to 2 at 400 ms. As in the
earlier runs, the reordering jitter, not the loss, makes most of the
retransmissions at 200 ms; at 400 ms there were about a tenth as many.
Sapflow's SRT receiver waits for reordering for at most an eighth of the
latency before it NAKs (`SAPFLOW_SRT_REORDER_WAIT_MAX_MS`), so a longer
latency gives it longer; that this explains the drop is inferred, not
measured here.

### What it shows

- **5%, the headline profile.** WHIP at 400 ms left 2.6 frames
  unrecovered, SRT at 400 ms 1.0 and SRT at 200 ms 10.2. Neither difference
  for WHIP is one worth claiming (t 1.09 against SRT at 400 ms, -1.24
  against 200 ms). This is the comparison at the same wait that A4.1's rule
  asks for; by count, WHIP left 1.6 frames more than SRT, with t 1.09.
- **1%.** WHIP left 10.0 frames unrecovered, both SRT arms 1.0 in every
  trial; t 2.20 against each, just under the 2.3 line. The earlier runs
  at a 200 ms wait gave 9.4 against 2.0 (t 1.28).
- **10%.** By count WHIP left 31.2 frames against SRT's 21.0 at 400 ms (t
  0.82) and 171.2 at 200 ms (t -5.34). By share of the frames each sent it
  is 13.6% against 2.3% and 19.0%: Chrome sent only 185 to 264 of its
  usual 840 frames at 10% loss, as it lowers its frame rate under loss, so
  the counts are not like for like. The earlier runs had 50.8 frames
  against SRT 200's 130.8 (19.8% against 14.5%).
- **SRT at 400 ms against 200 ms.** Fewer frames at 10% (-150.2, t -5.46)
  and no difference worth claiming at 5% (t -1.55) or 1% (both 1.0).
- **Chrome's frames sent.** Under loss Chrome sent fewer frames than in
  the clean runs at every profile (500 to 726 at 5%, 648 to 759 at 1%),
  so WHIP's counts come from fewer frames than SRT's 900. The clean WHIP
  runs sent 840 to 841 here, against 881 to 886 in the A4.1 retest.

### Limits

- Five seeds per arm and profile, one host, one Chrome version, the proxy's
  random loss model, a 70 to 100 ms round trip, and only the reordering
  jitter profile (the no-jitter and order-keeping 5% profiles were not
  rerun).
- The load gate held each start below 8; one SRT 400 trial at 5% ended at
  a load of 17.6. Interleaving spreads that across arms; it does not
  remove it.
- Frames unrecovered counts what reached the recording, so it includes the
  muxer's drops as well as the receiver's. Audio is outside the measure.
- End-to-end latency was not measured, and nothing here bears on the
  muxer break-offs noted in the A4.1 retest.
