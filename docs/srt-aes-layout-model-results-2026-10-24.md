# Matched M4 inline and boxed AES models, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Boxing cached AES keys reduced the measured static Caller, Sender and Receiver layouts by about 2 KiB. It also made the 16-byte cached packet cases roughly 30–37% slower in this model. With both keys occupied, the boxed material plus key payloads used 16 more logical bytes before allocator overhead. These results expose a layout/processing trade-off within Sapflow's own SRT implementation. They establish no server CPU, RSS, packet-tail, phone or energy result.

This is a new matched full-source cohort. Its timings remain separate from the earlier [minimal C4 crypto/sequence model](srt-crypto-model-results-2026-10-24.md). The scalar export, strict schema and SHA-256 manifest retain all **360 timing rows**: 160 inline and 200 boxed. All ten planned fresh processes passed; no attempt or row was discarded.

## Matched source and method

Each 14-file package contained the complete copied Sapflow SRT sans-IO source, a common test-only owner-layout probe, package manifest, lock and licence. Only `src/crypto.rs` differed. A keeps two `Option<PacketKey>` fields inline; B uses two `Option<Box<PacketKey>>` fields. Both reuse the same AES 0.9.3 encryption schedules. The preserved full-schedule reference rebuilds its schedule per packet.

Rust/Cargo 1.94.1 cross-compiled from an x86_64 compiler host to explicit `aarch64-apple-darwin`, using offline/locked single-job release builds, thin LTO, one codegen unit and symbol stripping. The same absolute lab source path and original checkout target were used for both builds. A's executable was copied out before a package-only clean and verified replacement with B source; B compiled fresh. Both frozen executables were verified thin ARM64 with `file`/`otool` and ran under `arch -arm64`. Every model process reported hardware acceleration enabled and debug assertions off. No compilation occurred during the ten measured processes.

Ordinary crate suites passed **71 tests for A** and **72 for B**, each with one ignored manual model. Separate identical owner-layout probes passed once per executable. B's additional unit test checks pointer-sized empty key slots. These counts describe the copied crate's unit suites.

Five fresh pairs ran in order **AB, BA, AB, BA, AB**: A ran first in three pairs and B in two. Each process used four rounds of 4,096 operations per timed arm; full/cached order reversed each round. AES-128 preceded AES-256, and packet sizes retained the order 16, 1,316 and 1,456 bytes. Each timing row contains two timed arms. A emits eight constructor and 24 packet rows; B emits those 32 plus eight installation rows. The extra installation phase follows B's common phases for each key size.

Packet loops include the same preallocated buffer reset, IV, output `black_box` and checksum accumulation. They allocate no keys and exclude KDF, refresh and networking. Complete output comparison occurs outside timing; completed-pair checksums are then asserted equal. All six packet-case checksums match across variants, rounds and processes. No camera or encoded media was used.

## Actual owner and key layouts

These are actual ARM64 `size_of` values from the complete copied types. They include inline descriptors, discriminants and alignment, and apply when optional crypto is absent too. They do not count buffers referenced by the objects.

| Object | Inline A, bytes | Boxed B, bytes | Smaller by, bytes |
| --- | ---: | ---: | ---: |
| `Material` | 2,064 | 64 | 2,000 |
| `Encryptor` | 2,112 | 112 | 2,000 |
| `Decryptor` | 2,128 | 120 | 2,008 |
| `Caller` | 2,608 | 608 | 2,000 |
| `Sender` | 2,576 | 576 | 2,000 |
| `Receiver` | 2,784 | 776 | 2,008 |

Caller/Sender/Receiver alignment falls from 16 to 8 bytes. Each `PacketKey` remains 1,008 bytes with 16-byte alignment. Inline key options reserve 1,008 bytes each even when empty; boxed options reserve one 8-byte nullable pointer each. Thus the two empty slot fields occupy 2,016 bytes in A and 16 bytes in B, with no key boxes for `None`. Plain owners therefore keep the smaller B type layout without allocating packet-key boxes.

