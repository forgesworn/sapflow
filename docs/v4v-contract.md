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
  `PREDEFINED:402` (throttled, by sapflow or the app) or `PREDEFINED:503`
  (the app did not answer usably). RTMP refusals are
  `NetStream.Publish.Denied`.

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
| Recording finished | `{"event":"recordSegmentComplete","path":"live/<eventId>","segmentPath":"/recordings/live/<eventId>/<unix-start>.mp4","segmentDuration":"3723.5"}` |
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
files map to `video/mp4`. Legacy callback JSON is unchanged and has no track
classification. Do not infer an old file's topology from the latest live
session, and do not equate a finalised epoch file with a complete-show replay.
See [callbacks and recordings](scoped-publish-sessions.md#callbacks-and-recordings)
for required identity, sync, incomplete-prefix and asynchronous adoption fences.

## Encoder settings v4v recommends (and checks from the playlist)

1 s keyframe interval, 720p, 24-30 fps, CBR 2,500-4,000 kbps at 720p, x264
veryfast / zerolatency. The keyframe interval sets the shortest possible
segment.
