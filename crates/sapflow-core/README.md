# sapflow-core

Shared types for the [sapflow](https://github.com/forgesworn/sapflow) live
media server and its mobile publishing apps: validated stream paths,
publish requests and credentials, stream-health callback events, rate and
log limits, publish-session state and recording file names. No networking,
no async runtime, no dependencies, so it builds for iOS and Android as well
as servers. MIT.

Most people want one of the other crates:

- [`sapflow`](https://crates.io/crates/sapflow): the server binary.
- [`sapflow-srt`](https://crates.io/crates/sapflow-srt): SRT in pure Rust.
- [`sapflow-hls`](https://crates.io/crates/sapflow-hls): LL-HLS packaging
  and RTMP ingest.

This crate is published because the server depends on it. Its types follow
what sapflow's host application expects (stream paths are
`live/event-<id>`); they are not a general media vocabulary. Version 0.1:
the API may change.
