# sapflow compared with mediamtx, and our libraries with the ones they replace

Checked on 22 October 2026 against sapflow at commit `eee669c` and mediamtx
v1.21.2 (released 2026-10-08, the latest release on that date). Every claim
below is tied to a file in this repository or to a public page, listed under
[Sources and method](#sources-and-method). Where something could not be
checked it says so. Performance figures (CPU, memory, latency, throughput) are
not repeated here: they live in [benchmarks.md](benchmarks.md) and are linked
from each section. The only measured figures quoted are binary and image sizes,
which that file already records against mediamtx 1.19.1, and a few ingest
figures in the SRT and RTMP library sections.

## Summary

sapflow is a small live media server written in Rust for one job: take a
publisher over SRT or RTMP (H.264 and AAC), serve it as one Low-Latency HLS
rendition, record it to fragmented MP4, and send an HTTP callback about its
health. It was written to replace mediamtx behind the v4v live stage, and it
is deliberately narrow. mediamtx is a general media router: it ingests RTSP,
WebRTC, SRT, RTMP, HLS, MPEG-TS, RTP and Media-over-QUIC, serves most of them
back out, and has an API, metrics, hooks, per-path authentication and a
playback server. sapflow has none of those and does not intend to.

Pick sapflow if your workload is exactly that one job, you want a single small
binary with a handful of environment variables, and you accept that it is an
experiment: it has not run in production, and every measurement so far is from
macOS (`README.md`, `docs/readiness.md`). Pick mediamtx for anything wider, or
whenever you need a broadly featured server with official releases today.

The Rust libraries inside sapflow (SRT, RTMP, LL-HLS packaging, a runtime-free
mobile publishing core) are written to be reusable, but are not published: every
crate is `publish = false` at version 0.0.0 (`Cargo.toml`), so none is on
crates.io. Neither number, 0.0.0 nor 1.21.2, says anything about maturity;
compare the evidence rows instead.

## Feature comparison: sapflow and mediamtx 1.21.2

Marks: ✓ supported, ✗ not supported, partial (explained). "mediamtx.yml" means
the file at tag v1.21.2.

### Ingest

| Feature | sapflow | mediamtx 1.21.2 |
|---------|---------|-----------------|
| SRT in | ✓ Own listener (`sapflow-srt`), live mode. MPEG-TS carrying H.264 and AAC. Stream ID `publish:live/<id>:<user>:<pass>` or the `#!::m=publish,r=...,u=...,s=...` form (`crates/sapflow-core/src/lib.rs`, `README.md`). | ✓ Via gosrt. Publish codecs listed: H265, H264, MPEG-4 Video, MPEG-1/2 Video, Opus, AAC, MP3, AC-3, KLV (mediamtx.org publish/srt page). |
| RTMP in | partial. Publish only, plain RTMP, H.264 and AAC only, no Enhanced RTMP (`crates/sapflow-hls/src/flv.rs` rejects any video codec but AVC). | ✓ Publish and read. RTMPS (`rtmpEncryption`, `rtmpsAddress: :1936`). Video AV1, VP9, H265, H264; audio Opus, FLAC, AAC, MP3, AC-3, G711, LPCM (mediamtx.org publish/rtmp page, mediamtx.yml). |
| RTSP in | ✗ | ✓ UDP, multicast, TCP; RTSPS (mediamtx.yml). |
| WebRTC in | ✗ | ✓ (README, `webrtc: true` in mediamtx.yml). |
| Other publish routes: MPEG-TS, RTP, HLS, Media-over-QUIC | ✗ | ✓ (README feature list). |
| Pull from a remote source (`source:` on a path) | ✗ | ✓ RTSP, RTMP, HLS, UDP MPEG-TS and RTP, SRT, WHEP, MoQT, Raspberry Pi camera (mediamtx.yml). |
| Codecs through the whole pipeline | partial. H.264 and AAC only. A TS stream whose video is not H.264 is refused (`UnsupportedVideo`, `crates/sapflow-hls/src/ingest.rs`); audio other than AAC is ignored. | ✓ Many (see rows above). Does not transcode: its README describes a media router and lists no transcoding. |
| Several publishers | ✓ `SAPFLOW_MAX_PUBLISHERS`, default 16 per protocol. A second publisher on a live path is refused (`docs/threat-model.md` T5). | ✓ Many paths. `overridePublisher: true` by default lets a new client disconnect the current publisher and take its place (mediamtx.yml); sapflow does the opposite. |

### Output and HLS

| Feature | sapflow | mediamtx 1.21.2 |
|---------|---------|-----------------|
| LL-HLS with parts | ✓ fMP4 parts. 200 ms parts and 1 s minimum segments are fixed in code (`MuxerConfig::default`), not settings. | ✓ `hlsVariant: lowLatency`, `hlsSegmentDuration: 1s`, `hlsPartDuration: 200ms` (mediamtx.yml). |
| Blocking playlist reload | ✓ `_HLS_msn`, `_HLS_part`; requests held up to 3 times the target duration (`docs/reference.md`, `docs/threat-model.md` T8). | ✓ (gohlslib v2.4.6, `muxer_stream.go` `handleMediaPlaylist`). |
| Preload hints | ✓ `EXT-X-PRELOAD-HINT` for the next part and the init map (`crates/sapflow-hls/src/playlist.rs`). | ✓ (gohlslib v2.4.6). |
| Delta playlists (`_HLS_skip`, `CAN-SKIP-UNTIL`) | ✗ `_HLS_skip` is ignored (`playlist.rs`, `docs/reference.md`). | ✓ (gohlslib v2.4.6 emits `CAN-SKIP-UNTIL` and handles `_HLS_skip`). |
| Classic HLS and MPEG-TS segments | partial. `SAPFLOW_HLS_MODE=whole` serves complete fMP4 segments without part advertising. No MPEG-TS segments. | ✓ `hlsVariant: mpegts`, `fmp4` or `lowLatency` (mediamtx.yml). |
| Multiple renditions or adaptive ladder | ✗ One video rendition. A second rendition is listed as later work (`GOALS.md`). | ✗ No transcoding, so no ladder. gohlslib writes one video track plus several audio tracks. |
| Same playlist for every viewer | ✓ Always. No sessions, no cookies, no viewer IPs logged (`docs/reference.md`, `docs/threat-model.md` T12). | partial. Per-viewer sessions with a cookie check by default; a request carrying `Authorization: Bearer <hlsCDNSecret>` gets the shared CDN session (`internal/servers/hls/http_server.go` at v1.21.2, mediamtx.yml). |
| TLS and HTTP/2 on the HLS port | ✗ HTTP/1.1 only, no TLS; put Caddy or similar in front. In sapflow's own tests Safari took the low-latency position only over HTTP/2 (`docs/interop.md`). | ✓ `hlsEncryption` with `hlsServerKey` and `hlsServerCert` (mediamtx.yml). |
| HLS tuning | partial. `SAPFLOW_SEGMENT_COUNT` (default 24, minimum 7) and `SAPFLOW_HLS_MODE`. | ✓ Variant, segment count (default 7), segment and part duration, max segment size, always-remux, directory (mediamtx.yml). |
| Other outputs: SRT, RTMP, RTSP, WebRTC (WHEP), MoQ | ✗ HLS only. `play` on RTMP is refused. | ✓ (README). |
| Forward or relay to other servers | ✗ | ✓ `forward`, plus pull and republish (mediamtx.yml, README). |
| Safari and iOS rules held across encoder faults | ✓ `TARGETDURATION` and `PART-TARGET` fixed once, a drop ends the segment and is marked `EXT-X-DISCONTINUITY`; checked on every rendered playlist by `playlist::RuleCheck` and a fuzz target (`docs/reference.md`). | partial. gohlslib raises `targetDuration` when a segment runs long and recomputes `partTargetDuration` per part, logging the change (gohlslib v2.4.6 `muxer_stream.go`). `docs/reference.md` records that Safari refused such playlists in sapflow's tests; whether mediamtx hits it in practice is not measured here. |

### Control, security and operations

| Feature | sapflow | mediamtx 1.21.2 |
|---------|---------|-----------------|
| Publisher authentication | partial. One global publisher user and password (`SAPFLOW_PUBLISH_USER`, `SAPFLOW_PUBLISH_PASS`), or `SAPFLOW_OPEN_PUBLISH=1` for lab use, or scoped per-event SRT sessions issued through a loopback-only control API (`docs/scoped-publish-sessions.md`). Constant-time comparison; 10 failures in 60 s from one address blocks it (`docs/threat-model.md` T4). Path must match `live/event-[a-z0-9-]+`. | ✓ `authMethod: internal`, `http` or `jwt`; per-user, per-action permissions (publish, read, playback, api, metrics, pprof), optionally restricted by path and by source IP (mediamtx.yml). |
| Viewer authentication | ✗ HLS is public by design; the application in front gates access (`docs/v4v-contract.md`). | ✓ Same authentication system covers read (mediamtx.yml). |
| Credentials in the RTMP key | ✓ `<id>?user=<user>&pass=<pass>` (`crates/sapflow-core/src/lib.rs`). | ✓ The same form is what v4v hands out for mediamtx (`docs/v4v-contract.md`). |
| SRT encryption | ✓ AES-128 or AES-256 in CTR mode, one server-wide passphrase that every caller must then use, no plaintext fallback once set (`SAPFLOW_SRT_PASSPHRASE`, `SAPFLOW_SRT_PBKEYLEN`, `docs/own-origin-encryption.md`). Stream ID stays in clear. | ✓ Per-path `srtPublishPassphrase` and `srtReadPassphrase`; mediamtx.yml has no key-length setting. Its library (gosrt) supports 16, 24 and 32 byte keys. |
| Recording | ✓ One fragmented MP4 file per show, named by start time. Per-path skip list, expiry (`SAPFLOW_RECORD_DELETE_AFTER`, default 7d), refuses to record below 2 GB free by default (`docs/threat-model.md` T10). Off unless `SAPFLOW_RECORDINGS` is set. | ✓ fMP4 or MPEG-TS; `recordSegmentDuration: 1h` splits long shows; `recordDeleteAfter: 1d`; off by default (`record: false`). Plus a playback server (`playback`, :9996) and `alwaysAvailableRecorded` (mediamtx.yml). |
| Playback of recordings | ✗ Files only. | ✓ Playback server (README). |
| Events and hooks | partial. An HTTP POST webhook to one URL with a shared token: `publish`, `unpublish`, `recordSegmentComplete` and a periodic `health` (bitrate, keyframe interval, audio, breaks) (`docs/v4v-contract.md`). Cannot run a command. | partial, different mechanism. Hooks run external commands: `runOnInit`, `runOnDemand`, `runOnAvailable`, `runOnUnavailable`, `runOnOnline`, `runOnOffline`, `runOnRead`, `runOnUnread`, `runOnRecordSegmentCreate`, `runOnRecordSegmentComplete`, `runOnConnect`, `runOnDisconnect` (mediamtx.yml). mediamtx.yml has an HTTP address for authentication but no HTTP event callback. In 1.19.1 the ready and not-ready hooks were `runOnReady` and `runOnNotReady`; 1.21.2 names them `runOnAvailable` and `runOnUnavailable`. |
| Control API | ✗ The loopback control API exists only to provision scoped SRT publishing sessions. | ✓ `api` (:9997, off by default) (mediamtx.yml). |
| Metrics | ✗ Logs (`RUST_LOG`) and the `health` callback only. | ✓ Prometheus-compatible `metrics` (:9998) and `pprof`, both off by default (mediamtx.yml). |
| Reconnect grace | ✓ A dropped publisher's stream, playlist and recording are held for `SAPFLOW_RECONNECT_GRACE` (default 90 s, maximum 3600); a returning publisher on either protocol continues them after a discontinuity (`docs/reference.md`, `docs/interop.md`). | Not verified here. mediamtx has `alwaysAvailable` (plays an offline segment on repeat), `hlsMuxerCloseAfter: 60s` and `overridePublisher`; whether a returning publisher continues the same playlist was not checked. |
| Configuration | partial. 27 `SAPFLOW_*` environment variables (26 in `--help`, plus `SAPFLOW_SRT_IMPL`, kept for compatibility: `own` is accepted, `srt-rs` is refused), each overridable by a flag. No config file, no reload; change a setting and restart (`crates/sapflow-server/src/config.rs`). | ✓ An 889-line YAML file with per-path overrides and regex paths, hot reload without dropping clients, and API-driven configuration (mediamtx.yml, README). |
| Platforms | partial. Developed and measured on macOS (Apple silicon); Linux in CI (Ubuntu) and a distroless container image. No Windows build or test is recorded (`docs/readiness.md`, `.github/workflows/ci.yml`, `Dockerfile`). | ✓ Linux, macOS, Windows; release archives for amd64, arm64, armv6 and armv7 on Linux, plus a Docker Hub image (README, release v1.21.2). |
| Packaging | Build from source with Cargo, or the `Dockerfile`. No release binaries yet. | Official release archives and image. |
| Size on disk | 3.5 MB binary, 38.5 MB arm64 container image. Recorded against mediamtx 1.19.1, not re-measured on 1.21.2 (`docs/benchmarks.md`). | 50.5 MB binary, 62.2 MB image, for 1.19.1 darwin/arm64 (`docs/benchmarks.md`). The 1.21.2 darwin/arm64 release archive is 27.5 MB compressed, which is not comparable to a binary size. |
| CPU, memory, latency under load | See [benchmarks.md](benchmarks.md). | See [benchmarks.md](benchmarks.md). |
| Language and memory safety | Rust. `unsafe` is forbidden workspace-wide (`unsafe_code = "forbid"` in `Cargo.toml`). | Go (garbage collected). Library dependencies not audited for `unsafe` here. |
| Licence | MIT | MIT |
| Production use | None yet. README status: experiment; GOALS.md M4 decision was to keep experimenting. | Not assessed here. |

## Choose mediamtx when

- You need a protocol sapflow lacks: RTSP (cameras), WebRTC in or out, SRT or
  RTMP output, MPEG-TS or RTP over UDP, Media-over-QUIC, or pulling a stream
  from another server.
- Your publishers use RTMPS, Enhanced RTMP, HEVC, AV1, VP9, Opus or any codec
  other than H.264 and AAC, or you need several audio tracks.
- You need per-user, per-path or per-IP permissions, HTTP or JWT
  authentication, or authenticated viewers.
- You want a control API, Prometheus metrics, hooks that run commands (for
  example a simulcast relay), a playback server, always-available streams or
  hot configuration reload.
- You need delta playlists, MPEG-TS HLS, TLS directly on the HLS port, or a
  tunable segment and part duration.
- You run Windows, or a 32-bit ARM board, or want official release binaries and
  an image today.
- You cannot afford to be the first production user of a single-maintainer
  server whose protocol stacks are all new code.

## Choose sapflow when

- The whole job is: SRT and/or RTMP in (H.264 and AAC), one LL-HLS rendition
  out, a recording, and a webhook. Nothing else needs to ride on the server.
- You want one small binary configured by a few environment variables, with no
  config file to maintain (3.5 MB against 50.5 MB for 1.19.1; see
  [benchmarks.md](benchmarks.md) for resource use).
- Every viewer should receive byte-identical, cacheable playlists and chunks,
  with no per-viewer session (the behaviour mediamtx reaches only in its
  CDN-secret mode).
- An encoder that drops (a phone on cellular) should not end the stream: the
  target duration and part target are fixed, drops become discontinuities, and
  a dropped publisher is held for a grace period with the same playlist and
  recording. In sapflow's tests hls.js and Safari carried on across drops,
  though Safari fell further behind live after each (`docs/interop.md`).
- You want an SRT passphrase that every caller is required to use, and an SRT
  receiver written with no panicking constructs outside tests (denied by lint)
  and fuzzed, so far for no more than 20 minutes per target.
- You intend to reuse the protocol code in an app: the SRT, RTMP and mobile
  publishing cores are sans-IO Rust with no runtime, camera or socket
  dependency (but see the library status below).
- You are willing to run an experiment, with mediamtx kept ready as the way
  back (`docs/readiness.md`, `docs/trial-plan.md`).

## Libraries: ours and the ones they replace

Common facts for every sapflow crate: licence MIT, version 0.0.0,
`publish = false` (not on crates.io), `unsafe` forbidden, Rust edition 2024.
Dependency counts are unique transitive crates on normal edges from
`Cargo.lock`, so they are not like for like with Go module counts, which are
the direct requirements in `go.mod` (and include test-only modules).

### SRT: `sapflow-srt` against libsrt, srt-rs and gosrt

| | sapflow-srt | libsrt (Haivision) | srt-rs (`srt-protocol`, `srt-tokio`) | gosrt (used by mediamtx) |
|---|-------------|--------------------|---------------------------------------|--------------------------|
| Language | Rust | C++ (C++03 or above) | Rust | Go |
| Version and status | 0.0.0, unpublished. Replaced srt-rs in sapflow on 2026-10-21. | v1.5.7, 2026-08-28; the reference implementation | 0.4.4, last release 2024-05-31; README says "NOT PRODUCTION READY". Removed from sapflow 2026-10-21. | v0.12.0, as pinned by mediamtx 1.21.2 |
| Licence | MIT | MPL-2.0 | Apache-2.0 | MIT |
| Memory safety | `unsafe` forbidden; `unwrap`, `expect`, `panic!` and indexing denied by lint outside tests (`lib.rs`) | No language-level guarantee (C++) | Rust, README states no `unsafe`. Fuzzing found one unauthenticated datagram that crashed it (`docs/threat-model.md` T1). | Go; no `unsafe` import in the library's own code at v0.12.0 (its vendored `x/net` and `x/sys` use it) |
| Sans-IO | ✓ `Listener::handle_datagram`, `poll_transmit`, `poll_timeout`; the caller owns the socket and timers. No async runtime. | ✗ Socket API with internal threads | partial. `srt-protocol` is state machines with no futures or tokio dependency (its README); `srt-tokio` is tokio | ✗ Owns its sockets (`Dial`, `Listen`) |
| Roles | Live-mode listener and receiver; bounded, paced caller and sender | Caller, listener, rendezvous; live and file; bonding groups | Listener, caller, rendezvous, send, receive | Caller and listener; live and message mode |
| Handshake versions | 5 only (no encoder tested needed 4) | Not checked here | Not checked here | 4 and 5 |
| Encryption | AES-128 and AES-256, CTR only. AES-192 and GCM are explicitly unsupported (`crypto.rs`). KM refresh and pre-announce; no plaintext downgrade when configured; wrong secret refused. CTR does not authenticate media. | AES-128, 192 and 256. CTR by default; AES-GCM since 1.5.2 behind a build-time preview option (socket options doc) | "Encryption" ticked in its README; modes not stated | AES-CTR (`crypto.go`); `pbkeylen` 16, 24, 32; `kmrefreshrate`, `kmpreannounce` |
| FEC and packet filter | ✗ | ✓ | Not stated | ✗ ("Not implemented") |
| Bonding, rendezvous, file mode | ✗ | ✓ | Rendezvous ✓; others not stated | ✗ (rendezvous, bonding, buffer mode, FileCC) |
| Dependencies | 7 direct (RustCrypto `aes`, `aes-kw`, `ctr`, `pbkdf2`, `sha1`; `zeroize`, `getrandom`), 22 transitive | C++ standard library; OpenSSL 1.1 for encryption (README) | Not counted | 5 direct requirements in `go.mod`, 3 indirect |
| Fuzz targets | 3: `own_srt_packet`, `own_srt_listener`, `own_srt_caller`; srt-protocol's crashing datagrams kept as regressions (`fuzz/README.md`) | Not checked | Not checked | 2 Go fuzz functions: `FuzzHandshake`, `FuzzPacket` |
| Interoperability evidence | Receiver: ffmpeg 8 (libsrt 1.5.3), `srt-live-transmit` (libsrt 1.5.4), OBS 32.1.2, Larix on iPhone, each with and without a passphrase; v4v's ingest gate. Sender: exact bytes into libsrt 1.5.4, no loss and six seeded impaired profiles. All in `docs/interop.md`, `docs/mobile-delivery-status.md`. | Is the reference; the others are measured against it | Was sapflow's receiver until 2026-10-21; compared as a black box | Accepts FFmpeg, GStreamer and OBS Studio publishers in mediamtx (publish/srt page); no independent run here |
| Written from | The SRT draft and recorded sessions only. Never libsrt or srt-rs source (`AGENTS.md`). | n/a | n/a | Its README says the published RFC |

Measured (2026-10-22, one M4, libsrt 1.5.7's `srt-live-transmit` as sender,
three runs per cell; conditions and limits in [benchmarks.md](benchmarks.md#srt-and-rtmp-our-implementations-against-libsrt-gosrt-and-mediamtxs-rtmp-2026-10-22)):
at 2, 5 and 10% loss, 200 and 2000 ms, plain and AES-128, sapflow-srt's
retransmission overhead range overlapped libsrt's in every condition while
gosrt's (mediamtx) averaged 0.2 to 0.9 points higher, but sapflow's recording dropped many more
frames than either after a packet left unrecovered at 200 ms (by design), and
with 50 concurrent SRT publishers it logged video parser errors that are not yet
explained or fixed. Per stream, with LL-HLS packaging, sapflow used 0.7 to 1.3% of
a core and 19 to 25 MB against mediamtx's 1.3 to 4.1% and 43 to 92 MB.

What the loss evidence does and does not say (`docs/interop.md`, "Our own SRT
under packet loss"): with libsrt's `srt-live-transmit` as sender over a
loopback proxy with an emulated 60 ms round trip, six seeds, 8 conditions
(3, 10 and 20% random loss and 8% bursty loss, at 200 ms and 2000 ms latency),
the receiver was within run-to-run spread of libsrt or better at every
condition. It left fewer packets unrecovered than libsrt at 200 ms with 10 and
20% random loss, and used less retransmission overhead at some 2000 ms
conditions. It was not shown to deliver more than libsrt in general. Limits: one
sender, one clip, no jitter or reordering, a busy shared machine. With a
passphrase, every packet was delivered at 2000 ms; at 200 ms overhead was above
libsrt's on a loaded machine and the run is to be repeated quietly
(`docs/readiness.md`, section 3).

Choose libsrt when you need the reference implementation, rendezvous, bonding,
FEC, file transfer, AES-192 or GCM, or a library with a long record across
vendors. Choose gosrt if you are in Go and need handshake version 4 or message
mode. Choose `sapflow-srt` when you want a small sans-IO Rust receiver and
sender you can embed with no C dependency, and you only need live mode with
AES-128 or AES-256. srt-rs is not recommended for new work given its last
release and its own README note.

### RTMP: `sapflow_hls::rtmp` against gortmplib, librtmp and nginx-rtmp-module

The RTMP code lives in `crates/sapflow-hls/src/rtmp`, inside the `sapflow-hls`
crate.

| | sapflow RTMP | gortmplib (used by mediamtx) | librtmp (rtmpdump) | nginx-rtmp-module |
|---|--------------|------------------------------|--------------------|-------------------|
| Language | Rust | Go | C (not stated on the pages read) | C |
| Version and status | 0.0.0, unpublished, part of `sapflow-hls` | v1.0.4, as pinned by mediamtx 1.21.2 | RTMPDump v2.4 dated 2011-07-20 on its man page; the project site points to a 2.6 in git | Latest tag v1.2.2, 2021-05-25; repository last pushed 2024-12-24 |
| Licence | MIT | MIT | LGPL for librtmp, GPLv2 for rtmpdump (project site) | BSD-2-Clause |
| Memory safety | `unsafe` forbidden; no unchecked length arithmetic or allocation from a declared length (`rtmp/mod.rs`) | Go; no `unsafe` import in the library's own code | No language-level guarantee (C) | No language-level guarantee (C) |
| Sans-IO | ✓ `Connection::feed`, `poll`, `take_output` | ✗ Works on `net.Conn` and `io.ReadWriter` | ✗ | ✗ Runs on nginx's event loop |
| Role | Server session: publish only, `play` refused. Separate publishing client for the mobile SDK (`rtmp/client.rs`). | Client and server; play and publish | Client library with "a few server functions" (man page) | Server: publish, play, relay, VOD |
| Handshake | Server answers a digest C1 with a digest S1 and signed S2, anything else with the original form. Client: original only, digest refused. | Includes Diffie-Hellman based RTMPE (README) | RTMPE supported | Not stated in the README |
| AMF0 | ✓ Bounded: depth 16, 4,096 values per message | ✓ | Not stated on the pages read | Not stated |
| AMF3 | partial. AMF3-typed data and command messages (types 15 and 17) are accepted only when the body after the first byte decodes as AMF0. There is no AMF3 value decoding. | AMF0 only per README; AMF3 appears only as message type constants | Not mentioned on the man page | Not stated |
| Enhanced RTMP, HEVC, AV1 | ✗ Legacy FLV with AVC and AAC only; anything else returns `UnsupportedVideo` | ✓ "Most Enhanced RTMP features"; AV1, VP9, H265, H264, Opus, FLAC, AAC, MP3, AC-3, G711, LPCM; multiple tracks | Not documented | README lists H264 and AAC |
| RTMPS | Server ✗ (terminate TLS in a proxy; `docs/trial-plan.md`). Client: the crate leaves TLS and certificate validation to the caller; the iOS adapter uses system trust, with a negative certificate check shown and trusted RTMPS not yet qualified (`docs/mobile-ios.md`). | ✓ | ✓ (man page: RTMPS, RTMPE, tunnelled variants) | Not stated in the README |
| Auth in key or query | The key and its query reach the application, which reads `user` and `pass` (`sapflow-core`) | n/a (mediamtx supplies authentication) | n/a | `on_publish` and other HTTP callbacks (README) |
| Chunk sizes and limits | Accepts 1 byte to 16 MiB chunks; announces 4096 itself; at most 64 chunk streams and 16 created streams; 1 MiB buffered before the publish is accepted, 40 MiB after; 10 s for handshake and publish (`docs/threat-model.md` T2) | Not checked | Not checked | Not checked |
| Dependencies | The digest handshake needs `hmac` and `sha2`, but the code ships in `sapflow-hls`: 8 direct and 46 transitive crates for the whole crate | 4 direct requirements in `go.mod` (mp4 library, `mediacommon`, `uuid`, `testify`), 1 indirect | OpenSSL by default; GnuTLS or PolarSSL alternatives; zlib | nginx itself |
| Fuzz targets | `rtmp_session`, `amf0`, `rtmp_client`, `flv_ingest`. Twenty-minute runs: `rtmp_session` 4.7 million inputs, `amf0` 45 million, no crash (`docs/threat-model.md`). `rtmp_client` was added later and has only 60 s runs on record. | 9 Go fuzz functions: chunk, handshake client and server, AMF0, raw message reader, message reader | Not checked | Not checked |
| Interoperability evidence | Server: ffmpeg 8.0.1, OBS 32.1.2, Larix on iPhone, v4v's ingest gate; matched `rml_rtmp` event for event on a recorded ffmpeg publish before that crate was removed. Client: an independent ffmpeg received 180 H.264 and 283 AAC frames. No record of the client publishing to nginx-rtmp-module or mediamtx. | Used by mediamtx; independent evidence not checked | Not assessed here | README lists Wirecast, FMS, Wowza, ffmpeg and others |

Measured (2026-10-22, one M4, 1, 10 and 50 concurrent ffmpeg publishers at
720p30 and 2.5 Mbit/s, no viewers, three runs per cell; see [benchmarks.md](benchmarks.md#srt-and-rtmp-our-implementations-against-libsrt-gosrt-and-mediamtxs-rtmp-2026-10-22)):
per stream, with LL-HLS packaging, sapflow's server used 0.2 to 0.6% of a core
and 19 to 25 MB against mediamtx's 0.4 to 1.5% and 40 to 90 MB, and every stream
produced a playlist. The RTMP code's own share and the handshake cost were not
separated from the server's.

Choose gortmplib (or mediamtx) when you need RTMPS, Enhanced RTMP, other codecs
or RTMP playback. Choose librtmp when you need a C client library with RTMPE
and RTMPS and can accept its age and licence terms. Choose nginx-rtmp-module
when you already run nginx and want RTMP publish, play and relay there.
Choose sapflow's RTMP when you want a bounded, sans-IO, fuzzed publish-only
server and client in Rust and H.264 with AAC is enough. Not tried: vMix, and
an RTMP publisher from sapflow's own client against a third-party server
(`docs/readiness.md`, `docs/interop.md`).

### LL-HLS packaging: `sapflow-hls` against gohlslib

| | sapflow-hls | gohlslib v2.4.6 (used by mediamtx) |
|---|-------------|------------------------------------|
| Language | Rust | Go |
| Version and status | 0.0.0, unpublished | v2.4.6, 2026-10-08 |
| Licence | MIT | MIT |
| Scope | Packager only: MPEG-TS and FLV demux in, fMP4, playlists and recording out. No HTTP serving (the server crate does that), no HLS client, no playlist parser. | Muxer, HTTP handler for it, HLS client, and a playlist parser and writer |
| Output formats | Low-latency fMP4, or whole-segment fMP4 | MPEG-TS, fMP4, Low-Latency |
| Codecs | H.264 and AAC | AV1, VP9, H265, H264, Opus, FLAC, AAC; KLV on write |
| Tracks | One video, one audio | One video, several audio |
| LL-HLS features | Blocking reload, preload hints, `PART-HOLD-BACK` at 3 times the part target (Apple authoring spec 14.3), no delta updates | Blocking reload, preload hints, delta updates, `PART-HOLD-BACK` at 2.5 times the part target, rendition report tags in the playlist package |
| Apple rule handling | `TARGETDURATION` and `PART-TARGET` fixed once; breaks and discontinuities instead of raising them; every render checked by `RuleCheck`, `scripts/check-playlist.py` and the `muxer_timing` fuzz target | Recomputes both values as the stream runs and logs changes |
| Memory safety | `unsafe` forbidden | Go; no `unsafe` import in the library's own code (examples excepted) |
| Dependencies | 8 direct, 46 transitive crates | 3 direct requirements in `go.mod` (`go-astits`, `mediacommon`, `testify`), 4 indirect |
| Fuzz targets | `ts_ingest`, `flv_ingest`, `muxer_timing`, `small_parsers` | 1 Go fuzz function (playlist unmarshal) |
| Evidence | Plays in hls.js, Safari on macOS and iPhone, the iOS simulator; v4v's ingest gates (`docs/readiness.md`) | Serves mediamtx; independent evidence not checked |

Measured (playlist and chunk response times, part fan-out): see
[benchmarks.md](benchmarks.md).

Choose gohlslib for a wider codec set, MPEG-TS variants, delta updates, a
client, or if you are in Go. Choose `sapflow-hls` for a Rust packager that
keeps Apple's rules under bad encoder timing and can be embedded without an
HTTP stack. Today it cannot be depended on without taking the RTMP and
ingest code along.

### Mobile publishing core: `sapflow-client` and `sapflow-ffi` against mobile SDKs

These are a different layer from capture SDKs. `sapflow-client` is a
runtime-free core: it validates hardware-encoded H.264 and AAC-LC samples,
owns bounded queues and session policy, writes MPEG-TS, and does no camera,
codec, network or file work. `sapflow-ffi` exposes it to Swift and Kotlin
through UniFFI 0.32.2. Platform adapters in `mobile/` supply capture,
hardware encoding and sockets. The project's own competitive review compares
apps (Larix, Moblin, IRLPro), not SDKs, and says nothing here contradicts it
(`docs/mobile-competitive-review.md`). The Larix SDK page could not be fetched
and is not assessed.

| | sapflow-client and sapflow-ffi | HaishinKit (Swift) | StreamPack (Kotlin) |
|---|---------------------------------|--------------------|---------------------|
| Language and platforms | Rust core; generated Swift and Kotlin bindings; iOS and Android adapters in `mobile/` | Swift; iOS 15+, macOS 12+, tvOS, visionOS (README) | Kotlin; Android API 21+ |
| Status | 0.0.0, `publish = false`, not distributed. Only finite local gates; physical-device, sustained and release gates are open (`docs/mobile-delivery-status.md`). | Release 2.2.5, 2026-03-28 | Release 3.2.1, 2026-10-03 |
| Licence | MIT | BSD-3-Clause | Apache-2.0 |
| Includes capture and encoding | ✗ By design; the native adapter owns them | ✓ Camera, microphone, screen, mixing | ✓ Camera2, MediaCodec, screen, audio-only mode |
| Protocols | Own SRT caller (AES-128 and AES-256 CTR) and own RTMP and RTMPS publisher; RTMPS TLS is supplied by the platform | RTMP, SRT (libsrt 1.5.7 dependency), WHEP and WHIP (alpha) | RTMP, RTMPS, Enhanced RTMP, SRT (libsrt) |
| Codecs | H.264 video, AAC-LC audio only (4-byte AVCC framing, 4 MiB sample cap) | Not checked | HEVC, AVC, VP9, AV1; AAC LC, HE, HEv2 and Opus |
| Distribution | None yet; source in this repository | Swift Package Manager | Maven Central |
| Dependencies | `sapflow-client` 33 transitive crates; `sapflow-ffi` 95 (UniFFI) | Not checked | Not checked |
| Fuzz targets | `mobile_media`, `rtmp_client`, `own_srt_caller`, `publish_session` | Not checked | Not checked |

Measured: no mobile figures are recorded in [benchmarks.md](benchmarks.md);
the native performance targets are unmeasured
(`docs/mobile-competitive-review.md`).

Use HaishinKit or StreamPack today if you want a ready-made capture-to-network
SDK for one platform. The sapflow crates are for a single Rust protocol and
session core shared by both platforms, and they are not yet something to
adopt: no external consumer exists and no package is published
(`docs/library-quality-plan.md`, LQ5 and LQ7).

## Gaps sapflow has today

Origin server:

- No production use. The first trial is a planned staged one on a single
  non-critical show, with mediamtx kept running (`docs/readiness.md`).
- Every benchmark and profile is from macOS. Linux, a wired two-machine run at
  250 and 750 full viewers, behind a real Caddy, and a show-length soak are all
  open (`docs/readiness.md`, section 3).
- The one side-by-side delay test against mediamtx put sapflow several seconds
  further behind in Safari over HTTP/1.1, because its target duration was 3
  against 2. It has not been repeated through Caddy over HTTP/2
  (`docs/interop.md`, `docs/readiness.md`).
- sapflow does not serve TLS or HTTP/2 itself, and Safari only takes the
  low-latency position over HTTP/2, so a proxy is mandatory for Safari
  (`docs/interop.md`).
- Ingest is H.264 and AAC only, over SRT and plain RTMP. No RTMPS server, no
  Enhanced RTMP, no HEVC or AV1, no RTSP, no WebRTC, no MPEG-TS over UDP.
- One output: HLS. No SRT or RTMP out, no relay, no WebRTC monitor
  (`docs/readiness.md`, section 4).
- One video rendition, no delta playlists, no MPEG-TS HLS, no tunable segment or
  part duration.
- Authentication is one global publisher credential, or scoped sessions through
  the loopback control API. Viewers are unauthenticated. No roles, JWT or
  per-path rules.
- No metrics endpoint, no general API, no hooks that run commands. Observability
  is logs and the `health` webhook.
- No config file and no hot reload.
- No Windows build or test is recorded, and there are no release binaries.
- Larix's first SRT attempt sometimes connects and sends no data, unexplained
  (`docs/interop.md`). vMix, Larix on Android and a locked phone are untried.
- One maintainer, and both ingest protocols are our own code with no upstream
  to lean on. Mitigations are fuzzing, the libsrt loss comparison and the
  mediamtx fallback (`docs/readiness.md`, section 4).

Libraries:

- Not published. All crates are `publish = false` at 0.0.0; there is no API
  stability promise, migration policy or external consumer
  (`docs/library-quality-plan.md`).
- RTMP cannot be taken alone: it sits inside `sapflow-hls`, with 46 transitive
  crates.
- SRT: live mode only, handshake version 5 only, AES-128 and AES-256 in CTR
  mode only (no GCM, no AES-192), no FEC, bonding, rendezvous or file mode. The
  stream ID and its credentials are always visible; media encryption does not
  hide them (`docs/threat-model.md`). A full encrypted qualification matrix is
  open (`GOALS.md`, M6).
- Fuzzing: ten targets ran 20 minutes each on GitHub on 2026-10-21 with no
  crash. Targets added after that (including `own_srt_caller`) have only 60 s
  CI runs on record, and no target has run for hours (`docs/readiness.md`,
  `fuzz/README.md`).
- Mobile: no physical-device acceptance, only H.264 and AAC-LC, and no
  capture or codec code in the libraries by design
  (`docs/mobile-delivery-status.md`).

## Sources and method

sapflow: this repository at commit `eee669c` (README.md, GOALS.md, AGENTS.md,
the files under `docs/` cited above, `fuzz/README.md`, `crates/*/Cargo.toml`,
the crates' source, `crates/sapflow-server/src/config.rs`). Dependency counts:
`cargo tree -p <crate> --edges normal --locked --offline --prefix none`, reduced
to unique crate names and versions, excluding sapflow's own crates. Fuzz target
list: `fuzz/fuzz_targets`.

mediamtx v1.21.2 (release page: https://github.com/bluenviron/mediamtx/releases/tag/v1.21.2):

- README: https://github.com/bluenviron/mediamtx/tree/v1.21.2
- Configuration: https://raw.githubusercontent.com/bluenviron/mediamtx/v1.21.2/mediamtx.yml
- Dependencies: https://raw.githubusercontent.com/bluenviron/mediamtx/v1.21.2/go.mod
  (gohlslib v2.4.6, gortmplib v1.0.4, gosrt v0.12.0, pion WebRTC libraries)
- HLS sessions and CDN secret: `internal/servers/hls/http_server.go` at the tag
- Documentation pages: https://mediamtx.org/docs/publish/srt,
  https://mediamtx.org/docs/publish/rtmp,
  https://mediamtx.org/docs/features/hooks,
  https://mediamtx.org/docs/other/srt-specific-features,
  https://mediamtx.org/docs/other/rtmp-specific-features. The site is not
  versioned, so a page may describe a later release than the tag.

Go libraries, each read at the tag mediamtx pins: README, `go.mod`, licence
file, and a text search of the source for fuzz functions, `unsafe` imports,
`CAN-SKIP-UNTIL` handling and (for gosrt) the cipher mode:

- https://github.com/bluenviron/gohlslib (v2.4.6)
- https://github.com/bluenviron/gortmplib (v1.0.4)
- https://github.com/datarhei/gosrt (v0.12.0)

Others:

- libsrt: https://github.com/Haivision/srt (README, repository metadata and
  release v1.5.7; socket options page
  https://github.com/Haivision/srt/blob/master/docs/API/API-socket-options.md).
  libsrt source was not read.
- srt-rs: https://github.com/russelltg/srt-rs (README) and crates.io entries for
  `srt-protocol` and `srt-tokio`. srt-rs source was not read.
- librtmp: https://rtmpdump.mplayerhq.hu/ and https://rtmpdump.mplayerhq.hu/librtmp.3.html
- nginx-rtmp-module: https://github.com/arut/nginx-rtmp-module (README, tags)
- HaishinKit: https://github.com/HaishinKit/HaishinKit.swift (README, releases)
- StreamPack: https://github.com/ThibaultBee/StreamPack (README, releases)

Not verified: libsrt's passphrase length limit against sapflow's (sapflow
accepts 10 to 79 bytes; libsrt's options page says 10 to 80 characters);
libsrt's and srt-rs's supported handshake versions; whether mediamtx continues
a playlist across a publisher reconnect; librtmp's implementation language,
AMF3 handling and exact current release; nginx-rtmp-module's RTMPS, AMF3 and
Enhanced RTMP support (the README does not mention them); fuzzing in libsrt,
srt-rs, librtmp, nginx-rtmp-module, HaishinKit and StreamPack; the Larix SDK.
