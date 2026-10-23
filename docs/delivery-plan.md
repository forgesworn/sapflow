# Delivery plan

Written 2026-10-23, after the trial preparation. It sequences the open goals
in [GOALS.md](../GOALS.md#remaining-work-2026-10-23): what each task needs,
who carries it, how it is shown done, and what it unblocks. Detailed gates
stay in the documents each phase links to.

## Who carries what

| Owner | Area |
|-------|------|
| Server work | The server and protocol crates: `sapflow`, `sapflow-srt`, `sapflow-rtmp`, `sapflow-hls`, `sapflow-core`, `sapflow-client`'s muxer and queues |
| App work | The iOS and Android apps under `mobile/` and `sapflow-ffi`'s platform adapters |
| Maintainer | Credentials, production changes on the media host, store accounts, releases to crates.io, product decisions |

Two rules from 23 October:

- One session drives the test phone at a time. A second session launching
  apps on it ended two runs early and produced a false audio failure.
- App files are changed on the app branch only. Server work that needs an app
  change (logging, a new callback) hands the commits over rather than editing
  the same files on another branch.

## Phase 1: the broadcaster app holds a show

Goal: our app publishes for the length of a show, through network changes,
without reconnecting by itself or stopping capture.

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 1.1 | Fold `c322d21`, `75650fc`, `a4c721e`, `da9c86a` (branch `diag/srt-fail-reason`) into the app branch: failure reasons in logcat, the SRT caller's own close reason | App work | none | The app branch builds and its unit tests pass with them |
| 1.2 | Show-length run on Android: Wi-Fi off and on, mobile data, a 60 s outage, the screen locked once | Server work, phone reserved | 1.1, a free phone | One stream on the server throughout; every drop resumed inside the grace; no `SapflowSrt` or `SapflowCapture` failure except during the planned outages; one complete recording on the server and one on the phone |
| 1.3 | Act on whatever 1.2 logs. Known candidates: the 32-resend limit in the SRT sender (`RetryLimit`) during stalls, and the audio encoder failure seen once at 16:21 UTC | Server work for the SRT sender, app work for audio | 1.2 | 1.2 passes again after the fix |
| 1.4 | iOS: the same fixes and the same run | App work | 1.1 to 1.3 | As 1.2, on an iPhone |
| 1.5 | Release `sapflow-srt` 0.2.0 with the too-late drop (`SendStats` gained fields, a break for 0.1) | Maintainer publishes; server work prepares | 1.2 | The crate is on crates.io and docs.rs builds |

Unblocks: phase 3, and using our app rather than Larix for a show.

## Phase 2: the first show on sapflow

Goal: one real show served by sapflow, with mediamtx ready to take back
1935 in about 5 seconds. Steps and rollback: [trial-plan.md](trial-plan.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 2.1 | Load run on Linux behind Caddy at 250 and 750 full viewers (`scripts/bench-remote.sh`) | Server work | a wired second machine | Results replace the macOS-only figures in the trial plan's audience cap |
| 2.2 | Safari delay through Caddy over HTTP/2, against mediamtx on the same stream | Server work with the maintainer's iPhone | none | A measured figure for each server in the trial plan |
| 2.3 | Decide how viewers recover after a rollback: today v4v's player stays stalled until the page is reloaded | Maintainer decides; v4v change if wanted | none | Either a v4v change that reloads the source, or the reload written into the rollback steps |
| 2.4 | Change the publisher password on the media host | Maintainer | a quiet window | New password in mediamtx, sapflow and v4v's templates; both servers restarted; a test signal accepted |
| 2.5 | The show itself, Larix as the encoder | Maintainer runs; server work watches | 2.1 to 2.4 | The show completes on sapflow, or a rollback is executed and timed; the review is written into the trial plan |

Unblocks: switching production to sapflow by default.

## Phase 3: our app replaces Larix

Goal: creators can use our app for any v4v show. Milestones B3R to B5 in
[mobile-streaming-plan.md](mobile-streaming-plan.md); priorities in
[mobile-competitive-review.md](mobile-competitive-review.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 3.1 | RTMPS with certificate validation | App work; RTMP client in `sapflow-rtmp` is server work | phase 1 | Publishes to a trusted RTMPS endpoint; refuses an untrusted certificate |
| 3.2 | Bitrate that adapts to the link, driven by queue age and SRT feedback | App work with server work on the sender | phase 1 | Holds a stream on a link that cannot carry the starting bitrate, without dropping to reconnects |
| 3.3 | Audio-only shows that keep running with the screen locked and in the background | App work | phase 1 | An hour of audio-only with the screen locked, on both platforms |
| 3.4 | External and USB microphones; camera controls; pause, mute and standby; saved profiles | App work | phase 1 | Each feature's acceptance in the mobile plan |
| 3.5 | QR and link destination import (in testing on the app branch) | App work | none | Merged with its tests; no credentials in shareable links |
| 3.6 | Signed builds: Zapstore first, then the app stores, with free unlimited v4v streaming in every build | Maintainer for accounts and signing; app work for builds | 3.1 to 3.5 | Installable builds from each channel |
| 3.7 | Point v4v's encoder guidance at our app instead of Larix | Server work prepares the v4v change; maintainer merges (merging v4v deploys production) | 3.6, phase 2 | v4v's setup pages offer our app first |

## Phase 4: libraries for other developers

Goal: other people can build on our crates and SDK. Work queue LQ1 to LQ8 in
[library-quality-plan.md](library-quality-plan.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 4.1 | Swift and Kotlin SDK contract: ownership, threads, cancellation, errors, compatibility policy | App work | phase 1 | Documented, with examples that compile in CI |
| 4.2 | Installable packages: Swift Package Manager and Maven | App work; maintainer publishes | 4.1 | A fresh project installs and runs an example |
| 4.3 | Repeatable interoperability against OBS and FFmpeg, and a performance scorecard | Server work | none | Runs in CI or by one script, with results kept |
| 4.4 | A second real consumer of the libraries, besides our app | Server work or app work | 4.2 | Built only from the public interface |
| 4.5 | Decide on an OBS adapter, then scenes, mixing and a desktop studio | Maintainer | 4.3 | Written decisions with scope and acceptance |
| 4.6 | Publish the marketing page as the repository's homepage (draft exists as a private page) | Maintainer reviews; server work publishes to GitHub Pages | none | The page is live and linked from the repository |

## Phase 5: later

- RelaySwarm peer playback in the apps (B6), after phase 3.
- A second rendition and wider formats (B7).
- Server features left out until a show needs them: RTMPS ingest, HEVC and
  AV1, delta playlists, a metrics endpoint, a config file, release binaries
  ([comparison.md](comparison.md), "Gaps sapflow has today").

## Order

Phases 1 and 2 run side by side: phase 2 uses Larix, so it does not wait for
the app. Phase 3 starts when phase 1 passes. Phase 4's interoperability and
scorecard work (4.3) can start now; the SDK work waits for phase 1. Phase 5
waits for phase 3.
