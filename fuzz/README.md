# Fuzz targets

The Python TV preview allowlist has a separate finite mutation target:
`python3 -B fuzz/tv_preview_fuzz.py 20000`. It exercises bounded hostile
manifests and resource/query paths without opening sockets or processing media.
`TestOneInput(bytes)` is available for external Python fuzz engines. The CI
smoke job runs the mutation campaign and privacy regressions. This campaign is
not coverage-guided and does not replace the Rust libFuzzer/ASan targets below.

`realtime_queue` exercises the opaque real-time frame scheduler, its scalar
limits, per-stream sequence/dependency recovery, audio priority, deadlines,
backwards clocks and ciphertext ownership release. It interprets at most 256
twenty-byte records from at most 8192 input bytes. The host model checks every
drop/pop/close releases an owned frame exactly once and retains at most 4096
claimed ciphertext bytes, 16 frames and eight stream lifetimes. Its generated
seeds are scheduling metadata, not encoded media or historical crashes. The
target does not decrypt media, parse codec payloads, use a browser or establish
network performance. CI discovers the target automatically. Run the finite
gate with the ordinary shared target and ASan/overflow options, retaining a
separate execution receipt from source preparation.

The native iOS metadata-only QuickTime AAC priming parser has a separate Swift
libFuzzer/AddressSanitizer target, `mobile/ios/Fuzz/QuickTimeAudioPrimingFuzz.swift`.
Run `bash mobile/ios/fuzz-quicktime-priming.sh` from the repository root for a
finite 20-second local run (1–60 seconds via `SAPFLOW_HEADER_FUZZ_SECONDS`). It
also covers `QuickTimeMovieLayout` through contiguous and sparse 64-bit virtual
file inputs. It checks that reads remain exactly eight bytes, total read count
stays bounded, payloads are skipped and the sole movie is final. Focused sparse
layout regressions run with `bash mobile/ios/test-quicktime-layout.sh`. The target
uses synthetic v0/v1/extended-header seeds, optionally accepts payload-free
headers from generated native fixtures, and checks bounded output, exact frame
accounting and refusal of double application. Scratch binaries and mutated
corpora are removed; scalar receipts, logs and any failing input remain under
`target/ios-quicktime-priming-fuzz/`. This launcher is not yet a hosted CI gate
or a production file-install/durability qualification. If Apple Swift rejects
`-sanitize=fuzzer`, the launcher tries Swift edge/counter/PC/compare SanitizerCoverage plus
AddressSanitizer, linked with an installed Clang or already-built worktree
libFuzzer archive (or `SAPFLOW_LIBFUZZER_ARCHIVE`). It verifies actual coverage
counter, PC and compare references and requires actual engine/corpus coverage
statistics. The fallback targets the running macOS major version, records the
runtime hash and retains any archive deployment-version linker warning; it does
not change the app's deployment target. It never builds Rust, downloads
a runtime or silently replaces coverage-guided fuzzing with random mutations;
an unavailable compiler/runtime leaves this gate failing explicitly.

The Android append-context binary parser has a separate JVM target,
`mobile/android/fuzz/RecordingAppendContextFuzz.kt`. Its Jazzer-compatible
`@JvmStatic fuzzerTestOneInput(ByteArray)` compiles in the same Kotlin module as
actual `RecordingAppendContext`, with no Jazzer API/dependency or SDK/FFI import.
It caps input at 265 bytes, exercises original bytes and a separately generated
valid envelope with bounded field mutations and a repaired checksum. Accepted
contexts must canonically re-encode exactly and refuse independently wrong
recording identity, descriptor identities or hardlinks. Only the parser's
expected `IllegalArgumentException` is caught; underflow/assertion/other failures
surface. Original input bytes remain unchanged.

`fuzz/seeds/android_append_context/` contains two synthetic 219-byte metadata
seeds using fixed fake IDs and signed stat extremes, not media or device facts.
The target's standalone `main` verifies and replays exactly those two generated
seed hashes. Run it with `java -cp "$APPEND_CONTEXT_CLASSPATH"
dev.forgesworn.sapflow.broadcaster.RecordingAppendContextFuzz` after compiling the
actual codec/source closure and target. This deterministic seed replay is not
coverage-guided fuzzing.

