# Scoped native publishing sessions

The origin can authorise one short-lived encrypted SRT session per event. The
creator service owns prepare, refresh and End; the native broadcaster receives
only its session's admission credentials and independent transport secret.
This is an origin/connector boundary. Native creator login, installed mobile
consumers, production deployment and the complete B4 journey need separate
qualification.

## Configuration and network boundary

Enable all three settings together:

```sh
SAPFLOW_CONTROL_ADDR=127.0.0.1:8091
SAPFLOW_CONTROL_TOKEN=<dedicated-private-control-token>
SAPFLOW_SESSION_STORE=/absolute/private-directory/sessions.json
```

The control token must have 32 through 256 ASCII token characters. Its listener
accepts literal loopback addresses only. Put remote access behind a private TLS
proxy and authenticate the creator service there. The public HLS listener has
no control routes. Control responses have `Cache-Control: no-store`, no CORS
permission and no grant secrets. Never give the control or machine callback
secret to the phone.

Scoped mode disables RTMP and refuses global publishing credentials, open
publishing and global SRT keys. Existing explicitly configured lab mode remains
available separately. Visible SRT admission credentials must never authorise
RTMP/RTMPS. A later scoped RTMPS connector needs its own transport-bound grant.

The metadata path must be absolute, without symlink components, in an existing
directory owned by the service user with mode `0700`. The store and persistent
lock are regular, single-link, owned `0600` files. A lifetime exclusive lock
prevents two origins from sharing the authority. Startup creates and durably
records an empty authority before listening. A missing store beside an existing
lock is an error requiring operator investigation; it is never an automatic
reset to an empty authority.

## Control contract version 1

Every request uses `Authorization: Bearer <control-token>`. JSON bodies are
limited to 16 KiB, reject unknown fields and use camel-case names. The listener
bounds connections (16 at once), the request head (16 KiB, 32 headers, a 2 KiB
target), the time to receive the head (5 s) and the body and handler (10 s
together). It serves one request per connection and closes it
(`connection: close`), takes only origin-form targets with a single
`content-length` body (no chunked or repeated lengths) and checks the bearer
token before it reads any body. Mutations that have
started retain a bounded owned task through durable completion if the HTTP
caller disconnects or times out. A timeout is not proof that the mutation failed;
retry the identical request or query status.

`PUT /v1/publish-sessions/{sessionId}` provisions or refreshes:

| Field | Requirement |
|-------|-------------|
| `contractVersion` | `1` |
| `eventId`, `streamPath` | Valid event ID and exactly `live/<eventId>` |
| `eventGeneration` | Positive JavaScript-safe integer, monotonically increasing for replacement sessions |
| `sessionId` | 32 lower-case hex characters, equal to the route ID and unique among retained event rows |
| `revision` | Positive JavaScript-safe integer; higher for each changed command |
| `idempotencyKey` | 32 lower-case hex characters; unchanged for an exact retry |
| `expiresAtUnixMs` | Integer epoch milliseconds, strictly in the future and at most 15 minutes ahead |
| `reconnectGraceMs` | Integer from zero through 3,600,000 |
| `transport` | `srt` |
| `publishUser` | Exactly `sessionId` |
| `publishPassword` | Independent CSPRNG-generated 32-byte token, encoded as 43 base64url characters |
| `srt` | `cipher: "aes256-ctr"` and a separate 43-character base64url `passphrase` |
| `limits` | Positive `maxPublishBps` at or below the server cap; `maxQueueBytes` from 1500 through 4,194,304 |

A refresh uses the same identity and immutable configuration, with a higher
revision, new idempotency key and strictly increased expiry. It preserves the
active keys. Secret rotation requires a replacement session with a higher event
generation. Reusing one token as both password and passphrase is refused.
Session identifiers and SRT control traffic are visible on the network;
AES-CTR does not authenticate each media packet.

`POST /v1/publish-sessions/{sessionId}/revoke` takes only `contractVersion`,
`eventId`, `eventGeneration`, `sessionId`, `revision` and `idempotencyKey`.
Use a new higher revision for End and retry that exact packet. Revocation is
terminal. Revoke-before-provision creates a durable denied tombstone.

