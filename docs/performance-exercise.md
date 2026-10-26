# M4 performance exercise

Use the Apple M4 Max as the repeatable engineering test platform for Sapflow
and the libraries the application actually uses.  Optimise an identified cost,
then compare the same behaviour before and after.  Keep server throughput,
library cost, native binding cost and physical-phone acceptance separate.

The earlier measurements are in [benchmarks.md](benchmarks.md).  The original
[profile](profiling.md) predates our current RTMP and SRT implementations;
its attribution cannot establish where the current version spends CPU.

The [first current-source screening](performance-results-2026-10-23.md) is complete.
It identifies a worker-count CPU saving and a client measurement-window issue;
the wider protocol, binding and field gates below remain separate work.

## First experiment: HTTP workers

The own HTTP path defaults to one worker per logical core.  Test the hypothesis
that waking and scheduling sixteen workers for a single stream costs more than
using fewer workers.  The first comparison covered the then-unchanged Hyper
path (since removed), the own loop at its default, the own loop with one
worker, and MediaMTX 1.21.2.  The runner now offers `own`, `own-1`, `own-4` and
`mediamtx`.  This changes a documented setting, not protocol limits.

The initial screening used 750 full-media viewers, three rotating rounds and
a 60-second full-load target per treatment.  The standard runner also measures idle
RSS and twenty seconds of single-publisher CPU after a 25-second warm-up.
That historical loader included ramp traffic in its counters and percentiles.
The current loader admits requests by start time inside a fixed post-ramp
interval.  It stops admitting at the deadline and retains those requests'
completions during a drain bounded by the existing header/body timeouts.
This keeps slow successful responses and late failures in the measured
distribution, rather than clipping the latency tail at the deadline.  The
receipt records drained request counts and maximum post-window completion
time separately from the server CPU interval.  It retains excluded warm-up request/failure totals;
the runner rejects failures in either collection.  Fan-out uses each response's
actual completion time, rather than the time its concurrent partner finishes.
The process sampler uses a monotonic deadline instead of accumulating polling
overhead, and retains its actual interval in the receipt.  The CPU interval starts
after the shell's ramp sleep; process startup and sampling still introduce a small
offset relative to the loader's own interval.  Keep that limit with the results.
Keep failed trials and their logs.  A zero exit status without viewer output
or with request errors is not a passing measurement.

```sh
# Run on the M4, in the checkout being measured.  Use its existing target/.
# Set this only if the installed compiler's default target is x86_64.
export CARGO_BUILD_TARGET=aarch64-apple-darwin
python3 -B tests/test_performance.py
python3 -B tests/test_measure_process.py
python3 scripts/performance.py \
  --output scratch/performance-<run-id> \
  --viewers 250 750 --seconds 60 --rounds 4 \
  --variants own own-1 own-4 mediamtx \
  --mediamtx <verified-arm64-mediamtx-binary>
```

Four rotating rounds put every treatment in each order position once.  The
runner requires a whole number of cycles.  Don't tune settings between repeat
rounds.  New measurements supersede the historical method only for their own
source/binary receipt; they are not evidence of a server implementation gain.

The runner retains binary hashes and architecture, source revision and tracked
diff hash, runner and benchmark script hashes, lockfile hash, toolchain, macOS,
descriptor limit, host load, per-trial output, errors, medians and ranges.
The server and viewer builds are separate to avoid Cargo feature unification.
It raises its own descriptor limit, refuses occupied benchmark ports, clears
inherited Sapflow settings and owns only the children it starts.  Results
directories must be new so a rerun cannot overwrite a failed receipt.

MediaMTX 1.21.2 emits a separate audio rendition that the existing simulated
viewers do not fetch.  Its measured load therefore understates a full
audio/video player.  The loader fetches both parts and periodic whole segments,
so its received bytes are not unique programme bytes or browser bandwidth.
The shell's one-second polling also adds sampling overhead to its CPU window;
retain this limitation when reading absolute CPU figures.

The feed is FFmpeg's generated test pattern and tone.  No camera footage,
extracted frame or phone recording is an input.  The runner stores scalar
measurements, not captured media.  Its raw logs and JSON start under ignored
`scratch/`; retain sanitised scalar evidence and receipt hashes in the repository,
then remove the task-owned scratch copy.

## Gates before accepting an optimisation

1. Freeze the source, lockfile, native ARM64 binaries, compiler, workload and
   measurement plan.  Record the M4's load and any existing workloads.  Leave
   unrelated services and emulators alone; a busy host reduces what can be
   concluded from a run.
2. Run baseline and candidate in alternating order, with fresh server processes
   and the same generated media.  Use at least three repeats.  Retain ranges,
   failure counts and all trials, including discarded attempts and their reason.
3. Require a saving beyond run-to-run spread.  As an initial engineering gate,
   aim for at least 10% lower process CPU or memory with no material p99 or
   delivery regression.  This is a decision threshold, not a statistical
   confidence claim.  Repeat a close result with more balanced rounds.
