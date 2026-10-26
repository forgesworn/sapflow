# Media performance qualification, 24 October 2026

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

The target is lower CPU, retained memory and allocation cost at the same
delivered media quality, including impaired networks, slow consumers and
overload recovery. A favourable loopback throughput cell does not qualify
every transport, operating system or embedding application. Keep each result
attached to its source, binary, workload and resource-accounting scope.

Our existing implementations take priority. Measurements guide improvements
and expose limits; external implementations remain comparison references or
fill capabilities we have not implemented. Sans-IO protocol libraries and the
Mio viewer backend are distinct from the server's Tokio adapters and recording.

## Current evidence

| Component and workload | Evidence | Next acceptance gate |
| --- | --- | --- |
| Sapflow viewer HTTP, RTMP ingest, generated 720p30 H.264/AAC | [Balanced M4 collection](performance-results-2026-10-24.md), all 25 trials / 50 intervals at public `b2668893d48e1e21e9c52c64f951f896d3856ef8`; 20 Sapflow trials had zero errors, two MediaMTX trials had three errors | Overall clean-request gate failed; diagnose the connection-close interaction and qualify current-source candidates separately |
| Sapflow native RTMP ingest/cache, 1 and 50 publishers | [Matched M4 A/B](native-server-performance-results-2026-10-24.md): all 16 trials passed; at 50 publishers, median CPU was 20.73% lower and sampled peak RSS 41.78% lower | No timed viewers, recording or callbacks; native HTTP and nominal SRT now have separate linked cohorts, with recording, slow-consumer, Linux and physical-device gates still open |
| Sapflow native HTTP fan-out, 750 full-media simulated viewers | [Matched M4 A/B](native-http-performance-results-2026-10-24.md): all 16 cases qualified; own median RSS fell 36.24% with CPU unchanged; Hyper CPU rose 3.06%; playlist/segment p99 medians rose on both | Assess the measured memory/CPU/tail trade-off; other viewer counts, recording, proxy/HTTP2, slow consumers, Linux and physical-device gates remain |
| Hyper single-frame move, combined C1 versus C2 at 750 simulated viewers | [Separate eight-case M4 comparison](http-single-move-results-2026-10-24.md): all cases qualified; median RSS unchanged at 170.75 MiB, CPU 64.25% to 64.05%, with slightly higher p99 medians | No demonstrated whole-server benefit from the small move change; keep this fixture separate from the original 16 cases and other backend/transport gates |
| SRT sender and receiver over UDP | [Nominal M4 A/B](native-srt-performance-results-2026-10-24.md): all 24 timed cases and four probes qualified at 50 publishers; median server CPU fell 4.11% to 10.28% and sampled RSS 42.69% to 43.00%. [C3 rapid-refresh controls](srt-preflight-results-2026-10-24.md) remain separate | Requested URI KM 4096/1024; CLI reset notification 2047, final effective preannounce unverified. Allocation attribution, random/burst loss, RTT/jitter/reordering, collapse, slow consumers, Linux and physical-device gates remain; earlier failures retained |
| Cached SRT AES schedules, native M4 library model | [Five-process C4 model](srt-crypto-model-results-2026-10-24.md): all 160 timing rows retained; 1,316-byte packet batch averages fell 71.43% for AES-128 and 75.16% for AES-256; setup medians rose slightly and inline Material grew from 96 to 2,064 bytes | Boxed layout/lifecycle costs have a separate linked model below; whole-server CPU/RSS, network quality, phone and energy gates remain open |
| Inline versus boxed cached AES keys, separate native M4 full-source model | [Five matched pairs](srt-aes-layout-model-results-2026-10-24.md): all 360 rows retained; empty Caller/Sender layouts shrink 2,000 bytes and Receiver 2,008 bytes; paired 1,316-byte cached timings rise 0.55%/1.54% for AES-128/256 | Tiny-packet costs rise 30.33%/36.52%; both occupied keys use 16 more logical bytes before allocator overhead. No populated-session RSS, server CPU, network or phone result; keep these timings separate from the minimal C4 model |
| C3 plaintext media under random/burst loss, local M1 | [All 84 delivery trials retained](srt-loss-results-2026-10-24.md): 61 passed the frozen decode gate, 23 failed; own C3 passed 42/42, reference 19/42. Clean C3 video omitted 30 prefix and two tail AUs; at 200 ms/20% loss it retained 189 to 721 video packets | Overall comparison failed and source-equivalent quality is unproved. Recorded fMP4 and raw TS paths differ; positive packet counts/clean decoding do not prove complete media, delivery deadlines or transport superiority. Separate current-candidate, encrypted and native Linux qualification remains |
| TS startup candidates v3/v4, separate local M1 clean controls | [Two four-case ABBA controls](ts-startup-clean-control-results-2026-10-24.md): all eight frozen gates passed; newer outputs retain source video [0:898) versus old [30:898), and AAC [1:1405) versus old [49:1405), with exact ordered compressed-payload matches | Two video tail AUs, one prefix AAC frame and three tail AAC frames remain absent. No A/V timing, resource or current-C6 readiness claim; delayed-AAC budget, multi-PES ordering and bounded startup recovery require separate acceptance |
| Guarded current TS startup, separate local M1 clean controls | [Two current-only content controls](ts-startup-guarded-clean-results-2026-10-24.md): both retain exact ordered VCL [0:898) and coded AAC [1:1405); 851 workspace tests, 23 startup cases and finite TS/muxer fuzz passed for the bound source | Explicit source omissions remain. No CPU, RSS, energy, A/V synchronisation, complete-source or device claim; normal post-start buffering remains a separate resource gate |
| C8 AAC/video resource admission, separate local M1 clean controls | [Both current-only controls](ts-resource-clean-results-2026-10-24.md) retain exact ordered VCL [0:898) and coded AAC [1:1405); the same implementation passes 868 workspace tests, Clippy, formatting, finite TS/muxer fuzz and both HTTP backend smokes | Explicit source omissions remain. No CPU/RSS or comparative claim; global HLS residency and slow-consumer admission remain open |
| RTMP publisher and acceptor over TCP | Existing publish interop and origin tests; [C9 publisher admission](rtmp-publication-admission-results-2026-10-24.md) passes 16 new cases within 884 workspace tests, Clippy, formatting and a fresh own-backend end-to-end smoke | Isolate protocol cost from HLS/HTTP at 1/10/50 streams with declared publisher limits, partial writes, acknowledgement-window stalls and RTMPS adapter cost |
| HLS/fMP4 libraries | [Candidate M4 library models](library-performance-results-2026-10-24.md): six fresh cache pairs and four encoding layouts, with generated-byte and ownership tests | Full muxer/server CPU and retained-memory benefit with recording, multiple publishers and slow consumers; complete allocation and device evidence |
| Spry client UDP/TCP/TLS, UDP peer leg | [Matched current M4 collection](spry-standalone-results-2026-10-24.md): 323/324 passed, including all 216 Spry cases; [historical released qualification](https://github.com/forgesworn/spry-turn/blob/367efc77/docs/qualification-0.2.1.md) retained | Full comparison remains ineligible after one coturn TLS length/source rejection; no comparative winner summaries. Separate diagnostic, Linux/fairness and kernel/socket gates remain |
| RelaySwarm forced through Spry | [24-trial paired local M1 composition](spry-composition-results-2026-10-24.md), with the [earlier release-binary receipt](https://github.com/forgesworn/RelaySwarm/blob/34fa0b004c15434408c3ccca40cef339d774dccc/docs/interop/spry-turn-20261024.json) retained | Full matrix failed: UDP 3/4 for both, TCP 2/4 versus 4/4, TLS 1/4 versus 4/4, released then candidate. Trace stage timing and close reasons before server attribution or application performance acceptance |
| Native compressed-media client and Swift/Kotlin FFI | The independent native SDK branch has an admission microbenchmark and correctness tests; these are separate from the public-server sweep | Complete-path copy/allocation attribution and sustained release-build measurements on physical phones |
| Broadcaster versus Larix, Moblin and IRLPro | [Earlier control](iphone-short-check-results-2026-10-24.md) stopped before Start. A [separate short capture](iphone-functional-short-results-2026-10-24.md) now started and produced a decoded 135-second local recording, but the origin received no SRT publication. [Frozen comparison method and proposed budgets](native-performance-contract.md#outperforming-larix-and-other-native-broadcasters) retained | Prove media delivery and retain active transport observations before sampling, then match phone, encoder/quality, origin, screen/radio/thermal state and recording; whole-device energy, memory, delivery and recovery remain unqualified. Larix was not attempted |

The [public HTTP collection](performance-results-2026-10-24.md) measures the
unchanged public server. The [native HTTP A/B](native-http-performance-results-2026-10-24.md)
uses separate source/binary and pre-encoded fixture identities; do not pool the
timings. Separate candidates receive no performance credit from the public
sweep. MediaMTX there is the verified upstream ARM64 `v1.21.2` executable.

## Spry results that change the priorities

The released standalone M4 matrix has 108 trials and 53,075,648 checked
forwarded packets. At 128 allocations, 1,200-byte payloads and an eight-packet
window per worker, the three-run medians are:

| Client transport | Spry packets/s | coturn packets/s | Difference |
| --- | ---: | ---: | ---: |
| UDP | 271,644 | 223,168 | +21.7% |
| TCP | 302,240 | 305,790 | -1.2% |
| TLS | 264,697 | 196,161 | +34.9% |

These are closed-loop measurements with existing host workloads and one relay
loop. Complete-run CPU includes setup and inspection; it is not forwarding-only
CPU. The peer leg remains UDP for all three rows. This is not RFC 6062 TCP
allocation support.

The constrained ARM64 Linux VM loses five of six UDP cells. At the same
allocation/payload cell, UDP delivers 56,511 versus 66,823 packets/s, with
loaded RTT p99 of 11.073 versus 5.712 ms. Stream output buffering also raises
Spry TCP RSS above coturn in the separate 32-packet-window controlled M4
experiment. Retain those limits alongside the favourable M4 cells. Connected
UDP uses another client-facing socket per eligible allocation, whose kernel
memory is outside process RSS.

## Current implementation work

1. **SRT ACK capacity estimator:** replace the heap-backed 16-gap history and
   per-estimate heap scratch with inline history and stack scratch. Preserve
   upper-median and expiry semantics across sequence wrap, holes, duplicates
   and reordering. Account for the larger empty receiver object as well as the
   eliminated populated-ACK allocation. The combined candidate now has a
   separate nominal SRT measurement; it does not isolate this estimator.
2. **fMP4 assembly:** remove the temporary compressed-payload buffer and copy
   each sample directly into the final `mdat`. Preserve exact box bytes,
   track offsets, sample duration and native recording behaviour. The separate
   shared-chunk candidate also removes joined segment storage while keeping
   original per-part release granularity; it changes the pre-release Rust
   `Segment.data` type, with no FFI ABI change.
3. **Spry TCP/TLS readiness:** avoid removing and reinserting the same
   connection in its hash table merely to read it. Keep partial/coalesced
   framing, backpressure, close/deallocation and malformed-client isolation.
   A separate finite UDP dispatch test now fails the proposed 64-packet turn
   contract: one poll consumed all 192 queued requests. Later healthy-client
   and expiry assertions were not reached in that original red. The corrected
   candidate bounds all four drains and prunes retired IO entries once per
   dispatch/sweep batch. Its final source passes 26 all-feature, 21 portable,
   one no-default and six turn-only tests, plus formatting, Clippy and all 34
   wire interop tests. Actual readiness and full finite reply collection are
   explicit test preconditions; earlier fixture failures remain retained.
   Independent readback found 28 recorded PIDs/nine groups gone and all 57
   fixture ports free. This is finite correctness/fairness evidence, with no
   new CPU, RSS, throughput or sustained-load acceptance.
4. **Receipt qualification:** reject mismatched server/workload/sampler
   declarations and zero-byte results, retain rejected trial metadata and
   preserve server/encoder/viewer diagnostics. The predeclared sampler
   tolerance is the larger of one second or 2% of the planned interval.
5. **Recording backlog:** frozen C3 caps original bytes per recorder. C5 source
   now shares admission across epochs and connections: 512 MiB globally,
   64 MiB and two writers per stream path, plus a global writer count of
   saturating `10 * max_publishers`, clamped to 1 through 1,024. Lazy first-part
   spawn preserves never-started epochs and visible refusal/incomplete outcomes.
   All 29 recorder tests passed within the 851-test guarded workspace gate.
   The writer permit now spans a final flush barrier, including fatal paths,
   so Tokio's copied writes finish before admission credits are released.
   Clippy and formatting passed; sustained recording and runtime gates remain.
   Active rebased copies and Tokio's existing per-file buffer remain extra
   storage. These are logical admission bounds, not a total RSS or speed claim.
6. **Packet encryption follow-up:** frozen C3 constructs a full AES schedule
   per packet. C4 caches the encryption-only schedule beside each existing key
   slot and borrows it into fresh CTR state for each packet IV. The separate
   [native M4 model](srt-crypto-model-results-2026-10-24.md) measures this frozen
   inline implementation, including its storage and setup costs. Current C4b
   moves each occupied packet key and schedule into one Box, avoiding the large
   absent inline slots. All 101 SRT tests passed, with one manual model ignored;
   its combined guarded snapshot passed 851 workspace tests, clippy and formatting.
   The [matched inline/boxed M4 model](srt-aes-layout-model-results-2026-10-24.md)
   is terminal, independently checked and cleaned. It measures the smaller
   empty/plain layouts alongside populated-key and packet-processing costs.
   Its timing must not be pooled with the minimal C4 package.
   The maintained
   [AES encryption-only type](https://docs.rs/aes/0.9.3/aes/struct.Aes128Enc.html)
   and [CTR initialisation API](https://docs.rs/ctr/0.10.1/ctr/struct.CtrCore.html)
   support this investigation; it receives no credit from the frozen C3 results.
7. **TS startup retention:** C3's clean generated recording omitted 30 prefix
   video AUs. Separate combined-candidate controls retain the first IDR and
   subsequent video until AAC configuration, with exact retained VCL equality.
   Two uncertain EOF video AUs remain excluded. Recoverable startup parser loss
   now drops the held GOP and waits for a fresh IDR. The
   [frozen clean controls](ts-startup-clean-control-results-2026-10-24.md) also
   establish retained AAC payload equality within their stated ranges, without
   proving A/V synchronisation. Five additive regressions failed on v4.
   Further executed regressions then exposed split-PES catch-up, continuing
   AAC after one IDR and a backwards successor escaping the startup phase.
   The guarded implementation now passes all 851 workspace tests, including
   23 startup cases, plus clippy and formatting. Byte/count/span limits have
   independent executable cases. Separate 60-second TS-ingest and muxer-timing
   fuzz runs completed without findings. Both own and Hyper smoke commands
   passed; their outer driver retained an immediate TCP-bind cleanup failure.
   A later independent check found all 246 recorded processes/two groups gone
   and all 21 ports free; the earlier kernel state was not captured. Both fresh
   [final-binary controls](ts-startup-guarded-clean-results-2026-10-24.md)
   then passed exact VCL [0:898) and AAC [1:1405)
   comparisons at requested 2000/200 ms settings. Their eight child leaders
   exited zero; 91 recorded PIDs/eight groups were gone, ten ports free and
   execution inputs unchanged. Source omissions remain explicit. Whole-server
   resource, sustained-operation and device gates remain.
8. **Post-start sample retention:** executable reds reproduced AAC admission
   beyond an exhausted segment budget and video metadata growth with empty or
   tiny repeated-timestamp samples. The corrected candidate limits video-led
   AAC payload alongside charged segment video, reserves its pending frame,
   and separately caps live AAC and video entries at 16,384 each. Refusal is
   typed and preserves accepted state; real cuts, interruption and existing
   break recovery release entry credit. The pre-release legacy audio methods
   now return Results. All 868 workspace tests passed with five ignored,
   plus Clippy and formatting. This does not bound encoded HLS windows, pinned
   HTTP bodies or process RSS. The [retained-media contract](retained-media-contract.md)
   records the separate registry, response-owner and RTMP-admission gates;
   its global limits are proposals awaiting implementation and qualification.
9. **RTMP publication admission:** the [C9 correction](rtmp-publication-admission-results-2026-10-24.md)
   enforces the documented `N` current publishers alongside `4N` connections.
   A regression first reproduced two accepted paths at `N = 1`; the same case
   and 15 further quota, ownership, wire and recording tests now pass.
   Same-path takeover transfers credit; retirement releases it during held
   grace; stale owners cannot release a successor's slot. Final checks passed
   884 workspace tests, Clippy, formatting and the fresh own-backend smoke.
   There is no new performance or global HLS residency claim.

The [library model results](library-performance-results-2026-10-24.md) now
measure generated fMP4 encoding and shared-cache storage. Further whole-server workloads and
other protocol candidates still need their own paired measurements after
correctness checks.

## Required adverse-load work

- **Continuous saturation:** verify other clients and expiry timers progress
  while UDP, accepted sockets or authenticated peer traffic remain ready.
  Spry's new per-turn drain limits need a sustained test; a finite flood
  followed by recovery does not prove fairness. The limits must retain
  readiness correctly on both kqueue and epoll.
- **Valid stream bursts:** a local real-socket regression reproduces closure
  with 240,128 queued bytes in four individually valid STUN frames, before
  parsing at the 128 KiB aggregate input cap. The candidate interleaves reads
  and parsing while preserving the frame/input limits. Its scheduling quantum
  must retain queued resumption, service other clients and expiry, and count
  TLS socket/control work as well as plaintext. This remains an unreleased
  candidate pending its final correctness and matched performance checks.
- **Accounting under refusal:** distinguish successful/queued forwarding from
  send failures or output-queue closure. Spry's peer-to-client counters can
  currently increment despite send failure. Generator payload checks are
  independent of those counters.
- **Slow library consumers:** state and test the required drain discipline for
  SRT event/transmit/delivery queues. A bounded network input window does not
  bound output retained by an embedding that keeps feeding without draining.
- **Recording epochs and reconnects:** the current 64 MiB original-byte pool
  in frozen C3 is created per recorder. C5's application/path pools now have
  deterministic overlap, refusal and release tests. Review real I/O ownership,
  cancellation and ordinary rollover behaviour, then qualify sustained
  recording under shared pressure; preserve visible failure/incomplete outcomes.
- **SRT burst gaps:** profile repeated scans for the first present packet
  behind a leading loss gap and the bounded loss-range scans. Preserve TSBPD,
  recovery and too-late-drop semantics while reducing work.

For each supported nominal, impaired, overloaded and recovery profile, record
useful delivered media, CPU, RSS, native/managed allocation rate, queue
bytes/age, descriptors, relevant kernel drops and p99 deadlines. Failed and
non-delivering attempts remain part of the receipt. CPU savings obtained by
dropping useful work or lowering quality fail acceptance.

## Measurement sequence

The reserved M4 cohorts are complete: public HTTP, native RTMP and HTTP,
separate single-frame move, library models, Spry and nominal SRT. Each keeps
its own source, binary, qualification and resource scope; the public HTTP
and full Spry comparisons retain their failed gates.

The separate M1 C3 loss collection is terminal with its failed overall gate
and confirmed source omission retained. It supplies no resource comparison
or evidence for the newer C4/C5 candidates.

Next qualify current-source loss/encryption, recording, slow viewers,
sustained operation, HTTP/2 through the production-shaped proxy and native
Linux. Physical phone comparisons and real-network/NAT acceptance retain
their own gates. The connected iPhone short check stopped at an automation
guard before Start, with no media-qualified resource interval. No Larix
comparison or energy result is established.

Use the existing worktree `target/`, run independent throughput treatments
sequentially, preserve unrelated services, and remove task-owned build and
fixture resources only after their evidence has been retained.
