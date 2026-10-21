# Encoder interoperability

Which real encoders have published to sapflow, and how. Add a row when one
is tried.

| Date | Encoder | Protocol | Settings | Result |
|------|---------|----------|----------|--------|
| 2026-10-20 | ffmpeg 8.0.1 | RTMP, SRT | `scripts/smoke.sh`, v4v's ingest gate | Pass |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) | SRT | `scripts/smoke.sh` | Pass |
| 2026-10-20 | OBS 32.1.2 | RTMP | x264 2500 kbit/s CBR, 1 s keyframes, no B-frames; credentials in the stream key | Servable in 1.1 s, ran 3 min 20 s, clean unpublish |
| 2026-10-20 | OBS 32.1.2 (libsrt 1.5.2) | SRT | as above, `latency=2000000` as v4v's Studio sets | Servable in 3.2 s |
| 2026-10-20 | OBS 32.1.2 | RTMP | OBS's default x264 (250-frame keyframes, B-frames) | Servable in 9.3 s; one 8.33 s GOP per segment, target duration 8; decodes cleanly |
| 2026-10-20 | Larix Broadcaster (iPhone) | RTMP | Larix defaults (H.264 Baseline 720p30, AAC mono, 2 s keyframes); credentials in the URL | Servable in 1.0 s; 2 s segments decode cleanly, A/V start within 7 ms |
| 2026-10-20 | Larix Broadcaster (iPhone) | SRT | Larix defaults, credentials in the `streamid`, 2 s latency | Servable in 4.5 s; 24 segments decode cleanly; 4 late packets dropped once over Wi-Fi (one segment 59 frames of 60) |
| 2026-10-20 | Larix Broadcaster (iPhone), Safari on macOS playing | SRT | Larix defaults over Wi-Fi | Safari refused the stream (crossed-out play button) after Wi-Fi stalls (one frame of 0.367 s) raised `PART-TARGET` to 0.534 s while parts stayed 0.2 to 0.233 s. Fixed in a94da2f: `PART-TARGET` fixed at 0.234 s, long frames clamped (`docs/reference.md`, Part timing) |
| 2026-10-21 | Larix Broadcaster (iPhone), Safari on macOS playing | SRT | Larix defaults, Wi-Fi switched off briefly, served by a94da2f | Part rules held (`PART-TARGET` 0.234 throughout), but the drop left a 6 s segment and `EXT-X-TARGETDURATION` rose from 2 to 6; Safari stopped. Fixed in d480bb9: the target is fixed and a drop ends the segment, marked as a discontinuity (`docs/reference.md`, Target duration and breaks). An SRT silence of 5 s still ends the stream (M7) |
| 2026-10-21 | Larix Broadcaster (iPhone), Safari on macOS playing | SRT (`SAPFLOW_SRT_IMPL=own`) and RTMP at once | Larix defaults over Wi-Fi, served by d480bb9; Wi-Fi switched off twice | Both streams stayed live through drops of 3.1 and 4.4 s, each marked as a discontinuity; Safari carried on (by eye); recordings 98.6 s (SRT) and 107.8 s (RTMP). One RTMP reconnect was refused as already live until the old connection timed out, then accepted 3 s later |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) | SRT, `SAPFLOW_SRT_IMPL=own` | `SRT_IMPLS="srt-rs own" scripts/smoke.sh` | Pass with both implementations (refusals 400, 401, 409 included) |
| 2026-10-20 | ffmpeg 8.1.2 (libsrt 1.5.3, in Docker) | SRT, `SAPFLOW_SRT_IMPL=own` | 720p30 x264 2.5 Mbit/s, `latency=2000000` | Accepted at 2000 ms latency, servable in 3.5 s, clean shutdown and unpublish |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) with `passphrase=` | SRT, `SAPFLOW_SRT_IMPL=own` | 200 ms | Refused in the handshake: libsrt reports `ERROR:UNSECURE`, "Password required or unexpected" (encryption is M6b) |

