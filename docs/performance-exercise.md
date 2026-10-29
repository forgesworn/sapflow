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

## Linux and slow viewers

Two of the gates above need more than the macOS screening: Linux/epoll
behaviour and slow consumers.  The loader and runner support both.  Linux
results are a separate cohort.  Don't pool them with macOS receipts or quote a
saving from one against the other.

### Slow consumers

`sapflow-bench --slow N --slow-rate BYTES_PER_SEC` makes N of the full-media
viewers slow consumers.  `performance.py` takes the same two options and
`bench.sh` the `SLOW` and `SLOW_RATE` environment variables.  The slow viewers
are counted in `--viewers` and behave like any other full viewer, except that
each reads a response body frame by frame and, after every frame, waits until
`bytes / rate` has elapsed since the body began.  The unread rest stays in the
connection, so TCP backpressure can reach the server.  The 15 second body
timeout still applies, waits included.  `--slow` can't exceed the full-media
viewer count, `--slow-rate` is required with it, and the runner needs at
least one ordinary viewer left in every viewer count.

The ordinary figures keep their format and are computed over the other
viewers only (part reach is out of the viewers that are not slow), so older
receipts stay comparable.  A section of lines starting `slow ` follows them:
requests, errors by kind, playlist and segment p50 and p99, and parts fetched.
Falling behind live should appear as 404 or 410 responses or timeouts.  The
runner records these under `slow` in each measurement and doesn't fail a trial
on them; any error from an ordinary viewer still fails it.

Pacing stops the reader, not the kernel.  Socket buffers, the HTTP client's
read buffer and, over HTTP/2, the 1 MiB stream window absorb some bytes before
the server's write blocks, so a 200 ms part can fit in them entirely and a
whole segment is likelier to reach the limit.  Pick rates that bite: the
generated media is about 312 kB/s, so 50000 B/s falls behind live about sixfold.
Say which rates were tried; one rate doesn't qualify slow clients in general.

### Linux

Run the server, publisher and loader together inside one native linux/arm64
container on the M4 Max.  Don't use x86_64 under emulation: the runner refuses
it.  With `--platform linux` (the default is `m4`, unchanged) the runner
requires aarch64 Linux and records, in place of the macOS fields: the
`/etc/os-release` PRETTY_NAME, the kernel release, `os.cpu_count()`, the size
of the affinity mask, and the cgroup v2 `cpu.max` and `memory.max` when
present.  It also records the cores the process may use: the affinity mask
lowered to the CPU quota, rounded up, which is what the server's default
worker count sees.  The `own` treatment uses that count, not the host's.
The process sampler reads CPU from `/proc/<pid>/stat` (user plus system time at
clock-tick resolution, normally 10 ms) and RSS from `VmRSS`.  The macOS path
is unchanged.

The image needs the Rust toolchain named in `rust-toolchain.toml` (an image
based on the matching `rust` tag has it), a C compiler and linker, `ffmpeg`
with libx264, `lsof`, `file`, `curl`, `git`, `python3` and `bash`.  Nothing
needs `ps`.

```sh
# From a plain clone, not a linked worktree: a worktree's .git file points
# outside the mount.  The volume keeps Linux build output out of the macOS
# target/.  Leave CARGO_BUILD_TARGET unset.  --init reaps what the runner stops.
docker run --rm --init --cpus 8 --memory 8g \
  -v "$PWD":/src -v sapflow-linux-target:/src/target -w /src \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/src \
  <image> python3 scripts/performance.py --platform linux \
    --output scratch/performance-linux-<run-id> \
    --viewers 250 750 --seconds 60 --rounds 3 \
    --variants own own-1 own-4 \
    --slow 5 --slow-rate 50000
```

`--cpus` is a CPU quota, not pinning (`--cpuset-cpus` pins).  Both limits appear
in the receipt, so give every run you compare the same values.  Docker on a Mac
runs Linux in a virtual machine, which adds its own scheduling and memory
overhead to loopback and to the server; note the Docker version and the VM's
allocation beside the receipt.  `--mediamtx` takes a linux-arm64 binary on this
platform (`file -b` must report ELF and aarch64); a Mach-O binary is refused
here, as an ELF one is on `m4`.

A Linux receipt speaks to Linux/epoll for the treatments and container
settings measured, and a slow-viewer receipt to the rates tried.  Neither
closes the production-shaped proxy, sustained-run or physical-phone gates.

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