| Storage with two keys occupied | Inline A, bytes | Boxed B, bytes |
| --- | ---: | ---: |
| Material inline object | 2,064 | 64 |
| Extra boxed key payloads | 0 | 2,016 |
| Material plus key payloads | 2,064 | 2,080 |
| Reported KEK/two-SEK raw Vec capacity, AES-128 | 48 | 48 |
| Total including that raw capacity, AES-128 | 2,112 | 2,128 |
| Reported KEK/two-SEK raw Vec capacity, AES-256 | 96 | 96 |
| Total including that raw capacity, AES-256 | 2,160 | 2,176 |

A's key payloads are already inside its Material and are counted once. B moves those payloads into two boxes; their Vec descriptors and ciphers are included in the 2,016 bytes. The raw Vec capacities add only the KEK/SEK byte buffers. Box/vector allocator metadata, allocation rounding and the shared passphrase are excluded. The two occupied-key totals are **16 bytes larger in B**, before the overhead of two extra heap blocks. Allocation calls and RSS were not measured. Smaller empty/plain objects therefore do not establish lower populated-session RSS.

## Cached packet comparison

For each cell, a process median summarises its four 4,096-operation batch averages. The table then shows the median and range of five process medians. The paired change is calculated separately for each matched pair as `100 × (B/A − 1)`, then summarised across five pairs. It is not the ratio of the two displayed aggregate medians. These are batch-average summaries, with no individual-packet percentiles or significance test.

| AES | Bytes | Inline cached ns/op, median (range) | Boxed cached ns/op, median (range) | Paired boxed change %, median (range) | Full-schedule reference medians A / B, ns/op |
| --- | ---: | --- | --- | --- | --- |
| 128 | 16 | 11.368 (11.358 to 11.403) | 14.816 (14.791 to 14.933) | +30.33 (+29.71 to +31.48) | 322.606 / 322.922 |
| 128 | 1,316 | 137.685 (137.293 to 139.755) | 138.433 (137.751 to 141.495) | +0.55 (-1.43 to +2.59) | 501.149 / 496.211 |
| 128 | 1,456 | 141.174 (140.330 to 141.876) | 141.088 (140.727 to 142.217) | +0.16 (-0.79 to +1.32) | 513.906 / 512.080 |
| 256 | 16 | 12.146 (12.059 to 12.492) | 16.561 (16.418 to 16.632) | +36.52 (+32.13 to +37.92) | 491.791 / 494.614 |
| 256 | 1,316 | 169.118 (168.208 to 170.339) | 171.336 (169.602 to 172.750) | +1.54 (+0.29 to +1.85) | 689.311 / 686.020 |
| 256 | 1,456 | 174.845 (173.391 to 175.456) | 175.776 (175.547 to 177.678) | +1.33 (+0.05 to +1.81) | 700.587 / 707.387 |

The 16-byte cases show clear observed model penalties: paired medians of **+30.33% for AES-128** and **+36.52% for AES-256**. The differences between displayed aggregate medians are about 3.45 and 4.41 ns/op. Larger-packet paired medians range from +0.16% to +1.54%; some paired ranges cross zero. The AES-128 1,456-byte aggregate medians and paired change have different signs because the summaries use different operations. Every reference, cached and adverse row remains in the export.

## Constructors and separate boxed installation

Constructor values below are medians and ranges of **20 batch averages per arm**, rather than the five-process-median statistic used above. They include AES construction and drop, without raw-key allocation. Encryption-only construction is slightly slower than full construction for both AES-256 variants in these observed medians.