Output checked for the 1 s keyframe runs: H.264 High 1280x720 30 fps and
AAC-LC 48 kHz stereo, 1 s segments, 200 ms parts, a keyframe at the start of
each segment.

With OBS's defaults the stream plays, but segments follow the keyframes, so
viewers join up to about 8 s behind. v4v's advice to set 1 s keyframes
matters.

## Reproducing irregular encoder timing

`scripts/irregular-publish.sh` publishes ffmpeg's test signal with Larix's
timing faults: jittered frames, dropped frames and a 0.367 s stall every
10 s, and a network drop every 30 s (both tracks stop for `STALL` seconds,
default 3, and video resumes on a frame that is not a keyframe).
`scripts/check-playlist.py` polls a playlist and reports any render that
breaks the rules in `docs/reference.md` (part rules, a fixed
`TARGETDURATION` and `PART-TARGET`, `EXTINF` within the target,
consistent discontinuities). Against a sapflow on spare ports, so a running
one is left alone:

```bash
SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=0.0.0.0:18882 SAPFLOW_SRT_ADDR=0.0.0.0:18872 \
  SAPFLOW_RTMP_ADDR=0.0.0.0:11982 ./target/debug/sapflow &
scripts/irregular-publish.sh srt 127.0.0.1:18872 event-irr 600 &
python3 -I scripts/check-playlist.py http://127.0.0.1:18882/live/event-irr/stream.m3u8 60
```

Safari plays `http://<host>:18882/live/event-irr/index.m3u8`. On
2026-10-21, 80 s with two drops, the same stream to three builds:

| Build | SRT | RTMP |
|-------|-----|------|
| main (04ab35d) | `TARGETDURATION` 2 then 4, `PART-TARGET` 0.234 up to 3.067, about 500 rule failures | 2 then 4, 0.233 up to 3.100, about 500 |
| a94da2f (part timing only) | `TARGETDURATION` 2 then 4; `PART-TARGET` 0.234 throughout | 2 then 4; 0.234 throughout |
| d480bb9 | `TARGETDURATION` 3, `PART-TARGET` 0.234 throughout, two discontinuities, longest `EXTINF` 2.0 s, no render broke a rule | the same |

A recording of 60 s of it from d480bb9 lasts 50.8 s (the drops, and the
video waiting for keyframes, are left out), decodes cleanly and seeks.

## Delay through the server

`scripts/ingest-lag.py` publishes ffmpeg's test signal in real time and
times when each part appears in the playlist against when ffmpeg sent its
last frame: SRT latency plus whatever the server adds, without the player.
On 2026-10-21, own SRT and RTMP on the same server, 30 to 40 s each:

| Publish | Median lag | p90 |
|---------|------------|-----|
| RTMP | -0.23 s | 0.03 s |
| SRT, 200 ms latency | -0.12 s | 0.16 s |
| SRT, 2000 ms latency | 1.68 s | 1.97 s |
| SRT, 200 ms, ffmpeg's MPEG-TS defaults | 1.91 s | 2.12 s |

ffmpeg's read burst at the start puts every sample about 0.3 s early, so a
part is servable about when the SRT latency has passed: the server adds
nothing measurable. ffmpeg's own MPEG-TS defaults (`muxdelay`, PES
packing) add about 1.7 s, the same against srt-rs; `-muxdelay 0
-muxpreload 0 -pes_payload_size 0 -flush_packets 1` removes it. That is
the sender's delay, worth knowing when an ffmpeg-based encoder seems slow.

## Not yet tried

- Larix on Android.
- The phone locked or backgrounded; SRT with a passphrase.
- OBS reconnecting after a drop of over 5 s, which the grace period now
  carries on (Larix: see Stopping and restarting).
- OBS's Apple hardware encoder; vMix.

## Stopping and restarting