4. Require zero failed requests in this clean baseline, populated media samples
   and unchanged bytes/playlist rules.  The existing loader reports fan-out
   among observed receivers, not proof that every viewer decoded every part.
   Do not improve a score by dropping media, reducing viewers, weakening
   authentication or increasing queue bounds.
5. Check the relevant correctness tests and interoperability.  If code changes,
   run workspace fmt, clippy and tests and the applicable smoke/loss/fuzz gates.
   Re-run the affected benchmark after the final code change.
6. Validate the winner at both 250 and 750 viewers, with recording off/on,
   multiple publishers, HTTP/2 through the production-shaped proxy, slow
   clients and a sustained run.  Keep Linux/epoll and physical-phone gates
   explicit.  A macOS/kqueue screening cannot close them.

Don't use a profiling run as the uninstrumented throughput result.  Profile
the current hot path separately with symbols and reconcile sampled CPU with
process CPU time.  The existing profiling profile retains line tables.  A
stack sampler that counts blocked threads cannot establish on-CPU shares.

## Workloads and responsibility

| Layer | Experiment | Measures and acceptance |
| --- | --- | --- |
| Server HTTP | Own loop, 1/4/default workers; 250/750 full viewers | CPU per delivered byte/request, RSS, p50/p95/p99, fan-out reach, errors and descriptors; separate proxy/load-generator cost |
| SRT library | Sender and receiver separately; plain, AES-128/256, key refresh; frozen RTT/loss seeds and 200/2000 ms budgets | Useful bytes, unrecovered/late packets, retransmission overhead, CPU, queues, allocations and deadlines; use libsrt/gosrt executables as references |
| RTMP library | Publisher and acceptor separately; 1/10/50 streams; partial writes and acknowledgement windows | CPU per useful byte/sample, allocations, copies, bounded backlog, sustained delivery; separate RTMPS/TLS adapter cost |
| HLS/MP4 | Generated 720p30 and 1080p30/60 compressed input; audio-only; part rotation and segment assembly | CPU per media second, allocated bytes, payload copies, retained parts/segments, correct timestamps and playlist rules; no browser/network in the pure-library timing |
| Client and FFI | Swift and Kotlin release consumers; validate, enqueue, fan out, mux and drain | Per-call and batch CPU, copies, allocations, queue residence, handle lifetime and cancellation; retain the existing admission/End-race correctness checks |
| RelaySwarm / Kit | Origin-only, direct peer assist and forced TURN with the same media/deadlines | Decoded playback/stalls, origin bytes avoided, total network bytes including signalling/commitments/TURN, CPU/RSS and recovery; native/browser compatibility |
| Optional transfer/storage | Broadcast only versus bounded recording transfer/backup | Foreground latency, disk/network contention, memory ceilings, hashing/encryption cost and useful transfer throughput; only adopt a streaming interface that fits |
| Optional TURN | Spry candidate versus coturn; UDP/TCP/TLS client connections, IPv4/IPv6, allocation counts, bounded overload | Native ARM64 socket throughput, useful bytes, latency/loss, CPU, RSS, descriptor and kernel socket cost, credential/permission expiry; separate browser and physical NAT acceptance |

Run independent measurements sequentially on the M4.  Parallel benchmark
treatments compete for the same CPU, memory and loopback bandwidth.  Reserve
long loss and soak matrices after the shorter screen identifies a candidate.
No new benchmark dependency is required for the first HTTP experiment.

## Library adoption and TURN

The mandatory broadcast path is the compressed-media client, SRT or RTMP,
packaging, origin and HLS player.  Storage, pairing, recording transfer and
peer delivery are optional responsibilities, not reasons to link every
ForgeSworn library into the media process.  The detailed ownership map is in
[the native performance contract](native-performance-contract.md).

Spry belongs in the evaluation as an optional, separately deployed STUN/TURN
provider for WebRTC peers.  RelaySwarm already accepts operator-provided ICE
configuration.  TURN's role is relaying when peers can't connect directly,
as described in [RFC 8656](https://www.rfc-editor.org/rfc/rfc8656.html).
Ordinary SRT/RTMP clients don't speak TURN.  Spry's current source explicitly
does not implement a Sapflow transport adapter or transparent UDP forwarding.
It therefore isn't a required Sapflow dependency or an implemented SRT
reachability solution.  Measure peer delivery both direct and relayed before
deciding whether it saves bandwidth overall.

Keep coturn as the established comparison and a compatible deployment option.
The owner controls relay hosting, reachability and credentials.  A library
doesn't imply a publisher-operated TURN or credential service.

## Public source and marketing

Public repository visibility, a licence notice, package publication and
release qualification are four different checks.  Record first-party
copyright notices exactly and preserve third-party notices.  MIT doesn't
mean that every dependency has the same licence or author.

The ForgeSworn media page should explain each component's job, when it can be
omitted, current consumers, alternatives, licence/source links, and the exact
benchmark limits.  It should link the detailed
[comparison](comparison.md) and [measurements](benchmarks.md).  Private or
unreleased candidates must not be presented as public, qualified components.
The visibility/copyright audit is kept in private local notes until any
publication decisions are made.
