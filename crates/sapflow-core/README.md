# sapflow-core

Version 0.2.0-rc.1 is an experimental, opt-in prerelease. Physical show-length
and wider library acceptance remain open. Read the [migration and release notes](https://github.com/forgesworn/sapflow/blob/v0.2.0-rc.1/docs/releases/0.2.0-rc.1.md)
before upgrading from 0.1.0.

Shared types for the [sapflow](https://github.com/forgesworn/sapflow) live
media server and its mobile publishing apps: validated stream paths,
publish requests and credentials, stream-health callback events, rate and
log limits, publish-session state and recording file names. No networking,
no async runtime, no dependencies, so it builds for iOS and Android as well
as servers. MIT.

## PROXY protocol v2

`proxy_protocol` parses the binary PROXY protocol version 2 header a
layer-4 front proxy puts before a forwarded TCP connection. It does no I/O:
feed it the bytes received so far and it answers `NeedMore`, a header with
the number of bytes it used, or a typed error. It accepts PROXY over TCP on
IPv4 or IPv6, and LOCAL whatever its family (UNSPEC included); it refuses
version 1, and PROXY over UNSPEC, UDP or UNIX. A
header is at most 536 bytes with at most 32 TLVs; a CRC32C TLV is verified,
and ALPN, AUTHORITY and the other TLVs are exposed. `parse_trusted` refuses
any TCP peer other than the trusted proxy, comparing canonical addresses so
an IPv4-mapped IPv6 peer matches an IPv4 proxy. The module docs give each
decision and its reason.

Most people want one of the other crates:

- [`sapflow`](https://crates.io/crates/sapflow): the server binary.
- [`sapflow-srt`](https://crates.io/crates/sapflow-srt): SRT in pure Rust.
- [`sapflow-hls`](https://crates.io/crates/sapflow-hls): LL-HLS packaging
  and RTMP ingest.

This crate is prepared for publication because the server depends on it.
Its types follow what sapflow's host application expects (stream paths are
`live/event-<id>`); they are not a general media vocabulary. Pre-1.0:
the API may change.
