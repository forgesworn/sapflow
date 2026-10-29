# SRT capability and fixture controls, 24 October 2026

**The original timed SRT matrix did not start.** Its first AES-128 capability
probe failed. Three other capability probes and all 24 timed cases remain
unattempted in that frozen receipt. A separate nominal matrix has since
qualified all 24 timed cases and four untimed probes. Its
[native SRT CPU and memory comparison](native-srt-performance-results-2026-10-24.md)
has separate source identities, receipts and results.

The planned matrix uses the same generated, pre-encoded 720p30 H.264/AAC
programme as the [native RTMP ingest comparison](native-server-performance-results-2026-10-24.md).
It replaces RTMP publishers with libsrt-backed SRT callers, without changing
the server executables. No camera input is involved.

| Separate attempt | Outcome |
| --- | --- |
| Baseline AES-128, forced refresh requested at 32/8, forwarding proxy | HLS readiness timed out. The caller reported a KM response matching no key. No timed case started. |
| Baseline AES-128, default refresh, same proxy | HLS became ready, but the three-second decoded-media check timed out with sparse video. |
| libsrt-to-libsrt reference, AES-128, forced 32/8 | Setup failed because the sample CLI does not accept an arbitrary file output URI. No protocol conclusion follows. |
| Distinct corrected reference, captured console output | Decoder exited successfully: 89 video frames and 3.00017 seconds of audio in the three-second sample, with no decoder errors. Captured TS alignment still failed the later independent check below. |
| Baseline AES-128, default refresh, direct connection | Decoder exited successfully but produced only four video frames in approximately three seconds. The positive-video-count quality gate failed. The proxy is not necessary for this symptom. |

Every attempt has its own retained result and observed controller exit.
Independent readback found no live owned controller, supervisor, child or
process group, all fixture ports unused, and all sixteen candidate source
identities unchanged. Terminal cleanup does not clear a failed media gate.
The CLI also logged a preannouncement adjustment to 15 when the refresh
period was set to 32; requested URL values alone do not prove the effective
preannouncement setting.

## Input boundary investigation

The shared generated TS parses at full video cadence through Sapflow's
`TsIngest` with input chunks of 188, 1,000, 1,316 and 8,192 bytes, without a
dropped-packet report. Its independent PES walk also shows full cadence.

The captured corrected reference instead contains **434 all-zero 188-byte
slots**, exclusively at the tails of **129 blocks of 1,316 bytes**. Valid TS
packets retain continuous counters. Feeding these exact captured bytes to
`TsIngest` reproduces sparse video without any SRT or socket runtime.

This supports padding in the sample-tool path. It does not locate that
padding to the caller's input adapter or the reference listener's output
adapter, and does not establish what Sapflow received in the live control.
FFmpeg's successful recovery alone therefore does not establish a valid TS
fixture. The parser's existing loss and keyframe recovery checks remain in
place.

A separately declared UDP-source control now passes on the baseline server.
The same FFmpeg copy-mode feed emits UDP datagrams to an observer, which
forwards each unchanged datagram to the sample caller's UDP input. Its SRT
connection is direct. The actual 2,715 input datagrams range from 188 to
1,316 bytes, with zero bad lengths, bad sync slots or continuity gaps.
Decoding yields 86 video frames and three seconds of audio in three seconds,
with no decoder errors. Receiver close statistics show 2,405 received
packets, zero retransmissions, zero loss and zero drops. The boundary count
and receiver count cover different stop/drain boundaries; they are not a
packet-delivery ratio.

This establishes a usable clean-input diagnostic without weakening the TS
parser. It does not erase the earlier console-path failures. The result
receipt is
`a2a89448e8834fc2512f98554dc44b8a8e1161587271acdf10aacc8401d48528`.

## Rapid-refresh failure and correction

A second UDP-source baseline control still fails HLS readiness when rapid
32/8 refresh is requested. Its 7,255 actual input datagrams also have zero
bad lengths, sync errors or continuity gaps. A separate SRT metadata observer
records 2,051 KM requests and two responses, including the handshake, with
both data key flags and no observer error. The first standalone response
matches its request body; the next distinct request arrives only 387
microseconds after the previous request. This does not demonstrate a corrupt
response body. Aggregate key counts do not establish decrypted transition
ordering between those requests. Its retained failed receipt is
`89992c136045c15c5e29a1031f2e097acc114037a6ac01da6eca3ced2107c201`.

An independent local test through Sapflow's public `Caller` and `Listener`
APIs reproduces a supported rapid-refresh failure in both AES sizes: distinct
requests at 25 and 57 milliseconds receive only one response, and the caller
closes after 64 exact, ordered packets. The same generated payloads at slower
spacing deliver and acknowledge all 128 packets with four matching responses.
The receiver's fixed 100-millisecond KM gate suppresses the second request.

