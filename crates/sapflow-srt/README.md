# sapflow-srt

sapflow's own SRT: the receiving side of live-mode SRT, as a listener.
MIT, like the rest of sapflow.

Written from the SRT specification, [draft-sharabayko-srt-01], and from
watching real sessions on the wire: libsrt 1.5.4's `srt-live-transmit`
and ffmpeg 8.1.2 (libsrt 1.5.3), recorded through a UDP relay and read as
black boxes. No code was taken from srt-rs (Apache-2.0) or libsrt
(MPL-2.0). Where the specification and real encoders differ, the encoders
win; each difference is recorded in `docs/reference.md`.

[draft-sharabayko-srt-01]: https://datatracker.ietf.org/doc/draft-sharabayko-srt/

## Shape

Sans-IO, no async runtime, no dependencies, `unsafe` forbidden, and no
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

## Scope

Live mode, listener, receiving. Not file mode, rendezvous, caller mode,
groups, packet filters, sending, or (yet) encryption: a caller that asks
for a passphrase is refused in the handshake (`REJ_UNSECURE`).

## Tests

- Unit tests per module (`cargo test -p sapflow-srt`).
- `tests/differential.rs`: recorded publishes replayed through the listener
  must deliver what `srt-protocol` delivers for the same trace, and under
  loss exactly what libsrt's receiver delivered in the recorded session.
- Fuzz targets `own_srt_packet` and `own_srt_listener` (`fuzz/README.md`).
