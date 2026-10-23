# Trial plan: one show on sapflow

Written 2026-10-21 against `main` (10b7e40), for the maintainer to follow. It
turns the staged rollout in [readiness.md](readiness.md) into steps. The
trial is one non-critical show of the v4v live stage with mediamtx left
ready, and it ends in a decision (section 5), not a switch.

Historical checkpoint from 2026-10-21: OBS, Larix and
our own Android app have published over SRT (with and without a passphrase)
and RTMP (the docs record the first two only); v4v's player fix for jumping back to live after a drop (v4v #475)
is merged and was checked in real Safari; our Android app does not
reconnect by itself at that checkpoint (Larix and OBS did); the wired Linux
load test had not run. Later native development adds bounded reconnect on the
broadcaster branch, not yet on `main`, and records a finite Pixel recovery test in
[mobile delivery status](mobile-delivery-status.md). That evidence does not
qualify the native app for this complete-show trial. Since then (2026-10-22),
the decisions below were taken. Check current readiness and player/deployment
evidence when choosing the frozen trial revision.

Everything in angle brackets is a placeholder, filled in from the
deployment and never committed.

| Placeholder | Meaning |
|-------------|---------|
| `<commit>` | the sapflow commit the trial runs (section 2.1) |
| `<domain>` | the public v4v domain behind Caddy |
| `<media-host>` | the name performers' encoders connect to |
| `<app>`, `<app-port>` | v4v's app on the internal network, and its port |
| `<mediamtx>`, `<mediamtx-hls-port>` | mediamtx on the internal network, and its HLS port |
| `<sapflow>` | the sapflow service on the same internal network |
| `<trial-rtmp-port>` | sapflow's RTMP port on the media host for phase A of the dry run only; from the switch sapflow has 1935 |
| `<recordings-dir>` | the host directory v4v mounts read-only as `PULSEWIRE_LIVE_RECORDING_DIR` |
| `<publisher>`, `<publisher-password>`, `<callback-secret>` | the existing values (section 2.3) |
| `<eventId>` | the trial show's event id, `event-[a-z0-9-]+` |
| `<env-file>`, `<caddyfile>`, `<compose-dir>` | v4v's environment file, Caddyfile and compose directory on the host |

## Decisions to make before anything else

- [ ] **The show and its audience.** A show with no other stage live in the
  switch window (the switch is deployment-wide, section 6), whose planned
  peak is 50 concurrent viewers or fewer, inside the closed beta.
- [ ] **The performer's encoder.** Our own app first, if two things hold
  by the dry run: the broadcaster branch, with its reconnect, is on `main`
  and in the build the performer's phone runs, and phase A shows it
  resuming every drop inside the 90 s grace by itself and lasting the soak
  with its local recording intact. This is a complete-show qualification
  requirement, not acceptance from the finite Pixel test. It publishes to a
  manual destination (the RTMP or SRT URL below), as Larix does: its V4V mode
  uses per-session scoped encryption, which needs sapflow's control mode,
  and that mode turns RTMP and the shared publisher off, so it is not this
  trial. Otherwise Larix or OBS with auto-reconnect on. Either way Larix is
  installed on the performer's phone with the same settings, as the fallback
  encoder: the URL is the same for both (section 3).
- [ ] **The performer's protocol.** RTMP is the first choice: it is what v4v's
  reference mediamtx config serves (SRT is off there), it has the longer
  record (Larix resumed seven drops in six minutes), and it is the only
  protocol mediamtx can take back in a rollback. Use SRT only if the venue's
  uplink needs it, and then only after the dry run has shown the encoder
  connecting reliably.
- [x] **Where sapflow runs, and its ports** (2026-10-22). The sapflow
  container already on the host is the trial's sapflow, reconfigured as in
  section 2.2. At the switch it takes RTMP port 1935 from mediamtx, so the
  RTMP URL encoders use never changes; mediamtx keeps running without that
  port, and a rollback gives it back (section 4). The cost: rolling back
  restarts sapflow and needs a compose command on the host, rather than the
  performer switching to a second saved connection.
- [x] **SRT passphrase** (2026-10-22). Optional: `SAPFLOW_SRT_PASSPHRASE`
  set and `SAPFLOW_SRT_ENCRYPTION=optional`, so encoders with and without
  it both publish (section 2.3). Needs a `<commit>` that has the setting.
- [x] **Viewer HTTP** (2026-10-22). hyper, the default, for the first show.
  Our own event loop (`SAPFLOW_HTTP_IMPL=own`) is measured but has not
  served a show; it is a candidate for the second.
- [x] **Read production's live settings from the server.** They are not in
  the repository. On the production host, list the variable names and their
  scheme only, without printing secrets:
  `grep -oE '^PULSEWIRE_LIVE_[A-Z_]+=([a-z]+://)?' <env-file>`. Then check:
  - `PULSEWIRE_LIVE_RTMP_SERVER_URL_TEMPLATE` starts `rtmp://`. If it starts
    `rtmps://`, sapflow cannot take it directly: either the trial offers plain
    `rtmp://` to the one performer, or a TLS front (Caddy layer4 or stunnel)
    terminates RTMPS in front of sapflow's RTMP port, and the dry run covers it.
  - `PULSEWIRE_LIVE_STREAM_ID_TEMPLATE` is `{eventId}` (section 2.5).
  - The publisher password in mediamtx's `authInternalUsers` is letters and
    digits only (section 2.3).
  - Who owns mediamtx's recording directory, so sapflow's
    `SAPFLOW_RECORDINGS` gets the same owner (section 2.3).

  Read on 2026-10-22: RTMP is plain `rtmp://`; the stream id template is
  `{eventId}`; the publisher password is letters and digits; the recording
  directory belongs to the deploy user (mode 775), shared by mediamtx and
  read-only to the app. Two things differ from the reference config:
  - mediamtx has SRT off, but `PULSEWIRE_LIVE_SRT_URL_TEMPLATE` is set, and a
    sapflow container already on the host takes SRT on 8890 with its HLS on a
    loopback port. Decided 2026-10-22: that container is the trial's
    sapflow (section 2.2).
  - `PULSEWIRE_LIVE_PLAYBACK_PROBE_URL_TEMPLATE` is `https://`, not the
    internal `http://` form in section 2.5: read where it points before
    changing it.

  These observations describe the legacy trial deployment. They do not
  configure native V4V clients or replace their scoped destination grants,
  per-session encryption or approved playback origins.
