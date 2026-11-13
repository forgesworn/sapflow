# sapflow-srt

Version 0.2.0-rc.1 is an experimental, opt-in prerelease. Physical show-length
and wider library acceptance remain open. Read the [migration and release notes](https://github.com/forgesworn/sapflow/blob/v0.2.0-rc.1/docs/releases/0.2.0-rc.1.md)
before upgrading from 0.1.0.

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

```sh
cargo add sapflow-srt@=0.2.0-rc.1
```

API documentation: [docs.rs/sapflow-srt](https://docs.rs/sapflow-srt).

A complete listener that accepts every caller and prints each payload is
[`examples/listen.rs`](examples/listen.rs) (`cargo run -p sapflow-srt --example listen`).
Its core:

```rust
use sapflow_srt::{Config, Event, Listener, RejectCode};

let mut listener = Listener::new(Config::default(), Instant::now());
// for each datagram from the socket:
listener.handle_datagram(Instant::now(), peer, &datagram); // SocketAddr, &[u8]
// and whenever poll_timeout() comes due:
listener.handle_timeout(Instant::now());

while let Some(event) = listener.poll_event() {
    match event {
        // stream_id is the caller's raw bytes (Vec<u8>) and untrusted.
        Event::Connect { id, peer, stream_id } => {
            if stream_id == b"demo" {
                listener.accept(id, Instant::now());
            } else {
                listener.reject(id, RejectCode::PEER);
            }
        }
        Event::Data { id, payload, after_loss } => { /* Vec<u8> of MPEG-TS, in order, on time; after_loss: data just before it was given up on */ }
        Event::Closed { id, reason, stats } => { /* reason: Option<CloseReason> */ }
    }
}
while let Some((to, bytes)) = listener.poll_transmit() { // (SocketAddr, Vec<u8>)
    socket.send_to(&bytes, to)?;
}
let next: Option<Instant> = listener.poll_timeout(); // None when idle
```

`Config` sets the latency (default 120 ms; a caller asking for more gets
more), connection and handshake limits, buffer size, idle and accept
timeouts, encryption (`crypto`, `require_encryption`) and the reorder
wait (`reorder_wait_max`, default 50 ms): once a caller's path is seen to
reorder packets, the first NAK of a new gap is held for a little longer
than the latest packet has been late since the path last went a second
without reordering, so it is not resent for nothing. It
adapts only for callers that flag their retransmissions (`REXMITFLG`, as
libsrt does), and zero sends every NAK at the first gap. To refuse a
caller, `listener.reject(id, code)`, where `RejectCode` has the SRT
rejection codes as constants (`PEER`, `BADSECRET`, `RESOURCE`, ...) and
`RejectCode::http(status)` for the application range; to choose a per-caller
secret, `accept_with_crypto`. Sending is `Caller` (`examples/send.rs`).

When the application holds a passphrase per stream, set `app_keys` in
`Config`: a caller's key material then waits for the application even with
no global `crypto` secret. `check_key(id, &crypto)` unwraps it with the
application's passphrase without answering the caller, so it can be called
again and the caller can still be refused for another reason; it returns the
caller's key length (AES-128 or AES-256, whichever the caller chose) or a
`KeyCheckError` whose `reject_code()` says how to refuse.
`accept_with_key(id, now, &crypto)` then accepts only a caller whose key
matches, and never falls back to plaintext: no key material is refused with
`UNSECURE` whatever `allow_plaintext` says, a wrong key with `BADSECRET`.
A failed check must end in `reject`, never in `accept` or
`accept_with_crypto`, which still admit what the listener's own settings
allow. A passing check proves the key material was wrapped under the
passphrase, not that its sender made it: a replayed conclusion passes too.
The listener keeps no record of key material, so an application that admits
by key guards against replay itself: `pending_key_material(id)` gives a
waiting caller's `KMREQ`, `crypto::bound_key_material` the part of it the
check binds (compare that, not the whole message: edits elsewhere still
pass), and `caller_socket(id)` the caller's own socket id. sapflow's server
records the first caller to send each key material and refuses it from
anyone else.

```rust
use sapflow_srt::crypto::{CryptoConfig, KeySize};

// passphrase: the stream's own, 10 to 79 bytes; the caller picks the key length.
let key = CryptoConfig::new(passphrase, KeySize::Aes256)?;
match listener.check_key(id, &key) {
    Ok(_size) => { listener.accept_with_key(id, Instant::now(), &key); }
    Err(e) => { listener.reject(id, e.reject_code().unwrap_or(RejectCode::BADSECRET)); }
}
```

`examples/listen-app-key.rs` is a complete listener that does this.

| Module | What |
|--------|------|
| `seq` | 31-bit sequence numbers and 32-bit timestamps, wrapping |
| `packet` | every packet type, parsed and encoded, bounded by the datagram |
| `loss` | the loss list, held first reports and when to ask again |
| `rtt` | RTT from ACK/ACKACK |
| `receiver` | one connection: buffer, TSBPD delivery, too-late drop, drift, ACK, NAK with adaptive reorder wait, keepalive, idle timeout |
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
- `tests/caller.rs`: the caller and sender against our listener, including
  AES-128/256 packet-count key refresh every 32 packets at 1 ms spacing.
  Receiver controls check that cached or malformed KM floods, old-slot
  reordering and altered key flags cannot replenish spent refresh credit.
- `tests/black_box_sender.py`, `tests/black_box_crypto.py`: our sender and
  encryption against libsrt's tools, as black boxes.
- `tests/app_keys.rs`: application-held passphrases (`app_keys`,
  `check_key`, `accept_with_key`) with our caller at AES-128 and AES-256,
  encrypted media through, wrong and missing keys refused.
- Fuzz targets `own_srt_packet`, `own_srt_listener`, `own_srt_app_keys` and
  `own_srt_caller` (`fuzz/README.md`).
