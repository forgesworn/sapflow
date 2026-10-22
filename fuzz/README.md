# Fuzz targets

| Target | What it feeds |
|--------|---------------|
| `ts_ingest` | Arbitrary MPEG-TS chunks, explicit terminal drain, playlist rules, repeated finish and post-EOF refusal (SRT publisher) |
| `flv_ingest` | Arbitrary FLV video/audio tag bodies, with explicit track declarations in prefixed inputs (RTMP publisher) |
| `muxer_timing` | Arbitrary video or declared AAC-only timing into the LL-HLS muxer (steps, stalls, steps back, interruptions and codec epochs); every rendered playlist must pass `playlist::RuleCheck`: fixed `TARGETDURATION` and `PART-TARGET`, `EXTINF` within the target, the part rules, consistent discontinuities |
| `small_parsers` | Stream ids, RTMP keys, chunk names, HLS queries, ages, ASC, avcC, ADTS, Annex B, AVCC |
| `own_srt_packet` | Our SRT packet codec (`sapflow_srt::packet`): parse, then encode, parse again and encode again; both must agree |
| `own_srt_listener` | Our SRT listener (`sapflow_srt::Listener`) as the server drives it: datagrams from three peers, time steps, accept or refuse, the application closing connections. Each peer echoes its SYN cookie and addresses the socket id it was given, so data, ACKACKs, drop requests and shutdowns reach open connections. Checks that everything sent parses and that a timer exists exactly while connections do. Seeded with recorded sessions (see below) |
| `rtmp_session` | Our RTMP connection (`sapflow_hls::rtmp`): from C0 on arbitrary bytes, or replaying the recorded ffmpeg session to its publish and continuing with arbitrary chunk bytes so media reaches `FlvIngest`. Seeded from `crates/sapflow-hls/tests/fixtures/ffmpeg-rtmp-publish.bin` (see below) |
| `amf0` | AMF0 decoding (every RTMP command and metadata message) and stable re-encoding |
| `rtmp_client` | Own publishing client before or after an original publish exchange, then fragmented hostile receiver bytes; cancellation clears pending output |
| `mobile_media` | SDK H264/AAC configuration and compressed sample admission with bounded hostile framing and timing |
| `own_srt_caller` | Own caller/sender, optional original encrypted negotiation, AES128/256 refresh, hostile data/KM replays and mutations, bounded admission, clocks and cancellation |
| `signet_auth` | Bounded cold-auth credential/URI and fixed kind-21236 contract parsers; no runtime, relay, signer or account |
| `publish_session` | Production scoped-session JSON and checksummed metadata parsers, plus bounded pure authority commands, durable snapshots, connection claims and stale lease/restart/terminal fences; no I/O |

The pinned toolchain is newer than the installed nightly, so unlock nightly
features on it for these builds only. Builds share the repository's
`target/`.

```bash
cd fuzz
RUSTC_BOOTSTRAP=1 cargo fuzz run -O own_srt_packet -- -max_total_time=300
```

`regressions/<target>/` holds inputs that crashed before a fix; replay one
with `cargo fuzz run -O <target> regressions/<target>/<file>`. The
`own_srt_packet` inputs are datagrams that crashed srt-protocol (removed
2026-10-21), kept as hostile input for ours. The two `rtmp_session` inputs
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

Keep `-max_len=65536` when exercising the mobile publishing targets.
Their budget assertions cover retained media and immediate output; fuzz
runs are separate from sustained network or physical-device qualification.

`seeds/<target>/` contains deliberately valid native samples and original
publishing exchanges so mutation reaches the open state and sample parsers.
These are seed inputs, not historical crashes. CI and the nightly run copy
them into temporary corpora alongside any recorded regressions and retain
overflow checks. New targets still need actual local/CI execution receipts;
their inclusion in the workflow is not a passing fuzz result.

`seeds/small_parsers/` includes synthetic SRT attempt-correlation attributes:
valid and duplicate IDs, uppercase/malformed IDs, and exact/over-512-byte stream
IDs. The production parser's successful results are checked for bounded canonical
attempt IDs by the existing `small_parsers` target. These are regression seeds,
not historical crashes or a claim of completed fuzz-duration qualification.

`seeds/publish_session/` contains original synthetic PUT/revoke JSON, checksummed
empty/prepared/revoked/expired documents, and stateful command sequences. No
credential in these files grants access to a running service. The target feeds
raw bytes to the production parsers and shapes valid JSON to reach semantic
checks. It also starts four prepared rows and interprets up to 64 sixteen-byte
records as provision, refresh, revoke, reserve/commit/release, clock advance or
restore. Eight retained event rows bound memory; sixteen possible paths exercise
capacity refusal. Successful commands must retain monotonic generations,
revisions, connection epochs and terminal fences; restored admission stays
disabled. Mutated snapshots exercise structural rejection. For a finite run:

```bash
RUSTC_BOOTSTRAP=1 cargo fuzz run -O -a publish_session seeds/publish_session -- -runs=0 -max_len=65536
```

Use a temporary copy of these seeds for mutation runs so generated corpus files
do not become source fixtures. Actual local execution and CI remain separate
evidence from the presence of the target and seeds.

The AAC-only seeds use original scalar framing and synthetic sample placeholders;
they are parser/timing inputs, not independently decodable recordings. The two
`ts_ingest/aac-*` seeds are a framing byte followed by the first 150 packets of
the generated sine-wave fixtures documented in
[`tests/fixtures/README.md`](../crates/sapflow-hls/tests/fixtures/README.md).
They contain no camera or microphone recording. `rtmp_session/declared-aac`
adds explicit boolean metadata and AAC tag records after the existing synthetic
publish prefix; `rtmp_client/declared-audio` enables the actual client's cold
declaration before its existing peer fixture.

The three `ts_ingest/terminal-*` seeds contain a chunk byte, generated fixture
PAT/PMT and one original synthetic AAC start packet. They exercise complete
bounded terminal input, a raw stuffed-header overrun and a truncated declared
length. Their AAC payload placeholders establish parser/packaging behaviour,
not PCM playability. They contain no camera or microphone recording.

After the TS/PES resource-limit repair, all five generated `ts_ingest` seeds
replay successfully. A fresh local AddressSanitizer/overflow run completes
13,298 executions over 62 seconds. The focused wire tests separately exercise
the large payload/reservation thresholds and malformed-header accounting;
the small seeds and this finite run do not establish a process-RSS bound,
sustained performance, CI or native-device acceptance.

Historical FLV and video-timing regression formats remain unchanged. The `TRKS`
prefix selects four FLV record kinds: video, audio, legacy audio hint and a
nonempty explicit track set. The `AAC1` timing prefix selects 48 kHz mono or
44.1 kHz stereo, followed by the existing three-byte step/flag records; flags
also exercise interruption and codec changes. Real generated AAC decode,
server playback and native-device acceptance remain separate gates.

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
`regressions/own_srt_listener/real-session` (recorded for the srt-protocol
listener target, removed 2026-10-21; the format is the same).

`seeds/signet_auth/` contains generated-only fixed login fields, a synthetic
client credential and escaped duplicate keys. The target never creates a native
worker or connects to a relay. Use `-max_len=16384` for this cold-auth parser.
