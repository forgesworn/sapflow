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
bounds connections, headers, header time and request time. Mutations that have
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
| `state` | `inactive`, `prepared`, `connected`, `live`, `held`, `expired`, `revoked` |
| `admission` | `open` or `denied` |
| `protocol` | `none`, `active`, `stopping`, `stopped` |
| `recording` | `never-started`, `active`, `finalising`, `finalised`, `failed`, `incomplete` |
| `callbacks` | `disabled`, `none`, `pending`, `delivered`, `failed` |
| `observations` | Optional aggregate scalar counters; no media, history or credentials |

HTTP errors use static codes: 400 invalid request, 401 missing/wrong bearer,
403 denied session, 404 unknown session, 409 conflicting identity/revision,
413 oversized body, 429 capacity, 503 unusable authority and 504 request timeout.
Neither an acknowledged End nor a stopped socket alone proves recording or
callback completion. Poll the independent receipts to determine completion.

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
`recordingState: "finalised" | "incomplete"`. Completion requires successful
header finalisation, flush and file sync. An interrupted accepted prefix can be
reported incomplete; it must never advertise a complete-show replay. Orientation
or reconnect can produce multiple files. A finalised epoch file alone does not
prove that every epoch of the show was aggregated.

The bounded callback queue retains completion failures in status. Only a real
successful HTTP response records delivery. A dropped queue entry, exhausted
retry, recorder failure or uncertain restart cannot silently become success.
These are runtime receipts, not a transactional outbox surviving process restart.

The current V4V connector requires complete prior receipts before preparing a
replacement for the same event. An incomplete/failed recording therefore blocks
that replacement even after admission closes and the protocol stops. An explicit
owner recovery/abandonment workflow still needs implementation; do not turn the
incomplete receipt into success to get past this gate.

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
