# sapflow-hls

Low-Latency HLS packaging in pure Rust, and the ingest that feeds it: FLV
tags, MPEG-TS and H.264/AAC elementary streams in, fragmented MP4 parts,
segments and playlists out.
It is the packaging core of the [sapflow](https://github.com/forgesworn/sapflow)
live media server. MIT.

## What it does

- **LL-HLS muxer.** Cuts H.264 and AAC into fragmented MP4 parts and
  segments, and writes media and multivariant playlists with `EXT-X-PART`,
  `EXT-X-PRELOAD-HINT`, `EXT-X-SERVER-CONTROL` and blocking playlist reload
  (`_HLS_msn`, `_HLS_part`). It follows Apple's rules, including the ones
  that look arbitrary (a target duration that never changes, parts within
  the part target); each is traced to its source in the repository's
  `docs/reference.md`. Encoder reconnects and codec changes continue the
  same presentation with a discontinuity.
- **Ingest.** `TsIngest` takes MPEG-TS (what SRT carries), `FlvIngest`
  takes FLV tag bodies (what RTMP carries), `EsIngest` takes H.264 access
  units and AAC frames directly.

For RTMP itself, see [`sapflow-rtmp`](https://crates.io/crates/sapflow-rtmp);
its events feed `FlvIngest` directly. For SRT, see
[`sapflow-srt`](https://crates.io/crates/sapflow-srt); it carries MPEG-TS for
`TsIngest`.

Sans-IO throughout: no sockets, no async runtime, no `unsafe`. You own the
connections, the clock and the HTTP server.

## Use

```toml
[dependencies]
sapflow-hls = "0.1"
```

MPEG-TS in, a playlist out:

```rust
use std::time::SystemTime;
use sapflow_hls::{MuxerConfig, TsIngest, playlist};

let mut ingest = TsIngest::new(MuxerConfig::default());
// For each chunk of MPEG-TS from the encoder (any size):
let update = ingest.push(&ts_bytes, SystemTime::now())?;
if update.any() {
    // A part or segment finished: wake any blocked playlist requests.
}
if let Some(muxer) = ingest.muxer() {
    let m3u8 = playlist::media(muxer, "");
    // Serve m3u8, and the init section, parts and segments the muxer holds
    // (muxer.init(), muxer.part(id), muxer.segment(id)).
}
```

The sapflow server (`crates/sapflow-server` in the repository) shows the
whole path from socket to HTTP.

## Status

Version 0.1: the API may change. It has run against OBS, Larix, ffmpeg and
Safari on iPhone, and its parsers are fuzzed; see the repository's
`docs/interop.md` and `fuzz/`. H.264 and AAC only.
