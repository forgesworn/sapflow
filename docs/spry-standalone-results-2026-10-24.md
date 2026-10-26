# Matched M4 Spry comparison, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

All **324 planned cases were observed: 323 passed and one failed**. The full
comparison remains **ineligible** under its declared gate. No comparative
throughput, CPU or RSS winner summaries are published.

| Treatment | Passed | Failed |
| --- | ---: | ---: |
| Spry, rebuilt exact v0.2.1 source | 108 | 0 |
| Spry, current candidate | 108 | 0 |
| coturn | 107 | 1 |

The sanitised scalar export
retains all 324 rows, including the failed case with null metrics. Its
closed schema
and SHA-256 manifest
identify the export. Comparative cell summaries remain empty. Raw logs,
source bodies and private harness/validator helpers remain outside the export.

## Frozen workload and identities

The native Apple M4 Max comparison covers UDP/TCP/TLS client connections,
1/16/128 allocations and 160/1200-byte payloads. Each cell uses all six
treatment orders, with fresh processes, window 8, at least 200,000 checked
round trips and two seconds of throughput. Up to four load workers cycle
through allocations. Coturn relay-threads is zero. This is closed-loop
loopback delivery, not maximum capacity; the peer leg remains UDP, including
TCP/TLS rows. RFC 6062 TCP relaying is not covered.

Both Spry servers were built with matching Rust/Cargo 1.96.0 settings for
native ARM64. Baseline source is `367efc77c84db7a20eddf70ab243dcd13d9065ef`;
the candidate patch is
`d87fdee03d6a2b1acc52685e37766101cd546f1c48ec8f74375668c22822417c`.
Both passed 32 common wire tests before measurement; the candidate also passed
17 native unit tests. One frozen loader serves every treatment.

```text
rebuilt baseline  b8ded07c35259b0eef940f5d9a61b3ad186da721286a983f64b7dabd63ac3b89
candidate         267719587a44a251f09997bce672c4764dee87da942abb100af9ec61bbed2deb
coturn            5cd3ecf36b5101d93f4b925261dbb6ebc24424a65aad462bea82828bc62f0ac7
common loader     4021edd12afb4429d9f7c45d0d36759bdcd189375443b81532b8f9edce0c9476
```

The rebuilt baseline is not the published `0bbcfa0c` executable. The candidate
is not the earlier M1 browser candidate `7a70727f`. Loader and coturn binaries
also differ from their historical measurement artefacts. Keep those identities
and the separate [browser matrix](spry-composition-results-2026-10-24.md) distinct.

## Preserved failure and continuation

Original case **275**, round six, position two, failed on coturn TLS with
16 allocations and 1200-byte payloads. The recorded loader predicate rejects
a UDP peer datagram when its length or source differs from expectation. It
does not establish byte corruption. The failing phase, received length and
source address were not recorded, so the predicate cannot be narrowed further.

The original failed receipt, SHA `c50c059f64bbfa5f2fb3cd00bd40704e3d23ee6d9fb40339ff50dd82b17f9644`,
remains unchanged. A separate window collected only cases 276-324, with source,
plan, helper and binary identities checked before and after every case. All
49 passed; case 275 was never rerun or replaced. The continuation's supervised
zero exit applies only to that remaining work. The original collector's exit
was not directly observed. These windows do not constitute one uninterrupted
collection or restore comparison eligibility.

All retained logs are complete: zero truncation, omissions or retention errors
across the 324 cases. The failed case retains two server logs and its complete
874-byte exception trace. Capture ran after server stop/log close, outside the
measured phase, under fixed per-trial limits. End-of-window readbacks verified
artifacts/source, no live owned processes and an idle declared relay range;
removing the owned source/target remains a separate cleanup gate.

A post-failure netsimd socket-ownership observation and a later idle-range
readback do not establish overlap at the failed receive. No cause is assigned.
A separately identified 24-probe diagnostic later completed 24/24 without
reproducing the rejection, with supervised exit 0. Its readback verified zero
owned diagnostic processes, unchanged original binaries and `3004c31e` loader
source, and restoration of the original source manifest after removing only
the added example by hash. It does not clear case 275, restore comparison
eligibility or support performance rankings. Diagnostic SHA-256 identities:

```text
loader    4d5db19e18e4a9abaaa436e4f7125749b767ee073ff410f1aaf8d2d1e5908469
source    d369ed671a9b13fb94f1e3193c6a27b77e84aaea43df22a55f1eb2fffef8e52f
result    d242c5a9f036efc9ab2bf528a74669d1b3d3caa4ce24b9a0ff341595fff3e4d5
readback  0ba77c7c82bfb15a59b2968f850f7c0f8f43784f466e486119f65699117b4e8d
```

The Sapflow HTTP A/B is another separate workload.

## Resource interpretation

The loader checks framing, sequence and full payloads in both directions;
delivered forwarding counts are twice completed round trips, independent of
server statistics. CPU covers the server's complete lifetime, including
startup, setup, warm-up, sequential RTT and resource inspection, and excludes
the loader. It is not forwarding-only CPU. The recorded loader-to-result wall
ends before resource inspection and has a different boundary.

RSS peaks are sampled every 50 ms and exclude kernel memory. Descriptor/UDP
socket observations are quiescent snapshots while allocations remain live,
not peak socket usage. Process-attributed kernel memory is unavailable on
macOS. Duration floors change completed packet totals, so total lifetime CPU
alone cannot rank forwarding efficiency. Existing host services and the
separate continuation window further limit attribution. No WAN/NAT, physical
phone, browser or energy acceptance follows from this receipt.
