# sapflow-rtp

sapflow's own RTP, RTCP, SRTP and SRTCP, for receive-only WHIP ingest
(`docs/whip-ingest.md` at the repository root). MIT, like the rest of
sapflow.

Written from the RFCs only: RFC 3550 and RFC 3551 (RTP and RTCP), RFC 4585
and RFC 5104 (feedback), RFC 5506 (reduced-size RTCP), RFC 8285 (header
extensions), RFC 3711 (SRTP), RFC 7714 (AES-GCM) and RFC 5764 section 4.2
(DTLS-SRTP keys). No source of another RTP, RTCP or SRTP implementation was
read. FFmpeg is used only as a black box, to capture test traffic.

## Shape

Sans-IO: no sockets, clocks or threads. `unsafe` is denied, and outside
tests `unwrap`, `expect`, `panic!` and indexing are denied by lint. The
only dependencies are pinned RustCrypto primitives (`aes`, `ctr`,
`aes-gcm`, `hmac`, `sha1`), `subtle` and `zeroize`.

| Module | What |
|--------|------|
| `rtp` | A zero-copy, checked view of an RTP packet: fixed header, CSRCs, padding removed and validated, RFC 8285 one-byte and two-byte extensions with lookup by ID |
| `rtcp` | Compound and reduced-size RTCP: SR and RR with report blocks, generic NACK, PLI and FIR; SDES, BYE, APP, REMB, transport-cc and unknown types length-checked and skipped |
| `rtcp::build` | What sapflow sends: RR, SDES CNAME, NACK (losses coalesced into PID and BLP pairs, across the sequence wrap), PLI, FIR, and a compound packet that starts with the RR |
| `srtp` | `SRTP_AES128_CM_HMAC_SHA1_80` and `SRTP_AEAD_AES_128_GCM`: RFC 3711 key derivation, DTLS-SRTP keying material split by role, unprotect of RTP and RTCP, protect of RTCP, ROC and index estimation, per-SSRC replay windows (1024 by default, configurable), the SRTCP E flag and 31-bit index. No MKI |

```rust
use sapflow_rtp::{Config, Inbound, Outbound, Profile, Role, RtpPacket, split_keying_material};

// From the DTLS handshake: the profile and the exporter output.
let keys = split_keying_material(Profile::AeadAes128Gcm, Role::Server, &material)?;
let mut inbound = Inbound::new(&keys.remote, Config::default())?;
let mut outbound = Outbound::new(&keys.local)?;

// For each SRTP datagram, in place:
let len = inbound.unprotect_rtp(&mut datagram)?;
let packet = RtpPacket::parse(&datagram[..len])?;
```

Stream slots are bounded and never freed: 8 SSRCs for RTP and 8 for RTCP
by default (`Config::max_streams`, up to 64). An SSRC takes a slot with its
first authenticated packet and keeps it, so a ninth distinct SSRC is
refused (`SrtpError::TooManyStreams`) for the rest of the session. Freeing
a slot would forget that SSRC's replay window and let its old packets be
replayed.

GCM key derivation pads the 96-bit master salt with two zero octets on the
right, as RFC 7714 section 11 says after erratum 4938. No published vector
covers this end to end; the first check against a real GCM peer is the DTLS
stage (W2).

Packets are processed in place in the caller's buffer, and the unprotect
paths allocate nothing after the context is built. Tags are compared in
constant time. Keys, salts and cipher schedules are zeroed on drop.

## Tests

- RFC 3711 Appendix B.2 (AES-CM keystream) and B.3 (key derivation), and
  every RFC 7714 section 16 and 17 vector: the AES-128 ones through the
  real contexts, the AES-256 ones through the same IV, AAD and layout code
  (AES-256 is not an offered profile).
- Round trips for both profiles, ROC rollover across sequence wraps,
  replay (duplicate refused, too old refused, late never-received
  accepted), tampered tags and headers, the SRTCP E flag and index, the
  role split, and every prefix and bit flip of the parsers' inputs.
- FFmpeg 8.1.2 as a black box: the SRTP and SRTCP it sends under
  `AES_CM_128_HMAC_SHA1_80` unprotect with our code, and our protect gives
  back its exact bytes (`ffmpeg_srtp_interop`; the fixture and how it was
  made are in `tests/fixtures/README.md`).
- Fuzz targets `rtp_packet`, `rtcp_compound`, `srtp_unprotect` and
  `srtcp_unprotect` (`fuzz/README.md`). A unit test replays every seed
  through the same entry functions.
