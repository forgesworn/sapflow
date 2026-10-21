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
| 2026-10-21 | Larix Broadcaster (iPhone), Safari on macOS playing | SRT | Larix defaults, Wi-Fi switched off briefly, served by a94da2f | Part rules held (`PART-TARGET` 0.234 throughout), but the drop left a 6 s segment and `EXT-X-TARGETDURATION` rose from 2 to 6; Safari stopped. Fixed in d480bb9: the target is fixed and a drop ends the segment, marked as a discontinuity (`docs/reference.md`, Target duration and breaks). An SRT silence of 5 s then ended the stream; since M7 the stream is held for a reconnect (`SAPFLOW_RECONNECT_GRACE`) |
| 2026-10-21 | Larix Broadcaster (iPhone), Safari on macOS playing | SRT (`SAPFLOW_SRT_IMPL=own`) and RTMP at once | Larix defaults over Wi-Fi, served by d480bb9; Wi-Fi switched off twice | Both streams stayed live through drops of 3.1 and 4.4 s, each marked as a discontinuity; Safari carried on (by eye); recordings 98.6 s (SRT) and 107.8 s (RTMP). One RTMP reconnect was refused as already live until the old connection timed out, then accepted 3 s later |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) | SRT, `SAPFLOW_SRT_IMPL=own` | `SRT_IMPLS="srt-rs own" scripts/smoke.sh` | Pass with both implementations (refusals 400, 401, 409 included) |
| 2026-10-20 | ffmpeg 8.1.2 (libsrt 1.5.3, in Docker) | SRT, `SAPFLOW_SRT_IMPL=own` | 720p30 x264 2.5 Mbit/s, `latency=2000000` | Accepted at 2000 ms latency, servable in 3.5 s, clean shutdown and unpublish |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) with `passphrase=` | SRT, `SAPFLOW_SRT_IMPL=own` | 200 ms | Refused in the handshake: libsrt reports `ERROR:UNSECURE`, "Password required or unexpected" (encryption is M6b) |
| 2026-10-21 | OBS 32.1.2 (libsrt 1.5.2) | SRT, own receiver (the default) | OBS's default x264 (8.33 s keyframes), browser source, `latency=2000000`; served by 8ee5704 | Servable in 19.7 s (the long GOP); a segment decodes cleanly (250 frames, AAC). OBS quit and restarted 62 s later: resumed the held stream, one discontinuity, same chunk names |
| 2026-10-21 | OBS 32.1.2 (libsrt 1.5.2) | SRT with `passphrase=` and `pbkeylen=16`, own receiver | as above, server `SAPFLOW_SRT_PASSPHRASE` set | Servable in 19.6 s, decodes cleanly; with one letter of the passphrase wrong, refused in the handshake ("wrong passphrase or key length") |
| 2026-10-21 | Larix Broadcaster (iPhone) | SRT with a passphrase (AES-128), own receiver | Larix defaults over Wi-Fi, 2 s latency; served by 8ee5704 | Servable in 6.0 s, a segment decodes cleanly (60 frames, AAC). With the server's passphrase changed by one letter: refused nine times, about every 3 s; with it changed back, Larix reconnected by itself |

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

Glass to glass, 2026-10-21: Larix on an iPhone over Wi-Fi, our own SRT at
2 s latency, 2 s keyframes, filming a millisecond clock on the Mac, played
by Safari (macOS) straight from sapflow over plain HTTP/1.1 on the LAN: 10.8
s in two samples, on a freshly loaded page. Safari held about 8.8 s of
media ahead of the playhead, so about 9 s of the delay is the player's
starting distance from the live edge; Larix and the SRT latency are the
rest. Safari made blocking reloads and fetched every part, yet positioned
about 3 x `TARGETDURATION` back rather than `PART-HOLD-BACK`. mediamtx
(`latest-ffmpeg` image) given the same ffmpeg stream (2 s keyframes) side
by side behaved the same way relative to its own `TARGETDURATION` (2 against
our 3): 5.5 s held against 9.4 s, so sapflow played about 4 s further
behind.

The same Larix stream through a TLS and HTTP/2 front (Node's `http2`
module with a locally trusted certificate, standing in for Caddy): 3.0 and
3.2 s glass to glass, with Safari holding almost nothing ahead of the
playhead. Safari takes the low-latency position only over HTTP/2, as
Apple's LL-HLS guidance says; over HTTP/1.1 it falls back to 3 x
`TARGETDURATION`. Production goes through Caddy with HTTP/2, so measure
latency that way: here plain HTTP overstated it by about 7.7 s (10.8 s
against 3.0 to 3.2 s) at 2 s keyframes. The 2 s SRT latency is most of what remains.

