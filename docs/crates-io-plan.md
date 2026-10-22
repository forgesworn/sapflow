# Publishing the crates on crates.io

A checklist for the maintainer. Nothing here has been published; every step
that publishes, reserves a name or creates a credential is the maintainer's.

## Why

Rust users and coding agents look for libraries on crates.io, docs.rs and
lib.rs. Today the crates exist only in this repository (`publish = false`,
version 0.0.0), so a search for a Rust SRT, RTMP or LL-HLS crate cannot find
them, and using one means a git dependency.

## Names

Checked on 2026-10-22 against the crates.io API: `sapflow`, `sapflow-srt`,
`sapflow-hls`, `sapflow-core`, `sapflow-client`, `sapflow-ffi`,
`sapflow-server` and `sapflow-bench` were all unregistered. crates.io asks
that names are not reserved without code, so register each by publishing it.

## Which crates, in order

Publish in dependency order. Each internal `path` dependency needs a
`version` beside it (`sapflow-srt = { path = "../sapflow-srt", version = "0.1.0" }`).

| Order | Crate | Publish? | Notes |
|-------|-------|----------|-------|
| 1 | `sapflow-srt` | yes | No internal dependencies. The crate with the clearest audience: pure-Rust SRT with no libsrt |
| 2 | `sapflow-core` | yes | Needed by the server |
| 3 | `sapflow-hls` | yes | RTMP, MPEG-TS ingest and LL-HLS packaging |
| 4 | `sapflow-client` | later | Depends on `sapflow-srt`; its API is still moving with the mobile work |
| 5 | `sapflow-ffi` | no | A Swift/Kotlin binding, shipped as native packages, not as a Rust library |
| 6 | the server | yes, as `sapflow` | Rename the package from `sapflow-server` to `sapflow` so `cargo install sapflow` gives the `sapflow` binary |
| - | `sapflow-bench` | no | Internal test tool |

## Per crate, before publishing

- [ ] `publish = true` on the crate (the workspace default stays `false`, so
      `sapflow-bench` and `sapflow-ffi` cannot go out by accident).
- [ ] Version `0.1.0`. 0.x tells users the API may still change, which is
      true; `docs/library-quality-plan.md` LQ5 sets the bar for promising more.
- [ ] `description`, up to five `keywords` and `categories`, and `readme`.
      Suggested:
  - `sapflow-srt`: keywords `srt`, `streaming`, `sans-io`, `live`, `udp`;
    categories `network-programming`, `multimedia::video`.
  - `sapflow-hls`: keywords `hls`, `ll-hls`, `rtmp`, `mpeg-ts`, `streaming`;
    categories `multimedia::video`, `network-programming`.
  - `sapflow`: keywords `media-server`, `srt`, `rtmp`, `hls`, `streaming`;
    categories `multimedia::video`, `command-line-utilities`.
- [ ] A README per crate. `sapflow-srt` has one; `sapflow-hls`, `sapflow-core`
      and the server need one (the server can point at the root README).
- [ ] `cargo doc --no-deps -p <crate>` builds without warnings, and every
      public item has a doc comment (try `#![warn(missing_docs)]` per crate).
- [ ] `cargo package -p <crate> --list`: check what ships. The SRT test
      fixtures (`crates/sapflow-srt/tests/fixtures`, 240 KB, recorded from
      ffmpeg's generated test signal) are fine to ship but not needed; add
      `exclude = ["tests/fixtures"]` if size matters, and make the test skip
      when they are absent.
- [ ] `cargo publish --dry-run -p <crate>`.
- [ ] `THIRD_PARTY_LICENSES.txt` stays with the server; the libraries carry
      only their own MIT licence file.

## Credentials

Prefer crates.io trusted publishing from GitHub Actions (OpenID Connect):
configure the `forgesworn/sapflow` repository and a release workflow as a
trusted publisher for each crate on crates.io, and the workflow publishes
without a long-lived token. The first publish of a new crate still has to be
done by an owner with an API token; scope it to `publish-new`, give it a
short expiry, and revoke it afterwards.

## After publishing

- [ ] Replace the git dependency lines in `README.md`,
      `crates/sapflow-srt/README.md` and `llms.txt` with `cargo add sapflow-srt`
      and `cargo install sapflow`.
- [ ] Check the docs.rs pages build, and link them from the READMEs.
- [ ] Tag the release (`v0.1.0`) and attach release binaries if wanted.
