# Encoded publishing admission benchmark

The iOS adapter now requires an actual bounded SDK queue receipt before it
confirms publishing admission. This benchmark measures the added synchronous
cost of that receipt on a host. It is one part of the
[native performance contract](native-performance-contract.md), not a phone or
whole-stream performance result.

Run `bash mobile/ios/check-encoded-publishing-benchmark.sh` on an otherwise
quiet host with the existing Apple native package and FFmpeg installed. The
script generates its own nonperiodic audio, builds a Release consumer with
warnings as errors, retains scalar measurements and removes generated media,
children and compiler scratch. It accepts no recording or device input.

## Frozen first measurement

The first measured run used Apple M1, Swift 6.0.3 and the installed Apple package.
Its source, package, toolchain and plan hashes are retained under
`target/encoded-publishing-benchmark/20261022T213340Z-ff631daf/`.
The source packet is retained under
`target/privacy-standby/publishing-benchmark-v1/`.

Each scope uses 32 warm-up pairs followed by 400 measured pairs, alternating
the two path orders. Each timed batch processes the same 128 AAC-LC 48 kHz mono
units. Native sample preparation and queue construction are outside timing;
the new path includes two monotonic clock reads, SDK push/pop, its additional
post-pop admission check and popped handle destruction. The complete scope
calls the actual production validator once per unit on each path.

| Scope | Old path mean CPU per unit | Actual queue path mean CPU per unit | Paired median added CPU per unit |
| --- | --- | --- | --- |
| Already validated publishing | 0.032 microseconds | 2.889 microseconds | 2.812 microseconds |
| Single validation and publishing | 0.432 microseconds | 3.489 microseconds | 2.882 microseconds |

The old path ends at a void, network-free sink and supplies the cost baseline;
it cannot establish actual queue admission. Both paths use that same sink.
The measured path adds the real SDK receipt used by the production adapter.

These are distributions of 128-unit batch averages, not individual-unit
latency percentiles. The timed run lasted about 0.79 seconds. Thread CPU and
monotonic wall time are recorded separately. Both scopes passed the stability
criterion written before measurement: the spread of four consecutive
100-pair CPU-delta means must be at most 500 ns per unit or 25% of the absolute
paired median, whichever is larger. There was no rerun to obtain a passing
measurement.

## Ownership and correctness controls

Outside timing, the test verifies actual one-byte queue refusal, selective
publishing closure with accepted-prefix retention, exactly one queued native
sample before each pop, empty recording admission, returned-handle release and
the common 128-item, 4 MiB and one-second admission bounds. The logical encoded
pool is 33,048 bytes; a Core Media view may retain larger parent storage. The
generated source container is 38,021 bytes.

The queue shares the immutable Rust payload; it does not perform another codec
validation or payload copy. Its generated push/pop boundary adds eight native
calls and small wrapper/metadata-buffer ownership operations per unit. Those
counts come from the pinned source. Allocation rate, retained memory and RSS
were not instrumented by this benchmark.

Recording, capture, video, transport, dispatch scheduling, encoding, battery and
thermal costs are outside this measurement. It does not close the whole-SDK
binding-overhead budget or establish an advantage over Larix, Moblin or IRLPro.
Those comparisons retain their matched physical workload and sustained-run
requirements. Preserve the actual admission and End-race correctness checks
when considering a later combined SDK operation or other boundary optimisation.
