# Spry browser composition, 24 October 2026

Its machine-readable exports, schemas and SHA-256 manifests are kept in
private evidence rather than in the repository, following the retention
rule in `AGENTS.md`. This record carries the published figures.

The corrected Spry candidate passed 11 of 12 browser trials; the released
binary passed six of 12. **The complete 24-trial matrix still fails its full
gate.** These are local Apple M1 loopback functional observations using
RelaySwarm shadow mode. They establish neither M4 TURN throughput nor server
CPU or memory gains, causal server attribution, physical-device acceptance or
public-network/NAT interoperability.

The scalar results
retain all 24 trials, their original failure arrays, delivery/player counters
and exact identities. The SHA-256 manifest
covers that export. Raw browser/server receipts, network identifiers and
executables remain outside it. No failed trial was replaced or threshold tuned.

| Client transport | Released full-gate passes | Candidate full-gate passes |
| --- | ---: | ---: |
| UDP | 3/4 | 3/4 |
| TCP | 2/4 | 4/4 |
| TLS | 1/4 | 4/4 |

## Frozen comparison

Four paired rounds balance both order positions for each transport. Both
variants use RelaySwarm `34fa0b004c15434408c3ccca40cef339d774dccc`, the same
frozen controller and old interop fixture, per-viewer fixture credentials and
generated FFmpeg test-pattern video/sine audio. The recorded runtime is Node
24.21.0 and Google Chrome 155.0.8059.85, with hls.js 1.7.3. Dependency bytes
were not instrumented separately in each trial.

Each trial has two honest viewers, one intentional corrupt peer and an
origin-only baseline; two-second H.264/AAC HLS segments at 1500 kbit/s; one
wave, 30 seconds of observation, configured 3000 ms stagger and maxPeers 3.
The peer-arrival deadline remains **1500 ms**, with the original minimum
in-time share of 0.5 and all other gates retained. TURN's peer leg is UDP even
for TCP/TLS client connections; this is not RFC 6062 TCP relaying.

Both Spry variants share source base `367efc77c84db7a20eddf70ab243dcd13d9065ef`.
The candidate patch is `d87fdee03d6a2b1acc52685e37766101cd546f1c48ec8f74375668c22822417c`.
The export records full source/controller/fixture hashes and these executable
SHA-256 identities:

```text
released   0bbcfa0c3efd6cce6a749c706e144de1740e37eb44c146f37a8887319c7085fb
candidate  7a70727f22138599fea34defc239349e7d4db1091a51f1c0637d5ed424c6ddcb
```

All 24 trials match the recorded identities/settings and pass clean-stop
checks: no retained connected peers or held bytes, and the original loader
restored. There are zero page errors, honest-viewer upload refusals or
unverified peer payloads. Every trial detects and bans intentional corruption.
Those checks do not override delivery, player or selected-path failures.

## What the failures show

Candidate UDP trial 19 fails at in-time share 0.395. Its v0 receives 7,618,324
verified peer bytes but records zero in-time deliveries, 18 late and three
misses; its race median is 1640 ms. Its v1 has 17 in-time deliveries. Both
final relay/relay UDP paths carry bidirectional traffic, without ICE errors,
unintended disconnects or player stalls. Released UDP trial 20 passes at
0.767, with 16 v0 in-time deliveries and a 1436 ms median. Cache totals and
upload trajectories are similar. Shadow playback remains origin-backed:
the original gate's fallback wording describes lack of in-time peer-race
success, even when verified peer data arrives.

Application phase or pre-request HAVE/eligibility waiting is a hypothesis,
not a demonstrated cause. v0 request-only EWMAs are 20.4/34.3 ms, whereas
race medians include prior work and waiting. Their different aggregation
windows cannot be subtracted into an exact stage duration. Approximate v0/v1
uptime gaps differ by 201 ms alongside a 204 ms race-median change, but reports
are collected sequentially. The harness also waits for v0's corruption ban
after its configured stagger before joining v1, so startup phase varies.

Released UDP trial 13 passes the share threshold at 0.627 but fails the player
gate: v0/v1 stall twice/once against zero baseline stalls. Numeric ICE 701
errors appear four times per honest viewer. High host memory/swap pressure
and delayed peer readiness limit attribution. Trial 14's controller finishes
while its parent orchestrator is paused; the recorded driver wall includes
that suspension and load-after is sampled only on resume. Those values are
not active-window duration or ending host load and are omitted from the export.

Released TCP/TLS trials show repeated native-disconnected, channel-error and
upload-drain-timeout closures; the candidate records only intentional
integrity closures. Released TLS trial 24 also fails the final selected-path
gate after earlier verified delivery. This is a composition reliability
observation. The receipts lack server close reasons and per-key stage timing
needed to assign its cause. Final paths and bounded event logs do not establish
each segment's path history.

## Separate standalone correctness gate

A finite real-socket regression first reproduced closure with **240,128
preloaded bytes in four individually valid Binding frames**: reads reached
the 131,072-byte aggregate input cap before parsing. The candidate interleaves
reading and parsing without increasing input/frame bounds, retains partial
framing and TLS decoded-plaintext readiness, and avoids connection hash-table
remove/reinsert work.

Stream turns now allow at most 64 frames, 16 read attempts and 131,072 combined
socket/plaintext bytes, including TLS control work. A deduplicated deferred
queue resumes work without requiring another readiness edge. Tests exercise
preloaded resumption, unrelated clients and expiry progress during hot input,
malformed/incomplete/oversized frames and connection cleanup.

The exact candidate separately passed **17 all-feature Rust tests, 34 UDP/TCP/TLS
wire tests**, minimal feature configurations, formatting, warnings-denied
clippy and a finite stress qualification with 2,769 churn allocations. These
are historical local correctness receipts, not comparative performance results.
EOF with queued output under backpressure, wider UDP/accept fairness and
forwarding statistics under refusal retain separate gaps.

## Next diagnostic

Use a separately identified frozen diagnostic build and preserve every original
gate. Bound its per-viewer trace and report dropped entries. Correlate run-local
key/race aliases and link IDs without payloads or network identifiers:

- `hls-swarm.mjs` `#beginRace`, `#runRace`, `#waitForHave`: race start, key hash,
  eligibility reason, HAVE wait/wake, request start and peer arrival.
- `#originLoaded`, `#hold`, `#serveRequest`, `#settle`: origin fetch/digest,
  key-specific cache insert/evict, HAVE publication, holder lookup, verification
  and classification.
- `peer-link.mjs` `#onHave`, `request`, `#onWant`, `#serve`: HAVE receipt,
  first-byte/progress/result, WANT receipt, queued serving, bufferedAmount
  waits and send completion. Attach diagnostics to the shadow caller too.

Record monotonic clock/time origin, actual viewer join and corruption-ban wait
boundaries. Calibrate separate clocks with uncertainty or compare within one
clock. Correlate RTC path changes, renderer long tasks/lag and actual-window
host CPU/RSS/swap. Separate matched stream-server diagnostics need reason-coded
input/output-limit, TLS, EOF and I/O closes with byte high waters. This should
locate pre-request waiting, transfer delay or client/host stalls before assigning
server cost. The independent peer-margin branch remains separate.