A separate C3 source candidate grants one early distinct KM attempt after a
successfully decrypted forward key-slot transition. Cached duplicates retain
that allowance, malformed attempts spend it, and reordered old-key data cannot
create another allowance. Its full local workspace checks pass: 814 tests,
four existing ignored tests, clippy and formatting. The C3 delta from C2 is
`e455358fd8c99ddccb274545bbde619b0a4eb3c512e912be2bb53df453c17c48`.

Separate M4 controls now pass against the real libsrt caller in both AES
sizes, requesting the same rapid 32/8 profile. Each retains verified whole-TS
input, both data key slots, standalone KM exchanges, populated full ACKs,
clean decoded video/audio, closed transport statistics and independent
terminal/source readback.

| C3 rapid control | Video frames in 3 s | Audio, s | Received packets | Retransmitted / lost / dropped | Standalone KM requests / responses | Full ACKs |
| --- | ---: | ---: | ---: | --- | --- | ---: |
| AES-128 | 86 | 3.0 | 2,446 | 0 / 0 / 0 | 78 / 74 | 270 |
| AES-256 | 86 | 3.0 | 2,405 | 0 / 0 / 0 | 80 / 73 | 271 |

The observed ARM64 C3 executable is
`24b7fc7be864705696b5881f832a0bd7ed0d33e8343f6a4cde02c5942581ed68`.
Its matching Rust 1.94.1 build receipt is
`0fffc0e7d1417d9fb019b7bf039e4691684c01b4436ef6bd3de37b304a9299f1`.
The AES-128 result/readback are `7fe83c41...d4c842c` /
`70127f09...2c7679`; AES-256 are `c5c16155...35deab` /
`b0ad280c...710ab1`. Full digests and raw metadata remain in private evidence.
Both source archives and the separately restored checkout were rehashed.
These short clean controls do not qualify loss, WAN or fast lost-KM-response
recovery, and provide no comparative CPU or memory result.

## Separate nominal performance collection

The separate 24-case collection requested 4096/1024 refresh settings:
plaintext, AES-128 and AES-256, four alternating before/after pairs per mode,
50 publishers and the fixed 35-second warm-up/60-second server sample.
All four untimed nominal before/after encryption probes passed. They require
at least 30 seconds of sender exposure, both observed key slots, matching
standalone KM exchange, populated full ACKs, valid actual input and decoded
audio/video. All 24 timed cases also passed. The
[native SRT report](native-srt-performance-results-2026-10-24.md) retains the
comparative CPU/RSS results and actual transport counters. Each encrypted
caller emitted a reset notice for preannounce2047; final effective
preannounce was not independently established from the requested value1024.

This is a separate source/executable/input fixture with frozen bundle
`821792658cb38fb5a3ed8e58263585cef615e5a10ae655360e032c7251b28b5b`.
No original failure is replaced or cleared. All four probes and all 24 timed
cases passed their delivery, source, resource and independent completion
gates; those results are not pooled with the failed original fixture.

## Reproduction identities

Both native server executables were built with Rust 1.94.1 for ARM64. The
baseline source is `b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e` and the
candidate patch is
`e30f81f9b5f2b069647a37829b09020a08348c1df91c787fa9b3f335e67e5c44`.
The controls above used the baseline server where stated, not a later source
optimisation. The sample CLI is libsrt 1.5.7 and FFmpeg is 8.1.2.

```text
baseline server  5feff2f114ae557f335a04b67ea87c65127af0e459277a8d45c692f82f4640b0
candidate server af8dd75d0942ca130efaba2358c8c4a6c39622e38d88b605caad8dbc115c24bc
shared TS        e39da6332f74c1f2d9988d4f14ac963075ed7d96abd4e99f7c4fdef7ff298b40
reference TS     85c919c78ce5b0a44abb37c48596a674394e653d95b72b4760ea8048f5ce75ce
SRT CLI          f23423041f219a73a1b4a9168eb2d473b0a78c5407d71c10199b212e433e5cee
frozen runner    c1e725d85eae8ae7fa64cfc15eb325b2bfc1ff856d28873d19b7efbb7f4f92cc
failed receipt   6db670c3e9f3df799673ef22a684f32037e260f732505024a78200e0e3a21f66
terminal readback 11a4aa34dd8f0e7516e0a74411fe8b125f426308be798a8b63e94e898ac07a28
```

Raw media, private logs, machine paths, endpoint identifiers and process
identifiers are retained in private lab storage. Reference implementations
were executed as black boxes; their source was not inspected. These controls
retain their own scope alongside the separate nominal performance result.
Loss recovery, native Linux, WAN and physical phone behaviour remain open
in the [scorecard](performance-scorecard-2026-10-24.md).