Tried with Larix over RTMP on 2026-10-20, before the grace period:
stopping the broadcast ended the stream at once (the playlist was 404) and
restarting gave a new stream (media sequence 0, new chunk names). Since
then a publisher that leaves, or a silent one dropped (SRT after 5 s, RTMP
after 10 s), has its stream held for `SAPFLOW_RECONNECT_GRACE` (default
90 s): a restart within it carries on the same playlist and recording
after a discontinuity (`docs/reference.md`, Target duration and breaks).
With ffmpeg, the smoke test and v4v's ingest gates cover it; hls.js was
watched crossing a reconnect.

On 2026-10-21 Larix (iPhone, Wi-Fi) published over RTMP and our own SRT at
once. The RTMP connection dropped seven times in six minutes (the phone
closing it, gone 10 to 58 s each time); every reconnect resumed the held
stream, with `unpublish` at the drop and `publish` on the return. Safari
kept playing but ended about 10 s behind the camera, as each break left it
further back. The SRT stream had one break. Larix's first two SRT attempts
that session were accepted but sent no data, and timed out after 5 s; its
third, 20 s later, ran cleanly. Not yet explained. Not yet tried with OBS restarting.

## SRT under packet loss (2026-10-20)

`scripts/srt-loss.sh` compares sapflow's SRT receiver with libsrt's under
packet loss, with several seeds per condition. libsrt's `srt-live-transmit`
is the sender in every trial. It plays a generated 30 s clip (720p30,
2.5 Mbit/s, about 7,200 SRT packets) in real time through `srt-loss-proxy`
(in `crates/sapflow-bench`), which drops packets in both directions, delays
each by 30 ms (a 60 ms round trip), keeps them in order and never drops
handshakes. The receiver is sapflow or libsrt's own `srt-live-transmit`.
Conditions: random loss of 3, 10 and 20%, and bursty loss (Gilbert-Elliott:
8% on average, bursts of 8 packets on average), each at an SRT latency of
2000 ms and of 200 ms. Six seeds per condition; every cell is the mean and
sample standard deviation over them, and the receivers alternate seed by
seed so both meet the same machine load.

- **Retransmission overhead:** retransmitted data packets as a percentage of
  the original ones, counted by the proxy from the SRT headers. With no
  waste it is p / (1 - p) for a loss rate p (11% at 10%).
- **Not recovered in time:** original data packets for which no copy reached
  the receiver within the latency of the first send, counted by the proxy.
  The same definition for both receivers, whatever they do with the packets.
- **Video and audio delivered:** packets in the receiver's output against
  the same receiver's loss-free run. Decode messages: lines `ffmpeg -v error`
  prints decoding it. These two depend on what each receiver writes. sapflow
  demuxes the stream and drops frames a lost packet damaged; libsrt writes the
  damaged stream as it is, so its delivered count stays near 100% and its
  decode messages are many (89 in its loss-free 2000 ms run). Compare them
  between sapflow builds, not with libsrt.

Receivers: `upstream` is `srt-protocol` as published (no `NAKREPORT` or
`TLPKTDROP` in the handshake; asks again after 2, 3, 4... RTTs); `flags +
RTT` has the first two patches in `vendor/srt-protocol/PATCHES.md`
(advertise both, ask every RTT); `current` adds the deadline-aware
re-requests; `libsrt` is libsrt's receiver.

