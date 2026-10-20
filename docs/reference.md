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
| `EXT-X-TARGETDURATION` is the longest segment rounded to the nearest second, and may only rise during a stream; changing it breaks iOS. | `muxer_stream.go` `targetDuration`, `rotateSegments` | `sapflow_hls::TargetDuration` |
| Every part must be at least 85% of the longest part, so the part duration is chosen to suit every frame duration seen (video and audio). It is frozen after the first segment. | `muxer_segmenter.go` `findCompatiblePartDuration`, `fmp4AdjustPartDuration` | `sapflow_hls::compatible_part_duration` |
| The LL-HLS playlist starts with seven gap segments (same duration as the first real one); iOS needs them. This is why mediamtx refuses fewer than 7 segments. | `muxer_stream.go` `rotateSegments` | M1 |
| Blocking reload: a request with `_HLS_msn` beyond last + 1, or older than the window, gets 400; otherwise it waits until that part exists. `_HLS_part` without `_HLS_msn` is 400. | `muxer_stream.go` `handleMediaPlaylist` | M1 |
| `PART-HOLD-BACK` is 2.5 x part target; `CAN-SKIP-UNTIL` is 6 x target duration; `CAN-BLOCK-RELOAD=YES`. | `generateMediaPlaylistFMP4` | M1 |
| Segments are cut only on a keyframe once the minimum duration has passed, so segment length follows the encoder's keyframe interval. | `fmp4WriteSample` | M1 |
| fMP4 base time must not be negative; gohlslib adds a fixed start offset to every DTS. | `fmp4WriteSample` (`fmp4StartDTS`) | M1 |
| LL-HLS playlists are `Cache-Control: no-cache`; segments and parts are cacheable. | `mediaPlaylistMaxAge`, segment handlers | M1 |

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
| Async runtime, HTTP | `tokio` 1.54, `axum` 0.8.9 | `hyper-util` legacy client for the callback, no `reqwest` |
| SRT | `srt-tokio` 0.4.4 (pure Rust) | Last release 2024-05. Builds on Rust 1.94 and tokio 1.54, interoperates with libsrt's `srt-live-transmit`, and exposes the stream id before accept, so publishers are refused in the handshake. Risks: unmaintained upstream, and `srt-protocol` has a `panic!` on one malformed-handshake encode path (M5: fuzz it, or move to libsrt bindings). |
| SRT parsing | `srt-protocol` 0.4.4, vendored and patched | Fuzzing found that one unauthenticated datagram crashed the server; fixes and the reasons are in `vendor/srt-protocol/PATCHES.md`. |
| MPEG-TS demux | `mpeg2ts` 0.6.1 | Fed one 188-byte packet at a time from our buffer (`sapflow_hls::ingest`), because its reader treats some stream errors as fatal and stops at end of input. |
| H.264 | `h264-reader` 0.9.0 | Annex B splitting and SPS parsing. |
| AAC | `adts-reader` 0.4.0 | ADTS headers. It reports the raw-block count as the field plus one. |
| fMP4 writing | `mp4-atom` 0.16.2 | `ftyp`/`moov`/`moof`/`mdat` encoding. |
| RTMP (M2) | our own, `sapflow_hls::rtmp` | Publish only, sans-IO: handshake (original and digest forms), chunk stream, AMF0, the publish commands. Replaced `rml_rtmp` 0.8.0 (last release 2023-04), in which fuzzing found two panics on unauthenticated input. The digest handshake uses `hmac` 0.12.1 and `sha2` 0.10.9. |
| Fuzzing (M5) | `cargo-fuzz` | |

Licences: every dependency is permissive. `srt-tokio`, `srt-protocol` and
`sync_wrapper` are Apache-2.0 only (no NOTICE files); the rest are MIT, BSD,
Unicode-3.0 or dual-licensed. Distributed binaries and images must carry the
third-party licence texts; forking `srt-tokio` would keep Apache-2.0 on that
code.

Our own parsing is the SRT stream id, the RTMP stream key, the chunk file
names, the `_HLS_msn`/`_HLS_part` query, the FLV video and audio tag headers
(5 and 2 bytes), the AudioSpecificConfig (2 bytes) and NAL length prefixes,
all small and unit-tested, and, since 2026-10-20, the whole RTMP protocol
(publish only: `sapflow_hls::rtmp`). Each has a fuzz target.

### RTMP handshake findings (2026-10-20)

- Observed: ffmpeg 8.0.1 publishing sends a digest C1 (version 9.0.124.2)
  and echoes S1 as C2. ffmpeg playing (`ffmpeg -i rtmp://...`), which
  validates the server's digest, accepts our S1 and S2 and reaches the play
  refusal.
- From memory of librtmp's `handshake.h` (OBS's RTMP output), not re-read
  here: it sends a digest C1, verifies a digest S1 and signed S2 strictly
  when S1 carries a server version, and falls back to the original form
  when the version is zero. The unit tests check our answer against that
  algorithm; OBS itself has not been run against sapflow yet.
- So sapflow answers a digest C1 with a digest S1 (server version
  13.14.10.13, as nginx-rtmp) and a signed S2, and anything else with the
  original form (S2 echoes C1). C2 is not checked, as in nginx-rtmp,
  mediamtx and rml_rtmp. Larix and vMix are untested; both are known to
  work with nginx-rtmp and mediamtx, which answer the same way.

## Related

relayswarm (forgesworn) already has a Node HLS origin that signs a Nostr
commitment per chunk, two renditions and admission tickets, but it receives
via mediamtx and ffmpeg and serves 2 s classic HLS, not LL-HLS. It is the
consumer sapflow's origin should eventually feed.
