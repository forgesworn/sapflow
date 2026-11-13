# What v4v expects from its media server

Taken from the v4v repo as of 2026-10-20 (`docs/live-stage.md`, "Live Video
Boundary"; `docs/examples/mediamtx.yml`; `scripts/live-ingest-smoke.mjs`).
This is the historical legacy URL-template/callback contract. The current
[scoped-session contract](scoped-publish-sessions.md) adds private provisioning,
immutable session/recording identities and bounded terminal receipts; it needs
a corresponding V4V connector. These are separate integration contracts.

## Stream paths

- Every stage stream is `live/<eventId>`, with `<eventId>` matching
  `event-[a-z0-9-]+` (`PULSEWIRE_LIVE_STREAM_ID_TEMPLATE={eventId}`).
- Only the configured publisher, or one the app admits for that path (below),
  may push to those paths. Anyone may read HLS
  (v4v enforces its closed beta itself, in front).

## Publish authorisation

Two ways to decide who may publish, outside scoped sessions; set exactly one
(sapflow refuses to start with both, or with either and
`SAPFLOW_OPEN_PUBLISH=1`):

- `SAPFLOW_PUBLISH_USER` and `SAPFLOW_PUBLISH_PASS`: one shared publisher for
  every path. Any gig's credentials publish to every gig.
- `SAPFLOW_PUBLISH_AUTH_URL`: ask the app about each publisher, the request
  mediamtx's `authMethod: http` sends, so one app endpoint serves both media
  servers and each gig can have its own key. Scoped publish sessions
  (`SAPFLOW_CONTROL_ADDR`) refuse it, as they refuse the shared publisher.

```
POST http://<app-host>:5181/api/media/publish-auth?token=<app-token>&server=sapflow
content-type: application/json

{"user":"<user>","password":"<password>","ip":"<publisher address>","action":"publish",
 "path":"live/<eventId>","protocol":"srt","id":null,"query":""}
```

- `user` and `password` are what the publisher gave, from either SRT stream
  id form (`publish:live/<eventId>:<user>:<pass>` or
  `#!::m=publish,r=live/<eventId>,u=<user>,s=<pass>`) or the RTMP key's
  `?user=&pass=` query (percent-decoded). Missing ones are sent empty.
- `protocol` is `srt` or `rtmp`. `query` is always empty: sapflow does not
  keep the raw query or stream id, and the password is already in
  `password`. `id` is always `null`.
- Any 2xx admits. 401 or 403 refuses and counts as a failed attempt against
  sapflow's own throttle (10 a minute per address). 429 refuses without
  counting. Any other status, no answer within 3 s, or a connection that
  fails refuses without counting. There is one attempt and no retry; a
  redirect is not followed; the answer's body is never read.
- `http://` only: sapflow has no TLS client, so keep this on the private
  network. The whole URL is treated as a secret and never logged, and
  neither is the password.
- SRT refusals reach the caller as `PREDEFINED:401` (refused),
  `PREDEFINED:402` (throttled, by sapflow or the app, or a limit on
  publishers or memory) or `PREDEFINED:503` (the app did not answer usably).
  RTMP refusals are `NetStream.Publish.Denied`.
- A stage that admits can still be refused after the app's answer, by
  sapflow's own limits. That includes a new stage when sapflow's media
  memory would be too full for it (the streams' expected windows at 80% of
  the budget) or while eight stages are still starting, as after a restart
  (SRT `402`, RTMP "media memory limit"); an encoder reconnecting to a stage
  sapflow still holds is not refused for memory. A refused encoder that
  retries is let in once there is room; nothing queues it. Each retry asks
  the app again.

### A passphrase per SRT stream

With `SAPFLOW_SRT_APP_KEYS=1` (off by default; it needs
`SAPFLOW_PUBLISH_AUTH_URL` and is refused with scoped sessions), the app can
give each SRT stream its own passphrase, so the publish key, which an SRT
handshake carries in clear in the stream id, no longer admits on its own.

- SRT requests then carry one more field, `"srtKey":"accepted"`. sapflow
  sets it and no setting can. RTMP and WHIP requests never carry it, and
  with the switch off no request does, so the body is as above. An app
  that relies on a passphrase should refuse an SRT request without the
  field: a sapflow that sends none admits on the 2xx alone.
- A 2xx may carry the stream's passphrase in one `sapflow-srt-passphrase`
  header: 10 to 79 bytes, no NUL (libsrt's limits), exactly as the encoder
  is given it (libsrt does not percent-decode a passphrase). Spaces and
  tabs around the value are not part of it. sapflow then
  admits the caller only if its SRT key material was made with that
  passphrase, at key length 16 or 32, whichever the caller chose.
- The key is checked before the path is claimed. A caller with no key
  material is refused with `SRT_REJ_UNSECURE` (1011), one with another
  passphrase with `SRT_REJ_BADSECRET` (1010); either counts as a failed
  attempt against sapflow's throttle. A stage that is live, or held for a
  reconnect, keeps its publisher, recording and callbacks: nothing is
  replaced, no `unpublish` or `recordSegmentComplete` is sent. The app
  answered 2xx, so it hears nothing of such a refusal; only the encoder
  does.
- A 2xx without the header admits as before. A header sent twice, or
  outside the bounds, refuses as an unusable answer does (`PREDEFINED:503`,
  not counted). The header is read only from a 2xx to an SRT request.
- The passphrase is never logged and is dropped once the caller is
  answered; an accepted connection keeps only the keys derived from it. The "SRT publisher accepted" log line says how each publisher
  encrypts: `encryption="none"`, `"deployment-key"` (the global
  `SAPFLOW_SRT_PASSPHRASE`), `"app-key"` or `"session-key"` (scoped), with
  `key_len`.
- With app keys and no global passphrase, `SAPFLOW_SRT_ENCRYPTION=required`
  refuses every caller that sends no key material, in the handshake, before
  the app is asked. Left unset, plaintext callers still connect.
- Not for production yet: a passing check proves the key material was made
  with the passphrase, not that the caller made it. Someone who captured an
  encoder's handshake can replay it to the same stream until sapflow guards
  against replay.

## Ingest addresses v4v hands to encoders

| Template | Example value |
|----------|---------------|
| `PULSEWIRE_LIVE_RTMP_SERVER_URL_TEMPLATE` | `rtmp://<media-host>:1935/live` |
| `PULSEWIRE_LIVE_STREAM_KEY_TEMPLATE` | `{streamId}?user=<publisher>&pass=<publisher-pass>` |
| `PULSEWIRE_LIVE_SRT_URL_TEMPLATE` | `srt://<media-host>:8890?streamid=publish:{streamId}` (mediamtx form; credentials can ride in the streamid as `publish:<path>:<user>:<pass>`) |

Studio appends `&latency=2000000` (microseconds) to the SRT URL it shows for
OBS. Larix sets its own latency.

## Playback

- `PULSEWIRE_LIVE_HLS_URL_TEMPLATE`, e.g.
  `https://<your-domain>/live-hls/live/{streamId}/index.m3u8`, proxied by Caddy
  to the media server's HLS port (8888 in the reference config).
- `PULSEWIRE_LIVE_PLAYBACK_PROBE_URL_TEMPLATE`: the internal address v4v
  fetches to confirm the manifest before opening a stage.
- Reference LL-HLS settings: 1 s segments, 200 ms parts, 24 segments kept,
  remux as soon as the encoder connects (not on first viewer), CORS `*`.

## The callback

One shared secret for every path (`PULSEWIRE_LIVE_CALLBACK_SECRET`), the event
id taken from the last segment of the path:

```
POST http://<app>:5181/api/public/live-events/<eventId>/stream-health?token=<callback-secret>
content-type: application/json
```

| When | Body |
|------|------|
| Stream ready to play | `{"event":"publish","path":"live/<eventId>"}` |
| Stream gone | `{"event":"unpublish","path":"live/<eventId>"}` |
| Recording finished | `{"event":"recordSegmentComplete","path":"live/<eventId>","segmentPath":"/recordings/live/<eventId>/<unix-start>.mp4","segmentDuration":"3723.5","recordingState":"finalised","recordingReason":null}` |
| While live, every `SAPFLOW_HEALTH_INTERVAL` (sapflow only; default 30 s) | `{"event":"health","path":"live/<eventId>","health":{"bitrateKbps":2620,"keyframeIntervalMs":1000,"audio":true,"breaks":0}}` |

v4v reads `health.bitrateKbps` (Studio's bitrate and its lagging and
unstable thresholds) and leaves the stage's status alone for an event name
it does not know. It works out the keyframe interval from the playlist
itself, so `keyframeIntervalMs`, `audio` and `breaks` are not yet read by
v4v. The callback endpoint allows 120 requests a minute from one address:
at 30 s, 16 live stages use 32.

When an encoder drops, sapflow sends `unpublish` at once and holds the
stream for `SAPFLOW_RECONNECT_GRACE` (default 90 s); an encoder back in
time continues the same playlist and recording and sends `publish` again.
v4v holds the gig for `PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS` (default
90). Keep the two equal. `recordSegmentComplete` arrives when the stream
ends, so up to the grace period after the encoder's last drop.

When sapflow itself is stopped (SIGTERM or ctrl-c, as at a `docker compose
up` redeploy or a Docker restart), it takes no new encoder and ends every
stream at once, with no grace:

- A stage whose encoder is still connected gets `unpublish`, then
  `recordSegmentComplete` with `recordingState: "incomplete"` and
  `recordingReason: "server-stopped"`, or the reason its file already had.
- A stage held for a reconnect, which has had its `unpublish`, gets
  `recordSegmentComplete` as when its grace runs out, normally `finalised`.
- A file from the stage's previous codec epoch, still held for late audio,
  gets its own `recordSegmentComplete`, normally `finalised`.

With recording off, no `recordSegmentComplete` follows.

sapflow waits up to 7.5 s for its files and callbacks, then exits, inside
Docker's default 10 s stop timeout. A file still being written or a callback
still unsent by then (one being retried while v4v is down, say) goes
unreported, as does everything after a SIGKILL; the file stays on disk. An
encoder that reconnects after the restart starts a new stream and a new
file.

v4v rejects a callback whose path does not match the prepared session.

## Recording

- Fragmented MP4 (plays in a browser `<video>`), one file per show, named by
  start time in unix seconds (`recordPath: /recordings/%path/%s`).
- v4v only reads files under `PULSEWIRE_LIVE_RECORDING_DIR` (its mount of the
  same directory, read-only) and copies them into its own store.
- mediamtx keeps its copy for 7 days (`recordDeleteAfter: 7d`).

The one-file-per-show description above is historical legacy behaviour, not a
guarantee of current scoped recording. Current codec epochs can produce separate
files, with scoped names `<unix-start>-<recordingId>.mp4`. Scoped completions add
immutable `recordingTracks: {"hasVideo": boolean, "hasAudio": boolean}` from the
actual file initialization. Audio-only files map to `audio/mp4`; video-bearing
files map to `video/mp4`. Legacy callbacks have no track classification. Both callback modes now add
`recordingState: "finalised" | "incomplete"` and `recordingReason: string | null`
from the synchronised writer result. An incomplete file can decode successfully
while containing gaps or only a prefix; the app must retain and show that warning.
A finalised file is not proof of a complete show. Older callbacks without these
fields have unknown integrity, never an implicit finalised result. Retries keep
the exact same state and reason. Reason codes include `ingest-discarded-packets`,
`srt-unrecovered-loss`, `srt-timeout`, `srt-ingest-queue-full`,
`rtmp-protocol-error`, `over-bitrate`, `stale-media`, `srt-listener-stopped`,
`ingest-error`, `media-loss`, `end-of-input-loss`, `writer-behind`, `disk-low`,
`epoch-unrecorded` (a codec epoch before this file was served but not
recorded), `epoch-rate` (the encoder changed its codec configuration too
often: codec epochs after this file, or before it, were served but not
recorded; see `SAPFLOW_RECORDING_ROLL_*`) and `server-stopped` (sapflow
was stopped while the encoder was connected; see
[the callback](#the-callback)). A stream that changes
configuration often can therefore end a show with a stretch that no file
holds; the files on either side of it say so. A file keeps only its first
reason, so the file before such a stretch can read another code (say
`media-loss`, from the same batch), and the file after it reads
`epoch-rate` even if it later loses media too: either way it is
`incomplete`. A resumed file can also lack its own epoch's first seconds,
when the roll came back after those parts had left retention; its reason
is still `epoch-rate`.
Receivers must handle unknown future reason codes without rendering untrusted
provider text. Failed or unsynchronised files still produce no completion. Do not infer an old file's topology from the latest live
session, and do not equate a finalised epoch file with a complete-show replay.
See [callbacks and recordings](scoped-publish-sessions.md#callbacks-and-recordings)
for required identity, sync, incomplete-prefix and asynchronous adoption fences.

## Encoder settings v4v recommends (and checks from the playlist)

1 s keyframe interval, 720p, 24-30 fps, CBR 2,500-4,000 kbps at 720p, x264
veryfast / zerolatency. The keyframe interval sets the shortest possible
segment.
