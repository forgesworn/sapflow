# Goals

sapflow is a small, purpose-built live media server in Rust: SRT (and later
RTMP) in, Low-Latency HLS out, with recording and a stream-health callback.
It exists to replace mediamtx behind the v4v live stage with something that
does only what the stage needs, and to give the future native iOS and Android
apps a shared Rust core.

It is an experiment until milestone 4 says otherwise. Nothing here replaces
mediamtx in production before the numbers favour it and the user decides.

## Principles

- **Only what the stage needs.** One way in, one way out, recording, one
  callback. No RTSP, WebRTC, transcoding, playback API or config file the size
  of mediamtx's.
- **Do not hand-roll hostile-input parsers** without a reason. MPEG-TS and
  H.264 bitstream parsing come from maintained crates. The ingest protocols
  are the exception: no maintained Rust choice exists for RTMP or SRT, so
  sapflow carries its own, narrow to what the stage needs (see Decisions),
  and proves each against the reference implementation before it ships. Our other code is glue,
  policy and the LL-HLS playlist logic. Anything we parse ourselves gets a
  fuzz target before it faces the internet.
- **Private by default.** No outbound calls except the configured callback. Do
  not log viewer IPs unless asked. One shared viewer session, so playlists and
  chunk URLs are identical for everyone (cacheable, swarm-friendly), the way
  mediamtx's `hlsCDNSecret` mode behaves.
- **`#![forbid(unsafe_code)]`** across the workspace (set in `Cargo.toml`).
- **Phone-ready core.** `sapflow-core` has no networking or async runtime, so it
  can be built for iOS and Android with UniFFI later.
- **Measure, do not assert.** "Faster" and "smaller" are claims milestone 4
  checks against mediamtx 1.19.1.

## Milestones

### M0 Skeleton (done)

Workspace, three crates, the v4v contract written down, the iOS rules found in
gohlslib written down, build hygiene.

### M1 SRT in, LL-HLS out, callback (done)

- SRT listener (caller mode from the encoder), `streamid=publish:live/<eventId>`;
  publisher credentials checked before any media is accepted.
- MPEG-TS demux to H.264 + AAC.
- fMP4 LL-HLS: 1 s segments, 200 ms parts, blocking playlist reload
  (`_HLS_msn`, `_HLS_part`), served at `/live/<eventId>/index.m3u8` with the
  multivariant playlist, init file, segments and parts. Keep 24 segments.
- iOS rules from `docs/reference.md`: target duration only rises, part
  durations within 85%, the seven leading gap segments, no cookie dance.
- `publish` callback once the first segment is servable, `unpublish` when the
  encoder leaves.
- Refuse a second publisher on a path that is already live.

**Done when:** v4v's ingest gate passes in SRT mode against sapflow instead of
mediamtx (`PULSEWIRE_LIVE_SMOKE_PROTOCOL=srt npm run e2e:ingest`; the smoke
currently starts the mediamtx image and writes its config, so it needs a
media-server switch, which is a separate v4v PR), and the stream plays in
Safari on iPhone and in hls.js.

**Status (2026-10-20):** v4v's ingest gate passes in SRT mode against
sapflow (`npm run e2e:ingest-srt-sapflow`, on the v4v branch
`feat/ingest-gate-sapflow`, under review): credentials, the app's own
playback check, the fan-out relay, a dropped encoder held and resumed, and
the replay. Also verified with `scripts/smoke.sh`, in hls.js 1.5 in Chrome,
in Safari's native player on the iOS 18.3 simulator (2.5 minutes), and in
Safari on a physical iPhone over the local network: 1280x720, 72 s of
reported playback, no stalls or errors.

### M2 RTMP in (done)

OBS's default and Larix's. Same path, credential and callback rules as SRT.
**Done when:** the ingest gate passes in RTMP mode.

**Status (2026-10-20):** done. v4v's ingest gate passes in RTMP mode against
sapflow (`npm run e2e:ingest-sapflow`, same v4v branch), including Studio's
built-in test signal started and stopped twice. RTMP is publish only.

### M3 Recording (done)

