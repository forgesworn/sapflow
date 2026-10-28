# Codex handover: remaining work

Prepared 28 October 2026 from `30dedca7fdcae328c03fc9d99cfff1b927003e25`
(merged PR #28). This is a starting packet, not a new qualification result.
The [delivery plan](delivery-plan.md) owns task numbers, dependencies and
acceptance; [GOALS.md](../GOALS.md#remaining-work) owns the full backlog.
Update those records when a task's state changes rather than maintaining a
second completion ledger here.

Continuation update, 28 October: task 2.6 now has resident presentation and
shared encoded-backing admission, close-time cache release, absolute response
deadlines and local correctness evidence. The current contract and delivery
plan own its remaining matched-load qualification. The first-implementation
section below records the original brief; do not reimplement completed work.

## Start here

Suggested instruction for the next Codex session:

> Read AGENTS.md and docs/codex-handover.md. Continue delivery-plan task 2.6
> with matched nominal/adverse resource measurements on the selected build.
> Read docs/retained-media-contract.md for implemented bounds, tested source
> and remaining gates. Declare all limits and keep logical charges, RSS and
> kernel memory separate. Preserve unrelated work and existing live services.

Before editing:

1. Read the applicable parent and repository `AGENTS.md` instructions. Check
   `git status --short --branch`, `git log -5 --oneline`,
   `git worktree list` and the current main/PR state. At preparation this
   checkout was clean and detached at the revision above. Create a task branch
   before committing; preserve any newer or unrelated work found on arrival.
2. Read [the next-work checklist](delivery-plan.md#next-work),
   [readiness](readiness.md) and the
   [retained-media contract](retained-media-contract.md).
3. Recheck the source before treating historical receipts as current. Hyper
   and Axum removal, the own HTTP viewer loop, RTMP publication admission and
   recorder bounds are implemented. The continuation adds global presentation
   and encoded-backing limits and response deadlines; qualification remains
   open. Inspect the current contract before repeating the original brief.
4. If working on TV or devices, reserve the phone with the owner first and
   inspect existing processes before starting or stopping anything. Private
   operational details may be in ignored `CLAUDE-TV-INVESTIGATION.local.md`
   and `takeover-2026-10-26.local.md`. Check their ignore status before use;
   do not copy private details into committed documentation.

## Original implementation brief: task 2.6, retained HLS resources

The defect is ownership beyond admission: one publisher can leave useful HLS
caches across successive paths, and responses can keep fragments alive after
playlist eviction or registry removal. Connection and publisher limits do not
bound that retained media. A progressing slow response can also outlive the
existing idle timeout.

Read these entry points and follow the actual payload owners before changing
their types:

| Source | Responsibility |
|---|---|
| [main.rs](../crates/sapflow-server/src/main.rs) | App ownership and configuration wiring |
| [stream.rs](../crates/sapflow-server/src/stream.rs) | Registry, stream lifetime, reconnect hold and cached ingest state |
| [publish.rs](../crates/sapflow-server/src/publish.rs) | Publication generations and retirement |
| [media_body.rs](../crates/sapflow-server/src/media_body.rs) | Segment responses and retained chunk ownership |
| [own_http.rs](../crates/sapflow-server/src/own_http.rs) | Parked requests, response progress and timeouts |
| [muxer.rs](../crates/sapflow-hls/src/muxer.rs) and [fmp4.rs](../crates/sapflow-hls/src/fmp4.rs) | Fragment creation and shared encoded media |

Implement in reviewable steps, retaining the separate C9 publication quota:

1. **Reproduce retention.** With generated media and small injectable limits,
   populate eight useful paths within reconnect grace using one publisher.
   Retain response bodies while rotating the playlist window. Observe the
   expected failing resource-limit regressions before applying the fix.
2. **Bound resident presentations.** Use one App-owned residency manager.
   Count through the final stream owner, including removed registry entries
   retained by publications, grace tasks or parked requests. Refuse admission
   visibly at capacity; do not silently evict an active presentation.
3. **Bound fragment storage.** Charge each immutable backing once before
   publication, using its whole encoded allocation capacity, including headers
   and spare capacity. Every cache, segment, response, emitted-frame and
   recorder alias must retain the same charge until the final payload owner
   releases it. Use checked arithmetic. Counting alias lengths is incorrect.
4. **Cover lifecycle and refusal.** Close releases cached ingest state promptly
   while accepted response bytes remain valid. Cover codec replacement,
   reconnect, takeover and registry removal. Exhaustion must propagate to
   incomplete-recording status without labelling a refused suffix complete.
5. **Bound slow response ownership.** Define and document an explicit body
   deadline policy separately from the progress-reset idle timeout. Prove
   reclamation for stalled and trickling viewers without corrupting accepted
   bytes or breaking normal LL-HLS responses.

The contract proposes `4 * N` resident presentations capped at 4,096 (64 at
default publisher admission) and 512 MiB of fragment backing. These are
candidates to qualify, not measured defaults or operating guarantees. Declare
the selected configuration in results. Logical fragment charges, process RSS,
other buffers and kernel memory are separate measurements.

Task 2.6 acceptance must cover exact capacity, one-over refusal, overflow,
aliases surviving eviction, exact ordered response bytes, consumed-prefix
retention, final release exactly once, codec/reconnect changes, concurrent
ownership and exhaustion with recording/recovery. Preserve publication quota
transfer on same-path takeover and the current owner on cross-protocol refusal.
Follow all four qualification cases in the resource contract. Passing unit
tests alone does not qualify Linux audience capacity.

## Work after or alongside task 2.6

These lanes can proceed independently when devices and worktrees are available.
The task tables in the delivery plan define the full dependencies.

| Lane | Next work and remaining acceptance | Detailed record |
|---|---|---|
| Server, 2.1 to 2.5 | Refresh performance PRs #14/#15 against selected main and inspect their current native checks. After resource bounds, measure wired Linux/Caddy/HTTP2 at 250 and 750 full viewers, adverse/sustained load, memory, descriptors, latency and recording integrity. Complete encrypted SRT matrix and source-specific long fuzzing. Compare Safari delay with mediamtx; rehearse rollback before a real V4V show and explicit production decision. | [Trial plan](trial-plan.md), [readiness](readiness.md) |
| TV, TV1 to TV4 | Physical iPhone track changes, full programme loop, Stop and interruption recovery first. Then sustained public/private Tor playback, measured private publishing/higher-rate profiles, and consented real peers on separate networks with corrupt refusal, departure, origin fallback and savings. | [Player qualification](tv-player-qualification.md), [private publishing](private-tv-qualification.md), [peer integration](tv-relayswarm.md) |
| Native recovery, 1.6 | Qualify native AAC leading/tail, exact presentation, EOF and seeks on both platforms. Complete durable checkpoints and explicit recovery into a new file, preserving originals. Prove aggregate two-second source-presentation loss across the entire recording path before default adoption. | [Implementation order](recording-recovery.md#implementation-order), [mobile status](mobile-delivery-status.md) |
| Sustained apps, 1.1 to 1.4 | Audit current failure reporting before replaying old diagnostic patches. After recording acceptance, run two-hour physical Android and iOS shows through scheduled network changes/outages and supported lifecycle cases. Require one logical show and complete phone/origin recordings; explain and rerun failures. | [Mobile plan](mobile-streaming-plan.md), [mobile status](mobile-delivery-status.md) |
| Complete app/V4V journey, 3.1 to 3.7 | Physical RTMPS trust/refusal, encrypted SRT and bounded link adaptation; audio-only/background policy, external inputs, camera controls, standby and profile/QR import. Prove authentication, Prepare/Start/End, viewer media and replay. Measure latency/thermals and matched competitors. Qualify signed installs/upgrades per channel, Android without Google Play and free unlimited V4V before encoder-guidance changes. | [Mobile plan](mobile-streaming-plan.md), [native budgets](native-performance-contract.md), [competitive review](mobile-competitive-review.md) |
| Libraries/releases, 1.5 and 4.1 to 4.6 | Finish ABI/ownership/thread/cancellation and toolchain contracts; fresh external SPM/Maven consumers, actual archive notices/source access, planned SRT 0.2.0 migration and interoperability/performance scorecard. Prove a second real public-interface consumer. Maintainer decides publication, OBS/desktop scope and project homepage. | [Library quality plan](library-quality-plan.md) |
| Later, phase 5 | Native RelaySwarm B6, then second rendition/wider formats B7. Additional server formats, ingest, metrics/configuration and release binaries remain demand-led. | [Delivery order](delivery-plan.md#order), [GOALS.md](../GOALS.md#remaining-work) |

## Boundaries and evidence

- Keep publishing onion/Tor-only with no direct fallback. Retain the measured
  lower-rate profile until higher-rate qualification passes. Do not add TURN,
  raise bitrate or replace the player without a measured limitation and the
  relevant owner decision.
- Simulator/Chrome playback does not close physical iPhone acceptance;
  same-host peers do not close separate-network/NAT acceptance; public playback
  does not close sustained private Tor acceptance. Name the tested revision,
  platform, route, duration and failure conditions in every receipt.
- Preserve legacy recorder defaults until native presentation/recovery and
  adoption gates pass. Host-generated media and SDK builds are not physical
  capture, power-loss or installed recovery proof. Preserve original failed
  verdicts and distinguish process termination from physical power loss.
- Production switching, credential rotation, signing, package publication and
  contacting others retain their owner/maintainer gates. Verify the previously
  requested credential rotation privately in a quiet approved window. This
  handover grants no new operational permission; retain existing explicit
  session authorisation where applicable.

## Verification and finishing

For behavioural changes, run focused regressions first, then the repository
checks appropriate to the change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/smoke.sh
scripts/tidy.sh
```

Keep CI in step with the documented commands. Inspect its current workflow and
run required fuzz regression/smoke checks; native CI remains a separate required
check. Run the longer SRT loss campaign when relevant to protocol changes.
Dependency changes also need `python3 scripts/third-party-licences.py` and the
repository's dependency/parser review requirements.

Reuse this worktree's `target/`; never create a task-specific
`CARGO_TARGET_DIR` or override `CARGO_INCREMENTAL=0`. Never inspect or copy
libsrt/srt-rs implementation source; use them only as black boxes. Publish no
real camera footage, extracted frames, phone recordings, credentials, real
hostnames, IP addresses or personal names. Verify generated-fixture provenance.

Update affected contracts, GOALS and delivery status with the tested source,
commands, outcomes, evidence locations and gates still open. For authorised
shipping, use the required commit identity, stage only reviewed task paths,
read every PR review before merging, verify exact-head PR and merged-main CI
and perform the relevant public readback. CI is not production/device approval.
Stop task-created fixtures and remove only task-created scratch or safely
merged clean worktrees; preserve the owner's existing services and evidence.

No new implementation, device trial, production change or release was performed
to prepare this handover.
