# sapflow-srt

sapflow's own SRT: live-mode SRT as a listener and receiver, and as a
bounded, paced caller and sender, with optional AES-128/256-CTR encryption.
MIT, like the rest of sapflow.

Written from the SRT specification, [draft-sharabayko-srt-01], and from
watching real sessions on the wire: libsrt 1.5.4's `srt-live-transmit`
and ffmpeg 8.1.2 (libsrt 1.5.3), recorded through a UDP relay and read as
black boxes. No code was taken from srt-rs (Apache-2.0) or libsrt
(MPL-2.0). Where the specification and real encoders differ, the encoders
win; each difference is recorded in `docs/reference.md`.

[draft-sharabayko-srt-01]: https://datatracker.ietf.org/doc/draft-sharabayko-srt/

## Shape

Sans-IO, no async runtime, `unsafe` forbidden, no dependencies beyond pinned
RustCrypto primitives for encryption (`aes`, `ctr`, `aes-kw`, `pbkdf2`,
`sha1`) with `zeroize` and `getrandom`, and no
panicking constructs outside tests (`unwrap`, `expect`, `panic!` and
indexing are denied by lint). A `Listener` takes datagrams with their
source address and the time and gives back datagrams to send, events and
the next time it needs; the server owns the socket and the timer.

```rust
let mut l = sapflow_srt::Listener::new(sapflow_srt::Config::default(), Instant::now());
l.handle_datagram(now, peer, &datagram);   // or l.handle_timeout(now)
while let Some(ev) = l.poll_event() { /* Connect: accept or reject; Data; Closed */ }
while let Some((to, bytes)) = l.poll_transmit() { /* send */ }
let next = l.poll_timeout();               // None when there are no connections
```

| Module | What |
|--------|------|
| `seq` | 31-bit sequence numbers and 32-bit timestamps, wrapping |
| `packet` | every packet type, parsed and encoded, bounded by the datagram |
| `loss` | the loss list and when to ask again |
| `rtt` | RTT from ACK/ACKACK |
| `receiver` | one connection: buffer, TSBPD delivery, too-late drop, drift, ACK, NAK, keepalive, idle timeout |
| `listener` | version 5 handshake, SYN cookies keyed per process, stream id, latency negotiation, limits |
| `caller`, `sender` | live caller: handshake, paced bounded sending, retransmission from NAKs |
| `crypto` | AES-128/256-CTR key material: PBKDF2 key derivation, key wrap, refresh |

## Scope

Live mode only: listener/receiver and caller/sender. Not file mode,
rendezvous, groups or packet filters. Encryption is AES-128 or AES-256 in
CTR mode with strict key negotiation and refresh; AES-192 and GCM are refused.
CTR does not authenticate media, and control packets, including the stream
id, stay in clear. The supported profile is set out in `src/crypto.rs`.
Seamless reconnection, late-drop negotiation and adaptive bitrate on the
caller side are not yet qualified.

How it compares with libsrt and gosrt under loss, with measurements, is in
`docs/benchmarks.md` and `docs/comparison.md` at the repository root.

## Tests

- Unit tests per module (`cargo test -p sapflow-srt`).
- `tests/differential.rs`: recorded publishes replayed through the listener
  must deliver what `srt-protocol` delivered for the same trace (its
  output recorded before it was removed), and under
  loss exactly what libsrt's receiver delivered in the recorded session.
- `tests/caller.rs`: the caller and sender against our listener.
- `tests/black_box_sender.py`, `tests/black_box_crypto.py`: our sender and
  encryption against libsrt's tools, as black boxes.
- Fuzz targets `own_srt_packet`, `own_srt_listener` and `own_srt_caller`
  (`fuzz/README.md`).