Fragmented MP4 to `<recordings>/<path>/<unix-start>.mp4`, one file per show,
`recordSegmentComplete` callback with `segmentPath` and `segmentDuration`,
recording switchable off per path (the looped-videos case), old files expired
after a configured age. **Done when:** a recorded show becomes the gig's replay
in v4v end to end.

**Status (2026-10-20):** implemented. `SAPFLOW_RECORDINGS` turns it on,
`SAPFLOW_RECORD_SKIP` lists paths not to record, `SAPFLOW_RECORD_DELETE_AFTER`
(default 7d) expires old files. The file is the init segment plus every
part, with its total duration written into `mehd` when the show ends; it
plays and seeks in Chrome's `<video>` from 0:00 (served with Range support).
`recordSegmentComplete` carries the path as sapflow sees it and the duration.
In both v4v gates the gig's replay is sapflow's own recording: imported by
v4v from the shared recording directory, byte-for-byte a file sapflow wrote,
then verified, re-fetched and offered on the public stage.

### M4 Measure against mediamtx 1.19.1 (done: keep experimenting)

Same machine, same encoder input. Record: binary and image size, idle and
loaded memory, CPU per stream, glass-to-glass delay, and playlist and chunk
response times at 250 and 750 simulated viewers. Write the numbers to
`docs/benchmarks.md`. **Decision point for the user:** replace, keep
experimenting, or stop.

**Status (2026-10-20):** both servers measured on one machine with
`scripts/bench.sh` and `crates/sapflow-bench`; numbers in
`docs/benchmarks.md`. sapflow used about a third of mediamtx's CPU and
memory under load, with lower median response times. Failed requests seen
in early runs were traced to idle keep-alive connections closing after
10 s and fixed. Not measured: glass-to-glass
(needs a camera and a phone). **Decision (user, 2026-10-20): keep
experimenting**; v4v's gate, an iPhone check and a separate-machine test
come before any switch.

### M5 Harden (done; follow-ups in the threat model)

Fuzz targets for every parser we own, connection and size limits, slow-client
handling, publisher rate limits, a threat model, container image.

**Status (2026-10-20):** seven fuzz targets in `fuzz/` cover every parser we
own (RTMP and AMF0 included, since RTMP became ours), both ingest paths, and
the SRT packet parser and listener we depend on. Fuzzing found that one unauthenticated UDP datagram
crashed the server (`srt-protocol`) and that crafted RTMP bytes panicked
`rml_rtmp`; both were fixed in vendored copies (RTMP has since been replaced
by our own), and release builds unwind so a panic ends one connection. Limits: publishers per protocol, bitrate per
publisher, failed-login throttling, HTTP connection cap and header timeout,
buffer and segment caps, free disk for recording, rate-limited refusal logs.
`docs/threat-model.md` covers each threat and lists follow-ups (longer
fuzzing, SRT after the handshake). The `Dockerfile` builds a
38.5 MB arm64 image that runs and serves a stream.

### M6 Our own SRT (MIT)

srt-rs is unmaintained (last release 2024-05; its authors say they do not
support it and it never tracked the spec), so sapflow already maintains a
patched copy. Replace it with `sapflow-srt`: our own MIT implementation of
what the stage uses, written from the SRT specification
(draft-sharabayko-srt) and observed wire behaviour. No code is copied from
srt-rs (Apache-2.0) or libsrt (MPL-2.0).

Scope: live mode, listener, one publisher per connection, receiving;
handshake v5 (and v4 if an encoder needs it), stream id, loss reports
and periodic NAKs, too-late packet drop, TSBPD delivery, ACK/ACKACK and RTT,
keepalive and shutdown. Sans-IO (bytes and time in, packets and events
out), no async runtime, `unsafe` forbidden, so it builds for iOS and Android.
Out of scope: file mode, rendezvous, caller mode, groups and bonding,
packet filters (FEC), sending (a later milestone, for phones).

- **M6a, receive:** unencrypted, behind a switch (`SAPFLOW_SRT_IMPL`), with
  `srt-protocol` still the default.
- **M6b, encryption:** SRT passphrase (AES key wrap, AES-CTR), so the
  credential no longer travels in clear.
