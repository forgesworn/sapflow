# Reference notes

From reading mediamtx v1.19.1 and gohlslib v2.4.0 (both MIT) on 2026-10-20.
Use them as the reference for behaviour, not as code to copy blindly.

```bash
git clone --depth 1 --branch v1.19.1 https://github.com/bluenviron/mediamtx.git
git clone --depth 1 --branch v2.4.0  https://github.com/bluenviron/gohlslib.git
```

## Apple rules gohlslib implements

| Rule | Where in gohlslib | sapflow |
|------|-------------------|---------|
| `EXT-X-TARGETDURATION` must not change during a stream, and every segment's `EXTINF`, rounded to the nearest second, must be at most it. gohlslib raises it whenever a segment runs long, which Safari does not accept; sapflow fixes it once and breaks the segment instead (see [Target duration and breaks](#target-duration-and-breaks)). | `muxer_stream.go` `targetDuration`, `rotateSegments` | `sapflow_hls::TargetDuration`, `Muxer::break_stream` |
| No part may be longer than `PART-TARGET`, every part but the last of its segment must be at least 85% of it, and `PART-TARGET` must not change during a stream (see [Part timing](#part-timing) for the wording and how sapflow keeps it). gohlslib reads the rule as 85% of the longest part and recomputes `PART-TARGET` as the longest part in the window at every part, only logging when it changes, so one long frame raises it; do not copy that. | `muxer_segmenter.go` `findCompatiblePartDuration`, `fmp4AdjustPartDuration`; `muxer_stream.go` `partTargetDuration` | `sapflow_hls::PartTiming`, `Muxer::place_video` |
| The LL-HLS playlist starts with seven gap segments (same duration as the first real one); iOS needs them. This is why mediamtx refuses fewer than 7 segments. | `muxer_stream.go` `rotateSegments` | M1 |
| Blocking reload: a request with `_HLS_msn` beyond last + 1, or older than the window, gets 400; otherwise it waits until that part exists. `_HLS_part` without `_HLS_msn` is 400. | `muxer_stream.go` `handleMediaPlaylist` | M1 |
| The reference uses `PART-HOLD-BACK` at 2.5 x part target; Sapflow uses 3 x to meet [Apple authoring specification §14.3](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices/). `CAN-BLOCK-RELOAD=YES`; Sapflow does not offer delta updates. | `generateMediaPlaylistFMP4` | `sapflow_hls::playlist::media` |
| Segments are cut only on a keyframe once the minimum duration has passed, so segment length follows the encoder's keyframe interval. | `fmp4WriteSample` | M1 |
| fMP4 base time must not be negative; gohlslib adds a fixed start offset to every DTS. | `fmp4WriteSample` (`fmp4StartDTS`) | M1 |
| LL-HLS playlists are `Cache-Control: no-cache`; segments and parts are cacheable. | `mediaPlaylistMaxAge`, segment handlers | M1 |

## Part timing

The rule sapflow implements, from RFC 8216bis (draft-pantos-hls-rfc8216bis)
section 4.4.4.9 on `EXT-X-PART`, quoted from memory and not re-read here:
a Partial Segment's duration must be less than or equal to the Part Target
Duration, and at least 85% of it, except for the last Partial Segment of a
Parent Segment (the draft may also exempt `INDEPENDENT=YES` parts; sapflow
does not rely on that). `PART-TARGET` (4.4.3.7) is treated like
`EXT-X-TARGETDURATION`: fixed for the life of the stream. Safari on macOS
refused a Larix stream that broke all three (`PART-TARGET` climbed to
0.534 s while its parts stayed 0.2 s; `docs/interop.md`); which one it
tripped on is not known.
`scripts/check-playlist.py` and `sapflow_hls::playlist::RuleCheck` check
these three things, and the target-duration and discontinuity rules below,
against every rendered playlist.

How sapflow keeps it (`sapflow_hls::PartTiming`, `Muxer::place_video`):

- **Chosen once.** The first segment's video frames are held (nothing is
  served before the first segment exists, so this costs no latency) and the
  typical frame duration is taken from them: the mean, leaving out frames
  over 1.5 times the median, so a stall or dropped frame in the first
  second does not count. A nominal part is the fewest whole frames, at least
  five, that reach the configured minimum (200 ms) to within a tenth of a
  frame. `PART-TARGET` is set so that 85% of it, rounded up to a tick (the
  cut point), lies 1 ms below the nominal part, and is a whole number of
  milliseconds so it prints exactly. At 30 fps: parts of 6 frames (0.2 s),
  cut point 0.1989 s, `PART-TARGET` 0.234 s, `PART-HOLD-BACK` 0.702 s. The
  15% between the cut point and the target is about a frame wide (it must
  be at least one frame, hence five frames a part at least), so a part of
  whole frames always fits in it.
- **Cut on a threshold.** A part closes at the first frame boundary at or
  past the cut point. Jittered frames (Larix: 2998 to 3004 ticks) and
  RTMP's millisecond timestamps (33 and 34 ms frames) still give 6-frame
  parts, not the alternating 6 and 7 frames of a cut at exactly 200 ms. A
  dropped frame at the end of a part (5 frames plus a double) still fits
  under the target.
- **Long frames are clamped.** A frame longer than the room left before
  the target (a Wi-Fi stall: one Larix frame lasted 0.367 s) is written
  with its `trun` duration cut so the part ends exactly at the target; the
  next part's `tfdt` carries the true time, leaving a gap in the video
  track that audio still covers. Letting the part run long would exceed
  `PART-TARGET`; closing the part before the frame would leave it under
  85%; raising the target is the fault itself. Safari and hls.js tolerate a
  short gap in one track (hls.js steps over buffer holes); both were seen
  to carry on across stalls and reconnects (below, What players should do
  at a break).
- **EXTINF stays true.** A segment's `EXTINF` is still the media time it
  covers, so after a clamp its parts' `DURATION`s add up to less than its
  `EXTINF` by the gap. `Part::span` is the part's duration plus any gap
  after it; the spans add up to the segment, and the recorder sums spans so
  a recording's length is right.
- **Audio follows the timestamps.** When a part closes, AAC frames that
  start at or after its end move to the next part, so a part's audio
  covers its own time (to within one AAC frame, which is placed only when
  the next arrives).

## Target duration and breaks

RFC 8216bis 4.4.3.1 (from memory, not re-read here): every segment's
`EXTINF`, rounded to the nearest integer, must be at most
`EXT-X-TARGETDURATION`, and the target must not change. On 2026-10-21 a
Larix stream with Wi-Fi switched off briefly kept the part rules but left
a 6 s segment, so sapflow raised the target from 2 to 6 and Safari
stopped. How sapflow keeps the rule (`sapflow_hls::TargetDuration`,
`Muxer::break_stream`):

- **Fixed by the first segment.** The first segment follows the encoder's
  keyframe interval; the target is its duration rounded, plus one second
  of headroom for a keyframe that comes a little late (Larix's 2 s
  keyframes give 3). A segment may then run to just under the target plus
  half a second (it rounds to the target at most). The failure mode: an
  early extra keyframe (a scene cut) makes the first segment short, the
  target is then too small for the encoder's interval, and every later
  segment is cut short as below. The log line naming the target says to
  set the keyframe interval to at most it.
- **A break instead of a long segment.** A segment ends early (a break)
  when the gap between two video frames is over a second either way (a
  network drop, an encoder restarting its clock), or when it is about to
  run past the limit because no keyframe came in time. A segment cannot
  be cut on a frame that is not a keyframe: the multivariant playlist
  declares `EXT-X-INDEPENDENT-SEGMENTS`, and after a drop the frames that
  follow may reference lost ones. So video is dropped until the next
  keyframe. Gaps of a second or less are clamped into a held frame instead
  (Part timing, above): one second is under the target's headroom, so one
  such stall cannot by itself push a segment past the limit, and the
  length rule catches several.
- **Output time carries on.** The keyframe that resumes the stream is
  moved to the time the last segment ended, and everything after it, audio
  included, by the same amount. Output timestamps therefore only move
  forward with no hole, even across an encoder clock jumping back, and a
  recording (one fMP4 file) plays and seeks straight through; its length is
  the media kept.
- **Marked as a discontinuity.** The segment after a break carries
  `EXT-X-DISCONTINUITY`, before its `EXT-X-PROGRAM-DATE-TIME` and its
  parts (never among them), because media was lost and the wall clock
  jumped. `EXT-X-DISCONTINUITY-SEQUENCE` counts the marked segments that
  have left the window, so each media sequence number keeps the same
  discontinuity number from render to render. While the stream waits for
  the keyframe there is no open segment: blocking requests for the next
  segment, and for the part the preload hint names, wait (up to three
  target durations, then 503), and the hinted part becomes the new
  segment's first.
- **Audio.** AAC from before the resuming keyframe is dropped, and an AAC
  frame is never given a gap as its duration (a frame lasts 1024 samples;
  anything over three frames' time becomes one).
- **The first segment.** A break before the first segment is complete
  starts the stream over: nothing has been served, and the targets should
  come from a whole segment. The seven leading gap segments are only ever
  at the start of a stream.
- **An encoder that drops and reconnects** (an SRT publisher silent for
  5 s is dropped; RTMP when its socket closes or is idle for 10 s) is a
  break too. The stream, with its playlist, chunk names and recording, is
  held for `SAPFLOW_RECONNECT_GRACE` (default 90 s, v4v's own window). A
  publisher on the same path within it, over either protocol, takes the
  stream over: the muxer ends the run at the last frame
  (`Muxer::interrupt`), and the new connection's first keyframe, from
  whatever clock it brings, carries output time straight on after an
  `EXT-X-DISCONTINUITY`. Its parameter sets and AAC config must match the
  stream's: the init segment is fixed. If they do not (a new resolution),
  the stream starts afresh in place under new chunk names, and a new
  recording begins. v4v still hears `unpublish` at the drop (its stage
  reads "reconnecting") and `publish` on the return; `recordSegmentComplete`
  comes once, when the stream finally ends. Keep `SAPFLOW_RECONNECT_GRACE`
  equal to `PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS`. An encoder that
  changes network reconnects before its old connection has timed out: the
  new connection with the same credentials takes the stream over at once,
  as mediamtx's `overridePublisher` (its default) does, and the old one is
  closed on its next packet without sending `unpublish`. Scoped publish
  sessions keep one owner and refuse a second (`409`).

What players should do at a break (Safari on macOS seen to carry on
through two Larix Wi-Fi drops of 3.1 and 4.4 s on 2026-10-21, judged by
eye; hls.js 1.5.20 in Chrome, low-latency mode, logged crossing a 4 s
stall and then a full disconnect and reconnect on 2026-10-21, with no
fatal error; it logged non-fatal errors, recovered from: one
`fragParsingError` on the first fragment after the reconnect, and a
`levelLoadTimeOut` about once a minute, which are not yet explained): during a
drop the playlist stops growing, so Safari and hls.js play out what they
hold (about the part hold-back plus what was buffered) and then wait;
when the stream resumes they cross the discontinuity, resetting their
decoders, and carry on, a little further from live by about the time
spent waiting. hls.js logs a change of discontinuity sequence and may jump
forward to the live edge. A shorter stall (up to a second) shows as one
frame held while audio plays on.

## mediamtx behaviour worth not copying

- **Per-viewer sessions.** Each viewer gets a session, a cookie-check 302
  first, and a `session=` query parameter in every chunk URL when cookies fail
  (`internal/servers/hls/http_server.go`). That makes every viewer's playlist
  unique, so nothing can cache or share it. Its `hlsCDNSecret` mode skips all
  of that; sapflow should behave like that mode by default.
- **Playlist rebuilt per request** under one lock per stream, and every
  blocked viewer is woken on every part. Fine at hundreds of viewers; sapflow
  can render each playlist version once and hand out the same bytes.
- Self-upgrade code exists but only runs on `--upgrade` / `--check-version`.

## Crates chosen at M1 (2026-10-20)

Pinned in the crates' `Cargo.toml`. These are decisions the user may revisit.

| Need | Crate | Notes |
|------|-------|-------|
| Viewer HTTP | Our `own_http` backend on `mio` 1.2.4 | The only viewer backend; the Hyper one was removed after measuring about twice the CPU and several times the memory |
| Server runtime and control HTTP | `tokio` 1.54, `httparse` 1.10 | Ingest adapters, timers, recording, the private control API and the callback client. The control API is served by our own HTTP/1.1 reader and writer (`http_lite.rs`) on tokio sockets: one request per connection, bounded head and body, a fuzz target. The callback client is a plain HTTP/1.1 `POST` on a tokio socket (no TLS, no redirects, status and headers only). No `axum`, `hyper` or `reqwest`: the three of them and their `tower`, `http` and `futures` dependencies were removed in 2026-10 |
| SRT | `sapflow-srt`, our own | Live-mode receiver and sender, sans-IO, MIT; written from draft-sharabayko-srt-01 and recorded sessions (findings below). Serves the port alone since 2026-10-21. Passphrase encryption (M6b) uses RustCrypto's `aes`, `aes-kw`, `ctr`, `pbkdf2` and `sha1`, plus `getrandom` and `zeroize`; nothing else. |
| SRT until 2026-10-21 (removed) | `srt-tokio` 0.4.4 with `srt-protocol` 0.4.4, vendored and patched | Last release 2024-05. Fuzzing found that one unauthenticated datagram crashed the server; we patched our copy. Removed once OBS and Larix had published to our own receiver, with and without a passphrase (M6c). The patches are in the history before that date. |
| MPEG-TS demux | `mpeg2ts` 0.6.1 | Fed one 188-byte packet at a time from our buffer (`sapflow_hls::ingest`), because its reader treats some stream errors as fatal and stops at end of input. |
| H.264 | `h264-reader` 0.9.0 | Annex B splitting and SPS parsing. |
| AAC | `adts-reader` 0.4.0 | ADTS headers. It reports the raw-block count as the field plus one. |
| fMP4 writing | `mp4-atom` 0.16.2 | `ftyp`/`moov`/`moof`/`mdat` encoding. |
| RTMP (M2) | our own, `sapflow-rtmp` | Publish only, sans-IO: handshake (original and digest forms), chunk stream, AMF0, the publish commands. Replaced `rml_rtmp` 0.8.0 (last release 2023-04), in which fuzzing found two panics on unauthenticated input. The digest handshake uses `hmac` 0.12.1 and `sha2` 0.10.9. |
| Fuzzing (M5) | `cargo-fuzz` | |

Licences: every dependency is permissive. `sync_wrapper` is Apache-2.0
only (no NOTICE file); the rest are MIT, BSD, Unicode-3.0 or dual-licensed.
Distributed binaries and images must carry the third-party licence texts.

Our own parsing is the SRT stream id, the RTMP stream key, the chunk file
names, the `_HLS_msn`/`_HLS_part` query, the FLV video and audio tag headers
(5 and 2 bytes), the AudioSpecificConfig (2 bytes) and NAL length prefixes,
all small and unit-tested, and, since 2026-10-20, the whole RTMP protocol
(publish only: `sapflow-rtmp`) and the whole SRT receiver (`sapflow-srt`: every packet type, the handshake and its
extensions, loss lists). Each has a fuzz target.

### RTMP handshake findings (2026-10-20)

- Observed: ffmpeg 8.0.1 publishing sends a digest C1 (version 9.0.124.2)
  and echoes S1 as C2. ffmpeg playing (`ffmpeg -i rtmp://...`), which
  validates the server's digest, accepts our S1 and S2 and reaches the play
  refusal.
- From memory of librtmp's `handshake.h` (OBS's RTMP output), not re-read
  here: it sends a digest C1, verifies a digest S1 and signed S2 strictly
  when S1 carries a server version, and falls back to the original form
  when the version is zero. The unit tests check our answer against that
  algorithm; OBS has since published over RTMP to sapflow
  (`docs/interop.md`).
- So sapflow answers a digest C1 with a digest S1 (server version
  13.14.10.13, as nginx-rtmp) and a signed S2, and anything else with the
  original form (S2 echoes C1). C2 is not checked, as in nginx-rtmp,
  mediamtx and rml_rtmp. Larix has since published over RTMP
  (`docs/interop.md`); vMix is untested, and is known to work with
  nginx-rtmp and mediamtx, which answer the same way.

### SRT wire findings (2026-10-20)

From draft-sharabayko-srt-01 and sessions recorded through a UDP relay:
libsrt 1.5.4 (`srt-live-transmit`) as caller and listener, and ffmpeg 8.1.2
with libsrt 1.5.3 (in Docker) as caller. Marked **observed** where what
real peers do differs from, or is not in, the specification; `sapflow-srt`
follows the observed behaviour.

- **Observed:** libsrt's listener answers the induction with the caller's
  own socket id in the SRT Socket ID field (the specification says the
  listener's) and echoes the caller's sequence number, MTU and window.
  The caller's conclusion is sent to destination socket id 0, not to the
  id from the induction reply. sapflow-srt echoes the caller's id too and
  finds a conclusion by its source address and the caller's socket id.
- **Observed:** the conclusion reply carries the listener's new socket id,
  the cookie echoed, Extension Field 1 (HSRSP) and only the HSRSP
  extension, not the stream id. libsrt advertises flags 0xBF there, which
  includes PACKET_FILTER although no filter was agreed; sapflow-srt sends
  0x3F (TSBPD both ways, CRYPT, TLPKTDROP, PERIODICNAK, REXMITFLG) and
  version 1.5.0, which libsrt 1.5.3 and 1.5.4 accept.
- The Peer IP Address field holds the address as 32-bit little-endian
  words: 127.0.0.1 is `01 00 00 7f` (observed, matching the text). The
  stream id is little-endian words too, its last word padded with zeros
  inside the word (`read:live/event-x` ends `00 00 00 78`).
- **Observed:** a refusal is a handshake whose type is the reason: 1000 and
  up for the protocol's, 2000 plus an HTTP status for the application's,
  which libsrt prints as `PREDEFINED:401`. srt-tokio sent 400, 401, 402
  (overload, and our throttle), 404 and 409 for sapflow's refusals;
  sapflow-srt sends the same.
- **Observed:** a libsrt receiver sends a full ACK at most every 10 ms and
  only when the acknowledged position has moved (the specification reads
  as every 10 ms); keepalives every second when idle, both ways.
- **Observed:** its first RTT sample replaces the starting 100 ms / 50 ms:
  the ACK after the first ACKACK reports that sample as the RTT and half
  of it as the variance. Later samples are smoothed as specified (7/8,
  3/4). Its first ACKs report a link capacity of 1000 packets per second.
- **Observed:** libsrt sends a NAK range for two consecutive lost numbers
  (the specification describes ranges for more than two); sapflow-srt
  does the same, and accepts either.
- **Observed:** periodic NAK reports list every outstanding loss about
  every (RTT + 4 x RTTVar) / 2, so libsrt asks again before a
  retransmission could have arrived. sapflow-srt asks again per loss after
  RTT + 4 x RTTVar, every quarter RTT (at most eight times) once the
  delivery deadline is under three RTTs away, and never once a
  retransmission could no longer arrive in time: the deadline-aware policy
  `docs/interop.md` measured for the patched srt-protocol, with the
  ordinary interval widened from one RTT, which raced retransmissions
  already on their way (`docs/interop.md`).
- **Observed:** ffmpeg (libsrt 1.5.3) sets the order flag on every data
  packet and sends payloads of whole TS packets (188 to 1,316 bytes);
  `srt-live-transmit` leaves the flag clear and sends 1,456-byte payloads
  with no regard to TS packet boundaries. Live mode ignores the flag.
- **Observed, srt-protocol fault:** when it skips a packet that is too
  late, srt-protocol also discards the next packet, even one that arrived
  in time. On the recorded 200 ms session under 20% loss it delivered 57
  packets where libsrt's receiver and sapflow-srt delivered the same 59
  (`crates/sapflow-srt/tests/differential.rs`).

## Related

relayswarm (forgesworn) already has a Node HLS origin that signs a Nostr
commitment per chunk, two renditions and admission tickets, but it receives
via mediamtx and ffmpeg and serves 2 s classic HLS, not LL-HLS. It is the
consumer sapflow's origin should eventually feed.