| Loss | Latency | Receiver | n | Retransmission overhead | Not recovered in time | Video delivered | Audio delivered | Decode messages |
| ------ | --------: | ---------- | --: | ------------------------: | ----------------------: | ----------------: | ----------------: | ----------------: |
| 3% random | 2000 ms | upstream | 6 | 20.9% ± 2.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | flags + RTT | 6 | 3.4% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | current | 6 | 3.5% ± 0.3 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | libsrt | 6 | 3.5% ± 0.1 | 0.00% ± 0.01 | 100.07% ± 0.06 | 99.96% ± 0.09 | 31.0 ± 44.9 |
| 10% random | 2000 ms | upstream | 6 | 73.9% ± 4.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | flags + RTT | 6 | 11.7% ± 0.4 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | current | 6 | 11.8% ± 0.5 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | libsrt | 6 | 12.6% ± 0.6 | 0.00% ± 0.01 | 100.02% ± 0.05 | 99.96% ± 0.09 | 74.5 ± 35.5 |
| 20% random | 2000 ms | upstream | 6 | 149.3% ± 3.6 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 20% random | 2000 ms | flags + RTT | 6 | 26.6% ± 0.5 | 0.00% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 20% random | 2000 ms | current | 6 | 26.8% ± 0.7 | 0.01% ± 0.01 | 99.92% ± 0.12 | 100.00% ± 0.00 | 0.8 ± 1.3 |
| 20% random | 2000 ms | libsrt | 6 | 29.1% ± 0.9 | 0.00% ± 0.01 | 100.07% ± 0.06 | 99.96% ± 0.09 | 31.0 ± 44.9 |
| bursty 8% | 2000 ms | upstream | 6 | 54.7% ± 7.4 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| bursty 8% | 2000 ms | flags + RTT | 6 | 9.8% ± 1.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| bursty 8% | 2000 ms | current | 6 | 9.8% ± 1.0 | 0.04% ± 0.07 | 99.88% ± 0.23 | 99.90% ± 0.24 | 1.2 ± 1.8 |
| bursty 8% | 2000 ms | libsrt | 6 | 10.7% ± 1.0 | 0.03% ± 0.06 | 100.04% ± 0.09 | 99.94% ± 0.15 | 45.3 ± 47.5 |
| 3% random | 200 ms | upstream | 6 | 19.0% ± 3.7 | 0.08% ± 0.03 | 99.86% ± 0.19 | 100.00% ± 0.00 | 2.0 ± 1.8 |
| 3% random | 200 ms | flags + RTT | 6 | 3.3% ± 0.2 | 0.03% ± 0.02 | 99.90% ± 0.11 | 99.70% ± 0.33 | 1.2 ± 1.5 |
| 3% random | 200 ms | current | 6 | 4.1% ± 0.4 | 0.01% ± 0.02 | 99.96% ± 0.09 | 99.90% ± 0.24 | 0.5 ± 1.2 |
| 3% random | 200 ms | libsrt | 6 | 3.4% ± 0.1 | 0.02% ± 0.01 | 99.89% ± 0.00 | 100.00% ± 0.00 | 90.3 ± 1.6 |
| 10% random | 200 ms | upstream | 6 | 67.9% ± 11.2 | 1.10% ± 0.42 | 92.36% ± 5.12 | 93.94% ± 4.77 | 90.5 ± 45.5 |
| 10% random | 200 ms | flags + RTT | 6 | 11.9% ± 0.4 | 0.64% ± 0.45 | 94.46% ± 4.99 | 93.68% ± 5.75 | 54.5 ± 45.8 |
| 10% random | 200 ms | current | 6 | 14.5% ± 1.2 | 0.37% ± 0.36 | 97.26% ± 4.24 | 97.72% ± 3.64 | 28.5 ± 35.2 |
| 10% random | 200 ms | libsrt | 6 | 13.4% ± 1.5 | 0.39% ± 0.54 | 99.94% ± 0.06 | 99.49% ± 0.53 | 70.8 ± 47.3 |
| 20% random | 200 ms | upstream | 6 | 110.9% ± 25.9 | 3.93% ± 1.62 | 73.53% ± 10.90 | 70.70% ± 11.99 | 276.8 ± 76.5 |
| 20% random | 200 ms | flags + RTT | 6 | 24.2% ± 2.0 | 3.98% ± 1.78 | 74.23% ± 13.88 | 69.93% ± 15.59 | 266.2 ± 83.6 |
| 20% random | 200 ms | current | 6 | 29.2% ± 0.6 | 2.41% ± 1.28 | 83.78% ± 9.81 | 83.32% ± 10.32 | 175.0 ± 73.3 |
| 20% random | 200 ms | libsrt | 6 | 29.0% ± 2.3 | 2.20% ± 1.29 | 99.89% ± 0.07 | 97.20% ± 2.33 | 203.2 ± 94.1 |
| bursty 8% | 200 ms | upstream | 6 | 49.5% ± 4.6 | 0.41% ± 0.22 | 98.96% ± 0.43 | 98.91% ± 0.88 | 12.7 ± 3.1 |
| bursty 8% | 200 ms | flags + RTT | 6 | 9.3% ± 1.2 | 1.26% ± 0.34 | 98.32% ± 0.39 | 97.92% ± 0.50 | 15.8 ± 3.3 |
| bursty 8% | 200 ms | current | 6 | 11.2% ± 1.3 | 0.88% ± 0.26 | 98.59% ± 0.42 | 98.41% ± 0.49 | 14.0 ± 5.1 |
| bursty 8% | 200 ms | libsrt | 6 | 10.2% ± 1.0 | 0.89% ± 0.39 | 99.72% ± 0.18 | 99.56% ± 0.43 | 79.8 ± 35.3 |