- **M6c, switch:** make it the default, remove `vendor/srt-protocol`,
  `srt-tokio` and their dependencies.

**Done when** (all measured, not asserted):
- fuzz targets for the packet parser and the connection, clean for 20
  minutes each, regressions committed;
- `scripts/smoke.sh` and v4v's SRT gate (`e2e:ingest-srt-sapflow`) pass;
- ffmpeg, `srt-live-transmit` (libsrt 1.5), OBS and Larix publish, with
  and without a passphrase;
- `scripts/srt-loss.sh` over at least 6 seeds: unrecovered packets and
  retransmission overhead within run-to-run spread of libsrt, or better, at
  every condition; no worse than the patched `srt-protocol`;
- CPU and memory per stream no worse than `srt-protocol` (the profiling
  harness);
- `srt-protocol`'s known faults absent by design: no panic on any input, a
  per-process SYN cookie key (T16), no per-packet `getsockname`, no idle
  timer tick.

**Status (2026-10-20): M6a done.** `crates/sapflow-srt` serves SRT with
`SAPFLOW_SRT_IMPL=own` (in `--help`); `srt-tokio` stays the default. Live
mode, listener, receive: version 5 handshake with SYN cookies keyed per
process, stream id, latency negotiation, TSBPD with drift handling and
too-late drop, ACK/ACKACK/RTT, light ACKs, immediate and repeated NAKs,
keepalive, shutdown, idle timeout, connection, buffer, loss-list and
handshake-rate limits; encrypted callers refused (`REJ_UNSECURE`). The
server shares admission and media code with the `srt-tokio` path. Each
proof:

- Unit tests per layer (54 in the crate) and a differential test: three
  recorded publishes (`srt-live-transmit`; ffmpeg 8.1.2 with libsrt 1.5.3;
  `srt-live-transmit` at 200 ms under 20% loss) replayed through
  `srt-protocol`, as a black box, and through ours. Identical payloads
  without loss; under loss ours delivers exactly what libsrt's receiver
  delivered in the recorded session, and `srt-protocol` two packets fewer
  (it discards the in-time packet after one it skips as too late; a fault
  in the default path, recorded in `docs/reference.md`).
- Fuzzing, 20 minutes each with overflow checks: `own_srt_packet` 207
  million inputs and `own_srt_listener` 4.2 million (whole sessions, after
  handshakes too); no crash, so no regressions to commit. Earlier
  5-minute runs (45 million and 1.8 million) were clean too.
- `SRT_IMPLS="srt-rs own" scripts/smoke.sh` passes with both; v4v's SRT
  gate (`npm run e2e:ingest-srt-sapflow`) passes with
  `SAPFLOW_SRT_IMPL=own` set in the environment (no v4v change needed).
- `scripts/srt-loss.sh`, 6 seeds, 8 conditions: within run-to-run spread
  of libsrt or better at every condition (lower overhead at 10% and 20%
  random loss at 2000 ms, fewer packets unrecovered at 10% and 20% at
  200 ms) and no worse than `srt-protocol` at any. The first run was 2
  points worse than `srt-protocol` on overhead at 2000 ms; the cause
  (re-requests racing retransmissions) and the fix are in
  `docs/interop.md`.
- CPU and memory per 720p stream level with `srt-protocol` within the noise
  (0.76% against 0.95% of a core, 4 runs each); idle, no timer and no
  context switches at all in 300 s (`docs/profiling.md`).
- By design: no panicking constructs outside tests (denied by lint), the
  per-process cookie key (T16), one `recv_from` socket with no per-packet
  `getsockname`, no timer without a connection.

Still open for M6 as a whole: OBS against `own` (Larix on an iPhone
published to `own` over Wi-Fi on 2026-10-21, alone and alongside an RTMP
publish), anything with a passphrase (M6b), and
the version 4 handshake, which no encoder tested so far needed (every one
used version 5).

### M7 Stage resilience and production readiness