- [ ] **Who decides a rollback on the night**, and the channel to the
  performer that stays open throughout (a call or a chat, not Studio).

## 1. Scope and success criteria

In scope: ingest, LL-HLS to viewers through Caddy, recording into v4v's
replay, the stream-health callback, and the way back. Out of scope: more
than one stage at once, a second rendition, encoders not listed in
[interop.md](interop.md), a viewer count past what section 6 allows.

"It worked" means every row below held for the whole show, measured as
stated. Where a number is chosen, not measured, the last column says so; the
dry run (section 2.6) and the show itself are what test the choices.

| # | Criterion | Threshold | How measured | Source |
|---|-----------|-----------|--------------|--------|
| 1 | The stream stayed up | sapflow never restarted (`RestartCount` 0, no `sapflow stopped` in its log); every `unpublish` has an encoder or network drop behind it; every drop resumed within the grace (90 s, with `publish` sent again); v4v's gig never left `live` | `docker inspect`, sapflow log, v4v integration feed and Studio | Resumes measured in [interop.md](interop.md) "Stopping and restarting" (seven RTMP drops resumed, OBS after 62 s); grace from [v4v-contract.md](v4v-contract.md) |
| 2 | No viewer-visible stall beyond 5 s outside an encoder drop | none; shorter stalls are counted and reviewed | Safari `?stats=1` (stalls, stalled time) on an iPhone and a Mac; `live:monitor` never reports `down` | Chosen. 5 s is under the 6 s at which sapflow ends a held request itself (3 x the 2 s target that 1 s keyframes give, [interop.md](interop.md) "Behind Caddy"); no viewer-stall figures exist |
| 3 | Delay over HTTP/2 within the band | The band is at most 4 s: at the first reading and again within 30 s of each resumed drop; median at most 5 s over the show. Expect about 3 s over SRT at 2000 ms and less over RTMP | A phone filming a millisecond clock once at the start (the method behind the first figure in the last column), then the `?stats=1` estimate through the show | 3.0 and 3.2 s measured, Larix to Safari over HTTP/2 (a Node front standing in for Caddy), 2 s SRT latency; v4v expects 2 to 3 s ([interop.md](interop.md) "Delay through the server"). hls.js held 1.6 s back through real Caddy at 1 s keyframes (excludes encoder and ingest; "Behind Caddy"). 30 s: v4v's catch-up waits 15 s between jumps (`docs/live-stage.md`, Latency). 4 s and 5 s are chosen. 10.8 s means Safari fell back to HTTP/1.1 |
| 4 | Recording complete and playable | one file `<recordings-dir>/live/<eventId>/<unix-start>.mp4` for the show; `ffprobe` length within 5% of the on-air time (publish to last unpublish, minus drops, which the file leaves out); plays and seeks start to end in Chrome and Safari `<video>`, audio present; v4v imported it as the gig's replay | `ffprobe`, a browser, Studio "Publish the replay" | [GOALS.md](../GOALS.md) M3; a 60 s run with drops recorded 50.8 s ([interop.md](interop.md) "Reproducing irregular encoder timing") |
| 5 | Callbacks received by v4v | every `stream event` line in sapflow's log has a matching entry in v4v's integration feed; no `callback refused`, `callback failed` or `callback timed out`; `health` about every 30 s (count within 1 of show length / 30 s); exactly one `recordSegmentComplete`, within the grace of the last drop | sapflow log against the integration feed | [v4v-contract.md](v4v-contract.md). sapflow tries a callback four times (backoff 0.5, 1 and 2 s) and does not queue it durably |
| 6 | sapflow's CPU and memory inside a bound | CPU mean at most 15% of one core; RSS at most 100 MB and at most 25 MB above its value ten minutes in; open descriptors under half the limit and back near baseline when viewers leave | `docker stats` every 30 s to a file; `/proc/<pid>/fd` from the host (the image has no shell) | Measured on macOS only: 0.3 to 0.4% and 17 to 24 MB with no viewers, 10 to 11.5% and 45 to 56 MB at 250 viewers, 26 to 30% and 66 to 77 MB at 750 ([benchmarks.md](benchmarks.md)). Bounds and the growth figure are chosen: no show-length run exists and nothing has run on Linux |
| 7 | Caddy served HLS without errors | no 5xx on `/live-hls/` for the show; no `dial tcp ... connection refused` in Caddy's log | Caddy error log; a JSON access log for that path for the show only | 502s at 250 viewers with the upstream pool at its default of 32, none at 1024 ([benchmarks.md](benchmarks.md), HTTP/2 section; Docker on a Mac, one run each) |

Recorded but not bounded, because nothing measures them yet: Caddy's own CPU
and memory, and the host's load average.

## 2. Before the night

### 2.1 Name the build

No release tag exists. The trial runs a commit, and the image and every log
carry it.

- [ ] Choose `<commit>` on `main`, at or after the commits that add
  `SAPFLOW_SRT_ENCRYPTION`, stop on SIGTERM (without it, a rollback waits
  10 s for Docker to kill sapflow) and report a recording with a gap as
  finished (without it, an SRT drop during the show means v4v gets no
  replay). Confirm CI is green on that exact commit
  (Ubuntu: fmt, clippy, tests, `scripts/smoke.sh`, fuzz regressions and 60 s
  per target).
