# M4 cached AES crypto model, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

Caching the AES encryption schedule reduced the measured packet batch averages in all six generated cases. Setup medians were slightly slower for both key sizes. The inline cache also increased `Material` from **96 to 2,064 bytes**. These results describe the frozen C4 crypto model and its memory trade-off; they provide no whole-server CPU, individual latency, energy, phone or protocol-superiority result.

The scalar export, strict schema and SHA-256 manifest retain all five processes and all 160 timing rows. No attempt or row was discarded. Raw logs, source archives and compiled artifacts remain private.

## Method and source

One standalone library executable compared two paths through Sapflow's own crypto code, using the same `aes` 0.9.3 primitive dependency. The `PerPacketMaterial` reference rebuilds the full AES schedule for each packet. C4 `Material` retains an encryption schedule in each occupied key slot and reuses it. The package contains copied crypto/sequence modules, a wrapper, lockfile, manifest and licence. It has seven pinned direct dependency declarations and 23 registry lock entries.

The run used an Apple M4 Max with 16 logical CPUs and 64 GiB RAM. Owner workloads remained active. `memory_pressure` reported 74% system-wide free before and 75% afterwards; load averages were 1.15/2.59/3.98 before and 1.28/2.46/3.88 afterwards. This was a shared host.

Installed Rust/Cargo 1.94.1 had an x86_64 compiler host and explicitly built `aarch64-apple-darwin`. Cargo used release optimisation level 3, thin LTO, one codegen unit and symbol stripping; debug assertions were off. `file` and `otool` verified a thin ARM64 executable, and each test process ran with `arch -arm64`. The build was offline, locked and single-job.

The ordinary library tests passed **15 tests**, with the manual model ignored once. Then **five fresh native processes** ran that exact ignored model. Each process executed four rounds of 4,096 operations per variant and case. Full/cached order alternated by round, giving two rounds in each order per process. AES-128 preceded AES-256; packet lengths retained the fixed order 16, 1,316 and 1,456 bytes. There was no rotation of those case orders.

Generated input and output buffers were preallocated. Packet timing includes the identical input reset, IV construction, output `black_box` and checksum. It excludes KDF, allocation, key refresh and networking. Key slots alternate and each batch crosses the 31-bit sequence wrap. Full output equality is checked outside timing. Checksum accumulation is timed; each completed pair's sums are asserted equal afterwards. All six packet-case checksums were stable across every round/process. Setup timing includes construction and drop of the full AES type or its encryption-only type. No camera or encoded media was used.

## Packet results

Each table value summarises **20 batch-average nanoseconds per operation**: four rounds in each of five fresh processes. Parenthesised values are the minimum and maximum batch averages. They are not medians or p99 values of individual packets. A negative change means the cached median was lower.

| AES | Payload, bytes | Full schedule median (range), ns | Cached median (range), ns | Cached change |
| --- | ---: | --- | --- | ---: |
| 128 | 16 | 212.519 (203.806 to 469.889) | 7.395 (6.632 to 25.828) | -96.52% |
| 128 | 1,316 | 313.217 (303.721 to 786.865) | 89.483 (87.229 to 262.746) | -71.43% |
| 128 | 1,456 | 318.547 (313.497 to 783.630) | 90.581 (89.467 to 223.897) | -71.56% |
| 256 | 16 | 311.249 (308.156 to 420.797) | 7.792 (7.650 to 10.702) | -97.50% |
| 256 | 1,316 | 433.258 (430.115 to 575.562) | 107.600 (106.863 to 140.147) | -75.16% |
| 256 | 1,456 | 442.185 (437.948 to 577.444) | 110.107 (108.724 to 140.035) | -75.10% |

## Setup results

| AES | Full construction/drop median (range), ns | Encryption-only construction/drop median (range), ns | Cached change |
| --- | --- | --- | ---: |
| 128 | 198.792 (192.647 to 483.978) | 204.921 (192.637 to 483.510) | +3.08% |
| 256 | 302.882 (300.415 to 445.709) | 303.212 (300.812 to 423.726) | +0.11% |

The setup medians were **3.08% slower for AES-128** and **0.11% slower for AES-256**. Their observed ranges overlap. No significance test was performed. Caching moves schedule construction outside the packet loop; these setup results do not support a universal speed improvement.

## Inline storage cost

| Measured layout | Bytes |
| --- | ---: |
| Former `PerPacketMaterial` | 96 |
| C4 `Material` | 2,064 |
| Increase | 1,968 |
| `PacketCipher` | 976 |
| `PacketKey` / `Option<PacketKey>` | 1,008 / 1,008 |
| Former raw `Option<Secret>` | 24 |
| C4 `Encryptor` / `Decryptor` | 2,112 / 2,128 |

Both key slots remain inline, including when absent. The material increase is exactly `2 × (1,008 − 24) = 1,968` bytes in this build. In the C4 integration, caller/sender and receiver crypto states also sit in inline `Option` fields. Consequently, plaintext owners still reserve the larger inline storage even with `None`. This is a source-derived layout consequence; the standalone lab did not measure complete plaintext owner structures or their RSS.

With two occupied slots, the reported raw Vec capacities total 48 bytes for AES-128 and 96 bytes for AES-256, including the KEK and two packet secrets. Allocator metadata and the shared passphrase are excluded. No heap-allocation count or RSS measurement was taken. Later boxed-key or other layout variants require their own proof; this result stays bound to the C4 inline source hash.

## Provenance and cleanup

| Identity | SHA-256 |
| --- | --- |
| Frozen C4 crypto source | `c8b2b0a627ef4d2a278f50e1b91f98f3ddc18ac348dd46104530aed0076654e4` |
| Seven-member source archive | `afe969c8c6ae5964ecd2e5fb047c86134cbc63004427517d12ad74dc94b2fc1a` |
| Package manifest | `1b8ca93d8fb1bd0177f03a53da07a41aa4f7ae934701ee6e1c4af7fd9cae7cb3` |
| Measured ARM64 executable, 858,160 bytes | `7b973af8d90bcc94f8dddd25e4b11a5203f9a509a612645d814ce0727994fe93` |
| Original private result receipt | `a18f4ff6a295b74df8a07e1f1749d5ef826bad08ef1bc2727e936a8166662394` |
| Final independent cleanup readback | `b0044ad1f3cdb70667878b2386889685295a0551922146a095c1f035993d5344` |

The export also records every package-file identity, the compiler/Cargo hashes, all ten model stdout/stderr hashes, ordinary-test receipts and transfer/cleanup identities. The unchanged owner checkout remained on clean `main` at `6ec2af8e67a76d01ca981e07a4cdbafac713cb6a`; that checkout identity is separate from the copied C4 model source.

The initially absent original checkout target was created atomically with an owner marker. Cargo metadata proved that exact target location. After collection, all 40 evidence files and 198 target artifacts were transferred and hash/mode-verified locally before cleanup. Foreign-artifact and open-handle guards passed; Cargo clean exited zero against only the owned target. The exact private lab was then removed. A fresh independent readback confirmed both paths absent, zero owned process groups/network descriptors, and all **155 tracked raw Git blobs and modes unchanged**. No owner workload was stopped.

These local/private receipts establish this bounded model run and cleanup. They do not establish hosted CI or a published SDK release. The measurements guide optimisation of Sapflow's own SRT implementation, which remains the implementation direction. Network delivery, source-complete media, whole-server resources, sustained use, physical devices and energy keep their separate gates.