**Status (2026-10-21):** the grace period and the health callback are done;
CI's first run on GitHub passed (2026-10-21: format, clippy and tests;
the smoke test with both SRT implementations; fuzz regressions and 60 s
per target). The Linux and Caddy measurements are not done.
- `SAPFLOW_RECONNECT_GRACE` (default 90 s, equal to v4v's
  `PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS`): a dropped publisher's stream,
  playlist and recording are held; a reconnect on either protocol carries
  them on after `EXT-X-DISCONTINUITY`. The split with v4v: sapflow sends
  `unpublish` at the drop and `publish` on the return (v4v shows
  "reconnecting" and holds the gig); sapflow keeps the media continuous.
  Proven by unit tests, `scripts/smoke.sh` (both SRT implementations: same
  chunk names, a discontinuity, playlist rules across it, one recording),
  v4v's `e2e:ingest-srt-sapflow` (with `SAPFLOW_SRT_IMPL=own`) and
  `e2e:ingest-sapflow` with `SAPFLOW_RECONNECT_GRACE=10` to match the
  gate's window, and hls.js in Chrome across a stall and a reconnect
  (`docs/reference.md`). Safari across a reconnect, as against a stall, is
  not yet watched.
- The `health` callback every `SAPFLOW_HEALTH_INTERVAL` (default 30 s):
  bitrate, keyframe interval, audio, breaks (`docs/v4v-contract.md`). v4v
  reads the bitrate today; it reads the keyframe interval from the playlist.

- An encoder that drops (phone locked, network switch) and returns within a
  grace period continues the same stream with `EXT-X-DISCONTINUITY`, and
  hls.js and Safari keep playing; agree the split with v4v's own hold.
- The stream-health callback reports keyframe interval, bitrate and audio,
  so v4v Studio can tell a performer to set 1 s keyframes.
- Measured on Linux behind Caddy: the wired 250 and 750 full-viewer run,
  and glass-to-glass delay.
- CI on the public repo: tests, clippy, fmt, short fuzz runs, nightly long
  fuzzing (`.github/workflows/ci.yml`, `fuzz-nightly.yml`). `ci.yml` passed
  on its first run on GitHub; the nightly fuzz run is not yet seen.

### Later

- `sapflow-core` and `sapflow-srt` for iOS and Android via UniFFI: SRT
  sending (go live from the phone), relayswarm chunk verification, a native swarm peer.
- relayswarm's signed segment commitments produced at the origin.
- A second rendition.

## Decisions made

- **2026-10-20, M4:** keep experimenting; separate-machine testing before any
  production use (first pass across Wi-Fi in `docs/benchmarks.md`).
- **2026-10-20, protocol libraries:** no SRT implementation of our own: keep
  the patched `srt-protocol` under fuzzing, with bindings to libsrt as the
  fallback. Since then its handshake flags and re-requests are patched and
  it matches libsrt under loss over several seeds (`docs/interop.md`).
- **2026-10-20, RTMP is our own, publish only** (user's decision): rather
  than carry a patched `rml_rtmp` (last release 2023-04, two panics on
  unauthenticated input), sapflow has its own sans-IO implementation in
  `sapflow_hls::rtmp`: handshake (original and digest forms), chunk stream,
  AMF0 and the publish commands; playback is refused. It matched `rml_rtmp`
  event for event on a recorded ffmpeg publish before `rml_rtmp` was
  removed, passes the smoke test, and is fuzzed (`rtmp_session`, `amf0`).

- **2026-10-20, publishing:** public as `forgesworn/sapflow`, MIT, starting
  from a single initial commit.
- **2026-10-20, delivery:** the user treats M0 to M5 as delivered. Their
  follow-ups: a wired two-machine run at 250 and 750 full viewers, review of
  the v4v PR, and any move to production.
- **2026-10-20, upstream:** no reports to `srt-rs` or `rml_rtmp`; our
  fixes live in `vendor/srt-protocol` and our own RTMP.

- **2026-10-20, own SRT:** the user decided to replace srt-rs with our own
  MIT implementation (M6), developed inside sapflow first, unencrypted
  receive first and encryption before it becomes the default. This
  supersedes "no SRT implementation of our own" above.

## Decisions owed by the user

None at present.
