# sapflow-dtls

sapflow's own DTLS 1.2 server for receive-only WHIP ingest
(`docs/whip-ingest.md` at the repository root), limited to the profile
WebRTC browsers use, with DTLS-SRTP key export. MIT, like the rest of
sapflow.

Written from the RFCs only: RFC 6347 (DTLS 1.2) over RFC 5246 (TLS 1.2),
RFC 5289 and RFC 5288 (the AES-GCM suite and its nonce), RFC 8422 (ECDHE and
ECDSA), RFC 7627 (extended master secret), RFC 5746 (renegotiation
indication), RFC 5764 and RFC 5705 (DTLS-SRTP and the exporter), RFC 8446
section 4.1.3 (the downgrade sentinel, so it is never sent), RFC 8122, RFC
8827 and RFC 8842 (fingerprints and roles), and RFC 9147 only to recognise a
DTLS 1.3 offer. No source of another TLS or DTLS implementation was read.
OpenSSL 4 is used only as a black box.

## The profile

Everything outside it is refused with a fatal alert and a typed
`Error`.

- DTLS 1.2, server role. A hello that also offers 1.3 is answered with 1.2;
  `supported_versions` and `key_share` are ignored, and the server random
  never ends in a TLS 1.3 downgrade sentinel.
- `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`, P-256, `ecdsa_secp256r1_sha256`.
- The extended master secret is required. `renegotiation_info` or the SCSV
  is required; renegotiation is refused with a `no_renegotiation` warning.
- `use_srtp`: `SRTP_AEAD_AES_128_GCM` if offered, else
  `SRTP_AES128_CM_HMAC_SHA1_80`. A non-empty `srtp_mki` is refused.
- Mutual certificates. Ours is a self-signed ECDSA P-256 certificate made
  once per process (`Identity`), hand-encoded DER valid from a day before
  start to a year after. The client's is accepted only if the SHA-256 of its
  DER equals one of the one to four fingerprints the caller passes from the
  offer (`Fingerprint`; RFC 8122 allows several). The comparison is made
  before the DER is parsed, in constant time against every candidate, and
  nothing can turn it off. Dates and issuers are not checked (RFC 8122,
  RFC 8827). A client with only an RSA key sends an empty Certificate
  (`NoCertificate`); an RSA certificate that is sent is
  `UnsupportedCertificate`.
- A HelloVerifyRequest carries version DTLS 1.0, as RFC 6347 section 4.2.1
  says DTLS 1.2 servers should.
- No session resumption, no application data (counted and dropped), no
  compression.

## Shape

Sans-IO: no sockets, clocks or threads. `unsafe` is denied, and outside
tests `unwrap`, `expect`, `panic!` and indexing are denied by lint.

```rust
use sapflow_dtls::{Config, Event, Fingerprint, Identity, Server};
use std::sync::Arc;

// Once per process; the fingerprint goes in every SDP answer.
let identity = Arc::new(Identity::generate(unix_time)?);
// Per session, with the offer's a=fingerprint values (one to four).
let peer = Fingerprint::parse_sdp(offer_fingerprint).ok_or(BadOffer)?;
let mut dtls = Server::new(Config::default(), identity.clone(), &[peer])?;

dtls.handle_datagram(now, &datagram)?;      // Err: failed, alert queued
while let Some(d) = dtls.poll_transmit() { send(d) }
if let Some(at) = dtls.poll_timeout() { /* call handle_timeout(at) then */ }
if let Some(Event::Connected(c)) = dtls.poll_event() {
    // c.profile, c.peer_fingerprint, and c.keying_material for
    // sapflow_rtp::srtp::split_keying_material.
}
```

| Module | What |
|--------|------|
| `record` | Record framing, the 48-bit sequence number, epoch 1 AES-128-GCM (explicit nonce read from the wire, 13-byte AAD over the plaintext length) and the 64-record replay window, applied to epoch 1 only |
| `message` | The handshake header and fragments; parsers for ClientHello (with every extension the profile reads), Certificate, ClientKeyExchange, CertificateVerify and Finished; builders for our flight; negotiation |
| `reassembly` | Fragment reassembly with caps on message size, coverage-adding fragments per message and bytes buffered, checked before allocation; overlapping and out-of-order fragments; duplicates dropped without counting; a conflicting fragment for the same seq kept as one of at most four candidates, so it does not block the real message while the buffer has room; a candidate over the fragment cap dropped when the seq has another; the first-started complete candidate released |
| `flight` | Our flights kept as plaintext and re-recorded on each send (new sequence numbers and nonces), fragmented to the MTU |
| `crypto` | The TLS 1.2 PRF, extended master secret, key block, Finished and the RFC 5705 exporter |
| `identity` | Our certificate, fingerprints, the peer's P-256 key |
| `server` | The state machine, timers, retransmission and the bounds below |

