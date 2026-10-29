# Delivery plan

Reviewed 2026-10-28 against `3c7e1aa`, superseding the task states written on
23 October. It sequences [the current goals](../GOALS.md#remaining-work) and
names the owner, dependencies and remaining acceptance for each task. Source,
CI, physical tests, deployed results and releases are separate evidence. This
review does not run a new device or host trial.

The own HTTP loop, Hyper/Axum removal and private TV workflow are merged.
Current-main CI passed. Detailed historical receipts remain in their linked
records; they do not make every later revision qualified.

## Next work

The full remaining-work inventory is in [GOALS.md](../GOALS.md#remaining-work).
For a new coding session, use the [Codex handover](codex-handover.md) for
startup checks, the first implementation steps, source entry points and gates.
The immediate queue is:

- [ ] **Next server resource qualification, 2.6:** PR #30 (`348bdf8`, merged
  28 October) implements resident presentation admission, whole-allocation shared backing
  charges, close-time cache release and absolute response deadlines. Generated
  correctness checks cover refusal, aliases, recording/recovery and slow
  responses. Complete matched nominal/adverse measurements with declared
  limits under the [retained-media contract](retained-media-contract.md).
  Its defaults are selected policies, not measured operating guarantees;
  task 2.6 remains open for qualification.
- [ ] **Then server qualification, 2.1 to 2.5:** refresh performance PRs #14
  and #15 against the selected main revision and resolve their failed native
  checks. Complete wired Linux/Caddy/HTTP2 load, Safari delay and rollback
  tests before a real V4V show and the maintainer's production decision.
- [ ] **Next TV acceptance, TV1:** reserve the physical iPhone and name the
  tested build. Pass track changes, a full programme loop, Stop and
  interruption recovery. Simulator and Chrome receipts do not close this gate.
- [ ] **TV follow-ups, TV2 to TV4:** qualify sustained public and private Tor
  playback, then real peers on separate networks. Measure private publishing
  and higher-rate profiles before promotion; retain the measured lower-rate
  profile and onion-only publishing route while those gates remain open.
- [ ] **Native recording, 1.6:** qualify native AAC presentation and EOF,
  durable checkpoints, recover-to-new-file and the aggregate two-second loss
  bound on both platforms. Then run the two-hour physical Android/iOS shows
  in 1.2 and 1.4 with complete phone and origin recordings.

Server, TV and native work can progress independently, with each phone reserved
for one session. The tables below retain owners, dependencies and acceptance
for the rest of the app/V4V journey, libraries, signed releases and later work.
Library contracts and consumer checks can progress now; publication and the
production switch retain their separate maintainer gates.

## Who carries what

| Owner | Area |
|-------|------|
| Server work | The server and protocol crates: `sapflow`, `sapflow-srt`, `sapflow-rtmp`, `sapflow-hls`, `sapflow-core`, `sapflow-client`'s muxer and queues |
| App work | The iOS and Android apps under `mobile/` and `sapflow-ffi`'s platform adapters |
| Maintainer | Credentials, production changes on the media host, store accounts, releases to crates.io, product decisions |

Coordination:

- One session drives the test phone at a time. A second session launching
  apps on it ended two runs early and produced a false audio failure.
- Coordinate app changes with the active app worktree. Hand over changes
  rather than editing another session's files or blindly replaying old patches.
- Preserve the publishing, Tor and receiver services needed by the owner.
  Stop task-owned fixtures after recording their results. This plan does not
  authorise production changes, publication or contacting another person.

## TV playback and private publishing

Existing delivery includes onion-only publishing, private preview, the receiver's
own player and verified peer assistance. Native app B6 remains separate.

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| TV1 | Qualify actual Safari/iPhone | Player work with maintainer's device | Reserved device, named build | Track changes, a full programme loop, Stop and interruption recovery pass without hidden reloads or skipped buffered programme |
| TV2 | Qualify long public playback and private Tor preview | Player and publishing work | Named receiver, publisher and preview revisions | Stalls, retries, outages, buffer progression and end-to-end delay are measured on both routes; current retry handling passes |
| TV3 | Qualify private publishing and higher-rate profiles | Publishing work | Approved test window | Real-time progression, jitter/reconnects and socket/packet observations are retained; a failing higher-rate profile is not promoted |
| TV4 | Qualify real separate-network peers | Player/RelaySwarm work | Two consented devices on different networks | Verified media, corrupt refusal, peer departure, origin fallback and net savings pass without weakening private-preview restrictions |

Evidence and limits: [player](tv-player-qualification.md),
[private publishing](private-tv-qualification.md) and
[peer integration](tv-relayswarm.md). Do not add TURN or change the privacy
route without a measured need and the relevant owner decision.

## Phase 1: the broadcaster app holds a show

Goal: our app holds one logical show through supported network changes and
bounded reconnects, with complete recordings. Foreground video and opted-in
audio-only background behaviour follow their separate platform policies.

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 1.1 | Audit current SRT-attempt and capture failure reporting against the old diagnostic branch | App work | Current app source | Reasons are observable and tested; apply only missing behaviour rather than automatically replaying old commits |
| 1.2 | Two-hour Android run with Wi-Fi/mobile changes, a 60 s outage and supported lock/background cases | App and server work, phone reserved | 1.1, 1.6, frozen reference profile | One logical stream; supported drops resume within grace; no unplanned stop/stall; complete phone and origin recordings |
| 1.3 | Resolve current-run failures, retaining historical unexplained audio failures | App work; server work for protocol defects | 1.2 observations | The failing condition is explained and the whole acceptance run passes after correction; implemented late-media drop alone is not sustained acceptance |
| 1.4 | Equivalent physical iOS run | App work | 1.1, 1.6, reserved iPhone | The same sustained, recording and supported lifecycle gates pass on iPhone |
| 1.5 | Prepare the planned `sapflow-srt` 0.2.0 release and migration notes | Server work prepares; maintainer publishes | Supported API, interoperability and release qualification | Published crate/docs match the reviewed source; consumers can migrate from the 0.1 statistics API |
| 1.6 | Complete native recording/recovery acceptance | App and SDK work | Source-bound native AAC and presentation facts | Durable checkpoints, recover-to-new-file, exact EOF/presentation and the aggregate two-second loss bound pass on both platforms before default adoption |

Recording owners, programme metadata and checkpoint foundations already exist.
Follow [recording recovery's implementation order](recording-recovery.md#implementation-order)
and the [mobile status record](mobile-delivery-status.md); generated host media
is not physical capture proof.

Unblocks: phase 3, and using our app rather than Larix for a show.

## Phase 2: the first show on sapflow

Goal: one real V4V show served by sapflow, with a measured mediamtx rollback.
TV deployment does not complete this production-switch gate. Steps and prior
rehearsal evidence: [trial-plan.md](trial-plan.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 2.1 | Wired Linux/Caddy/HTTP2 load at 250 and 750 full viewers, with adverse and sustained cases | Server work | 2.6, a wired second machine, named build | CPU/RSS, retained media, descriptors, failures, latency and recordings are measured; results set the trial's audience cap |
| 2.2 | Safari delay through Caddy over HTTP/2, against mediamtx on the same stream | Server work with the maintainer's iPhone | none | A measured figure for each server in the trial plan |
| 2.3 | Revalidate current V4V viewer recovery during rollback | Maintainer decides; V4V work if needed | Current player and trial build | Observed automatic recovery or explicit reload steps are recorded; the historical stall report is not a current test |
| 2.4 | Verify the previously requested publisher-credential rotation | Maintainer | Quiet window and private host audit | Affected servers, V4V templates and saved encoders are updated; restarts and a test signal pass; credentials stay private |
| 2.5 | The show itself, with a qualified encoder such as Larix | Maintainer runs; server work watches | 2.1 to 2.4, current ingest gates and rehearsed rollback | Show and replay complete, or rollback is executed and timed; maintainer records the production decision |
| 2.6 | Bound retained HLS paths/bytes and slow-viewer ownership | Server work | Retained-media contract and existing admission controls | Exhaustion, final-owner release, reconnect/codec changes, incomplete recordings and slow-response deadlines pass; accounting and RSS stay separate |

Refresh open performance PRs [#14](https://github.com/forgesworn/sapflow/pull/14)
(evidence) and [#15](https://github.com/forgesworn/sapflow/pull/15) (Linux/slow-viewer
harness) against current main and resolve their last failed native checks.
Short screens and historical Hyper comparisons do not complete 2.1. Complete
the encrypted SRT matrix alongside this work. A five-hour campaign on eight
parser and ingest targets passed on 29 October ([fuzz receipts](../fuzz/README.md));
repeat it on the selected trial build if parser code changes.

Unblocks: switching production to sapflow by default.

## Phase 3: our app replaces Larix

Goal: creators can use our app for any v4v show. Milestones B3R to B5 in
[mobile-streaming-plan.md](mobile-streaming-plan.md); priorities in
[mobile-competitive-review.md](mobile-competitive-review.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 3.1 | Qualify implemented native RTMPS adapters | App work; server work for client defects | Current packages, reserved devices | Trusted publishing and certificate/hostname refusal pass on both phones with bounded queues and cleanup |
| 3.2 | Complete and qualify link adaptation using queue age and SRT feedback | App and server work | Current feedback path, physical network | Reduced-capacity link meets the frozen quality budget without unbounded queues or needless reconnects |
| 3.3 | Qualify audio-only and opted-in background ownership | App work | Phase 1, platform policy | Sustained locked/background audio, interruptions, return and Stop pass on both platforms |
| 3.4 | Qualify external/USB input, camera controls, mute/standby and profiles | App work | Current controls, supported input matrix | Each advertised feature passes the mobile plan; missing behaviour is implemented before promotion |
| 3.5 | Qualify reviewed profile, QR and link import | App work | Current source/build checks | Physical import/refusal/permission tests pass; import never starts transmission or exposes credentials in shareable links |
| 3.6 | Signed Zapstore/direct Android and app-store builds with free unlimited V4V | Maintainer for accounts/signing; app work for builds | Phase 1, 3.1 to 3.5, complete V4V journey and notices | Actual installs, upgrades, release readback and free-V4V behaviour pass per channel, including Android without Google Play |
| 3.7 | Point v4v's encoder guidance at our app instead of Larix | Server work prepares the v4v change; maintainer merges (merging v4v deploys production) | 3.6, phase 2 | v4v's setup pages offer our app first |

## Phase 4: libraries for other developers

Goal: other people can build on our crates and SDK. Work queue LQ1 to LQ8 in
[library-quality-plan.md](library-quality-plan.md).

| # | Task | Owner | Needs | Done when |
|---|------|-------|-------|-----------|
| 4.1 | Complete existing SDK contracts and compatibility policy | App and SDK work | Current direct C ABI and Swift/Kotlin consumers | Ownership, threads, cancellation, errors and supported toolchains are documented and exercised by current consumers |
| 4.2 | Qualify public installable packages: Swift Package Manager and Maven | App work; maintainer publishes | 4.1 | Fresh external installs run examples; actual archives include required notices and usable covered-source access |
| 4.3 | Complete repeatable interoperability and performance scorecard | Server work | Existing scripts and evidence; refreshed PRs #14/#15 | Supported OBS/FFmpeg/encrypted SRT/RTMPS profiles are repeatable; complete-path and physical measurements retain their scope |
| 4.4 | A second real consumer of the libraries, besides our app | Server work or app work | 4.2 | Built only from the public interface |
| 4.5 | Decide on an OBS adapter, then scenes, mixing and a desktop studio | Maintainer | 4.3 | Written decisions with scope and acceptance |
| 4.6 | Finish the repository homepage proposal | Maintainer reviews; server work prepares | Reviewed current page and publication instruction | A project homepage is live and linked from the repository; its GitHub homepage field was still empty at this review |

## Phase 5: later

- RelaySwarm peer playback in the apps (B6), after phase 3.
- A second rendition and wider formats (B7).
- Server features left out until a show needs them: RTMPS ingest, HEVC and
  AV1, delta playlists, a metrics endpoint, a config file, release binaries
  ([comparison.md](comparison.md), "Gaps sapflow has today").

## Order

TV qualification, native recording and server resource bounds can progress
independently. Phases 1 and 2 run side by side: phase 2 can use Larix and does
not wait for the app. Phase 3 source already exists in part; its release needs
phase 1 and its own physical gates. Phase 4 contracts, consumer checks and
interoperability can progress now; publication waits for their acceptance and
maintainer instruction. Phase 5 retains the mobile plan's B5/B6/B7 dependencies.