`GET /v1/publish-sessions/{sessionId}` and successful mutations return the same
secret-free receipt:

| Field | Values or meaning |
|-------|-------------------|
| Identity | `contractVersion`, `eventId`, `eventGeneration`, `sessionId`, `revision` |
| `expiresAtUnixMs` | Grant expiry; zero only for a never-provisioned terminal tombstone |
| `connectionEpoch` | Durable per-event high-water mark, reserved before accepting the protocol |
| `connectionAttemptId` | `null` or the current accepted/bound SRT connection's 32-character lower-case hexadecimal correlation ID |
| `freshMedia` | `null` or the current connection's independently decodable completed-part descriptor below; encoded availability only |
| `state` | `inactive`, `prepared`, `connected`, `live`, `held`, `expired`, `revoked` |
| `admission` | `open` or `denied` |
| `protocol` | `none`, `active`, `stopping`, `stopped` |
| `recording` | `never-started`, `active`, `finalising`, `finalised`, `failed`, `incomplete` |
| `callbacks` | `disabled`, `none`, `pending`, `delivered`, `failed` |
| `observations` | Optional aggregate scalar counters; no media, history or credentials |

HTTP errors use static codes in `{"ok":false,"error":<code>}` bodies, with
`cache-control: no-store`: 400 invalid request (including chunked or repeated
length framing), 401 missing/wrong bearer, 403 denied session, 404 unknown
session or unknown path, 405 wrong method (with `allow`), 408 request head not
received in time, 409 conflicting identity/revision, 413 oversized body, 414
oversized target, 429 capacity, 431 oversized head or too many headers, 503
unusable authority and 504 request timeout.
Neither an acknowledged End nor a stopped socket alone proves recording or
callback completion. Poll the independent receipts to determine completion.

## Per-attempt correlation

A native SRT caller can send a fresh CSPRNG-generated 16-byte attempt identifier,
encoded as 32 lower-case hexadecimal characters, in this access-control form:

```text
#!::m=publish,r=live/event-example,u=<sessionId>,s=<publishPassword>,forgesworn_attempt=0123456789abcdef0123456789abcdef
```

