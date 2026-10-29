# M4 library and cache measurements, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

The shared-fragment cache model used **49.78% less peak process RSS** than the
former joined-copy model. The direct `mdat` writer also reduced the measured
batch-average encoding time in all four generated layouts, including a
single small audio unit. These are measurements of an unreleased native SDK
candidate and two finite library models. Whole-server CPU, physical phones,
codec quality and energy retain their own acceptance gates.

The scalar evidence
and SHA-256 manifest
retain all 27 result/timing files. Every manifest entry was independently
checked, and the tables below were recomputed from the individual receipts.

## Source, machine and comparison

The candidate was the native SDK source base
`b2e07a85ce7536b5b51ca6fb699b8658cf2b7b6e`, with candidate patch SHA-256
`e30f81f9b5f2b069647a37829b09020a08348c1df91c787fa9b3f335e67e5c44`.
The result contains the hashes of all 16 measured performance files. Candidate
source bodies and executables are outside this scalar export; this receipt
does not establish a published release or hosted CI result.

Measurements ran sequentially on one Apple M4 Max: 16 logical CPUs, 64 GiB
RAM and macOS 26.6.2. Before collection, `memory_pressure` reported 71%
system-wide memory free. Recorded load was roughly one to two on the
16-CPU host. Rust 1.94.1 produced thin
ARM64 executables. The installed compiler's recorded host is x86_64; the
executables' Mach-O architecture and their runtime `aarch64` declarations
verify the measured target. The fMP4 receipt reports debug assertions off.

| Executable | Bytes | SHA-256 |
| --- | ---: | --- |
| `hls-cache-memory` | 369,696 | `62cae38f91183af44026a872bd260e50cdb7144f341b458219761efb3c85ce4b` |
| `fmp4-encode-bench` | 734,240 | `9d129a3816367ae0737d251c3b37b9ccaa0e6773244f712f2aec097ddb9d762a` |

Key source identities recorded in the same receipt:

```text
fmp4.rs                 c66b7537a75d852521159b5ed3dbda70cc90cdfd475d458e5504641c25a15648
muxer.rs                774d79b68edcf4e64f5390377bd5962344c24df3a1d95b7b044a7d2748822fe2
fmp4-encode-bench.rs    cd74dc5c1fd22a63c2e2d98c9998a4dd3216070c8155e3c70a0b127f9c664869
fmp4_reference.rs       fd7a3b821e8f525d63d6e191a99a58e43b82cef2001d10725c750021c3c9c650
hls-cache-memory.rs     91769968365cbdbea1710d8fadeeab9b98c0234f39ee564ab35fd79b8a59d949
```

The separate [public HTTP screening](performance-results-2026-10-24.md)
measured unchanged public revision `b2668893`, before these implementation
changes. It cannot attribute whole-server gains to this candidate. That
screen's failed clean-request gate remains recorded.

## Retained-cache model

Six paired rounds ran the two variants in twelve fresh processes, reversing
their order each round. `/usr/bin/time -l` measured each process's maximum
resident set size. Fresh processes avoid carrying one variant's allocator
cache into the other variant's result.

Each process retained 50 model streams, 24 segments per stream, five parts
per segment and 65,536 bytes per part. Both variants kept original independent
`Arc<Part>` buffers in segment and part-lookup indexes. The joined reference
additionally retained the legacy `BytesMut` concatenation. The shared variant
used the actual candidate `SegmentData`, whose immutable chunk handles refer
to those same original allocations.

The originals totalled **393,216,000 bytes, exactly 375 MiB**. Joined storage
therefore held 750 MiB of logical payload; shared storage held 375 MiB.
Metadata and allocator overhead are additional. Distinct deterministic
SplitMix64 streams filled and touched each part, avoiding short periodic
patterns that could favour memory compression. These bytes model storage;
they are neither real compressed media nor cryptographic randomness.

| Model | Logical payload, MiB | Median maximum RSS, bytes | Median maximum RSS, MiB | Range of six maximum RSS values, MiB |
| --- | ---: | ---: | ---: | --- |
| Joined reference | 750 | 792,125,440 | 755.4297 | 755.4219 to 755.4844 |
| Shared fragments | 375 | 397,828,096 | 379.3984 | 379.3906 to 379.4063 |

The difference between medians is **394,297,344 bytes, or 376.03125 MiB**.
The 49.7771% reduction uses joined median maximum RSS as its denominator.
Every process verified all 393,216,000 body bytes and their order against
the generated originals. Every body checksum was
`0b1c093049aebf6a` using the receipt's FNV-1a-64 check.

This process contains a cache-ownership model, rather than a running muxer,
server, viewer or recorder. Its RSS excludes kernel socket memory and does
not estimate memory retained by slow responses, recording backlog or codec
pipelines. The measured reduction applies to this model and these dimensions.

## Fragment encoding

One executable compared the frozen former writer with the candidate writer.
It ran ten balanced rounds, each with 10,000 encoding calls per variant and
layout: 80 timed batches in total. Variant order reversed each round. The
reference and candidate therefore occupied each order position five times
for every layout.