`current` minus libsrt, with a Welch t statistic over the seeds (with 6
seeds, |t| below about 2.3 is noise):

| Loss | Latency | Overhead, points | t | Not recovered, points | t |
| ------ | --------: | -----------------: | --: | ----------------------: | --: |
| 3% random | 2000 ms | -0.1 | -0.4 | -0.00 | -1.0 |
| 10% random | 2000 ms | -0.8 | -2.6 | -0.00 | -1.0 |
| 20% random | 2000 ms | -2.3 | -4.9 | +0.01 | 1.9 |
| bursty 8% | 2000 ms | -0.9 | -1.5 | +0.01 | 0.3 |
| 3% random | 200 ms | +0.7 | 4.5 | -0.00 | -0.5 |
| 10% random | 200 ms | +1.0 | 1.3 | -0.02 | -0.1 |
| 20% random | 200 ms | +0.2 | 0.3 | +0.22 | 0.3 |
| bursty 8% | 200 ms | +1.0 | 1.4 | -0.01 | -0.0 |

What it shows:

- **The handshake patch matters most.** `upstream` leaves libsrt's sender to
  retransmit on its own timer: 21, 74 and 149% overhead at 3, 10 and 20%
  random loss (55% bursty), against 3.5, 12.6 and 29.1% (10.7%) for libsrt's
  receiver. Every receiver recovered everything at 2000 ms; the difference is
  what it cost.
- **At 2000 ms `current` is level with libsrt's overhead or a little lower**:
  0.8 points lower at 10% loss (t -2.6) and 2.3 lower at 20% (t -4.9), with
  the same complete delivery. Up to 8% fewer retransmissions is the only
  respect in which it comes out ahead.
- **At 200 ms `current` equals libsrt.** Packets not recovered in time differ
  by -0.02, +0.22 and -0.01 points at 10%, 20% and bursty loss (|t| 0.3 or
  less), and overhead is level at 20% and 0.7 to 1.0 points above libsrt's
  at 3%, 10% and bursty loss. The deadline-aware change is what closes the
  gap: `flags + RTT` leaves 1.78 points more unrecovered than libsrt at 20%
  (t 2.0) and 0.37 more under bursts (t 1.8), borderline at 6 seeds. Pooling
  the three batches that ran both builds (11 or 12 same-seed pairs), the
  change leaves 1.6, 0.45 and 0.2 points fewer packets unrecovered at 20%,
  10% and bursty loss (t 9.7, 4.8 and 2.4), at 0.6 to 0.8 points more
  overhead than libsrt at 3%, 10% and bursty loss (t 3 to 6).
- **It is not better than libsrt.** Nothing here shows `current` delivering
  more than libsrt at any condition; the spread does not support that claim.