- [ ] On the build machine, at `<commit>`: `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `scripts/smoke.sh` (it builds and tests the debug binary).
- [ ] Build the release binary there and run both v4v ingest gates against
  it, from a v4v worktree of `feat/ingest-gate-sapflow` (not merged to v4v
  `main` as of 2026-10-21, and based before #475 to #477, so merge `main`
  into a local copy of it first; its changes are the gate script and a
  playlist helper, not the player):
  `SAPFLOW_BIN=<path to target/release/sapflow> npm run e2e:ingest-sapflow`
  and `... npm run e2e:ingest-srt-sapflow`. The merge conflicts in
  `scripts/live-ingest-smoke.mjs` (keep both sides), and the SRT suite needs
  `SAPFLOW_RECONNECT_GRACE: "10"` in its smoke environment to match the
  app's 10 s grace. Both passed this way on 2026-10-22. The SRT suite already uses the
  stream id form of section 2.5. The gates run the macOS binary; CI's smoke
  run is the only Linux evidence for ingest until the dry run.
- [ ] Build the image from `<commit>` with the repository's `Dockerfile`
  (`--locked`, pinned toolchain) for the media host's platform; tag it
  `sapflow:<commit>`; record the image id and `sapflow --version`.

### 2.2 The existing container, beside mediamtx

The sapflow container already on the host becomes the trial's: same name,
new image and environment. It runs on the same internal network as v4v's
app, Caddy and mediamtx. The callback must be plain `http://` on the private network
(`SAPFLOW_CALLBACK_URL` refuses anything else), and in v4v's compose file the
app only `expose`s its port, so a host process could not reach it. If the
production layout differs, adapt, and keep the callback private.

```yaml
  sapflow:
    image: sapflow:<commit>
    restart: unless-stopped
    env_file: <sapflow-env-file>      # mode 600; section 2.3
    ports:
      - "<trial-rtmp-port>:1935"      # phase A; "1935:1935" from the switch
      - "8890:8890/udp"               # as the container has it now
    volumes:
      - <recordings-dir>:/recordings
    ulimits:
      nofile: { soft: 65536, hard: 65536 }
```

The HLS port (8888 inside the image) need not be published: Caddy reaches it
by service name. The existing container's loopback HLS port can stay.

Port 1935 moves between the two servers by recreating both containers, since
Docker fixes a container's ports when it is created. Prepare two copies of
the compose file in `<compose-dir>`, differing only in who publishes 1935:
`compose.mediamtx-rtmp.yml` (as today: mediamtx has 1935, sapflow
`<trial-rtmp-port>`) and `compose.sapflow-rtmp.yml` (sapflow has 1935,
mediamtx publishes no RTMP port). Each move is then one command:
`docker compose -f <file> up -d --no-deps mediamtx sapflow`. Port lists in override
files are appended, not replaced, so whole copies are simpler than
overrides.

- [ ] mediamtx's image and config stay as they are; only its published RTMP
  port moves. Record the image id and a checksum of its config so
  "untouched" is checkable later.
- [ ] Open the firewall for `<trial-rtmp-port>` (TCP) for phase A. 1935 and
  8890/udp are open already. Docker-published ports can bypass host
  firewall rules: check the listeners, not the rule set.
- [ ] Time both moves in phase B, and check after each that the right
  container holds 1935 (`docker port mediamtx`, `docker port sapflow`).
- [ ] `<recordings-dir>` is writable by the image's non-root user (uid 65532),
  mediamtx can still write to it, and v4v's read-only mount can read what
  sapflow writes (the dry run proves the last).
- [ ] Free disk: sapflow records only above `SAPFLOW_RECORD_MIN_FREE_GB`
  (2 by default) and stops if it falls below. At v4v's top recommended
  bitrate (4,000 kbit/s) a show is about 2 GB an hour. Leave room for the
  show and the test signals, with margin.
- [ ] Start it. The log must show `RTMP listening`, `HLS listening`,
  `recording shows`, and an `open-file limit` line at info level. An
  `open-file limit is low` warning is a no-go (systemd's 1,024 would have
  failed at 250 viewers). If you run it under systemd instead, set
  `LimitNOFILE` the same way.

### 2.3 The sapflow environment, mapped from mediamtx

The reference mediamtx config is `docs/examples/mediamtx.yml` in v4v. The
production config is not in either repository: read it and compare line by
line against this table before the dry run. The settings below, with
placeholders, are in [examples/trial-sapflow.env](examples/trial-sapflow.env):
copy it to the host as `<sapflow-env-file>` and fill it in there.