A separate local Jazzer 0.30.0 run instruments the actual parser and completes
1,779,234 coverage-guided executions in 61 reported seconds, with no findings
(`cov: 199`, `ft: 330`). The official macOS tool archive is digest-verified;
the tested source/JAR/dependencies and 23 protected inputs remain unchanged.
Its owned process exits successfully and closes without cleanup errors. Actual
logs, corpus and independent outcome review are retained privately under
`target/privacy-standby/append-context-jazzer-actual-v1/`.
This is a finite parser campaign. Longer campaigns and hosted CI integration
remain open; it establishes no native Android IO, codec, interop, recovery or
source-EOF qualification. No Rust build is involved.

| Target | What it feeds |
|--------|---------------|
| `ts_ingest` | Arbitrary MPEG-TS chunks, explicit terminal drain, playlist rules, repeated finish and post-EOF refusal (SRT publisher) |
| `flv_ingest` | Arbitrary FLV video/audio tag bodies, with explicit track declarations in prefixed inputs (RTMP publisher) |
| `muxer_timing` | Arbitrary video or declared AAC-only timing into the LL-HLS muxer (steps, stalls, steps back, interruptions and codec epochs), plus opted-in native recording ledger/header/fragment metadata via `NREC`; rendered playlists must pass `playlist::RuleCheck` |
| `small_parsers` | Stream ids, RTMP keys, chunk names, HLS queries, ages, ASC, avcC, ADTS, Annex B, AVCC |
| `own_srt_packet` | Our SRT packet codec (`sapflow_srt::packet`): parse, then encode, parse again and encode again; both must agree |
| `own_srt_listener` | Our SRT listener (`sapflow_srt::Listener`) as the server drives it: datagrams from three peers, time steps, accept or refuse, the application closing connections. Each peer echoes its SYN cookie and addresses the socket id it was given, so data, ACKACKs, drop requests and shutdowns reach open connections. Checks that everything sent parses and that a timer exists exactly while connections do. Seeded with recorded sessions (see below) |
| `rtmp_session` | Our RTMP connection (`sapflow_rtmp`): from C0 on arbitrary bytes, or replaying the recorded ffmpeg session to its publish and continuing with arbitrary chunk bytes so media reaches `FlvIngest`. Seeded from `crates/sapflow-rtmp/tests/fixtures/ffmpeg-rtmp-publish.bin` (see below) |
| `amf0` | AMF0 decoding (every RTMP command and metadata message) and stable re-encoding |
| `rtmp_client` | Own publishing client before or after an original publish exchange, then fragmented hostile receiver bytes; cancellation clears pending output |
| `mobile_media` | SDK H264/AAC configuration and compressed sample admission with bounded hostile framing and timing |
| `own_srt_caller` | Own caller/sender, optional original encrypted negotiation, AES128/256 refresh, hostile data/KM replays and mutations, bounded admission, clocks and cancellation |
| `signet_auth` | Bounded cold-auth credential/URI and fixed kind-21236 contract parsers; no runtime, relay, signer or account |
| `publish_session` | Production scoped-session JSON and checksummed metadata parsers, plus bounded pure authority commands, durable snapshots, connection claims and stale lease/restart/terminal fences; no I/O |
| `http1_request` | HTTP/1.1 request framing for the viewer event loop (`own_http.rs`): arbitrary bytes taken one request at a time; progress, the head limit, refusal of bodies, and that later bytes never change an earlier request |
| `http_lite` | The private control API's request heads and the callback client's response heads and URLs (`http_lite.rs`): progress and the head, target and header limits, CRLF lines throughout, a head that never changes with later bytes, and that every accepted callback URL is sent as exactly one request head the server side reads back as the same `POST` |

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

The `NREC` prefix in `muxer_timing` exercises the production native recording
timing ledger and the recording-only fMP4 header/fragment helpers. At most 64
32-byte records from at most 4096 bytes select valid generation EOF, mutated
generation facts, matching or conflicting final EOF, duplicate first-head,
reset, rational source conversion, hostile presentation metadata or original
fragment payload preservation. Every successful ledger fold checks the coded
extent; a rejected event must leave metadata unchanged. Known valid commands
reach final EOF and fixed initial/final header layouts without requiring random
mutation to guess an entire valid receipt. The three `native-recording-*` seeds
are original scalar controls, not media or historical crash inputs. Historical
non-prefixed and `AAC1` cases keep their prior paths.

