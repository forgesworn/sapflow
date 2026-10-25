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

For declared A/V TS startup, video can wait for AAC configuration and catch up
in timestamp order. The retained video prefix has limits of 64 access units,
1 MiB of summed video/parameter capacities and one second of DTS span. During
catch-up, retained AAC has separate limits of 64 KiB of logical payload and
64 frames; its retained effective timestamp span is at most one second after
each synchronous admission/replay step. Byte/count checks precede copying;
one real AAC successor can advance replay before the span check. Input/PES
buffers, metadata and the HLS cache are additional storage, so these figures
are not a total RSS limit.

Exhausting an optional startup budget reports media loss, releases the held
prefix and waits for a fresh IDR. The transport remains open. Transport/PES
input-limit failures still refuse further input. Startup preparation assigns
no guessed video duration: a real successor supplies it. Ordinary TS EOF
conservatively excludes uncertain final PES/sample data; a decodable recording
alone does not prove that every source sample was retained.

Video-led AAC retention has two explicit admission limits: 64 MiB for charged
open-segment video plus retained AAC (including its pending frame), and 16,384
live AAC samples across the pending frame and current unsealed part. The latter
bounds entries even for one-byte payloads and repeated timestamps. Pending video,
Bytes backing allocations, encoded HLS windows, TS/PES buffers and allocator
overhead are separate; these are not whole-process RSS limits. No new wall-clock,
media-time or EOF duration policy is introduced.

Video has a separate 16,384 live-entry limit across its pending frame,
duration-known learning queue and current open part. Empty samples and
repeated DTS still consume entries; their existing below-limit behaviour
is unchanged. Admission returns `MuxError::SegmentTooLarge` before mutation
when another retained entry would exceed the limit. Existing ignored
input and real timestamp/target-break recovery remain available. Real
part rotation, interruption and reconfiguration release entry credit
through their existing owners; there is no new clock, audio-timed cut or
terminal duration. Encoded windows, backing capacity, allocation transients
and whole-process memory remain outside this entry-count claim.

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

Completed segments share their existing part buffers. `Segment.data` is now
`muxer::SegmentData`, rather than a joined `Bytes`: use `len()` for the full
Content-Length and stream `chunks()` in order. Cloning shares immutable chunk
metadata and copies no media. Empty chunks are permitted. `Part.data` remains
an independent `Bytes` buffer, so holding one part does not pin the other
parts of its segment. This is an explicit change to the pre-release Rust API;
the segment's wire bytes and chunk order remain the same.

The pre-release `Muxer::push_audio`, `EsIngest::audio` and `FlvIngest::audio`
entry points now return `Result<(), _>` instead of `()`. Handle and propagate
refusal; do not mark a recording complete after a refused frame. The existing
`push_audio_at`/`audio_at` APIs retain `Result<Update, _>` and propagate the same
video-led admission errors. Production ingest already uses these checked
updating APIs. A caller can interrupt the accepted prefix and resume at a fresh
IDR using the existing reconnect policy; a refusal does not invent a held video
duration or silently cut on audio timestamps.

The generated cache-storage model compares shared fragments with the former
joined copy while retaining the same original part indexes. Build once, then
measure each variant in a fresh process to avoid allocator-cache carryover:

```sh
cargo build --release -p sapflow-hls --example hls-cache-memory
/usr/bin/time -l target/release/examples/hls-cache-memory --variant shared --streams 50 --segments 24 --parts 5 --part-bytes 65536
/usr/bin/time -l target/release/examples/hls-cache-memory --variant joined --streams 50 --segments 24 --parts 5 --part-bytes 65536
```

These explicit dimensions retain about 375 MiB of generated original bytes,
plus about 375 MiB for the joined variant, before metadata. Defaults are tiny.
Both variants check every body byte/order against deterministic pseudo-random
word streams and emit matching content checksums. Each part has a distinct
seed; periodic byte patterns could otherwise favour memory compression on
macOS. These are synthetic storage bytes, not real compressed media or
cryptographic randomness. The model caps joined logical payload at
2 GiB and bounds part metadata counts. It does not run a muxer, server, sockets,
recording or codecs; process RSS describes this cache model only.

## Status

Version 0.1: the API may change. It has run against OBS, Larix, ffmpeg and
Safari on iPhone, and its parsers are fuzzed; see the repository's
`docs/interop.md` and `fuzz/`. H.264 and AAC only.
