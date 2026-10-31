# sapflow

[![CI](https://github.com/forgesworn/sapflow/actions/workflows/ci.yml/badge.svg)](https://github.com/forgesworn/sapflow/actions/workflows/ci.yml)

A small live media server in Rust for one job: take a stream over SRT or
RTMP (H.264 and AAC), serve it as Low-Latency HLS, record it to fragmented
MP4, and tell your app how the stream is doing. One 3.3 MB binary, a handful
of environment variables, no `unsafe`. The SRT, RTMP and LL-HLS code is our
own, written as sans-IO Rust libraries that an iOS or Android app can share.

It was built to replace [mediamtx](https://github.com/bluenviron/mediamtx)
behind a live music stage, and is deliberately narrow. If you need RTSP,
WebRTC, pulling from cameras, an API or many codecs, use mediamtx (see
[When to choose mediamtx](#when-to-choose-mediamtx-instead)).

## How it compares

Against mediamtx 1.21.2 on the same machine, same input, same simulated
LL-HLS viewers (every viewer fetching video), medians of three runs:

| | sapflow | mediamtx 1.21.2 |
|---|---|---|
| CPU, 250 / 750 viewers | 18% / 62% of a core | 60% / 155% |
| Peak memory, 250 / 750 viewers | 69 / 216 MB | 161 / 274 MB |
| New part reaching every viewer, median, 250 / 750 | 13 / 28 ms | 21 / 34 ms |
| Failed requests, 12 loaded runs | 0 of 3.55 million | 1 of 3.56 million |
| Binary / idle memory | 3.3 / 3.4 MB | 52 / 32 MB |
| CPU / memory per RTMP publisher, 50 at once | 0.2-0.4% / 20 MB | 0.4-0.7% / 40-42 MB |
| Delay through the server | same | same |

Conditions: an Apple M4 Max on macOS, everything over loopback, plain
HTTP/1.1, no CDN or proxy. It measures each server's own cost, not a network.
mediamtx 1.21 also serves audio as a separate rendition that the simulated
viewers do not fetch, which slightly understates its load. Full method,
ranges and every caveat: [docs/benchmarks.md](docs/benchmarks.md).

Our SRT receiver against libsrt 1.5.7 and gosrt (mediamtx's SRT), 2 to 10%
random loss, plain and AES-128, three runs each: it asked for no more
retransmissions than either, and at a tight 200 ms latency left the fewest
packets unrecovered (56 against 70 and 78 over six runs at 10% loss). At 2 s latency none
of the three lost anything. Details:
[docs/benchmarks.md](docs/benchmarks.md#srt-and-rtmp-our-implementations-against-libsrt-gosrt-and-mediamtxs-rtmp-2026-10-22).

A feature-by-feature comparison with mediamtx, and of our libraries with
libsrt, srt-rs, gosrt, gortmplib, librtmp, nginx-rtmp-module and gohlslib, is
in [docs/comparison.md](docs/comparison.md).

## When to choose mediamtx instead

- You need RTSP, WebRTC, MPEG-TS/RTP, Media-over-QUIC, or to pull from
  cameras and other servers.
- You need codecs beyond H.264 and AAC (HEVC, AV1, VP9, Opus, ...), RTMPS,
  or Enhanced RTMP.
- You want an HTTP API, Prometheus metrics, per-path auth rules or hooks
  that run commands.
- You need delta playlists (`_HLS_skip`) or several renditions.
- You want a server with years of production use behind it today.

Choose sapflow when the job is exactly "one encoder in, LL-HLS out,
recorded", you care about CPU, memory and a small attack surface, and you
want the protocol code as reusable, fuzzed, memory-safe Rust.

## Status

Reviewed 28 October 2026 against `3c7e1aa`: the own HTTP viewer loop,
Hyper/Axum removal, private Tor publishing and receiver TV player are merged,
and current-main CI passes. Earlier origin checks passed v4v's ingest gate,
Safari playback on iPhone (3.0 to 3.2 s glass to glass over HTTP/2) and
recovery after encoder drops. Those receipts do not qualify the newer TV
player on a physical iPhone.

The TV player has successful generated-media Chrome loops of about 13.5
minutes. Actual Safari/iPhone acceptance, long private-preview and publishing
runs, separate-network peer savings and wired Linux/Caddy audience capacity
remain open. Retained-HLS load qualification, native recording recovery,
complete phone shows and signed mobile releases also remain open. There is
no recorded acceptance of a real V4V production show in these documents.

The crates are version 0.1.0 on [crates.io](https://crates.io/crates/sapflow).
The current queue and acceptance gates are in [GOALS.md](GOALS.md#remaining-work),
[docs/delivery-plan.md](docs/delivery-plan.md#next-work) and
[docs/readiness.md](docs/readiness.md); rollout steps remain in
[docs/trial-plan.md](docs/trial-plan.md).
New coding sessions can start with the [Codex handover](docs/codex-handover.md).

| Crate | What it is |
|-------|------------|
| `sapflow-srt` | SRT listener/receiver and caller/sender, sans-IO, AES-128/256-CTR |
| `sapflow-rtmp` | RTMP publishing, server side and client, sans-IO |
| `sapflow-hls` | FLV and MPEG-TS ingest, LL-HLS packaging, playlists, Apple's rules |
| `sapflow-core` | Shared types: stream paths, callback events. No async runtime |
| `sapflow-client` | Runtime-free media validation, bounded queues and session policy for apps |
| `sapflow-ffi` | Typed Swift/Kotlin boundary for the shared core |
| `sapflow` | The server binary (`crates/sapflow-server`): ingest, HTTP, callbacks, recording |

The server serves viewers from our own Mio HTTP event loop; the Hyper viewer
backend was removed, and `SAPFLOW_HTTP_IMPL=hyper` now stops startup. The
shared protocol and compressed media libraries have no async runtime. The
server still uses Tokio for ingest adapters, the private control API and
callbacks, timers and recording; the control API and the callback client are our
own small HTTP/1.1 code (`crates/sapflow-server/src/http_lite.rs`), with no Axum
or Hyper.

`cargo add sapflow-srt` (or `sapflow-rtmp`, `sapflow-hls`) for a library,
`cargo install sapflow` for the server. API docs are on
[docs.rs](https://docs.rs/sapflow-srt). Rust 1.94, edition 2024. A complete SRT listener is
[crates/sapflow-srt/examples/listen.rs](crates/sapflow-srt/examples/listen.rs);
for RTMP, `sapflow_rtmp::Connection` accepts a publish. No C dependencies.

## Build and run

```bash
cargo test --workspace
scripts/smoke.sh     # end to end: SRT and RTMP in, LL-HLS out, recording
scripts/tidy.sh
```

Run it:

```bash
SAPFLOW_PUBLISH_USER=<publisher> SAPFLOW_PUBLISH_PASS=<password> \
SAPFLOW_CALLBACK_URL='http://<app>:5181/api/public/live-events/{eventId}/stream-health' \
SAPFLOW_CALLBACK_SECRET=<callback-secret> \
cargo run --release -p sapflow
```

Publish to `srt://<host>:8890?streamid=publish:live/<eventId>:<publisher>:<password>`
or `rtmp://<host>:1935/live` with key `<eventId>?user=<publisher>&pass=<password>`,
and play `http://<host>:8888/live/<eventId>/index.m3u8`. `sapflow --help`
lists every setting. To give every gig its own key, set
`SAPFLOW_PUBLISH_AUTH_URL` instead of the user and password, and sapflow asks
the app about each publisher ([the v4v contract](docs/v4v-contract.md#publish-authorisation)).

For a continuously looping folder of prepared videos, see the
[TV channel guide](docs/tv-channel.md), including separate live gig paths and an
optional HAProxy TLS/HTTP2 example.

sapflow serves HTTP/1.1 without TLS. Put Caddy or another HTTP/2 proxy in
front: Safari takes the low-latency position only over HTTP/2 (about 3 s
glass to glass through it, against about 10 s over HTTP/1.1; see
[docs/interop.md](docs/interop.md)). There is a container image
([Dockerfile](Dockerfile)); no prebuilt release binaries yet.

`SAPFLOW_HLS_MODE=whole` serves complete segments without low-latency part
advertising. Use it when a delivery path cannot fetch parts quickly enough;
the default remains `low-latency`. Both modes serve the same encoded media,
segment timeline and codec changes.

Resident presentations are limited by `SAPFLOW_MAX_PRESENTATIONS` (also
`--max-presentations`), defaulting to four times `SAPFLOW_MAX_PUBLISHERS`,
capped at 4,096 (64 by default). Both protocols share the domain. A stream
stays counted through its final owner, including reconnect hold and removed
streams retained by publications or parked requests. At capacity a new
presentation is refused; same-path reconnects reuse their reservation. Close
releases cached ingest and playlists while accepted responses retain valid
bytes.

`SAPFLOW_MAX_FRAGMENT_BYTES` (`--max-fragment-bytes`) independently limits shared
encoded init/fragment backing capacity to 512 MiB by default. Each allocation
is charged once, including headers and spare Vec capacity. Cache, response,
sliced and recorder aliases keep that charge through their final payload
owner. Exhaustion refuses media and preserves the admitted recording prefix
as incomplete. The cap covers retained media, not the whole process: on
Linux the server reads its cgroup memory limit at startup and warns when
the cap is above three quarters of it, since a high-bitrate publisher could
then get the server killed instead of refused.

`SAPFLOW_HTTP_BODY_TIMEOUT` (`--http-body-timeout`) gives each ready framed
response 120 seconds by default, configurable from 1 through 3,600. Successful
writes do not extend it. Parked LL-HLS waiting is excluded, and each keep-alive
response gets a fresh timer. Expiry closes the incomplete response. These are
logical resource policies; RSS, kernel memory and audience capacity need
separate qualification in the [resource contract](docs/retained-media-contract.md).

Recording limits retained encoded originals to 512 MiB across the server and
64 MiB per stream path, including queued and active parts from draining codec
epochs and old connections. These byte ceilings are reduced if the platform's
semaphore cannot represent them. Each path allows two writers; the server allows
`10 * SAPFLOW_MAX_PUBLISHERS`, capped at 1,024 writers (160 by default). This
allows draining predecessors alongside current writers, subject to those caps.
Each writer also retains its 256-message queue cap. Empty epochs start no task
or file. Refusing the first part reports failure without a completion; refusing
later backlog drains and syncs the accepted prefix as incomplete. Oversized
inputs, malformed admitted fragments and write failures publish no recording
completion. Recording refusal leaves ingest and HLS running.

These limits count part lengths conservatively even when bytes share HLS cache
storage. Active rebased copies can add up to the charged original lengths;
Tokio file buffers, task/channel metadata and the HLS cache are additional.
Writer slots remain charged through file finalisation and drop. These are
recording resource bounds, not a cap on total process memory or a throughput
qualification.

- [Goals and milestones](GOALS.md)
- [Benchmarks against mediamtx, libsrt and gosrt](docs/benchmarks.md)
- [M4 performance exercise and library measurement gates](docs/performance-exercise.md)
- [Feature comparison with mediamtx and the libraries we replace](docs/comparison.md)
- [Standalone mobile broadcaster and SDK delivery plan](docs/mobile-streaming-plan.md)
- [Native performance and ForgeSworn library reuse](docs/native-performance-contract.md)
- [Reusable media libraries and OBS integration plan](docs/library-quality-plan.md)
- [Mobile competition, feature priorities and free V4V distribution](docs/mobile-competitive-review.md)
- [What v4v expects from its media server](docs/v4v-contract.md)
- [Scoped native publishing sessions and private control API](docs/scoped-publish-sessions.md)
- [Prepared TV publishing through an onion receiver](docs/private-tv-publishing.md)
- [Receiver TV page and Tor-only local preview](docs/tv-player.md)
- [Encoders tried so far](docs/interop.md)
- [Apple's LL-HLS rules and the mediamtx/gohlslib reference](docs/reference.md)
- [Agent guide](AGENTS.md)

## Acknowledgements

- The Apple LL-HLS rules sapflow follows were learned by reading
  [mediamtx](https://github.com/bluenviron/mediamtx) and
  [gohlslib](https://github.com/bluenviron/gohlslib) (both MIT); see
  [docs/reference.md](docs/reference.md).
- SRT is served by our own receiver, `crates/sapflow-srt` (MIT), written
  from the SRT specification (draft-sharabayko-srt) and observed wire
  behaviour, not from srt-rs or libsrt.
  [srt-rs](https://github.com/russelltg/srt-rs) (Apache-2.0), patched,
  served SRT until 2026-10-21 and was the reference our receiver was
  measured against.
- Every dependency's licence is reproduced in
  [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt), which the container
  image also carries.

## Part of ForgeSworn

[ForgeSworn](https://forgesworn.dev) builds open-source cryptographic identity,
payments, and coordination tools for Nostr.

## Support

If you find sapflow useful, consider supporting development:

- **GitHub Sponsors:** [TheCryptoDonkey](https://github.com/sponsors/TheCryptoDonkey)
- **Lightning:** `profusemeat89@walletofsatoshi.com`
- **Nostr zaps:** `npub1mgvlrnf5hm9yf0n5mf9nqmvarhvxkc6remu5ec3vf8r0txqkuk7su0e7q2`
- **Geyser:** [forgesworn](https://geyser.fund/project/forgesworn)

## Licence

MIT, see [LICENSE](LICENSE).