| Variant | AES | Full constructor/drop ns/op, median (range) | Encryption-only constructor/drop ns/op, median (range) | Change between medians |
| --- | --- | --- | --- | ---: |
| A-inline | 128 | 304.957 (303.518 to 318.105) | 303.965 (302.399 to 314.463) | -0.33% |
| B-boxed | 128 | 304.621 (303.579 to 309.672) | 303.950 (302.389 to 323.018) | -0.22% |
| A-inline | 256 | 476.135 (470.876 to 497.854) | 479.218 (472.371 to 519.165) | +0.65% |
| B-boxed | 256 | 477.661 (469.940 to 490.601) | 478.714 (472.198 to 498.820) | +0.22% |

B alone additionally times a raw secret Vec allocation/zeroising drop against a complete raw Vec plus boxed cached-key construction/zeroising drop and deallocation. It includes schedule construction and boxing together; their difference cannot be assigned solely to the Box allocation. There is **no measured inline key-install counterpart**. KDF, refresh protocol and networking are excluded.

| Boxed-only phase | AES | Raw-secret ns/op, median (range) | Complete boxed cached-key ns/op, median (range) |
| --- | --- | --- | --- |
| Install/drop | 128 | 30.833 (29.673 to 35.583) | 410.146 (405.365 to 430.257) |
| Install/drop | 256 | 40.497 (37.679 to 43.955) | 637.873 (621.724 to 689.972) |

These lifecycle costs are additional to the packet-loop measurements. They guide key-storage decisions; this model supplies no complete refresh-cycle, sustained-session or server resource result.

## Source identity, host limits and cleanup

| Identity | Inline A SHA-256 | Boxed B SHA-256 |
| --- | --- | --- |
| Crypto source | `c8b2b0a627ef4d2a278f50e1b91f98f3ddc18ac348dd46104530aed0076654e4` | `17a01e78198d4dcec45b46d6bcc7448ff1486d4cdb2a1b2acbda473185bba370` |
| Source archive | `1c5bf60686411a0960d020f6bd7db51c1807e26e73bf1e392edc9fd070760d79` | `0b562b1a05520d583c6a83ded15bd6aef0de6c361a74df7d2c2b27443ec188a4` |
| Native executable | `1fe2fb04a2be36836ede289bae3928d5e8c390ac7be66332a9b77cc8c5257357` | `b898f80de6a33d41d5a6a32ab8758aeae754b38fd7b73f9055a04fa148a0cd54` |

The common package manifest is `0cca810929f33f3c7ddd477ad41a2b6a448a03f481f87cd465d01dc4dd32fc57`; the common owner-probe wrapper is `2877f02daa23c4a54488ed62b8b19030af7067ab3dc3504951b889d88973e7be`. Executable sizes are 1,074,704 bytes for A and 1,074,720 for B. The export records all 14 member identities per variant, compiler/Cargo hashes, all 20 model stdout/stderr identities, ordinary-suite/probe identities and cleanup receipts. These hashes bind this cohort; later source/layout variants require separate proof.

The shared Apple M4 Max had 16 logical CPUs and 64 GiB RAM. Owner workloads stayed active. Memory-pressure free percentage was 72% before and after. Load averages were 4.85/5.09/4.68 before builds and 6.40/5.59/4.91 after the model; all ten short processes observed the same available load sample, approximately 6.402/5.591/4.911. No host-isolation or broad significance claim follows.

Before cleanup, local verification checked 77 retained files and all 191 target artefacts, both source archives/all members and both executable hashes. Foreign-artifact, owner-marker/inode and open-handle guards passed. Cargo clean exited zero against the task-owned original target, and the exact private lab was removed. Fresh independent readback confirmed both paths absent, zero live owned groups/network descriptors, and all **155 original tracked raw Git blobs and modes unchanged** on clean main. No owner workload was stopped.

Raw logs, full source archives, native executables and build artefacts remain private. No media was involved or exported. This report adds no server CPU/RSS, packet-tail, physical-device, energy or protocol-superiority claim, and does not establish a hosted CI result or published release. The measurements guide optimisation of Sapflow's own SRT implementation. The earlier minimal model remains a separate result.