| mediamtx setting or hook | sapflow | Trial value |
|--------------------------|---------|-------------|
| `authInternalUsers` publisher user and password, publish only on `~^live/event-[a-z0-9-]+$` | `SAPFLOW_PUBLISH_USER`, `SAPFLOW_PUBLISH_PASS`; the path rule is built in | The same `<publisher>` and `<publisher-password>` as mediamtx, so the stream key is valid on both servers. Letters and digits only: the SRT stream id splits on `:` and does not decode, and the RTMP key is split on `&` and `?` |
| Anyone may read | HLS is always public | n/a |
| `rtmp: yes`, `rtmpAddress` | `SAPFLOW_RTMP_ADDR` (default `0.0.0.0:1935`) | default inside the container; published as `<trial-rtmp-port>` in phase A and as 1935 from the switch. Plain RTMP only: sapflow has no RTMPS |
| `srt: no` | `SAPFLOW_SRT_ADDR` (default `0.0.0.0:8890`) | default inside the container; published as 8890, as now. New capability |
| `hls`, `hlsAddress` | `SAPFLOW_HTTP_ADDR` (default `0.0.0.0:8888`) | default; not published |
| `hlsVariant: lowLatency`, 1 s segments, 200 ms parts | built in, not settable | Segments follow the encoder's keyframes: keep 1 s keyframes |
| `hlsSegmentCount: 24` | `SAPFLOW_SEGMENT_COUNT` | 24 (default; minimum 7) |
| `hlsAlwaysRemux: yes` | built in: served from the first segment after publish | n/a |
| `hlsAllowOrigins: ['*']` | built in: `Access-Control-Allow-Origin: *` | Caddy must not add its own |
| `record: yes`, `recordFormat: fmp4`, `recordPath: /recordings/%path/%s` | `SAPFLOW_RECORDINGS=/recordings` | `<dir>/live/<eventId>/<unix-start>.mp4`, fragmented MP4: the same layout, so the same directory serves v4v |
| `recordSegmentDuration: 12h`, `recordPartDuration` | no equivalent | one file per show |
| `recordDeleteAfter: 7d` | `SAPFLOW_RECORD_DELETE_AFTER` | `7d` (default). sapflow's expiry deletes any `<digits>.mp4` under `live/*/` older than that, including mediamtx's: the same policy, so nothing kept changes |
| A programme path with `record: no` (v4v's recorded programmes) | `SAPFLOW_RECORD_SKIP` (comma-separated, trailing `*` matches a prefix) | the programme path, if production has one. The trial show is a live performer, not a programme |
| `runOnReady`, `runOnNotReady`, `runOnRecordSegmentComplete` with `token=<callback-secret>` and `$(basename $MTX_PATH)` | `SAPFLOW_CALLBACK_URL`, `SAPFLOW_CALLBACK_SECRET` | `http://<app>:<app-port>/api/public/live-events/{eventId}/stream-health` and `<callback-secret>`; `{eventId}` is the last path segment, the token is appended |
| (none) | `SAPFLOW_HEALTH_INTERVAL` | 30 (default): `health` events v4v reads the bitrate from |
| v4v's `PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS` | `SAPFLOW_RECONNECT_GRACE` | 90, set explicitly on both sides: they must be equal |
| `webrtc`, `rtsp`, the API and metrics | not offered | Check production does not use them. Any other `runOn*` hook (a simulcast relay, for one) cannot be carried over |
| (none) | `SAPFLOW_SRT_PASSPHRASE`, `SAPFLOW_SRT_ENCRYPTION` | a long random passphrase (10 to 79 bytes) and `optional`: encoders without it still publish, so the URL v4v hands out need not change; one that sets it must match, at the default key length 16 (`docs/own-origin-encryption.md`). Give it to an encoder by hand, not in v4v's template |
| (none) | `SAPFLOW_HTTP_IMPL` | unset (hyper), as decided above |
| (none) | `SAPFLOW_OPEN_PUBLISH`, `SAPFLOW_SRT_IMPL` | never set (srt-rs is gone; any `SAPFLOW_SRT_IMPL` other than `own` stops startup) |
| `logLevel: info` | `RUST_LOG` | `info` (default) |

If production's existing publisher password has any other character, rotate
it on both servers (mediamtx's `authInternalUsers` and v4v's key template)
before the dry run: the whole rollback depends on one key being valid on
both.

Other limits stay at their defaults: 16 publishers per protocol, 40 Mbit/s
per publisher.

### 2.4 Caddy

The repository has only the turnkey Caddyfile (`reverse_proxy app:<app-port>`);
the production HLS block is not in it. Check how `/live-hls/` reaches
mediamtx today, then:

```
(sapflow_hls) {
	reverse_proxy <sapflow>:8888 {
		transport http {
			keepalive_idle_conns_per_host 1024
		}
	}
}

# the existing site block: keep what is there, add one handle for the dry run
	handle_path /live-hls-trial/* {
		import sapflow_hls
	}
# the existing /live-hls/ handle stays pointed at mediamtx until the switch;
# the switch replaces its body with `import sapflow_hls`
```

- [ ] The pool value (1024) and the directive name come from
  [benchmarks.md](benchmarks.md); the lines the benchmark passed are not
  recorded. Run `caddy validate` on the real file with the Caddy version in
  use (2.11.7 was tested).
- [ ] Add no CORS header for these paths: sapflow sets
  `Access-Control-Allow-Origin: *`, and two copies break clients.
- [ ] HTTP/2 comes with TLS on the public site. Check, never assume:
  `curl --http2 -sI https://<domain>/live-hls-trial/live/<eventId>/index.m3u8`
  reports `HTTP/2` (a 404 with no stream is the right answer), with one
  `access-control-allow-origin` header.
- [ ] For the show only, turn on a JSON access log for the HLS path
  (criterion 7). It records viewer IPs, which sapflow itself does not log:
  keep it short-lived and delete it after section 5.
- [ ] Never give Safari sapflow's port or an `http://` URL: 10.8 s against
  3.0 to 3.2 s.

### 2.5 v4v environment

v4v needs no code change. It reaches the media server through these
templates, and the callback. They take effect when the app restarts, and a
show's Studio session stores the URLs they produced at the moment the
session is prepared (see section 3, step 8). Back up `<env-file>` first and
never print it.

| Variable | Now (reference form) | During the trial | Why |
|----------|----------------------|------------------|-----|
| `PULSEWIRE_LIVE_STREAM_ID_TEMPLATE` | `{eventId}` | unchanged | Both servers name the callback's event from the path's last segment, and sapflow needs `live/event-[a-z0-9-]+`. `.env.example`'s default `{eventId}-live` would not work; confirm production has the first form |
| `PULSEWIRE_LIVE_RTMP_SERVER_URL_TEMPLATE` | `rtmp://<media-host>:1935/live` | unchanged | Whoever holds 1935 receives it. Production's is plain `rtmp://` (read 2026-10-22) |
| `PULSEWIRE_LIVE_STREAM_KEY_TEMPLATE` | `{streamId}?user=<publisher>&pass=<publisher-password>` | unchanged | Same credentials on both servers |
| `PULSEWIRE_LIVE_SRT_URL_TEMPLATE` | set, to the existing sapflow container on 8890 | `srt://<media-host>:8890?streamid=publish:live/{streamId}:<publisher>:<publisher-password>`; check the current value has this form | The `live/` prefix and the credentials are both required. Studio appends `&latency=2000000` for OBS; Larix has its own latency box (2000 ms). No `passphrase=`: it is optional |
| `PULSEWIRE_LIVE_HLS_URL_TEMPLATE` | `https://<domain>/live-hls/live/{streamId}/index.m3u8` | unchanged | Only Caddy's upstream behind that path changes |
| `PULSEWIRE_LIVE_PLAYBACK_PROBE_URL_TEMPLATE` | `http://<mediamtx>:<mediamtx-hls-port>/live/{streamId}/index.m3u8` | `http://<sapflow>:8888/live/{streamId}/index.m3u8` | The app fetches this to open a stage and to feed its source stats every 5 s. Left on mediamtx it gets a 404, and the stage stays at `receiving` |
| `PULSEWIRE_LIVE_CALLBACK_SECRET` | `<callback-secret>` | unchanged | Equal to `SAPFLOW_CALLBACK_SECRET` |
| `PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS` | 90 | `90`, explicit | Equal to `SAPFLOW_RECONNECT_GRACE` |
| `PULSEWIRE_LIVE_RECORDING_DIR` | v4v's mount of the recordings directory | unchanged | sapflow writes the same layout into the same directory |
| `PULSEWIRE_LIVE_INGEST_PROVIDER`, `_PROTOCOL` | labels (`.env.example` defaults `_PROTOCOL` to `rtmps`) | `_PROTOCOL=rtmp` to match the RTMP template (or `srt`); `_PROVIDER` unchanged | Stored with each session, offered as a per-show Studio field and shown in the setup view. The server code does not branch on either; the client's display uses were not all traced, so set `_PROTOCOL` to the scheme the template really has. The templates are what encoders use |

Studio's test signal dials whatever host the RTMP or SRT template names, so
once 1935 has moved it tests sapflow.

### 2.6 Dry run

Two phases. Do not go to the night until both pass and section 4's blanks are
filled in.

**Phase A: sapflow on `<trial-rtmp-port>` and 8890, v4v untouched.** No v4v change, so
production is unaffected. Leave `SAPFLOW_CALLBACK_URL` unset here (events are
only logged; a callback for an event v4v does not know would only be
refused); the callback path is tested in phase B.

- [ ] Publish a test signal from a different machine and network to
  `<trial-rtmp-port>` and to 8890 over SRT, with and without the passphrase,
  on a path such as
  `live/event-dry-run`, with the real `<publisher>` credentials. Play it at
  `https://<domain>/live-hls-trial/live/event-dry-run/index.m3u8` in Safari on
  an iPhone and a Mac. Confirm `curl --http2` and the single CORS header.
- [ ] Play it from a web client on another origin (hls.js on a different
  origin, or a web Nostr client). No such client has tried sapflow.
- [ ] A real phone, the performer's encoder, from the venue's network (if it
  can be reached) and from cellular: our own app (if it is the candidate),
  Larix and OBS over RTMP and, if offered, SRT, with 1 s
  keyframes, 2000 ms SRT latency, CBR 2,500 to 4,000 kbit/s. Many cold
  starts, keeping sapflow's log and the phone's for any that fail (Larix's
  first SRT attempt sometimes fails and a retry works; unexplained). Confirm
  the venue network passes UDP (on the night RTMP is on 1935, as now).
- [ ] With the phone locked or the app backgrounded: record what happens. It
  is untested (`readiness.md` section 3); the performer is told not to lock it.
- [ ] Drop each encoder (network off, then on) and return within 90 s: same
  playlist and chunk names, a discontinuity, Safari carries on. Once, return
  after 90 s: the stream ends and a new one starts. Our own app is the
  performer's encoder only if it resumes every drop here by itself, with
  nobody touching the phone.
- [ ] Delay against mediamtx through the same Caddy: the same ffmpeg stream to
  both servers, Safari on `/live-hls-trial/` and on the current `/live-hls/`,
  a clock in frame. This is the one comparison still missing (the only
  side-by-side was over HTTP/1.1, sapflow about 4 s further back).
- [ ] A soak as long as the planned show, with the planned drops and a few
  viewers, sampling `docker stats`, descriptors and the recording. No run
  longer than minutes exists. Judge criteria 1, 6 and 7 against it.
- [ ] The recording: one file, `ffprobe` length against on-air time,
  plays and seeks in Chrome and Safari.
- [ ] If the wired Linux machine is free, run `scripts/bench-remote.sh` at
  250 and 750 viewers with `BENCH_URL` and `BENCH_ARGS='--http2 --ca <root.crt>'`
  against the real Caddy. Not a gate for this trial; if it runs, replace the
  audience cap in section 6 with its result.

**Phase B: the switch and the way back, once, in a quiet window.** No stage
live or scheduled. This is the one place the real v4v is changed, and it is
restored at the end.

- [ ] Back up `<env-file>` and `<caddyfile>` under dated names. Apply
  section 2.5, restart the app by the normal deploy procedure, move 1935 to
  sapflow (section 2.2), and apply the Caddy switch and reload.
- [ ] Prepare a draft rehearsal event in Studio. Run the Studio test signal
  over RTMP (and SRT, if offered): the stage opens by the app's own probe,
  the `publish` and `health` callbacks appear in the integration feed, and
  Safari with `?stats=1` shows a latency in the band.
  `npm run live:monitor -- --base https://<domain> --all --once` exits 0.
- [ ] Stop the test signal and wait for sapflow's log to say
  `no reconnect within the grace period; stream ended`, then for the
  `recordSegmentComplete`. A publisher that connects inside the grace takes
  over the same stream and recording, so a show begun straight after the test
  signal would share its file, and v4v ignores a segment that began during a
  test run.
- [ ] Publish from the real phone to the rehearsal stage, drop and resume
  inside 90 s: Studio reads `reconnecting`, then `live`; Safari's `?stats=1`
  returns to the band. End the show; the recording arrives under "Publish the
  replay" and plays.
- [ ] **Rehearse the rollback (section 4) with the phone publishing and a
  Safari viewer watching.** Time each step into the blanks in section 4. Note
  what Studio shows afterwards: does the stage stay `live` when mediamtx's
  `publish` arrives with the app's probe still pointing at sapflow? Nothing in
  the repositories answers that.
- [ ] Restore `<env-file>` and `<caddyfile>`, restart the app, reload Caddy,
  check 1935 is mediamtx's again, and confirm a mediamtx stage works with
  the original templates (a Studio test signal). Production now matches its
  state before the window.

## 3. The switch on the night

Roles: one person holds each of the watches in the table below, and the
rollback decision belongs to the person named in the decisions above. On
the night nobody edits anything not listed here.

Before the window:

- [ ] No other stage is live or scheduled until the show ends and section 5's
  restore is done. Tell the performer the protocol and the encoder, and that
  a rollback needs nothing from them unless the encoder gives up
  reconnecting (section 4).
- [ ] The performer's encoder holds the RTMP connection Studio shows
  (`<media-host>:1935`), the same for both servers. If it is SRT, it also
  holds the RTMP connection: mediamtx has SRT off, so a rollback means RTMP.
  If the encoder is our own app, Larix holds the same settings. The key is
  `<eventId>?user=<publisher>&pass=<publisher-password>` while
  `PULSEWIRE_LIVE_STREAM_KEY_TEMPLATE` has no `{streamKey}` in it (the form
  in section 2.5): it has no random part, so it can be saved before step 8.
  If production's template does use `{streamKey}`, the key is random and
  changes at step 8, so save the settings after it.
- [ ] Disk free for the show; sapflow and mediamtx containers up; the image
  and config ids match section 2.2; `docker inspect` shows `RestartCount` 0.

The switch:

1. [ ] Back up `<env-file>` and `<caddyfile>` under dated names.
2. [ ] Apply section 2.5 to `<env-file>`; restart the app by the normal deploy
   procedure; confirm it is up and a mediamtx-era stage page still loads.
3. [ ] Move 1935 to sapflow:
   `docker compose -f compose.sapflow-rtmp.yml up -d --no-deps mediamtx sapflow`, then
   `docker port sapflow` shows 1935 and `docker port mediamtx` does not. The
   log shows `RTMP listening` and `SRT listening ... encryption="optional"`.
4. [ ] In `<caddyfile>`, replace the body of the `/live-hls/` handle with
   `import sapflow_hls`. `caddy validate`, then reload (graceful).
5. [ ] `curl --http2 -sI https://<domain>/live-hls/live/<eventId>/index.m3u8`:
   `HTTP/2`, a 404 (no stream yet), one CORS header.
6. [ ] Start the watches (table below), the `live:monitor` with
   `--all --interval 15 --heartbeat 30`, the Caddy access log for the path,
   and the `docker stats` sampler.
7. [ ] Start `docker logs -f sapflow` and keep it where the watcher can see it.
8. [ ] In Studio, prepare the show's session now. Its Prepare button always
   rotates secrets (the client sends `rotateSecrets: true` and the server's
   default is the same); that is harmless with the key template of
   section 2.5, because the key and stream id do not change, only the stored
   URLs and the webhook token (unused: the shared secret is what counts). It
   also resets the stage to standby and clears its playback state, and v4v
   refuses it while the stage is receiving media unless forced, so it is a
   before-the-show step only, never part of a rollback. Confirm Studio shows
   `<media-host>:1935` and the SRT URL of section 2.5. Copy them to the
   performer.
9. [ ] Studio test signal over RTMP until the stage is `live`; Safari
   `?stats=1` shows a latency in the band; the phone-and-clock measurement once.
   Stop it, then wait for `no reconnect within the grace period; stream ended`
   and the test's `recordSegmentComplete` before the performer connects
   (section 2.6, phase B).
10. [ ] The performer connects at soundcheck, not at doors. If it fails: one
   retry, then the other protocol once, then (if the encoder is our app)
   Larix; the known Larix SRT first-attempt failure may need up to three
   tries. Keep the failing attempt's log lines.

What to watch:

| Watch | Where | Bad |
|-------|-------|-----|
| sapflow | `docker logs -f sapflow` | `publish refused`, `callback refused`, `callback failed`, `callback timed out`, `SRT publisher idle`, `no reconnect within the grace period`, `recording fell behind`, `recordings disk nearly full`, `sapflow stopped`, any restart |
| Health callback | v4v integration feed | `health` missing for over 60 s while sapflow shows the stream live; `publish` or `unpublish` in sapflow's log with no entry |
| v4v status | Studio stream status, source stats, `live:monitor` | `receiving` that does not become `live`; `reconnecting` past 90 s; source stats showing long segments, timing jumps or a stopped playlist; `down` |
| Viewer | Safari `?stats=1` on an iPhone and a Mac, through `https://<domain>` only | stalls, latency over the band, or the engine not on HTTP/2 (10 s and up) |
| Caddy | error log, access log on `/live-hls/` | any 5xx, `dial tcp ... connection refused` |
| Host | `docker stats`, `/proc/<pid>/fd`, free disk | criterion 6 exceeded, disk under the recording margin |

**Go or no-go before the audience arrives.** All of:

- [ ] Ingest stable on the performer's real encoder and link, 1 s segments in
  source stats, no long segments, bitrate steady, `health` arriving.
- [ ] The stage `live`, the `publish` callback seen, the recording file growing
  and readable from v4v's side.
- [ ] Safari delay within the band over HTTP/2 (clock measurement), no stall.
- [ ] One deliberate drop and return inside 90 s: `reconnecting` then `live`,
  Safari back in the band.
- [ ] Caddy clean, resources inside criterion 6 at the current viewer count.
- [ ] mediamtx up and its HLS port answering (a 404 is fine); the rollback
  command (section 4, step 2) typed into a shell on the host, not run; the
  decision owner and the channel are confirmed.

Any item failing is a no-go. A no-go before the audience is a pre-audience
rollback (section 4), which needs nothing from the performer but a
reconnect.

## 4. Rollback

Triggers. Any one, called by the decision owner, once the checkpoint has
passed; before it, any failed item above:

| # | Trigger |
|---|---------|
| R1 | sapflow exits or restarts (container restart, `sapflow stopped`) |
| R2 | The playlist through Caddy is a 404 or 503 for more than 15 s while the encoder shows connected and sapflow's log shows the stream live |
| R3 | A viewer-visible stall of more than 15 s outside an encoder drop, or three stalls of more than 5 s in ten minutes |
| R4 | The encoder cannot reconnect: a drop that does not resume while the encoder is up and sapflow keeps refusing it (`publish refused`), after one retry |
| R5 | Resource runaway: RSS climbing steadily past 200 MB, CPU pinned above 50% of a core for two minutes, `Too many open files` or `open-file limit is low` in the log, or 5xx on more than 1% of `/live-hls/` requests for a minute |

Chosen, not measured, like the thresholds in section 1. These are deliberately
harder failures than the success criteria: a rollback during a show ends the
stream for the audience. Callback failures, a recording fault and a Safari
latency above the band are not triggers once the stage is live: they go in
the review. The recording has a backup: the performer's encoder records
locally too, if it can.

Steps during a show, in this order so viewers' playlist moves only once
mediamtx is serving:

1. [ ] Say "rolling back" on the performer channel: the encoder will drop
   and should reconnect by itself; if it gives up, restart it with the same
   settings. An SRT encoder switches to its RTMP connection.
2. [ ] Move 1935 back to mediamtx:
   `docker compose -f compose.mediamtx-rtmp.yml up -d --no-deps mediamtx sapflow`, and
   `docker port mediamtx` shows 1935. This restarts sapflow, which ends its
   held stream. The encoder's next attempt reaches mediamtx, whose own hook
   sends `publish` for the same event path; v4v accepts it (same secret,
   same path).
3. [ ] Check mediamtx's manifest on its internal address:
   `curl -s http://<mediamtx>:<mediamtx-hls-port>/live/<eventId>/index.m3u8`
   shows parts. (mediamtx's own cookie redirect, if shown, is normal.)
4. [ ] In `<caddyfile>`, put the `/live-hls/` handle back to mediamtx (the
   dated backup), `caddy validate`, reload.
5. [ ] Check Safari and a second client on the stage: playing, `live` in Studio.
   Studio's source stats keep polling the probe, which still points at
   sapflow, so they will show a stopped playlist for the rest of the night:
   expected, not a second fault.
6. [ ] sapflow's recording stopped at the restart and is readable up to
   there, but it sent no `recordSegmentComplete` for it. v4v imports
   mediamtx's recording as the replay; if sapflow's part is wanted, upload it
   by hand. Both files are kept: sapflow's under `<recordings-dir>`,
   mediamtx's in the same tree.
7. [ ] After the show: restore `<env-file>` from its backup and restart the
   app (never during the show), then confirm a mediamtx stage works with the
   original templates.

If steps 1 to 4 outlast v4v's grace (90 s) the gig leaves `live` and the
NIP-53 activity changes, which asks the host to re-sign. The app's probe
still points at sapflow, so the stage will not reopen by itself: use Studio's
"Set live" by hand (the supported last resort, logged as the operator's claim).
If `playbackReady` does survive mediamtx's `publish`, none of that is needed;
phase B found out.

Before the audience (a no-go), the order is free: move 1935 back, restore
`<caddyfile>`, then `<env-file>` and the app restart; the performer only
reconnects.

What people see during a mid-show rollback:

| Who | Experience |
|-----|------------|
| Performer | Nothing, if the encoder reconnects by itself; otherwise restarts it with the same settings (RTMP, if it was on SRT) |
| Viewers | The picture freezes at the drop (the held stream stops growing), then the playlist changes origin when Caddy is reloaded. hls.js clients (v4v's player, Chrome) recover by jumping to live; Safari's native player is expected to stall and may need a page reload. Chunk names and media sequence both change |
| Stage | `reconnecting`, then `live`, if inside 90 s |
| Replay | Possibly only the longer of two recordings |

Rehearsal result (fill in during phase B):

| Measure | Seconds |
|---------|---------|
| Decision to 1935 back on mediamtx, the encoder reconnected and mediamtx's `publish` seen by v4v | ____ |
| Caddy restored and reloaded after that | ____ |
| Stage `live` and playing again in Safari | ____ |
| Longest viewer-visible gap | ____ |
| Inside the 90 s grace? | yes / no |
| Stage stayed `live` with the probe on sapflow? | yes / no |
| Safari recovered without a reload? | yes / no |

This plan moves 1935 rather than giving the performer a second saved
connection to mediamtx on its own port (the earlier draft): the URL never
changes and the performer has nothing to switch, at the cost of a rollback
that restarts sapflow and depends on a command on the host. If phase B shows
the move is slow, or the encoder does not come back by itself, reconsider.

## 5. After the show

Collect before anything is restarted or pruned:

- [ ] sapflow's log for the whole window (`docker logs --since`), and
  `docker inspect` for `RestartCount` and the image id.
- [ ] The recording files, `ffprobe` output for each, and whether Studio
  offers the replay.
- [ ] v4v's integration feed for the window, against sapflow's `stream event`
  lines (criterion 5).
- [ ] Caddy's error log and the access log for `/live-hls/` (status counts,
  peak concurrent clients), then delete the access log.
- [ ] The `docker stats` file and descriptor samples; the Safari `?stats=1`
  readings at the start, after each drop and at the end (latency, stalls,
  stalled time); the clock measurement.
- [ ] Any failed encoder attempt: the phone's log and sapflow's lines.
- [ ] What the performer noticed, in their words.
- [ ] Restore `<env-file>` and `<caddyfile>` and remove the trial path from
  Caddy, unless a second show is already decided. Confirm a mediamtx stage.

Fill in the seven criteria of section 1 with the measured value against each
threshold. The questions the trial has to answer before a second show or a
full switch:

1. Did every criterion hold, and which thresholds were wrong (chosen numbers
   in criteria 2, 3 and 6 are the likeliest)?
2. How does delay compare with mediamtx through the same Caddy?
3. What do CPU, memory and descriptors look like on Linux over a whole show?
4. Was the Caddy pool (1024) enough? Any 502, and at what peak audience?
5. Did Larix's first-attempt SRT failure recur, and does a capture explain it?
6. Did Safari return to the band after each drop with v4v's player fix?
7. Did the stage stay `live` and keep its playback state across a reconnect
   without the probe (answered by phase B and, if it fired, the rollback)?
8. Did the recording import byte for byte as the replay, and was the length
   right?
9. Was every callback delivered, and did any need Studio's "Set live" or a
   manual replay upload?
10. What would a full switch still need: the wired 250 and 750 viewer run
    behind Caddy, a longer soak, the other encoders (locked phone, Android,
    vMix), SRT handshake version 4?

The outcome goes in [GOALS.md](../GOALS.md): widen, repeat or stop.

## 6. Known risks carried into the trial

| Risk | Mitigation on the night |
|------|-------------------------|
| Larix's first SRT attempt sometimes fails and a retry works (unexplained) | RTMP first. If SRT: connect at soundcheck, allow up to three tries, keep auto-reconnect on, keep the logs |
| Our own app: reconnect is new and unmerged as of 2026-10-22; one short Pixel recovery test, no show-length run | Used only if it is on `main` and passes phase A's drops; Larix installed with the same settings as the fallback |
| A rollback restarts sapflow and needs a command on the host | The command prepared and typed in before doors (section 3); timed in phase B; sapflow stops on SIGTERM at once |
| mediamtx has SRT off: an SRT performer has no SRT fallback | RTMP first; an SRT encoder also holds the RTMP connection |
| No wired Linux load test; the largest all-viewers-playing measurement through Caddy over HTTP/2 is 50 viewers (250 reached 0 failed requests with the pool at 1024, but on a shared Mac behind Docker Desktop, link-bound) | Planned peak 50 concurrent, closed beta only, the pool at 1024, watches for 5xx and CPU, R5 as the way out. The 750-viewer numbers are single-machine macOS |
| Nothing has run on Linux except CI's smoke test and the image serving a stream | Phase A's soak and phase B are the first Linux evidence; criterion 6 bounds are chosen, not measured |
| No show-length run | The soak in phase A; RSS growth in criterion 6 |
| Safari is 10.8 s behind over HTTP/1.1 | Safari only through `https://<domain>`; `curl --http2` at every Caddy change; the clock measurement at the checkpoint |
| Safari drift after drops | v4v's catch-up fix is merged and was checked in real Safari; the deliberate drop at the checkpoint and criterion 3 recheck it |
| The switch is deployment-wide, not per show: the URL templates and the probe are one set, and Caddy's `/live-hls/` has one upstream. A session prepared earlier keeps its old (mediamtx) URLs, but its viewers would be sent to sapflow | No other stage live or scheduled in the window. A narrower variant exists in v4v (a show's RTMP, SRT and playback URLs are editable per show in Studio, and Caddy can route by event id), but the probe template is still global, so it is not used here |
| Rollback mid-show is not free: it ends the stream for a moment, may need a Safari reload, and can leave a partial replay | Rehearsed with times in section 4; triggers are hard failures only; a pre-audience no-go costs nothing |
| A test signal and the show sharing one recording (a publisher inside the grace takes over the held stream) | Wait for `stream ended` after the test signal, before the performer connects |
| Callbacks are not durable (four tries over about 4 s, then dropped) | A lost `publish`: Studio "Set live" by hand. A lost `recordSegmentComplete`: the file is on disk; upload it as the replay by hand |
| Plain RTMP, credentials in the clear (as in v4v's reference mediamtx config); SRT's stream id carries them in clear too | Production uses `rtmp://` (read 2026-10-22); use a long random publisher password. The SRT passphrase is optional, so it protects the media of encoders that use it, not the credentials |
| Features sapflow lacks: WebRTC monitor, hooks that run commands (a simulcast relay), the control API | Read the production mediamtx config for each before the dry run; do not trial a show that uses one |
| Own protocol code, one maintainer, no upstream | Fuzzing, the loss comparison against libsrt, and mediamtx kept ready with a rehearsed way back |
| The recordings directory: non-root writer, shared with mediamtx, expiry applies to both | Section 2.2 checks; the dry run proves v4v can read the file |
| Clocks: v4v's lag figures assume its clock and the media server's agree | Same host; if not, ignore the lag line and use the clock measurement |