The local recording-header gate compiled this target with AddressSanitizer and
overflow checks, replayed all six timing seeds, and completed 9,047 executions
in 61 seconds without a failure. This finite run does not exercise `sapflow-ffi`'s opaque checkpoint owner identity, native output
drain or actual file durability; the fuzz crate has no FFI dependency. Those
boundaries have separate direct recording tests, and native AAC decoder/seek
and B5 recovery qualification remain separate gates. Pure header snapshots here
only exercise the same covered-frame arithmetic, never stand in for actual
sealed SDK checkpoint receipts. No package/dependency change is required.

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

Native recording follow-on SOURCE CANDIDATE: four generated `native-final-duration-*` NREC seeds exercise container final-duration acceptance/refusal and unchanged original entries/payloads. The target also reads completed-generation coverage without mutation. Actual FFI lookbehind ownership/byte budgets are direct unit-test scope, not modelled as fuzzed runtime ownership. No new replay/sanitizer run has been claimed until this source is compiled and executed.


### Portable recording checkpoints

`recording_checkpoint` exercises the shared bounded native-recording snapshot
parser, canonical roundtrip, metadata hash, media scalar semantics and exact
successor rules. It does not parse media atoms, touch files, validate a live FFI
handle, measure decoder output or certify write/fsync/recovery ownership.
Scalar-only seeds have no camera or private media provenance.

The V1 envelope is545..66080bytes with an opaque1..65536byte header. Existing
CI/nightly discover this target automatically. Their65536byte raw mutation
limit is retained: structured arms also build and mutate the full66080byte
valid envelope internally; direct unit controls exercise all size boundaries.
Local finite runs may select `-max_len=66081` to include oversized raw inputs.
Use the worktree target and the normal ASan/overflow flags; never set a fresh
CARGO_TARGET_DIR. Candidate source preparation is not a compiled/replayed or
fuzzed result. Record actual gates before claiming those outcomes.

The same `recording_checkpoint` target also exercises the separately typed
`recording_programme_checkpoint` SFCP0002 decoder and canonical encoder. It keeps
every V1 arm and adds structured V2 unknown-tail prefixes, complete submitted
window geometry, exact successors/terminal rejection and checksum-valid field
mutations. Source/coded/movie origins and queued input components remain scalar
claims, not native or storage proof. Both codecs reject the other version; V1
reserved bytes and wire semantics are unchanged. The V2 maximum remains66,080
bytes and structured arms reach it without changing the existing CI raw-input
cap or adding a target/dependency.

### Generated native recorder segment-file parser (JVM)

The qualification-only Android debug parser has a dependency-free Jazzer entry
`dev.forgesworn.sapflow.broadcaster.NativeAacRecorderFileAdmissionFuzz.fuzzerTestOneInput(byte[])`
in `app/src/testDebug/.../NativeAacRecorderFileAdmissionTest.kt`. Compile it with
the real debug parser and the existing JUnit test classpath; no Jazzer API import
is required. The target bounds input at 65,536 bytes, preserves the supplied
bytes and catches only expected `IllegalArgumentException` refusals. Seed with
the synthetic short/long fragment layout used by its ten JUnit controls; do not
use camera or private source identities. A coverage-guided campaign and replay
are pending. Parser/host checks do not qualify native codecs or presentation.

### TV complete-fragment playlists and channel rendezvous (JavaScript)

`node fuzz/tv_playlist_fuzz.mjs 20000` exercises the bounded Sapflow-only master
and media parsers used by the MIT browser engine and receiver mirror.
`node fuzz/tv_swarm_fuzz.mjs 20000` mutates JSON event/filter inputs to the
single-channel signalling validators after building the pinned sidecar with
`python3 tools/tv-relayswarm/fetch.py && node tools/tv-relayswarm/build.mjs`.
Both are finite deterministic mutation gates, not coverage-guided campaigns.
CI's smoke job runs them alongside the private preview policy mutation gate.