The canonical custom key is `forgesworn_attempt`, following the company-prefixed
custom-key convention in the primary [SRT Internet-Draft, Appendix B.2](https://datatracker.ietf.org/doc/html/draft-sharabayko-srt-01#appendix-B.2).
This is an expired informational draft's recommended convention, not a registered
standard attribute. `x-sapflow-attempt` is not an alias. The parser refuses duplicate
canonical attributes, missing/invalid values and case/whitespace variants of the
reserved key. The entire stream ID retains its 512-byte limit. Other existing
publisher forms remain compatible; colon-form SRT and RTMP requests have no
attempt binding.

Authenticated receipts add `connectionAttemptId: string | null`. It becomes
nonnull only after valid scoped admission, protocol acceptance and binding to
the exact current stream generation. It can be available in `connected` before
a fresh media part exists; when `freshMedia` is present, both values come from
the same current connection snapshot and its `connectionEpoch`. A credential
refresh preserves the current connection's ID. A reconnect must supply a new ID;
it never inherits the prior one. Held, cancelled, revoked, expired, unbound or
replaced connections expose null. Restart also exposes null: correlation IDs are
runtime metadata and are not written into the durable grant snapshot.

The native adapter must compare the exact fresh random attempt ID alongside the
session identity, generation, connection epoch and its local attempt token before
and after a finite decode observation. Missing or mismatched IDs cannot confirm
that attempt. The value is caller-supplied visible correlation metadata, not an
extra credential, uniqueness authority, replay-prevention mechanism, peer
authentication or encryption guarantee. Reusing an ID destroys its causal value;
the origin does not maintain an unbounded nonce-history table. The attribute does
not change publishing permission, key selection, grant expiry or callback bodies.
Never log or persist a full credential-bearing stream ID to diagnose correlation.

## Fresh encoded-media boundary

`freshMedia` is `null` until the current accepted connection emits a completed
independent video part. Once available its exact additive shape is:

```json
{
  "connectionEpoch": 2,
  "partId": 15,
  "initializationId": 1,
  "initPath": "/live/example/abcdef012345_init1.mp4",
  "partPath": "/live/example/abcdef012345_part15.mp4",
  "hasVideo": true,
  "hasAudio": true
}
```

The containing receipt binds `eventId`, `eventGeneration`, `sessionId` and
`revision`. Descriptor `connectionEpoch` equals the containing receipt's epoch;
all IDs are JavaScript-safe integers, with a positive connection epoch and
nonnegative part/initialization IDs. `hasVideo` is always true in this initial
video-origin profile. `hasAudio` declares the initialization's audio track; it
does not claim that an audio frame was received or decoded.

Paths are canonical leading-slash relative origin paths for that exact event,
with one shared lower-case hexadecimal presentation prefix of one through twelve
characters. Initialization zero uses `_init.mp4`; positive initialization IDs use
`_init<ID>.mp4`. Parts use `_part<ID>.mp4`. These are existing immutable HLS
identities, without credentials, query strings or an authority. A connector maps
them through trusted **public playback routing**, including any configured proxy
prefix; it must never resolve them against the private control API URL.

The old connection's pending run is sealed under the registry/session locks
before the replacement captures its completed-part floor. A pending first IDR,
codec-restart notification, retained old playlist or old completed part cannot
satisfy this boundary. The descriptor selects a retained independent part at or
above that floor from the current initialization. It holds no media references
and cannot extend retention. It becomes null while held, cancelled, revoked,
expired, replaced, after restart, or after a codec reset until a matching fresh
independent part exists. No retained matching part also yields null. Scoped
`state: "live"` and nonnull `freshMedia` derive from the same locked snapshot;
otherwise an active accepted connection remains `connected`. Legacy lab HLS
continuation and codec-epoch numbering remain unchanged.

**This is not decoded-media proof.** A native connector must bind its local
attempt to a fresh authenticated origin connection epoch, fetch only the
appropriate fresh media, observe actual decode progress for every declared
track, and recheck the session/epoch before confirming recovery. HWM reservation
alone, a ready socket, a manifest probe, or this descriptor alone cannot clear
`NativeSession`'s outage deadline. Binding requires the current `connectionAttemptId` echo and previous-attempt
cleanup; an epoch high-water increase alone can also reflect a failed admission.
Publishers without an attempt ID remain usable but do not expose this confirmation
binding. Delayed responses and decode completions need local destination/attempt
fences as well as the origin identity.

## Durability and runtime admission

The pure authority in `sapflow-core` bounds retained event rows to 4096. It never
evicts terminal fences to make room; capacity fails closed. Fingerprints bind
validated canonical commands and immutable policy without persisting secrets.
The durable document is bounded to 4 MiB and checksummed. Replacement writes a
new exclusive temporary file, syncs it, renames it and syncs the parent directory.
An uncertain durability result poisons the running authority and closes admission.
The checksum detects corruption, not a privileged operator rolling storage back.

Clock floors, terminal states and per-event connection epochs survive restart.
Restart restores every grant with admission disabled and without keys. An exact
retry of the old provision remains inactive; a fresh higher-revision refresh
before terminal expiry must restore the matching credentials. Expired/revoked
sessions require a new generation. Prior-process recording/callback work with an
existing connection epoch is conservatively incomplete/failed, rather than
inventing successful completion.

Disk sync never holds the fast media gate or UDP protocol loop. A claim's new
epoch is durable before protocol acceptance. Revocation and expiry close the
runtime gate, terminate that exact stream generation and wake an idle SRT owner.
Media ingress and recording fan-out pass through the same gate. Late work from
a released, cancelled or replaced lease cannot mutate its successor. Only
termination of the real native SRT connection records `protocol: stopped`.

A refresh staged before expiry can finish syncing after the previous deadline.
The old transport may already have been cancelled. It stays cancelled; the
durable extension permits a new connection with the identical keys and a new
epoch. Status must not describe that cancelled owner as live or held.

## Callbacks and recordings

Scoped callbacks carry `contractVersion`, `eventId`, `eventGeneration`,
`sessionId`, `connectionEpoch` and positive JavaScript-safe `eventSequence`.
Publication and recorder share one sequence within an epoch. The receiver fences
telemetry by the current identity, epoch and increasing sequence. A matching
final recording from an older connection epoch may finish after reconnect; it
requires its independent bounded `recordingId` receipt and an identity recheck
after any asynchronous copy. A replaced session/generation never becomes current
again through an old callback.

Recording completion additionally includes a 32-character `recordingId` and
`recordingState: "finalised" | "incomplete"`, and the additive field
`recordingTracks: {"hasVideo": boolean, "hasAudio": boolean}`. Both track keys
are present and at least one is true. The producer captures this non-empty
topology once from that recording's first initialization, using the same tracks
for its initial and final file headers. A later live codec epoch or requested
phone profile cannot relabel the recording. Only scoped completion callbacks
include this field; legacy callback bytes are unchanged.

This is a pre-1.0 Rust source-API migration: code constructing
`StreamEvent::RecordSegmentComplete` must supply the new required typed
`recording_tracks: RecordingTracks` field. Exhaustive field patterns must include
it or use `..`. `RecordingTracks::new` refuses both flags false. The Rust source
change does not alter legacy JSON bytes, the creator grant/status schema or
native binding APIs.

Receivers may map audio-only to `audio/mp4`, and any video-bearing recording to
`video/mp4`. Missing or malformed topology is unknown, not permission to infer
tracks from current live metadata. New automatic topology-dependent adoption
requires the field. Keep the exact recording identity, path, state and topology
immutable across callback retries and any asynchronous copy; recheck the current
prepared session/generation after copying. A matching older connection epoch
within the same session can finish later, but replacement or abandonment cannot
be undone by that callback. Already adopted replay metadata is not relabelled.
Topology describes the stored file's tracks, not decoding, complete-show
coverage or permission to publish an incomplete recording.

Completion requires successful
header finalisation, flush and file sync. An interrupted accepted prefix can be
reported incomplete; it must never advertise a complete-show replay. Orientation
or reconnect can produce multiple files. A finalised epoch file alone does not
prove that every epoch of the show was aggregated.

Scoped files use `<unix-start>-<recordingId>.mp4`, with the exact callback ID
and the start second captured by the writer. Atomic creation refuses a duplicate
identity without overwriting it or advancing the timestamp. Distinct same-second
epochs have no sixty-file collision limit. Legacy publishers retain their
Unix-second filenames. Expiry recognises only the exact legacy or scoped grammar;
consumers must recognise the scoped suffix before inferring a replay start.

The bounded callback queue retains completion failures in status. Only a real
successful HTTP response records delivery. A dropped queue entry, exhausted
retry, recorder failure or uncertain restart cannot silently become success.
These are runtime receipts, not a transactional outbox surviving process restart.

The V4V connector supports explicit acknowledgement to abandon automatic
recording adoption after a terminal incomplete/failed receipt. It rechecks fresh
authenticated origin status and preserves the failure with End incomplete. The
acknowledgement returns no grant and never starts a successor. A separate explicit
Prepare can consume that exact acknowledged identity and reserve one successor;
lost replies retry the saved request. An ended event may return to scheduled only
when adopting that authorised successor. Cancelled/archived events and events
with an existing adopted recording or replay remain blocked. Late abandoned
recording copies and callbacks cannot attach to the replacement.

## Verification boundaries

Core, store, manager, HTTP, recorder and callback tests cover command conflicts,
terminal/restart fences, slow and abandoned durable operations, stale leases,
expiry, real file finalisation and actual callback responses. The
`publish_session` fuzz target exercises the production control/store parsers and
bounded stateful authority transitions without filesystem or network access.
Generated-media connector tests separately exercise the real creator API,
encrypted ingest, HLS decode and replay. None of these substitutes for physical
phone login/refresh/End, sustained performance, deployment or owner acceptance.

Real camera footage, extracted frames and private recordings stay outside Git
and public test artefacts. Source-controlled media fixtures require verified
synthetic provenance.
