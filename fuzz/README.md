# Fuzz targets

| Target | What it feeds |
|--------|---------------|
| `ts_ingest` | Arbitrary MPEG-TS in arbitrary chunks (SRT publisher) |
| `flv_ingest` | Arbitrary FLV video/audio tag bodies (RTMP publisher) |
| `small_parsers` | Stream ids, RTMP keys, chunk names, HLS queries, ages, ASC, avcC, ADTS, Annex B, AVCC |
| `srt_packet` | `srt-protocol` packet parse, then re-encode (every datagram to the SRT port) |
| `srt_listener` | `srt-protocol`'s listener and, once a handshake completes, the connection: datagrams from several peers (each echoing its SYN cookie, as a client does), access-control answers, timers. Seeded with a captured real session (`corpus/srt_listener/real-session`, gitignored; see below) |
| `rtmp_session` | Our RTMP connection (`sapflow_hls::rtmp`): from C0 on arbitrary bytes, or replaying the recorded ffmpeg session to its publish and continuing with arbitrary chunk bytes so media reaches `FlvIngest`. Seeded from `crates/sapflow-hls/tests/fixtures/ffmpeg-rtmp-publish.bin` (see below) |
| `amf0` | AMF0 decoding (every RTMP command and metadata message) and stable re-encoding |

The pinned toolchain is newer than the installed nightly, so unlock nightly
features on it for these builds only. Builds share the repository's
`target/`.

```bash
cd fuzz
RUSTC_BOOTSTRAP=1 cargo fuzz run -O srt_packet -- -max_total_time=300
```

`regressions/<target>/` holds inputs that crashed before a fix; replay one
with `cargo fuzz run -O <target> regressions/<target>/<file>`. The
`srt_*` fixes live in `vendor/srt-protocol`. The two `rtmp_session` inputs
crashed `rml_rtmp`, which sapflow no longer uses; they stay as seeds.

To seed the RTMP targets: the first byte of an `rtmp_session` input picks
the mode (bit 0) and read size, so `0x40` followed by the fixture replays
the whole session, and `0x41` followed by the fixture from byte 3478 (just
after its publish command) continues after the replayed publish. The
`amf0` corpus can start from the command bodies in the fixture (search for
`02 00 07 connect` and the like).

To recapture the SRT session seed: run sapflow on 127.0.0.1:8890, proxy a
`srt-live-transmit` publish through a UDP relay that writes each client
datagram as `[0, step, len_hi, len_lo, bytes]`, and save the result as
`corpus/srt_listener/real-session`. A copy is kept in
`regressions/srt_listener/real-session` so it survives a clean checkout.
