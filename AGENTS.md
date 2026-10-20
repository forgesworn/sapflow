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
| `sapflow-server` | The `sapflow` binary: ingest, HTTP, callbacks, recording. |

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

## Conventions

- British English in prose and identifiers. No em dashes.
- Commits: `type: description`, no `Co-Authored-By` trailers. Commit as
  `TheCryptoDonkey <TheCryptoDonkey@users.noreply.github.com>`.
- The repository is `github.com/forgesworn/sapflow`. Push only on the
  maintainer's instruction.
- No real hostnames, IPs or personal names in committed files; use
  placeholders. Private notes go in `*.local.md` (gitignored).
- `unsafe` is forbidden workspace-wide.
- Every new behaviour gets a test. Pure logic belongs in `sapflow-hls` or
  `sapflow-core`, not in the server crate.
- Before adding a crate, check it is maintained and pin the current version;
  prefer fewer dependencies. Anything parsing outside input gets a fuzz
  target (`fuzz/README.md`).
- `vendor/srt-protocol` is a patched copy (Apache-2.0); record every change
  in its `PATCHES.md`.
