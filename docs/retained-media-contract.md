# Retained HLS resource contract

This is a proposed follow-up to the 24 October performance audit. The server
does not yet enforce a global HLS residency budget. Recorder admission and
muxer input limits cover different owners.

| Current resource | Limit and scope |
| --- | --- |
| SRT publishers | `N`, default 16, including pending admissions |
| RTMP connections and publishers | `4N` connections; C9 separately admits `N` current publication owners |
| Resident stream paths | No registry count or byte admission limit |
| HLS window | Default 24 entries per path, configurable from 7 through 1,000, plus open media |
| Reconnect hold | Default 90 seconds, configurable up to 3,600 seconds per useful path |
| HTTP connections | 20,000 per backend; this is not a media-byte limit |

The relevant owners are in [the registry and stream](../crates/sapflow-server/src/stream.rs),
[publication retirement](../crates/sapflow-server/src/publish.rs),
[HTTP bodies](../crates/sapflow-server/src/media_body.rs) and
[the own HTTP connection](../crates/sapflow-server/src/own_http.rs).

One active publisher can populate successive paths and leave useful caches in
reconnect hold. Publications, grace tasks and parked requests can retain a
stream after registry removal. Closing a stream currently marks it closed
without immediately releasing its cached ingest state.

An incomplete segment response retains its shared chunk array, including
consumed prefix chunks. Evicting that segment from the playlist does not free
payloads retained by responses. Hyper frames may outlive their body object.
The own backend's 60-second idle timeout resets on successful progress; the
Hyper adapter sets no absolute response-body deadline. Recording queues are
another owner, independently covered by recording admission.

The proposed contract has one App-owned residency manager with two limits:

- Count resident presentations through their final stream owner. Initially
  evaluate `4 * N`, capped at 4,096, giving 64 at the default publisher limit.
- Charge each immutable fragment backing once, before publishing it. Initially
  evaluate a 512 MiB budget for whole encoded Vec capacities at the default
  configuration, including headers and spare capacity.
  All cache, segment, response, emitted-frame and recorder aliases retain that
  same charge. Release follows the final payload owner, including after cache
  eviction, codec replacement, reconnect or registry removal.

These are candidate limits to qualify, not measured operating guarantees.
Input/PES buffers, pending samples, encoding temporaries, allocation slack,
metadata and kernel socket buffers remain separate. A fragment charge is not
a process RSS measurement. Counting every alias's length would overcount
shared storage.

At capacity, refuse a new presentation or fragment visibly. Preserve already
accepted bytes and propagate ingest refusal to incomplete-recording status.
Do not evict an active presentation silently or describe a refused suffix as a
complete recording. Close should release cached state promptly while existing
response owners retain their valid bytes. Slow-viewer reclamation needs its
own explicit deadline policy and measurements.

Qualification uses generated media and small injected limits:

1. With one active publisher, populate eight distinct useful paths before
   grace expires. First reproduce current retention, then prove admission at
   the configured resident limit and release after the final owner drops.
2. Retain segment bodies, rotate beyond their windows, and verify lookup
   eviction while the retained bodies still supply exact ordered bytes.
   Drop counters must distinguish consumed-prefix retention from payload
   release.
3. Exercise exact byte capacity, one-over refusal, arithmetic overflow, cached
   aliases, codec changes, reconnects, stalled own responses and Hyper frames
   retained after body completion. Charges must survive every alias and
   return exactly once.
4. Prove recording status, publisher admission and recovery under exhaustion.
   Repeat matched nominal and adverse-load measurements with all configured
   limits declared. Keep logical accounting, RSS and kernel-memory evidence
   separate.

C9 supplies the separate RTMP publication gate. Its permit belongs to the
current session generation. Same-path takeover transfers that permit; retirement
returns it even while a presentation remains in reconnect hold. A detached
reconnect must acquire available capacity again. Cross-protocol refusal leaves
the existing owner intact. The separate `4N` connection cap remains in place.

The previous code accepted two distinct RTMP publishers with `N = 1` in an
executed regression. The correction passes 16 new quota, ownership, wire and
recording tests within an 884-test workspace run, plus Clippy and formatting.
These are admission correctness checks. They do not implement the proposed
global HLS budget or qualify throughput, RSS or sustained availability.
