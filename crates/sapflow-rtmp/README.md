# sapflow-rtmp

RTMP publishing in pure Rust, sans-IO: the server side of a publish
(`Connection`) and a publishing client (`client::Publisher`). Original and
digest handshakes, the chunk stream, AMF0 and the publish commands. No
sockets, no async runtime, no `unsafe`, no C. MIT.

It is the RTMP ingest of the [sapflow](https://github.com/forgesworn/sapflow)
live media server and the RTMP output of its mobile publishing SDK. Tested
against ffmpeg and Larix Broadcaster, and replayed from a recorded ffmpeg
session in its unit tests.

## Why another RTMP library

- **Hostile input first.** Every byte before the publish is accepted is
  untrusted: nothing indexes without a bound, does unchecked arithmetic on a
  length or allocates from a declared length. Limits on chunk streams,
  message sizes and AMF0 nesting. Fuzzed (`rtmp_session`, `rtmp_client`,
  `amf0`). It replaced `rml_rtmp`, in which fuzzing found two panics on
  unauthenticated input.
- **Sans-IO.** Bytes in, events and bytes out. Use it with tokio, mio, std
  threads or anything else; you own the socket, timeouts and policy.
- **Small.** Two dependencies, `hmac` and `sha2`, for the digest handshake.

## Use

```toml
[dependencies]
sapflow-rtmp = "0.1"
```

Accepting a publish:

```rust
use sapflow_rtmp::{Connection, Event};

let mut conn = Connection::new(seed); // seed fills the handshake's random bytes
conn.feed(&bytes_from_socket)?;
while let Some(event) = conn.poll()? {
    match event {
        // Check the key, then accept_publish() or refuse_publish(code, why).
        Event::Publish { app, key } => conn.accept_publish(),
        Event::Metadata { audio } => { /* onMetaData: whether audio is coming */ }
        // FLV tag bodies with millisecond timestamps.
        Event::Video { timestamp, data } => {}
        Event::Audio { timestamp, data } => {}
        Event::Finished => {}
    }
}
socket.write_all(&conn.take_output())?;
```

The FLV tag bodies go straight into `sapflow_hls::FlvIngest` for
Low-Latency HLS ([`sapflow-hls`](https://crates.io/crates/sapflow-hls)).

Publishing: [`examples/rtmp-publish.rs`](examples/rtmp-publish.rs) is a
complete publisher that sends an FLV file to a server.

## Scope

Publish only: no playback (`play`), no RTMPS (bring your own TLS). Commands
and metadata are AMF0; the AMF3 message types some encoders send are
unwrapped to their AMF0 contents. Version 0.1: the API may
change.
