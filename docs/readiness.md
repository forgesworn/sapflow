# Readiness to replace mediamtx

## Current readiness, 28 October 2026

Reviewed repository evidence at `3c7e1aa`. The own HTTP viewer loop,
Hyper/Axum removal, private Tor publishing and receiver TV player are merged.
[Current-main CI passed](https://github.com/forgesworn/sapflow/actions/runs/40581289226).
This is a documentation review, not a fresh audit of deployed settings, a
device test or a production-switch decision.

`GOALS.md` still requires favourable measurements and an explicit maintainer
decision before replacing mediamtx for the V4V stage. Existing TV deployment
and short player tests do not complete that gate. No accepted real V4V show
or wired Linux/Caddy capacity result is recorded here.

| Area | Evidence already recorded | Remaining acceptance |
|---|---|---|
| Origin ingest and recording | RTMP/SRT encoder interoperability, V4V ingest/replay gates, reconnects and a two-hour trial-preparation run | Re-run the ingest gates and rollback on the selected current build; complete a real show and record the maintainer's decision |
| HTTP implementation and resources | The own viewer loop is the only backend; Hyper/Axum are removed; recorder and RTMP publication admission exist. PR #30 (`348bdf8`) adds final-owner resident presentation/fragment admission, close-time cache release and absolute response deadlines, with generated correctness checks | Matched nominal/adverse resource measurements and Linux audience qualification in the [retained-media contract](retained-media-contract.md); logical charges do not establish RSS or kernel bounds |
| Performance | Dated macOS comparisons in [benchmarks](benchmarks.md); evidence and Linux/slow-viewer follow-ups remain in open PRs #14/#15 | Refresh those PRs and their failed native checks; qualify wired Linux/Caddy/HTTP2 at 250/750 full viewers, adverse load and sustained operation on the selected build |
| SRT security and interoperability | Own encrypted sender/receiver, bidirectional independent checks and finite loss/rotation cases; a five-hour fuzz campaign on the SRT, RTMP, HTTP and ingest parsers at `19a2124` ([fuzz receipts](../fuzz/README.md)) | Complete the encrypted matrix; repeat long fuzzing on the selected trial build if parser code changes; retain unsupported-version and unauthenticated-CTR limits |
| TV player and private publishing | About 13.5-minute generated-media Chrome loops, signed peer verification, Tor-only publishing and bounded retries | Actual Safari/iPhone, long public/private runs, current-revision reconnect behaviour, measured latency, separate-network peers and sustained higher rates |
| Native mobile recording and apps | Direct native bindings, current SDK/app CI and scoped capture/recording controls | Native AAC presentation/EOF, durable recovery and aggregate two-second loss; complete physical shows, V4V journeys and signed installed releases |

Evidence: [trial preparation and rollback](trial-plan.md),
[TV player qualification](tv-player-qualification.md),
[private publishing qualification](private-tv-qualification.md),
[mobile delivery](mobile-delivery-status.md) and
[recording recovery](recording-recovery.md). Tasks, dependencies and owners are
in the [delivery plan](delivery-plan.md).

Before a V4V trial, verify the actual host's configuration, credentials,
recording volume, file limits, destination/encryption policy and viewer recovery.
Keep mediamtx and the measured rollback available. A previously requested
credential rotation remains unverified by this repository-only review. Do not
infer permission to change a live host or publish packages from this assessment.

## Historical assessment, 21 October 2026

The sections below retain the assessment against `985de00` and its subsequent
trial-preparation annotations. Their statements about unmerged V4V work,
macOS-only measurements, minutes-long runs, missing control APIs and nightly
target counts belong to those revisions. Use the current review above for the
remaining queue; use the dated records for the exact scope of each observation.

### 1. Recommendation (the maintainer's to make)

Ingest, packaging, recording, callbacks and reconnects work with real encoders
and v4v's gates, at about a third of mediamtx's CPU and memory on one machine.
That supports a staged trial on one non-critical show, with mediamtx left
running and a rehearsed way back. It does not yet support a full switch:
no benchmark, profile or load test has run on Linux, behind Caddy, over a
wired link at 250 and 750 viewers, which is the shape of the deployment, and
the v4v player fix for Safari drift is not merged (section 3).

### 2. Proven

| Area | What was shown | Where |
|------|----------------|-------|
| Ingest, real encoders | RTMP: ffmpeg, OBS, Larix (iPhone). SRT on our own receiver (the default): ffmpeg, `srt-live-transmit`, OBS, Larix, with and without a passphrase. All used handshake version 5. | `docs/interop.md` |
| LL-HLS | Plays in hls.js (Chrome), Safari on macOS, Safari on a physical iPhone (72 s, no stalls) and the iOS simulator. Every rendered playlist is checked against the Apple rules by tests, a fuzz target and a live checker. | `GOALS.md` M1; `docs/reference.md`; `docs/interop.md` "Reproducing irregular encoder timing" |
| Recording | One fMP4 file per show, plays and seeks in Chrome; v4v imports sapflow's own file, byte for byte, as the replay. | `GOALS.md` M3 |
| Callbacks, v4v gates | `publish`, `unpublish`, `recordSegmentComplete`, `health`. v4v's ingest gate passed over RTMP and SRT on 2026-10-20; the suites are on a v4v branch not yet merged. | `GOALS.md` M1 to M3, M6; `docs/v4v-contract.md` |
| Resources against mediamtx 1.19.1 | One Mac, 750 simulated viewers: 26 to 30% of a core against 103 to 107%, peak memory 66 to 77 MB against 263 to 267 MB, median playlist times about half, no failed requests after the keep-alive and open-file fixes. A second Mac over Wi-Fi: the same ratio. Worst-case segment and part fan-out times at 750 viewers were sometimes worse than mediamtx's. | `docs/benchmarks.md`; `docs/profiling.md` |
| Hardening | Thirteen fuzz targets run 20 minutes each nightly on GitHub, no crash. Limits on connections, sizes, bitrate, logins and disk. Threat model T1 to T16. The last completed CI run on `main` passed. | `docs/threat-model.md`; `fuzz/README.md` |
| Encryption | SRT passphrase (AES-CTR): OBS and Larix publish with it, a wrong one is refused, both directions against libsrt, key rotation; an independent review found no defects. CTR does not authenticate media; the stream id, which carries the credential, stays in clear. | `GOALS.md` M6b; `docs/interop.md`; `docs/threat-model.md` "Out of scope" |
| Reconnect grace | A dropped encoder is held 90 s, v4v's own window. Larix over RTMP resumed seven times in six minutes; OBS restarted after 62 s resumed. hls.js crossed the discontinuities; Safari carried on (by eye). | `docs/interop.md` "Stopping and restarting"; `docs/reference.md` "Target duration and breaks" |
| Delay | Camera to Safari 3.0 and 3.2 s over HTTP/2 (a Node HTTP/2 front stood in for Caddy); 10.8 s over plain HTTP/1.1, where Safari ignores the low-latency position. The server adds nothing measurable beyond the SRT latency. | `docs/interop.md` "Delay through the server" |

### 3. Not yet proven

| Gap | What would prove it | Needs |
|-----|---------------------|-------|
| Linux | Every measurement is from macOS (`docs/profiling.md` Limits). Only CI (tests, smoke test and fuzzing on Ubuntu) and the container image serving a stream have run on Linux. Repeat the benchmark and a long run on the trial host's OS. | A Linux machine |
| 250 and 750 full viewers, wired, behind Caddy | `scripts/bench-remote.sh` with Caddy and HTTP/2 in front, mediamtx back to back; Caddy in Docker on the Mac did not hold back blocking reloads or parts, added about 2 ms, and changed no bytes (`docs/interop.md`, Behind Caddy); `sapflow-bench` now speaks HTTP/2 over TLS (`--http2 --ca`, `docs/benchmarks.md`); raise Caddy's upstream keepalive pool, which gave 502s at 250 viewers in Docker. | A second wired Linux machine |
| Delay behind real Caddy, and against mediamtx | Larix to Safari through Caddy with mediamtx beside it. The only side-by-side was over HTTP/1.1, where sapflow sat about 4 s further back (target duration 3 against 2). | Phone, Mac, a clock |
| Encryption under loss, and fuzzed longer | `scripts/srt-loss.sh` with a passphrase and key refresh against libsrt: every packet delivered at 2000 ms, less overhead than libsrt; at 200 ms more overhead than libsrt, unencrypted too, on a heavily loaded machine, so rerun quietly (`docs/interop.md`). `own_srt_caller` was added after the 2026-10-21 nightly, so it has had 60 s CI runs only; then hours per target. | The next nightly; manual runs |
| A show-length run | No recorded run lasts more than minutes. Publish for hours with drops, sampling memory, open descriptors and the recording. | A Linux machine |
| Larix's first SRT attempt | Sometimes accepted but sends no data, times out after 5 s, and a later attempt works (`docs/interop.md`). Repeat cold starts with a capture at both ends. Unexplained. | The phone, a capture |
| Other encoders | Larix with the phone locked; Larix on Android; OBS's Apple hardware encoder; vMix. | Devices; vMix access |
| SRT handshake version 4 | No encoder tested needed it. Find one that does and test it, or document it as unsupported. | An older encoder |
| Safari after drops | Safari ended about 10 s behind the camera after seven drops. The v4v player fix is merged (v4v #475) but not yet watched in real Safari; rewatch with `?stats=1` across drops. | A Safari or iPhone check |

### 4. Risks for a switch, and mitigations

- **Rollback.** Keep mediamtx's config and image ready, untouched, and switch
  back by restoring the templates and Caddy upstream (section 5). Do it
  between shows: a rollback during a show ends the stream, and has not been
  rehearsed.
- **No in-binary fallback.** srt-rs was removed (M6c), so the only fallback
  is mediamtx. v4v's gate branch is not merged. Run the trial on a named
  build and re-run the smoke test and both gates against it.
- **Larix first attempt.** Unexplained. Tell performers to retry, keep the
  encoder's auto-reconnect on, and keep logs from the trial.
- **Safari drift.** Each drop left Safari further behind; the v4v fix
  (#475) jumps back to live, untested in real Safari. Check it before the
  trial, or choose a show on a wired encoder.
- **One maintainer, our own protocol code.** Both ingest protocols are ours,
  with no upstream to lean on. The answers are fuzzing, the loss comparison
  against libsrt and the mediamtx fallback.
- **Features sapflow lacks.** RTMP, RTSP and WebRTC playback (v4v's optional
  WebRTC monitor), hooks that run commands (a simulcast relay) and the control
  API. v4v's reference config has WebRTC off and v4v says nothing in
  production depends on the API, but the production host's config is not in
  these repositories: check it first.
- **Open-file limit.** systemd's usual 1,024 would have failed at 250 viewers
  (`docs/benchmarks.md`). Set `LimitNOFILE`, or the container's `ulimit`, well
  above the viewer count and read the startup log for the warning.
- **Passphrase.** Start the trial without `SAPFLOW_SRT_PASSPHRASE`: v4v's
  reference mediamtx config sets none (it has SRT off), and Studio's SRT URL
  carries one only if the template does. Enabling it is a separate decision that changes the
  URL v4v hands out. `SAPFLOW_SRT_ENCRYPTION=optional` sets a passphrase
  without forcing it on encoders, so the URL can stay as it is.

### 5. What a switch changes

v4v needs no code change (`docs/v4v-contract.md`): it reaches the media server
only through URL templates and the callback. The gate branch changes only the
gate script and a helper that script alone uses. The templates are one set per
deployment, so a trial is a window on the whole stage, or a second stack.

- **Media host.** Run sapflow where mediamtx ran (RTMP 1935, SRT 8890/udp,
  HLS 8888 are the defaults for both) and point the Caddy path for HLS at it.
  The playlist path, `/live/{streamId}/index.m3u8`, is the same.
- **v4v environment, only where host, port or credentials differ:**
  `PULSEWIRE_LIVE_RTMP_SERVER_URL_TEMPLATE`, `PULSEWIRE_LIVE_SRT_URL_TEMPLATE`
  and `PULSEWIRE_LIVE_PLAYBACK_PROBE_URL_TEMPLATE`. The SRT template needs the
  credentials in the stream id:
  `srt://<host>:8890?streamid=publish:live/{streamId}:<user>:<pass>`.
- **sapflow settings that must match v4v:** `SAPFLOW_PUBLISH_USER` and
  `SAPFLOW_PUBLISH_PASS` (the stream key's user and password),
  `SAPFLOW_CALLBACK_URL`
  (`http://<app>:5181/api/public/live-events/{eventId}/stream-health`),
  `SAPFLOW_CALLBACK_SECRET` (`PULSEWIRE_LIVE_CALLBACK_SECRET`),
  `SAPFLOW_RECORDINGS` (the directory v4v mounts as
  `PULSEWIRE_LIVE_RECORDING_DIR`) and `SAPFLOW_RECONNECT_GRACE`
  (`PULSEWIRE_LIVE_RECONNECT_GRACE_SECONDS`). They replace mediamtx's
  `runOnReady`, `runOnNotReady` and `runOnRecordSegmentComplete` hooks
  (mediamtx 1.19 names; 1.21 calls the first two `runOnAvailable` and
  `runOnUnavailable`).

### 6. Staged rollout

1. Choose the build (a named commit of main). Run `cargo test --workspace`,
   `scripts/smoke.sh` and both v4v ingest gates against it.
2. Done on 2026-10-21: the v4v player fix is merged and was checked in
   Safari, and the 200 ms loss comparison was rerun on a quiet machine
   (level with libsrt). Run the wired Linux load test if a machine is free.
   The detailed steps for the night are in `docs/trial-plan.md`.
3. Prepare a Linux trial host: sapflow on its own ports beside mediamtx, the
   open-file limit raised, recordings on the volume v4v reads, Caddy with TLS
   and HTTP/2 (never point Safari at sapflow directly: 10.8 s against 3.0 to
   3.2 s). Play a stream from a web client on another origin; sapflow sends
   `Access-Control-Allow-Origin: *`, but no such client has tried it.
4. Rehearse: the Studio test signal, then OBS and Larix over RTMP and SRT with
   1 s keyframes; drop each encoder and return within 90 s; confirm the replay
   reaches Studio; then switch back to mediamtx once.
5. Trial: one non-critical show with someone watching the logs, the `health`
   callbacks and Safari through Caddy. Change the templates and the Caddy
   upstream before the show; leave mediamtx running.
6. Rollback, at any step: restore the previous templates and Caddy upstream.
   After the trial the maintainer decides whether to widen it, repeat it or
   stop, and the outcome goes in `GOALS.md`.