## Behind Caddy, in Docker on the Mac (2026-10-21)

A stand-in for the M7 item "measured on Linux behind Caddy" while no second
wired machine is free. Caddy 2.11.7 (the official `caddy:2` image) runs in
Docker Desktop on the same Mac as sapflow (8ee5704, release build, on the
host) and the test clients. `tls internal` gives it its own CA, trusted only
by those clients. One port serves TLS with HTTP/2 (and offers HTTP/3);
`handle_path /live-hls/*` then `reverse_proxy` to sapflow's HLS port, the
shape `docs/v4v-contract.md` describes. The publisher is ffmpeg over RTMP,
720p30 x264 at 2.5 Mbit/s, 200 ms parts, keyframes every 1 s or 2 s.

```
HTTP_PORT=28888 RTMP_PORT=28935 CADDY_PORT=28943 SAPFLOW_BIN=<sapflow> \
  PLAYWRIGHT_DIR=<playwright package> HLS_JS=<hls.min.js> \
  scripts/behind-caddy.sh lag blocking check player     # KEYINT=2: 2 s keyframes
```

`scripts/behind-caddy.Caddyfile` is the Caddyfile. The clients are
`scripts/behind-caddy.mjs` (Node's `http2`, no packages) and
`scripts/behind-caddy-player.mjs` (hls.js 1.7.1 in an already installed
Playwright and Chrome; nothing was downloaded). The Mac was busy with other
work throughout (one-minute load average 20 to 177 on 8 cores), so absolute
times wobble; what counts was measured on the same stream at the same moment
by both paths.

**What Caddy needed:** nothing beyond the plain `reverse_proxy`. Tried
without effect: `flush_interval -1` (bodies have a length and are held until
ready, so there is nothing to flush early; two runs of 200 blocking reloads
cost a median 2.3 and 2.2 ms, against 2.4 and 2.2 ms for the plain config)
and `encode gzip zstd` (its default match leaves playlists and fMP4 alone: no
`Content-Encoding` on any response, with browser-style `Accept-Encoding`
offered). Forcing gzip onto playlists works (2,001 bytes became 417, median
cost 2.4 and 1.8 ms) but saves little. Caddy added `alt-svc` and
`via: 1.1 Caddy` and dropped nothing. sapflow sets
`access-control-allow-origin` itself, so Caddy should not add another.

**Correct through Caddy** (`check`, at both keyframe intervals, no failures):

- HTTP/2 negotiated (ALPN `h2`; `curl --http2` reports "using HTTP/2").
- The multivariant playlist, init segment, parts and segments identical to
  direct byte for byte, with the same `content-type`, `cache-control`
  (`no-cache` for playlists, `public, max-age=3600` for media),
  `content-length` and CORS header. Media playlists fetched in pairs just
  after a part appeared: 40 of 40 identical in each of four runs.
- 404 (unknown stream, unknown chunk) and 400 (`_HLS_msn=abc`, `_HLS_part`
  without `_HLS_msn`, `_HLS_msn` far ahead) kept, with empty bodies and the
  CORS header; the CORS preflight still 204.
- A preload-hinted part asked for before it existed was held until it did,
  then identical (12 of 12 in each run; about 200 ms, 2 to 5 ms over direct).
- A request held for sapflow's whole limit ended in its own 503 as soon as
  direct did: 6.00 against 6.04 s (`TARGETDURATION` 2), 9.00 against 9.03 s
  (3). Caddy cuts nothing short.
- `scripts/check-playlist.py` through Caddy (HTTP/1.1 over TLS: it has no
  HTTP/2) and direct: every render kept the rules.

**Cost and delay.** The first three rows are Caddy minus direct, the same
request asked on both paths at once; the last two are absolute.

| Measure | 1 s keyframes | 2 s keyframes |
|---------|---------------|---------------|
| Blocking reload for the next part, 200 samples, median / p90 / p99 | 1.6 / 3.0 / 16 ms | 1.3 / 3.5 / 24 ms |
| The same against a second plain connection (noise floor) | -0.03 / 0.6 / 22 ms | -0.01 / 0.4 / 5.1 ms |
| First sight of each new part, 445 parts, median / p90 / p99 | 1.7 / 4.1 / 15 ms | 2.0 / 6.0 / 16.5 ms |
| hls.js `lowLatencyMode`, Chrome 155 headless, direct over HTTP/1.1 | 1.51 s (1.17 to 2.11) | 2.14 s (1.08 to 3.50) |
| The same through Caddy over HTTP/2 | 1.62 s (1.21 to 2.17) | 2.10 s (1.18 to 3.60) |

Every blocking reload carried the part it asked for (none early), and no part
seen directly was missed through Caddy (0 of 453).

What each includes. The first three rows time sapflow to a client on
loopback: Caddy, TLS, HTTP/2 framing and Docker Desktop's two network hops,
with no player and no encoder (a native Caddy on Linux has fewer hops, so
this probably overstates its cost). The hls.js rows are `hls.latency`, the
playlist's live edge as hls.js last loaded it minus the playhead; the wall
clock against the programme date-time of the frame on screen agreed within
0.05 s. They include where hls.js chose to start and the age of its playlist;
they exclude the encoder, the ingest hop and the time sapflow takes to form a
part. Each is the median of the run medians (lowest to highest) over 8 and 6
runs of 70 s, direct and Caddy playing the same stream together, in
alternating start order. The spread is where hls.js happens to start: in the
first set the page that started second sat about 0.1 s further back (Caddy
second: +0.15 s on average, first: -0.08 s). Caddy minus direct averaged
+0.03 s (1 s keyframes) and +0.07 s (2 s), inside that effect. hls.js held
1.5 s (1 s keyframes) and 2.1 s (2 s) back; its target is `PART-HOLD-BACK`,
0.585 s.

Absolute part lag, measured as `scripts/ingest-lag.py` does, was the same on
both paths: -0.43 s on the quietest 90 s run at 1 s keyframes (every third of
the run alike), against -0.31 s from `ingest-lag.py` itself (2 s keyframes)
on the same machine; that is the encoder start-up bias noted above. On busier
runs the spread widened (p90 +0.27 s), and at 2 s keyframes the lag drifted
from +1.4 to -0.4 s over the run on both paths together, probably ffmpeg
starved of CPU.

Over four minutes both players drifted back together (2.9 to 5.9 s direct and
2.5 to 5.5 s through Caddy at 1 s keyframes; 1.7 to 4.1 s and 1.7 to 4.0 s at
2 s). hls.js does not speed up to catch the edge, and under that load the
browser played 1.2 to 1.8% slower than real time; hence the 70 s runs. Frames
dropped ranged from 0 to 5% a run: through Caddy 2.6% against 0.9% direct in
the first set but 1.1% against 1.0% in the second, so not put down to the
path.

**What this does not show.** Chrome does not show the difference Safari shows
between HTTP/1.1 and HTTP/2 (9 s against 3 s, earlier in this file), so the
Safari figure still rests on the Node stand-in. Caddy passed the same bytes
and headers over HTTP/2, so there is no reason Safari would differ, but nobody
has watched it through Caddy. Chrome stayed on HTTP/2 for every response
although Caddy offered HTTP/3 (its UDP port was published), so HTTP/3 is
untried.

**Still needs the wired Linux machine:** Caddy on Linux with a real
certificate and the real v4v Caddyfile (its `encode`, `header`, logging and
timeouts could each change this); Safari on an iPhone and a Mac through it;
HTTP/3; the 250 and 750 full-viewer runs through Caddy (`sapflow-bench` speaks
plain HTTP/1.1 only), and Caddy's CPU at that load; Caddy and sapflow on
separate hosts; and a quiet machine for a long soak, which the drift above
rules out here. `CADDY_URL` points the script at a Caddy that is already
running; `CADDY_NETWORK=host` runs the container on Linux's host network.

## Not yet tried

- Larix on Android.
- The phone locked or backgrounded.
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
third, 20 s later, ran cleanly. Not yet explained. OBS quit and
restarted 62 s later also resumed its held stream (table above).

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

## SRT under packet loss, encrypted (2026-10-21)

The harness of the two sections above with a passphrase on: `PASSPHRASE`,
`PBKEYLEN` and `KMREFRESH` in `scripts/srt-loss.sh` give libsrt's sender and
receiver `passphrase`, `pbkeylen`, and on the sender `kmrefreshrate` and
`kmpreannounce` (a quarter of the refresh rate), and sapflow
`SAPFLOW_SRT_PASSPHRASE` and `SAPFLOW_SRT_PBKEYLEN`. The sender is libsrt
1.5.4 in every trial, so it also does the key refreshes. The binary is a
release build of commit 8ee5704 (our own SRT, which has the passphrase).
Receivers: `own` is sapflow, `libsrt` is libsrt's `srt-live-transmit`. A
sapflow receiver not named `own` would run the patched srt-rs, which has no
passphrase, so the script refuses that combination.

```
PASSPHRASE=loss-test-key PBKEYLEN=16 KMREFRESH=1500 SEEDS=6 JOBS=2 \
  RECEIVERS="own libsrt" scripts/srt-loss.sh
PASSPHRASE=loss-test-key PBKEYLEN=32 KMREFRESH=2000 SEEDS=3 JOBS=2 \
  LOSSES="random:10 burst:8,8" RECEIVERS="own libsrt" scripts/srt-loss.sh
```

A 30 s clip is about 7,180 original data packets (7,142 to 7,182 across the
trials). A refresh every 1,500 packets sends the new key 375 packets ahead
(about 1.5 s), switches at about packets 1,500, 3,000, 4,500 and 6,000, and
so makes four switches in a clip; every 2,000 gives three. libsrt prints
`SRTO_KMREFRESHRATE=...: setting SRTO_KMPREANNOUNCE=...` when it starts: it
clamps its default pre-announce of 4096 packets before applying the one in
the URL. The URL value is the one in force (libsrt rejects one above
(refresh - 1) / 2), and the capture below shows the first KMREQ about 375
packets before the first switch.

Before the runs, three checks that encryption is on and that the key
refresh happens:

- **A wrong passphrase delivers nothing.** With a different passphrase on
  the sender (`SENDER_PASSPHRASE`), 4 of 4 short trials (10 s clip, one per
  receiver with and without loss) failed: both receivers refuse the
  connection in the handshake (libsrt logs `BADSECRET`, `Incorrect
  passphrase`), and no media was recorded or written.
- **The payload on the wire is not MPEG-TS.** A throwaway relay between the
  sender and the proxy (not in the repository) read the SRT headers in a 30 s
  trial at 20% random loss and 200 ms against each receiver, and in a third
  at AES-256 and 10%. Of 9,980 data packets (originals and retransmissions)
  in the first none had the MPEG-TS sync byte 0x47 at both offset 0 and
  offset 188 of its payload, which every plain packet has; nor had any of
  9,632 and 8,618 in the others.
- **The key switches, under loss.** The key flag of the original data
  packets, in the order sent, held one key for 1,500 packets, then the
  other for 1,501, then 1,501, 1,501 and 1,179 (against both receivers, at a
  refresh of 1,500), and 2,000, 2,001, 2,001 and 1,180 at a refresh of 2,000.
  In the AES-128 trial against `own` the sender sent 10 KMREQs for 5
  refreshes (the fifth, at packet 7,128, announces a switch the clip ends
  before), repeating a request when no answer came back, and 5 KMRSPs got
  back through the 20% loss; against libsrt, 9 KMREQs and 5 KMRSPs. The
  AES-256 trial at 10% needed no repeats (3 and 3).

AES-128, key refresh every 1,500 packets (four switches a clip), 6 seeds per
condition, the receivers interleaved seed by seed. One of the 100 trials
(libsrt at 3% and 200 ms, seed 5) died in the script and was run again
afterwards with the same settings:

| Loss | Latency | Receiver | n | Retransmission overhead | Not recovered in time | Video delivered | Audio delivered | Decode messages |
| ------ | --------: | ---------- | --: | ------------------------: | ----------------------: | ----------------: | ----------------: | ----------------: |
| 3% random | 2000 ms | own | 6 | 3.3% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 3% random | 2000 ms | libsrt | 6 | 3.6% ± 0.4 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 89.0 ± 0.0 |
| 10% random | 2000 ms | own | 6 | 11.7% ± 0.3 | 0.00% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | libsrt | 6 | 14.5% ± 0.4 | 0.00% ± 0.00 | 100.04% ± 0.06 | 100.00% ± 0.00 | 60.0 ± 44.9 |
| 20% random | 2000 ms | own | 6 | 26.1% ± 0.7 | 0.00% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 20% random | 2000 ms | libsrt | 6 | 31.1% ± 1.0 | 0.00% ± 0.01 | 99.96% ± 0.15 | 99.94% ± 0.15 | 72.8 ± 35.4 |
| bursty 8% | 2000 ms | own | 6 | 9.8% ± 1.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| bursty 8% | 2000 ms | libsrt | 6 | 12.0% ± 1.5 | 0.00% ± 0.01 | 100.00% ± 0.12 | 99.94% ± 0.15 | 58.8 ± 44.1 |
| 3% random | 200 ms | own | 6 | 5.2% ± 0.7 | 0.01% ± 0.01 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.3 ± 0.8 |
| 3% random | 200 ms | libsrt | 6 | 4.3% ± 0.4 | 0.01% ± 0.01 | 100.04% ± 0.06 | 100.00% ± 0.00 | 60.0 ± 44.9 |
| 10% random | 200 ms | own | 6 | 20.3% ± 1.6 | 0.23% ± 0.15 | 99.79% ± 0.32 | 99.80% ± 0.49 | 12.8 ± 14.0 |
| 10% random | 200 ms | libsrt | 6 | 15.9% ± 1.1 | 0.43% ± 0.13 | 100.06% ± 0.06 | 99.15% ± 0.72 | 66.5 ± 54.6 |
| 20% random | 200 ms | own | 6 | 37.7% ± 2.7 | 0.91% ± 0.16 | 99.25% ± 0.22 | 98.02% ± 0.81 | 54.5 ± 7.1 |
| 20% random | 200 ms | libsrt | 6 | 32.2% ± 2.1 | 1.96% ± 0.31 | 100.06% ± 0.09 | 97.19% ± 0.71 | 138.7 ± 23.6 |
| bursty 8% | 200 ms | own | 6 | 14.6% ± 3.1 | 0.75% ± 0.19 | 99.63% ± 0.09 | 99.80% ± 0.49 | 7.8 ± 1.7 |
| bursty 8% | 200 ms | libsrt | 6 | 11.3% ± 1.7 | 1.03% ± 0.51 | 99.83% ± 0.22 | 99.27% ± 0.60 | 39.0 ± 45.0 |

`own` minus libsrt, with a Welch t statistic over the 6 seeds (|t| below
about 2.3 is noise). Video and audio delivered and decode messages compare
sapflow builds only, as above, so they are left out:

| Loss | Latency | Overhead, points | t | Not recovered, points | t |
| ------ | --------: | -----------------: | --: | ----------------------: | --: |
| 3% random | 2000 ms | -0.3 | -1.7 | +0.00 | - |
| 10% random | 2000 ms | -2.8 | -12.6 | +0.00 | 1.0 |
| 20% random | 2000 ms | -4.9 | -10.1 | +0.00 | 0.0 |
| bursty 8% | 2000 ms | -2.2 | -3.0 | -0.00 | -1.0 |
| 3% random | 200 ms | +0.9 | 2.6 | -0.00 | -0.3 |
| 10% random | 200 ms | +4.4 | 5.6 | -0.20 | -2.5 |
| 20% random | 200 ms | +5.5 | 3.9 | -1.05 | -7.3 |
| bursty 8% | 200 ms | +3.3 | 2.3 | -0.28 | -1.3 |

AES-256, key refresh every 2,000 packets (3 switches a clip), 3 seeds, two
conditions at each latency:

| Loss | Latency | Receiver | n | Retransmission overhead | Not recovered in time | Video delivered | Audio delivered | Decode messages |
| ------ | --------: | ---------- | --: | ------------------------: | ----------------------: | ----------------: | ----------------: | ----------------: |
| 10% random | 2000 ms | own | 3 | 11.8% ± 0.2 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| 10% random | 2000 ms | libsrt | 3 | 15.2% ± 1.0 | 0.00% ± 0.00 | 99.89% ± 0.00 | 100.00% ± 0.00 | 89.0 ± 0.0 |
| bursty 8% | 2000 ms | own | 3 | 9.4% ± 1.2 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| bursty 8% | 2000 ms | libsrt | 3 | 10.7% ± 0.6 | 0.00% ± 0.00 | 99.89% ± 0.00 | 100.00% ± 0.00 | 89.0 ± 0.0 |
| 10% random | 200 ms | own | 3 | 16.1% ± 3.5 | 0.19% ± 0.09 | 99.73% ± 0.07 | 99.21% ± 0.69 | 11.7 ± 1.5 |
| 10% random | 200 ms | libsrt | 3 | 14.3% ± 1.0 | 0.37% ± 0.04 | 99.96% ± 0.06 | 99.74% ± 0.45 | 48.3 ± 49.6 |
| bursty 8% | 200 ms | own | 3 | 12.1% ± 2.1 | 0.63% ± 0.31 | 99.54% ± 0.20 | 99.60% ± 0.34 | 10.3 ± 2.5 |
| bursty 8% | 200 ms | libsrt | 3 | 10.7% ± 1.2 | 0.95% ± 0.32 | 99.56% ± 0.11 | 99.55% ± 0.40 | 73.3 ± 44.5 |

`own` minus libsrt, 3 seeds (with 3 seeds only |t| above about 3 to 4 is worth
claiming):

| Loss | Latency | Overhead, points | t | Not recovered, points | t |
| ------ | --------: | -----------------: | --: | ----------------------: | --: |
| 10% random | 2000 ms | -3.4 | -6.0 | +0.00 | - |
| bursty 8% | 2000 ms | -1.3 | -1.8 | +0.00 | - |
| 10% random | 200 ms | +1.8 | 0.9 | -0.18 | -3.2 |
| bursty 8% | 200 ms | +1.5 | 1.1 | -0.32 | -1.2 |

Does encryption, or the refresh, cost anything? The first batch above mostly
ran at a load average of 80 to 130 and its 200 ms overhead was well above the
M6a run's, so a second set ran in the same session at 200 ms with 4 seeds,
one run after another: unencrypted, AES-128 without a refresh, and AES-128
with one every 1,500 packets; then the binary of the M6a table (4722f34)
beside the current one, unencrypted. Overhead is the mean over the seeds,
then `own` minus libsrt with its t statistic (with 4 seeds only |t| above
about 3 is worth claiming); unrecovered is the mean for `own` and then
libsrt:

| Run | Loss | Own overhead | libsrt overhead | Own minus libsrt | Unrecovered, own / libsrt |
| ----- | ------ | -----------: | --------------: | ---------------: | ------------------------: |
| unencrypted | 10% random | 17.5% ± 2.5 | 14.8% ± 1.4 | +2.7 (t 1.9) | 0.17% / 0.28% |
| unencrypted | 20% random | 38.8% ± 1.4 | 31.8% ± 1.2 | +7.0 (t 7.5) | 1.07% / 1.98% |
| AES-128, no refresh | 10% random | 13.3% ± 1.9 | 12.9% ± 0.8 | +0.4 (t 0.4) | 0.18% / 0.28% |
| AES-128, no refresh | 20% random | 34.9% ± 4.9 | 31.7% ± 1.6 | +3.2 (t 1.2) | 0.99% / 1.59% |
| AES-128, refresh 1500 | 10% random | 19.2% ± 0.7 | 15.0% ± 0.3 | +4.2 (t 11.6) | 0.10% / 0.25% |
| AES-128, refresh 1500 | 20% random | 37.7% ± 2.7 | 32.5% ± 1.8 | +5.2 (t 3.2) | 0.95% / 1.79% |
| unencrypted, M6a binary 4722f34 | 10% random | 19.4% ± 0.7 | 15.3% ± 0.7 | +4.1 (t 8.5) | 0.16% / 0.26% |
| unencrypted, M6a binary 4722f34 | 20% random | 36.2% ± 1.2 | 30.8% ± 0.8 | +5.3 (t 7.5) | 0.93% / 1.71% |
| unencrypted, 8ee5704 (same run) | 10% random | 19.1% ± 1.1 | 15.3% ± 0.7 | +3.8 (t 5.8) | 0.14% / 0.26% |
| unencrypted, 8ee5704 (same run) | 20% random | 37.0% ± 1.7 | 30.8% ± 0.8 | +6.1 (t 6.6) | 0.99% / 1.71% |

At 2000 ms, unencrypted in the same session (4 seeds): `own` 11.7% ± 0.4 and
25.9% ± 0.5 overhead at 10% and 20% random loss against libsrt's 13.5% ± 0.7
and 32.3% ± 0.8, and 0.00% unrecovered for `own` in both.

What it shows (the machine was shared and heavily loaded: load average
about 5 when the first run started, then mostly 80 to 130):

- **Encryption with key refresh works under loss.** At 2000 ms `own`
  delivered every video and audio packet its loss-free run did (864 and
  1,344) with no decode messages in all 24 AES-128 trials (four conditions,
  6 seeds, four key switches each) and in all 6 AES-256 trials, and at most
  0.03% of packets unrecovered in any of the AES-128 ones (none in the
  AES-256 ones). A key that was not in place at a switch would make every
  packet until it arrived undecryptable, a run of packets (about 240 a
  second) and the frames they carry in each trial; none of the trials shows
  one.
- **At 2000 ms `own` is level with libsrt or lower on overhead**, the same
  as unencrypted: 0.3 points lower at 3% random (t -1.7), 2.8 at 10% (t
  -12.6), 4.9 at 20% (t -10.1) and 2.2 under bursts (t -3.0). Our own
  figures did not move from the unencrypted M6a run (3.3, 11.7, 26.1 and
  9.8%, against 3.3, 11.6, 25.7 and 9.6%); libsrt's did (14.5% and 31.1%
  at 10% and 20%, against 12.3% and 27.6%), and the same-session
  unencrypted run shows libsrt's higher figure there too (13.5% and
  32.3%). The larger lead than in M6a is libsrt's figure moving, not ours.