- **Tried and dropped:** also running the NAK timer every quarter RTT while a
  loss is near its deadline. At 20% loss and 200 ms (6 seeds, a separate
  batch) it left 1.11% ± 0.23 unrecovered against 1.94% ± 0.12 without it
  and 1.52% ± 0.30 for libsrt, but at 33.4% ± 1.9 overhead against 29.5% ±
  1.1 and 27.9% ± 1.3. At 10% and bursty loss it gained nothing beyond the
  noise for about 3 points more overhead than without it. It buys delivery
  with retransmissions and libsrt sits on the better side of that trade, so
  it is not kept.
- **`upstream` recovers bursts better at 200 ms** (0.41% ± 0.22 unrecovered
  against libsrt's 0.89% ± 0.39, t -2.6) because the sender retransmits on
  its own timer too, for 49.5% overhead against 10.2%.

Limits: one sender (libsrt 1.5.4, which OBS and Larix also use), one clip and
loopback with an emulated delay and independent loss in both directions, so
NAKs and ACKs are lost as often as data; no jitter, reordering, long round
trips, encryption or several connections. The machine was shared and busy
(load average from 13 to over 180 across these runs). Interleaving shares
that load between receivers within a batch, but absolute numbers move between
batches (libsrt at 20% and 200 ms was 2.2% unrecovered in one batch and 1.5%
in another), so compare within a batch. A full run with four receivers takes
about 40 minutes: `scripts/srt-loss.sh`, or `RECEIVERS="a=/path/sapflow-a
libsrt"` to compare builds.

## Our own SRT under packet loss (2026-10-20, M6a)

The same harness and conditions as above, with three receivers: `own` is
sapflow with `SAPFLOW_SRT_IMPL=own` (`sapflow-srt`), `srt-rs` the same
binary on the patched `srt-protocol` (what was `current` above), and
`libsrt` libsrt's `srt-live-transmit`. Six seeds per condition, receivers
interleaved seed by seed, `RECEIVERS="srt-rs own libsrt" scripts/srt-loss.sh`.

| Loss | Latency | Receiver | n | Retransmission overhead | Not recovered in time | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| 3% random | 2000 ms | srt-rs | 6 | 3.4% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | own | 6 | 3.3% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | libsrt | 6 | 3.3% ± 0.1 | 0.00% ± 0.00 | 100.02% ± 0.05 | 100.00% ± 0.00 | 74.5 ± 35.5 |
| 10% random | 2000 ms | srt-rs | 6 | 11.9% ± 0.4 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | own | 6 | 11.6% ± 0.4 | 0.00% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | libsrt | 6 | 12.3% ± 0.5 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 89.0 ± 0.0 |
| 20% random | 2000 ms | srt-rs | 6 | 26.5% ± 0.7 | 0.01% ± 0.01 | 99.85% ± 0.27 | 99.70% ± 0.50 | 1.3 ± 2.0 |
| 20% random | 2000 ms | own | 6 | 25.7% ± 0.8 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 20% random | 2000 ms | libsrt | 6 | 27.6% ± 0.9 | 0.00% ± 0.01 | 100.04% ± 0.06 | 99.96% ± 0.09 | 60.0 ± 44.9 |
| bursty 8% | 2000 ms | srt-rs | 6 | 10.0% ± 1.2 | 0.08% ± 0.12 | 99.77% ± 0.29 | 99.80% ± 0.31 | 1.7 ± 1.9 |
| bursty 8% | 2000 ms | own | 6 | 9.6% ± 1.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| bursty 8% | 2000 ms | libsrt | 6 | 10.1% ± 1.0 | 0.00% ± 0.00 | 100.04% ± 0.06 | 100.00% ± 0.00 | 60.0 ± 44.9 |
| 3% random | 200 ms | srt-rs | 6 | 4.0% ± 0.4 | 0.02% ± 0.01 | 99.88% ± 0.00 | 99.90% ± 0.24 | 2.5 ± 1.4 |
| 3% random | 200 ms | own | 6 | 3.4% ± 0.2 | 0.00% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 200 ms | libsrt | 6 | 3.5% ± 0.2 | 0.01% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 89.3 ± 0.8 |
| 10% random | 200 ms | srt-rs | 6 | 13.9% ± 1.0 | 0.20% ± 0.04 | 99.00% ± 0.47 | 99.01% ± 0.61 | 11.7 ± 1.8 |
| 10% random | 200 ms | own | 6 | 12.4% ± 0.7 | 0.13% ± 0.02 | 99.90% ± 0.17 | 99.60% ± 0.72 | 8.5 ± 3.0 |
| 10% random | 200 ms | libsrt | 6 | 12.4% ± 0.4 | 0.19% ± 0.03 | 100.00% ± 0.00 | 99.80% ± 0.30 | 100.0 ± 2.4 |
| 20% random | 200 ms | srt-rs | 6 | 28.1% ± 1.1 | 1.27% ± 0.18 | 92.03% ± 2.00 | 91.99% ± 3.12 | 101.5 ± 14.1 |
| 20% random | 200 ms | own | 6 | 26.1% ± 0.7 | 1.00% ± 0.08 | 99.19% ± 0.34 | 98.80% ± 1.19 | 65.3 ± 12.4 |
| 20% random | 200 ms | libsrt | 6 | 27.2% ± 1.0 | 1.40% ± 0.10 | 99.98% ± 0.05 | 98.12% ± 0.97 | 174.2 ± 6.3 |
| bursty 8% | 200 ms | srt-rs | 6 | 11.1% ± 1.7 | 0.93% ± 0.51 | 98.69% ± 0.98 | 98.81% ± 1.46 | 10.3 ± 6.3 |
| bursty 8% | 200 ms | own | 6 | 9.7% ± 1.2 | 0.83% ± 0.44 | 99.48% ± 0.52 | 99.60% ± 0.61 | 9.2 ± 8.3 |
| bursty 8% | 200 ms | libsrt | 6 | 10.2% ± 0.8 | 0.87% ± 0.37 | 99.94% ± 0.17 | 99.64% ± 0.45 | 66.8 ± 46.0 |

Each sapflow receiver minus libsrt, with a Welch t statistic over the seeds
(with 6 seeds, |t| below about 2.3 is noise):

| Loss | Latency | Receiver | Overhead, points | t | Not recovered, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| 3% random | 2000 ms | srt-rs | +0.0 | 0.6 | +0.00 | - | -0.02 | -1.0 | -74.5 | -5.1 |
| 3% random | 2000 ms | own | -0.1 | -0.9 | +0.00 | - | -0.02 | -1.0 | -74.5 | -5.1 |
| 10% random | 2000 ms | srt-rs | -0.4 | -1.7 | +0.00 | - | +0.00 | - | -89.0 | - |
| 10% random | 2000 ms | own | -0.8 | -3.0 | +0.00 | 1.0 | +0.00 | - | -89.0 | - |
| 20% random | 2000 ms | srt-rs | -1.0 | -2.2 | +0.01 | 1.3 | -0.19 | -1.7 | -58.7 | -3.2 |
| 20% random | 2000 ms | own | -1.8 | -3.8 | -0.00 | -1.0 | -0.04 | -1.6 | -60.0 | -3.3 |
| bursty 8% | 2000 ms | srt-rs | -0.1 | -0.2 | +0.08 | 1.6 | -0.27 | -2.2 | -58.3 | -3.2 |
| bursty 8% | 2000 ms | own | -0.5 | -0.9 | +0.00 | - | -0.04 | -1.6 | -60.0 | -3.3 |
| 3% random | 200 ms | srt-rs | +0.5 | 3.0 | +0.01 | 1.3 | -0.12 | - | -86.8 | -132.8 |
| 3% random | 200 ms | own | -0.0 | -0.3 | -0.01 | -1.5 | +0.00 | - | -89.3 | -268.0 |
| 10% random | 200 ms | srt-rs | +1.6 | 3.8 | +0.00 | 0.2 | -1.00 | -5.3 | -88.3 | -71.9 |
| 10% random | 200 ms | own | +0.0 | 0.1 | -0.06 | -4.5 | -0.10 | -1.4 | -91.5 | -57.7 |
| 20% random | 200 ms | srt-rs | +0.9 | 1.4 | -0.13 | -1.5 | -7.95 | -9.7 | -72.7 | -11.5 |
| 20% random | 200 ms | own | -1.1 | -2.1 | -0.40 | -7.3 | -0.79 | -5.7 | -108.8 | -19.1 |
| bursty 8% | 200 ms | srt-rs | +0.9 | 1.1 | +0.07 | 0.3 | -1.26 | -3.1 | -56.5 | -3.0 |
| bursty 8% | 200 ms | own | -0.5 | -0.8 | -0.04 | -0.2 | -0.47 | -2.1 | -57.7 | -3.0 |

What it shows (the binary was commit 4722f34; the machine was shared, load
average between about 8 and 17 when checked):

- **`own` is no worse than libsrt anywhere and no worse than srt-rs
  anywhere.** Retransmission overhead is level with libsrt's or lower:
  0.8 points lower at 10% random and 2000 ms (t -3.0), 1.8 lower at 20%
  random and 2000 ms (t -3.8), 1.1 lower at 20% random and 200 ms (t -2.1),
  level within noise at every other condition. It is at or below srt-rs's
  overhead at every condition.
- **At 200 ms it leaves fewer packets unrecovered than libsrt** at 10% and
  20% random loss: 0.13% against 0.19% (t -4.5) and 1.00% against 1.40%
  (t -7.3). Under bursts the two are level (0.83% against 0.87%). At 2000 ms
  all three recover everything, apart from srt-rs's 0.08% under bursts.
- **srt-rs delivers less video than `own` at 200 ms under loss** (92.0%
  against 99.2% at 20% random) by more than its unrecovered packets
  explain. The differential test found why: when srt-protocol skips a packet
  as too late it discards the next one too, even one that arrived in time
  (`docs/reference.md`). `own` drops only the packets that are missing.
- Video and audio delivered and decode messages still compare sapflow
  builds only: libsrt writes damaged streams out as they are.

Limits as above: one sender (libsrt 1.5.4), one clip, loopback with an
emulated 60 ms round trip and independent loss both ways, a shared machine.

How it got there:

- **The first full run failed "no worse than srt-rs" at 2000 ms.** `own`
  asked again for a loss exactly one RTT after the last request, and its
  retransmission overhead was 28.9% ± 0.5 against srt-rs's 26.7% ± 0.7 at
  20% random loss, and 12.2% ± 1.2 against 9.9% ± 0.9 (libsrt 10.2% ± 1.1,
  t 3.0) under bursts. At 200 ms it was already level with libsrt or
  better.
- **Why:** a bursty 2000 ms session recorded between the loss proxy and each
  receiver (seed 1, 15 s) showed `own` asking twice for 115 of 329 losses
  against srt-rs's 41 of 320. The retransmission arrived a median 63 ms
  after the first request (90th percentile 64 ms); `own`'s RTT estimate was
  63.6 ms with a variance of 0.6 ms, so its second request went out just
  before the packet it asked for arrived, and the libsrt sender sent it
  again. srt-rs's estimate on the same link reads about 70 ms, which leaves
  it a margin.
- **Fix:** ask again after RTT + 4 x RTTVar (the specification's allowance
  for a late but normal round trip), keeping the quarter-RTT re-requests
  near the deadline. On a 4-seed check at 2000 ms that brought overhead to
  25.9% ± 0.6 at 20% random (srt-rs 26.9% ± 0.6, libsrt 27.5% ± 0.8) and
  9.3% ± 1.2 under bursts (9.4% ± 1.2, 10.4% ± 1.4), with 200 ms unchanged.
  A wider margin (at least RTT/8) measured the same and was not kept.