## Bounds and defaults

| Setting | Default | Range |
|---|---|---|
| `mtu` (our datagrams) | 1200 | 256 to 16384 |
| `max_datagram` (accepted) | 1500 | 256 to 65535 |
| `initial_retransmit` | 1 s | 10 ms up to `max_retransmit` |
| `max_retransmit` | 60 s | up to 600 s |
| `min_resend_interval` (peer-triggered resends) | 200 ms | up to `max_retransmit` |
| `max_transmissions` (per flight, any trigger) | 6 | 1 to 32 |
| `hello_verify` | off | |
| `max_message_len` | 8192 | 512 to 65536 |
| `max_fragments` (per message) | 32 | 1 to 1024 |
| `max_buffered` (all reassembly) | 16384 | `max_message_len` to 1 MiB |

Fixed: at most 4 messages buffered ahead, 4 certificates in the peer's
chain, 16 datagrams queued for sending. One input datagram produces at most
one flight and one alert. A peer's retransmission triggers a resend only if
it repeats the start of the hello we answered (offset 0, same length, the
version and random equal) or, after the handshake, its authenticated
Finished arrives again; at most once per datagram, at least
`min_resend_interval` after the last send, and within `max_transmissions`.
One ECDSA signature, one ECDH and one verification per session; a fatal
alert is sent once, then nothing.

With the defaults an unanswered flight 4 is sent at 0, 1, 3, 7, 15 and 31 s
and the handshake fails at about 63 s, so the caller (W3) sets its own,
shorter handshake deadline.

The cookie exchange is off by default: the server is used only behind ICE,
which proves return routability for the address pair before any DTLS
datagram is accepted, and that is the cookie's purpose (RFC 6347 section
4.2.1 allows turning it off where amplification is not a problem). It saves
a round trip. Turning it on works (A2.1 runs it against OpenSSL).

## Dependencies

Only RustCrypto, pinned exactly: `p256` 0.14.0, `ecdsa` 0.17.0,
`aes-gcm` 0.11.1, `sha2` 0.11.1, `hmac` 0.13.0, `x509-cert` 0.3.0 and
`der` 0.8.3, with `elliptic-curve` held at 0.14.3 in the lockfiles. This
major line of the elliptic-curve crates was released in mid-2026. It is
young and has no published audit. If that matters, the fallback the design
names is `ring` (`docs/whip-ingest.md`, Cryptographic primitives), which
would bring C and assembly into the server build and needs its own
decision.

The resolver picked `elliptic-curve` 0.14.1 by default, because the vendored
nostr crates declare `rust-version` 1.85 and 0.14.3 needs 1.87. Pinning
0.14.3 with `cargo update --precise` is accepted, since the pinned toolchain
is 1.94.

## Evidence

- A2.1: `tests/openssl_interop.sh` runs OpenSSL 4 `s_client -dtls1_2` against
  `examples/udp_server.rs` on loopback for both SRTP profiles, with small
  MTUs, a dropped first or final flight and the cookie exchange, and
  compares the exported
  keying material. `tests/fixtures/openssl-keymatexport.txt` is the
  transcript; `tests/fixtures/openssl/` holds each run's datagrams and
  OpenSSL's key log, which the unit tests replay offline byte for byte.
- A2.2: every W0 ClientHello capture gets a correct first flight, then times
  out cleanly (no browser key to continue); one test per malformed,
  oversized, out-of-order and retransmitted case.
- A2.3: a mismatched fingerprint is refused with `bad_certificate`.
- A2.4: fuzz targets `dtls_record`, `dtls_server` and
  `dtls_handshake_messages` (`fuzz/`).

`bash crates/sapflow-dtls/tests/check.sh [target]` runs formatting, clippy
three ways, the tests and the fuzz crate check.