- **At 200 ms `own` leaves fewer packets unrecovered than libsrt** under
  loss that is not bursty: 0.20 points fewer at 10% (t -2.5) and 1.05 at
  20% (t -7.3), level at 3% and under bursts (0.28 fewer, t -1.3). The
  AES-256 batch agrees at 10% (0.18 fewer, t -3.2).
- **At 200 ms `own` costs more retransmissions than libsrt in this session**,
  by more than the spread: +4.4 points at 10% random (t 5.6), +5.5 at 20% (t
  3.9), +3.3 under bursts (t 2.3, borderline) and +0.9 at 3% (t 2.6). It is
  not the encryption: the unencrypted run in the same session shows +2.7 and
  +7.0 points, AES-128 without a refresh +0.4 and +3.2, and the binary of the
  M6a table, unencrypted, +4.1 and +5.3, where M6a measured it level (+0.0 and
  -1.1). The same code gives a different overhead at a different time: what
  about this session moved it (the load, most likely) was not isolated. The
  refresh does not show as a cost either: at 10% `own` had 19.2% and 20.3%
  overhead with refresh (the same-session run and the first batch) against
  17.5%, 19.1% and 19.4% unencrypted, and it left no more packets unrecovered
  with a refresh than without (0.10% and 0.95% against 0.18% and 0.99% with
  encryption but no refresh). The run without a refresh is the odd one out, at
  13.3% (three of its four seeds between 12.0% and 12.5%, level with libsrt,
  whose overhead was also about two points lower in it than in the others); it
  was not repeated, so a small cost of the refresh at 200 ms is not excluded.
