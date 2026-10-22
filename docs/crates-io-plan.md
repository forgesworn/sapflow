# Publishing the crates on crates.io

A checklist for the maintainer. Nothing here has been published; every step
that publishes, reserves a name or creates a credential is the maintainer's.
The preparation (metadata, version, READMEs, docs, the rename) is done; what
remains is the dry run, the credentials and the publish itself.

## Why

Rust users and coding agents look for libraries on crates.io, docs.rs and
lib.rs. Until then the crates exist only in this repository, so a search for a Rust SRT, RTMP or LL-HLS crate cannot find
them, and using one means a git dependency.

## Names

Checked on 2026-10-22 against the crates.io API: `sapflow`, `sapflow-srt`,
`sapflow-hls`, `sapflow-core`, `sapflow-client`, `sapflow-ffi`,
`sapflow-server` and `sapflow-bench` were all unregistered; `sapflow-rtmp`, split
out of `sapflow-hls` the same day, was too. crates.io asks
that names are not reserved without code, so register each by publishing it.

## Which crates, in order

Publish in dependency order. Each internal `path` dependency needs a
`version` beside it (`sapflow-srt = { path = "../sapflow-srt", version = "0.1.0" }`).

| Order | Crate | Publish? | Notes |
|-------|-------|----------|-------|
| 1 | `sapflow-srt` | yes | No internal dependencies. The crate with the clearest audience: pure-Rust SRT with no libsrt |
| 2 | `sapflow-core` | yes | Shared track declarations needed by HLS and RTMP, plus server types |
| 3 | `sapflow-hls` | yes | FLV and MPEG-TS ingest and LL-HLS packaging; normal dependency on `sapflow-core` |
| 3a | `sapflow-rtmp` | yes | RTMP publishing, server side and client; normal dependency on `sapflow-core`. Its dev-dependency on `sapflow-hls` (one integration test) is path-only, so `cargo publish` drops it |
| 4 | `sapflow-client` | later | Depends on `sapflow-srt`; its API is still moving with the mobile work |
| 5 | `sapflow-ffi` | no | A Swift/Kotlin binding, shipped as native packages, not as a Rust library |
| 6 | `sapflow` | yes | The server; the package was renamed from `sapflow-server` (its directory keeps the old name) so `cargo install sapflow` gives the `sapflow` binary |
| - | `sapflow-bench` | no | Internal test tool |

## Per crate, before publishing

- [x] `publish = true` on the crate (the workspace default stays `false`, so
      `sapflow-bench` and `sapflow-ffi` cannot go out by accident).
- [x] Version `0.1.0`. 0.x tells users the API may still change, which is
      true; `docs/library-quality-plan.md` LQ5 sets the bar for promising more.
- [x] `description`, up to five `keywords` and `categories`, and `readme`:
  - `sapflow-srt`: keywords `srt`, `streaming`, `sans-io`, `live`, `video`;
    categories `network-programming`, `multimedia::video`.
  - `sapflow-rtmp`: keywords `rtmp`, `streaming`, `sans-io`, `live`, `flv`;
    categories `network-programming`, `multimedia::video`.
  - `sapflow-hls`: keywords `hls`, `ll-hls`, `mpeg-ts`, `fmp4`, `streaming`;
    categories `multimedia::video`, `network-programming`.
  - `sapflow`: keywords `media-server`, `srt`, `rtmp`, `hls`, `streaming`;
    categories `multimedia::video`, `command-line-utilities`.
- [x] A README per crate; the server uses the root README.
- [x] `cargo doc --no-deps -p <crate>` builds without warnings, and every
      public item has a doc comment (`#![warn(missing_docs)]` in each library,
      so clippy in CI keeps it that way).
- [x] `cargo package -p <crate> --list`: check what ships. The test fixtures
      ship (SRT 240 KB, HLS 640 KB); every one is generated (ffmpeg test
      signal or solid colour), and the HLS tests include them at compile time.
- [ ] `cargo publish --workspace --dry-run --exclude sapflow-bench --exclude
      sapflow-ffi --exclude sapflow-client` (passed 2026-10-22). Clear
      `target/package` first: a tarball left from an earlier layout made cargo
      1.94.1 fail with "no hash listed".
- [x] Each crate carries the MIT `LICENSE` (a link to the root file, which
      `cargo package` copies in). `THIRD_PARTY_LICENSES.txt` stays with the
      container image.

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