Immutable generated inputs and warm-up were prepared outside timing. The
timed loop included encoding, output allocation and ordinary output drops;
complete final-fragment comparisons and checksums happened afterwards.
The inputs were reused between calls. Results below are medians and ranges
of **ten batch-average nanoseconds per fragment**, not individual-call
latency medians or p99 values.

| Generated layout | Fragment bytes | Reference batch average, ns, median (range) | Candidate batch average, ns, median (range) | Reduction between medians |
| --- | ---: | --- | --- | ---: |
| Ten small audio units | 3,456 | 1,806.9000 (1,619.642 to 1,878.425) | 428.9710 (422.812 to 439.367) | 76.26% |
| Mixed A/V payload sizes | 66,116 | 25,112.0605 (24,145.412 to 28,158.908) | 1,461.1000 (1,348.604 to 1,523.475) | 94.18% |
| Large IDR payload sizes | 691,744 | 329,501.3810 (324,969.833 to 331,893.967) | 8,696.1565 (8,608.904 to 8,872.350) | 97.36% |
| One small audio unit | 144 | 336.4230 (325.613 to 343.554) | 314.5730 (309.171 to 319.017) | 6.49% |

The audio-only layout holds ten 320-byte units; the smallest holds one
32-byte unit. Mixed A/V has six video-sized units of 20,000 then five times
8,500 bytes, plus ten 320-byte audio units. The large-IDR layout uses
512 KiB then five times 32 KiB, with the same audio payloads. The workload
names describe payload sizes, not decoded resolution, encoder quality or
valid codec bitstreams.

All reference/candidate batches had matching output byte counts and complete
final-fragment checks. Their final fragment checksums were:

| Receipt workload | Bytes emitted per 10,000-call batch | Final fragment FNV-1a-64 |
| --- | ---: | --- |
| `audio-only-small-payload-sizes` | 34,560,000 | `f0a617ee7d0e1f7c` |
| `mixed-720p-payload-sizes` | 661,160,000 | `fd3ef18ed287188b` |
| `large-idr-payload-sizes` | 6,917,440,000 | `12c174f40bd7dbf3` |
| `single-small-audio-payload-size` | 1,440,000 | `87a24b25015e900e` |

The candidate appends existing sample slices directly into the final `mdat`
with the same box framing. It removes the temporary compressed-payload Vec
and its second payload traversal through the dependency's byte-wise Vec
encoder. Separately, shared segment storage removes the joined segment
allocation and rotation copy. Native recording layouts, final audio duration,
track offsets and oversized-fragment refusal have correctness tests; this
timed experiment covers the four streaming layouts above.

## Correctness gate and API scope

The final local validation receipt records **810 workspace tests passed and
four ignored**, with workspace formatting and warnings-denied clippy passed.
The resolution/codec-epoch decode test was also run explicitly and passed.
Final Hyper and own-backend decoded synthetic-media smokes passed SRT and
RTMP ingest, HLS, recording, reconnect, optional/required encryption and
wrong/missing-secret refusal. All 16 measured performance-file hashes match
that validation receipt and the current candidate files. Its Cargo manifest
and lockfile also match the measured source base.

These are local receipts, with raw logs retained privately. Their recorded
SHA-256 identities are:

```text
validation receipt      795197a2b0de86697028c0a3fc5899d2f85f3ce8e5f98c346cc56130c8184870
workspace test log      8e04a365d46a0d7369b9a8d09308bb38ccbccbafea4249d427e2b2d88160bba3
explicit epoch decode   c3868198c6f174fe32d402a760b7191c8def79b803734f8eedfa72fc8905a924
Hyper smoke             7b3bf2c5a7f0e4ffb1fa66bb06225bc95de9c477fa5a7e6f9ca38f6419990e6b
own-backend smoke       0311e2d2f74d3996ea35f8e4a3b66d2bfcf4bc677c9dba3f17645cc452a553ce
```

`Segment.data` changes from contiguous `Bytes` to shared `SegmentData` in
this pre-release Rust API. Consumers use its total length and stream chunks
in order. `Part.data` remains an independent buffer. Both HTTP adapters have
exact-length and partial-write checks; existing full-200 behaviour for Range
requests is preserved. No FFI ABI change follows from this field type change.

## Remaining resource and qualification work

The SRT capacity-estimator change has a separate trade-off: matching aarch64
field layouts grow from 56 to 168 bytes when empty, while a populated
16-gap history falls from 184 to 168 bytes before allocator metadata. It
removes per-estimate allocation, but has no measured SRT CPU/RSS result here.

Recording now caps retained encoded originals at 64 MiB per recorder across
queued and active writes, alongside its 256-message cap. An active rebased
copy can add up to 64 MiB, plus the current default file buffer of up to
2 MiB, metadata and allocator/kernel storage. This is a retained-reference
byte bound; shared HLS/recorder buffers
are not necessarily distinct allocations. Global budgets across streams,
overlapping codec-epoch writers and slow/external consumers remain open.

Matched whole-server before/after measurements, sustained multiple-publisher
and slow-consumer runs, current SRT impairment/encryption tests, TCP/TLS
adapters, Linux, physical devices, energy and thermal behaviour remain
separate from these results. The [scorecard](performance-scorecard-2026-10-24.md)
and [exercise](performance-exercise.md) retain that wider scope.