- **Video and audio delivered and decode messages** are as in the
  unencrypted tables: they depend on what each receiver writes, so they
  compare sapflow builds, not libsrt.
- **Nothing in the logs.** The 82 sapflow logs kept from these runs hold only
  the two start-up warnings (`SAPFLOW_OPEN_PUBLISH` and no callback URL). The
  libsrt receiver logs hold its too-late drop warnings and teardown messages
  and no key or decryption message.

Limits as in the sections above (one sender, libsrt 1.5.4; one clip;
loopback with an emulated 60 ms round trip and independent loss both ways;
a shared machine), and:

- **No decrypt counter was read.** The receiver counts packets it could not
  decrypt and key requests it rejected (`encrypted`, `km_rejected` in the
  receiver's `Stats`), but the server only logs received, retransmitted, lost
  and dropped packets when a connection closes, at the `info` level (the
  harness runs it at `warn`). The argument above is indirect: the delivered
  counts, not a counter.
- **The key capture is three trials**, not every trial, and the relay adds a
  hop; it shows the sender's refreshes and the receivers answering them,
  not that every packet of every trial was decrypted.
- **The same-session set is 4 seeds** at two conditions and one latency, run
  one after another, not interleaved with each other (the two receivers
  within each run were), so a few points of overhead between those runs
  (13.3% against 19.2% for `own` at 10%) cannot be put down to the
  setting on this evidence.
- **Passphrase and key lengths:** AES-128 and AES-256 only, one passphrase,
  and refresh rates of 1,500 and 2,000 packets; the pre-announce was a
  quarter of the rate. A passphrase with `%20` is not covered (libsrt 1.5.3
  does not decode it).
