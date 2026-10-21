# Fuzz targets

| Target | What it feeds |
|--------|---------------|
| `ts_ingest` | Arbitrary MPEG-TS in arbitrary chunks (SRT publisher) |
| `flv_ingest` | Arbitrary FLV video/audio tag bodies (RTMP publisher) |
| `muxer_timing` | Arbitrary frame timing into the LL-HLS muxer (steps, stalls, steps back, keyframes anywhere, audio between); every rendered playlist must pass `playlist::RuleCheck`: fixed `TARGETDURATION` and `PART-TARGET`, `EXTINF` within the target, the part rules, consistent discontinuities |
| `small_parsers` | Stream ids, RTMP keys, chunk names, HLS queries, ages, ASC, avcC, ADTS, Annex B, AVCC |
| `srt_packet` | `srt-protocol` packet parse, then re-encode (every datagram to the SRT port) |
| `srt_listener` | `srt-protocol`'s listener and, once a handshake completes, the connection: datagrams from several peers (each echoing its SYN cookie, as a client does), access-control answers, timers. Seeded with a captured real session (`corpus/srt_listener/real-session`, gitignored; see below) |
| `own_srt_packet` | Our SRT packet codec (`sapflow_srt::packet`): parse, then encode, parse again and encode again; both must agree |
| `own_srt_listener` | Our SRT listener (`sapflow_srt::Listener`) as the server drives it: datagrams from three peers, time steps, accept or refuse, the application closing connections. Each peer echoes its SYN cookie and addresses the socket id it was given, so data, ACKACKs, drop requests and shutdowns reach open connections. Checks that everything sent parses and that a timer exists exactly while connections do. Seeded with recorded sessions (see below) |
| `rtmp_session` | Our RTMP connection (`sapflow_hls::rtmp`): from C0 on arbitrary bytes, or replaying the recorded ffmpeg session to its publish and continuing with arbitrary chunk bytes so media reaches `FlvIngest`. Seeded from `crates/sapflow-hls/tests/fixtures/ffmpeg-rtmp-publish.bin` (see below) |
| `amf0` | AMF0 decoding (every RTMP command and metadata message) and stable re-encoding |
| `rtmp_client` | Own publishing client before or after an original publish exchange, then fragmented hostile receiver bytes; cancellation clears pending output |
| `mobile_media` | SDK H264/AAC configuration and compressed sample admission with bounded hostile framing and timing |
| `own_srt_caller` | Own sender before or after an original caller/listener exchange, hostile receiver traffic, stray peers, bounded payload admission, time and cancellation |

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

Runs of the `own_srt_*` targets use `-a` too (overflow checks on), and the
listener target needs room for whole sessions:

```bash
RUSTC_BOOTSTRAP=1 cargo fuzz run -O -a own_srt_listener corpus/own_srt_listener -- -max_len=65536
```

The caller target uses a mode byte (bit 0 establishes the session), followed
by records `[operation, step_ms, len_hi, len_lo, bytes]`. The low two operation
bits select receiver input, stray-peer input, media admission or a timer.
Keep `-max_len=65536` when exercising these mobile publishing targets. Their
budget assertions cover retained media and immediate output; fuzz runs are
separate from sustained network or physical-device qualification.

`seeds/<target>/` contains deliberately valid native samples and original
publishing exchanges so mutation reaches the open state and sample parsers.
These are seed inputs, not historical crashes. CI and the nightly run copy
them into temporary corpora alongside any recorded regressions and retain
overflow checks. New targets still need actual local/CI execution receipts;
their inclusion in the workflow is not a passing fuzz result.

The own caller target uses a mode byte and records
`[operation, step_ms, len_hi, len_lo, bytes]`. Without the high mode bit, it
preserves the original plaintext handshake/admission format. With that bit,
it can negotiate AES128/256, select wrong/missing secrets, begin near sequence
wrap and send a bounded warmup through both key slots before mutation. Records
can replay or mutate captured data/KM packets, suppress replies, submit
oversized/stray datagrams, move time backwards or stop. Only setup admits a
connection and derives its key; hostile records cannot repeat PBKDF work.
See the target comments for bit assignments and fixed work/memory bounds.

Use `seeds/own_srt_caller/` for the published ACK and encrypted fixtures. These
are seed inputs, not historical core crashes. ASAN with overflow checks passed
all seven seeds and 20,980 executions over 61 seconds locally; this is not the
20-minute M6 gate or physical-network acceptance. To replay the seeds:

```bash
RUSTC_BOOTSTRAP=1 cargo fuzz run -O -a own_srt_caller seeds/own_srt_caller -- -runs=0 -max_len=65536
```

To seed `own_srt_listener`: each record is `[control, step in ms, len_hi,
len_lo, datagram]`, control 0 for the first peer with cookie and socket id
rewriting on. The caller's datagrams of
`crates/sapflow-srt/tests/fixtures/*.trace` (records `[u32 microseconds,
u16 length, datagram]`) convert directly; `own_srt_packet` can start from
the same datagrams one per file.

To recapture the SRT session seed: run sapflow on 127.0.0.1:8890, proxy a
`srt-live-transmit` publish through a UDP relay that writes each client
datagram as `[0, step, len_hi, len_lo, bytes]`, and save the result as
`corpus/srt_listener/real-session`. A copy is kept in
`regressions/srt_listener/real-session` so it survives a clean checkout.
