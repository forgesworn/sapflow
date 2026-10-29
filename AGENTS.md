# AGENTS.md - sapflow

Guidance for AI coding agents working in this repository.

## What this is

A small live media server in Rust that will replace mediamtx behind the v4v
live stage if it measures better. Read `GOALS.md` for scope and milestones,
`docs/v4v-contract.md` for what v4v expects, and `docs/reference.md` for the
iOS rules and the mediamtx/gohlslib source they came from.

| Crate | Role |
|-------|------|
| `sapflow-core` | Shared types (stream paths, callback events). No networking, no async runtime: it must build for iOS and Android later. |
| `sapflow-hls` | LL-HLS packaging: segmenting, parts, playlists, the Apple rules. |
| `sapflow-rtmp` | Our own RTMP, publish only, sans-IO: the server side of a publish and a publishing client. |
| `sapflow-srt` | Our own live SRT listener/receiver and caller/sender, sans-IO, using maintained cryptographic primitives. Written from the specification and wire observation only: never read or copy srt-rs (`srt-protocol`, `srt-tokio`) or libsrt source; run them only as black boxes. |
| `sapflow-client` | Runtime-free native compressed-media validation, bounded queues, session policy and MPEG-TS writing. No camera, codec, network, file or billing runtime. |
| `sapflow-ffi` | Typed Swift/Kotlin boundary for shared compressed samples and protocol/session state; platform adapters own sockets, TLS and native codecs. |
| `sapflow` (`crates/sapflow-server`) | The `sapflow` binary: ingest, HTTP, callbacks, recording. |

## Run and test

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/smoke.sh     # end to end: SRT and RTMP in, LL-HLS out, recording
scripts/srt-loss.sh  # SRT receivers under packet loss against libsrt (about 40 minutes)
scripts/tidy.sh          # build size; --clean empties target/
python3 scripts/third-party-licences.py   # after dependency changes
```

CI (`.github/workflows/ci.yml`) runs the fmt, clippy and test commands above,
the fuzz regressions plus 60 s per target, and
`scripts/smoke.sh`. `fuzz-nightly.yml` fuzzes each target for 20 minutes. Keep
the local commands and the workflow in step.

## Rust builds: keep them tidy

The Mac's disk is nearly full and Rust builds are large.

- One build directory: `target/` in this checkout, pinned by
  `.cargo/config.toml`. Never set `CARGO_TARGET_DIR` per task or subagent: each
  one recompiles every dependency.
- `CARGO_INCREMENTAL=0` is set for agent shells; leave it.
- Dev builds carry line tables only, dependencies no debug info (`Cargo.toml`).
- Run `scripts/tidy.sh` at the end of a session and `scripts/tidy.sh --clean`
  when `target/` passes a few GB or you are done for a while.
- Worktrees each have their own `target/`; clean one before removing it.
- Before cleaning a shared target, check for active builds, running binaries
  and native packaging work. Preserve installed runtimes and release packages;
  remove inactive compiler caches rather than keeping every build indefinitely.

## Scratch and benchmark retention

These rules apply to every clone, worktree and agent, including remote test
hosts. Scratch space is temporary, not an evidence archive.

- Keep small result summaries and handover notes by default. Record the source
  revision, command, workload, relevant environment, result and limitations.
  Do not retain raw output merely to prove that a check ran.
- Delete task-owned raw logs, per-sample traces, media captures, temporary
  generated fixtures and transfer archives after writing the summary. Clean up
  failed and interrupted runs too, once their failure has been recorded. Do
  this as part of finishing the task, without waiting for a separate cleanup
  request.
- Retain raw benchmark data only when the owner explicitly requests it for a
  particular investigation. Record its purpose, size, location and cleanup
  condition. An unresolved result is not blanket permission to retain every
  run, source snapshot or archive.
- Reuse existing tools and source checkouts. Do not copy whole source trees,
  toolchains or build directories into each benchmark run. Remove task-owned
  downloaded tools and copied executables when the task ends.
- Do not embed whole source, tool or media bundles in generated launcher
  scripts. Transfer only required files or small patches, and summarise
  relevant build outputs instead of repeatedly inventorying whole targets.
- Bound benchmark duration, log growth and output size before launching it.
  Check available disk space and clean completed runs before starting more.
  Stop or reduce diagnostics if output exceeds the planned storage budget.
- Before removing copied source trees, preserve unique unfinished changes as
  small patches or in their original checkout. Never discard unrelated work,
  active processes, secrets or installed service runtimes during cleanup.
- Check process arguments, working directories and open files before deleting
  a candidate. Use Git's worktree removal procedure for linked worktrees;
  never treat a checkout as disposable because it lives under `scratch/`.
- Cleanup means reducing total stored data. Moving bulk output elsewhere or
  retaining both an archive and its expanded contents does not count. Compact
  notes or unique source changes when useful; do not archive raw benchmark
  data as a substitute for deleting it.
- At handover, report remaining scratch/build sizes, anything deliberately
  retained and why, and any task-owned processes left running. Distinguish
  files removed from measured free-space gain, since snapshots can retain
  deleted blocks. Do not delete backup snapshots without explicit permission.

Keep private summaries in ignored `*.local.md` files. Put reusable, non-private
findings in the existing project documentation. Never commit raw camera media
or private benchmark output.

## Conventions

- Never commit, push or publish real camera footage, extracted camera frames,
  phone recordings or archives containing them. Keep these in ignored private
  lab storage. Only generated, non-camera media fixtures may be considered for
  source control; verify their provenance before staging. A media extension
  alone does not establish that a file is safe to publish.
- British English in prose and identifiers. No em dashes.
- Commits: `type: description`, no `Co-Authored-By` trailers. Commit as
  `TheCryptoDonkey <TheCryptoDonkey@users.noreply.github.com>`.
- The repository is `github.com/forgesworn/sapflow`. Push only on the
  maintainer's instruction.
- No real hostnames, IPs or personal names in committed files; use
  placeholders. Private notes go in `*.local.md` (gitignored).
- `unsafe` is denied workspace-wide. The only exception is the reviewed
  `capi.rs` C ABI module in each FFI crate; every other module keeps the deny.
- Every new behaviour gets a test. Pure logic belongs in `sapflow-hls` or
  `sapflow-core`, not in the server crate.
- Before adding a crate, check it is maintained and pin the current version;
  prefer fewer dependencies. Anything parsing outside input gets a fuzz
  target (`fuzz/README.md`).
