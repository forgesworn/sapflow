# sapflow

[![CI](https://github.com/forgesworn/sapflow/actions/workflows/ci.yml/badge.svg)](https://github.com/forgesworn/sapflow/actions/workflows/ci.yml)

A small live media server in Rust: SRT in, Low-Latency HLS out, recording and
a stream-health callback. Built to do only what a live stage needs, with a
core that iOS and Android apps can share.

Status: experiment (decision at M4: keep experimenting). SRT and RTMP in,
LL-HLS out, the callback and recording pass v4v's ingest gate locally and
play in Safari on iPhone; not yet tried in production. See
[GOALS.md](GOALS.md).

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
cargo run --release -p sapflow-server
```

Publish to `srt://<host>:8890?streamid=publish:live/<eventId>:<publisher>:<password>`
or `rtmp://<host>:1935/live` with key `<eventId>?user=<publisher>&pass=<password>`,
and play `http://<host>:8888/live/<eventId>/index.m3u8`. `sapflow --help`
lists every setting.

- [Goals and milestones](GOALS.md)
- [What v4v expects from its media server](docs/v4v-contract.md)
- [Encoders tried so far](docs/interop.md)
- [Apple's LL-HLS rules and the mediamtx/gohlslib reference](docs/reference.md)
- [Agent guide](AGENTS.md)

## Acknowledgements

- The Apple LL-HLS rules sapflow follows were learned by reading
  [mediamtx](https://github.com/bluenviron/mediamtx) and
  [gohlslib](https://github.com/bluenviron/gohlslib) (both MIT); see
  [docs/reference.md](docs/reference.md).
- SRT comes from [srt-rs](https://github.com/russelltg/srt-rs) (Apache-2.0)
  by default; `vendor/srt-protocol` is a patched copy, with every change
  listed in its `PATCHES.md` and marked in the source. Our own SRT receiver,
  `crates/sapflow-srt` (MIT, `SAPFLOW_SRT_IMPL=own`), was written from the
  SRT specification (draft-sharabayko-srt) and observed wire behaviour, not
  from srt-rs or libsrt.
- Every dependency's licence is reproduced in
  [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt), which the container
  image also carries.

## Part of ForgeSworn

[ForgeSworn](https://forgesworn.dev) builds open-source cryptographic identity,
payments, and coordination tools for Nostr.

## Licence

MIT, see [LICENSE](LICENSE).
