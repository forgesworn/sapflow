# sapflow-rtp

sapflow's own RTP, RTCP, SRTP and SRTCP, and the receive-side media
handling behind them, for receive-only WHIP ingest (`docs/whip-ingest.md`
at the repository root). MIT, like the rest of sapflow.

Written from the RFCs only: RFC 3550 and RFC 3551 (RTP and RTCP), RFC 4585
and RFC 5104 (feedback), RFC 5506 (reduced-size RTCP), RFC 8285 (header
extensions), RFC 3711 (SRTP), RFC 7714 (AES-GCM), RFC 5764 section 4.2
(DTLS-SRTP keys), RFC 6184 (H.264), RFC 7587 and RFC 6716 sections 3.1 to
3.4 (Opus) and RFC 4588 (RTX). No source of another RTP, RTCP or SRTP
implementation, depacketiser, jitter buffer or WebRTC stack was read.
FFmpeg is used only as a black box, to capture test traffic and to decode
the Opus silence frames.

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
| `h264` | RFC 6184 packetisation mode 1: single NAL units, STAP-A and FU-A into access units by timestamp and marker, with 4-byte length prefixes, SPS and PPS taken out and an IDR flag. STAP-B, MTAP and FU-B refused. An access unit that lost anything is reported incomplete, never delivered with holes |
| `opus` | The RFC 6716 TOC byte and packet framing (codes 0 to 3, padding, the 120 ms bound), constant encoded-silence packets, and a depacketiser that fills DTX and loss with silence of the stream's channel layout, marked apart |
| `rtx` | RFC 4588 payloads: the OSN, and padding-only probes |
| `receive` | The `Receiver`: per stream, a bounded reorder buffer, NACKs and RTT, RTX or same-SSRC retransmissions, keyframe requests (PLI or FIR) and the depacketisers, giving events and feedback |

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

## Receiving media

```rust
use sapflow_rtp::receive::{Codec, Event, Receiver, ReceiverConfig, RtxConfig, StreamConfig};

let mut rx = Receiver::new(ReceiverConfig::default(), now);
let mut video = StreamConfig::new(Codec::H264, 96);
video.rtx = Some(RtxConfig { payload_type: 97, ssrc: None });
(video.nack, video.pli) = (true, true);
let video = rx.add_stream(video)?;
let audio = rx.add_stream(StreamConfig::new(Codec::Opus, 111))?;

// For each authenticated RTP packet, and whenever poll_timeout() says:
rx.handle_rtp(now, &packet)?;
rx.handle_timeout(now);
while let Some(event) = rx.poll_event() { /* AccessUnit, Audio, Lost, ... */ }
while let Some(feedback) = rx.poll_feedback() { feedback.write(&mut rtcp, our_ssrc)?; }
```

Packets are routed by SSRC, or by payload type until an SSRC is bound.
Each stream keeps:

- **Reorder buffer.** Packets come out in sequence order. A gap waits up
  to `jitter` (200 ms, `SAPFLOW_WHIP_JITTER`) from when it was seen, then
  is declared lost (`Event::Lost`, with its sequence numbers). The first
  packets are held for `reorder_delay` (10 ms) in case earlier ones arrive
  late. Duplicates and packets older than what was released are dropped.
  A jump of more than `max_dropout` (3000) either way is a restart only
  once the next packet follows it; otherwise it is set aside. When the
  buffer is full (`max_packets` 1024, `max_bytes` 1 536 000) it releases
  from the front, declaring what is missing there lost, and a payload
  larger than `max_bytes` is refused.
- **NACK.** A gap is NACKed after `reorder_delay`, then every RTT and a
  quarter (at least 5 ms) while an answer can still arrive before the
  deadline, at most `max_nacks` (4) times. All due numbers for a stream go
  in one NACK, oldest first, and NACK bytes for the whole receiver are held
  to `nack_bitrate` (64 kbit/s) with a `nack_burst` (1500 bytes)
  allowance. RTT starts at 100 ms and is measured from a NACK to the
  retransmission of a packet NACKed once, from RTX only where RTX was
  negotiated (a late original would read as a short RTT).
- **Keyframes.** A PLI (or FIR, when only `ccm fir` was negotiated) on the
  first packet, every `keyframe_interval` (2 s), at once after
  unrecoverable loss, and every `keyframe_retry` (1 s) while waiting for an
  IDR, never closer than `keyframe_min_interval` (500 ms). Complete frames
  before the next IDR are dropped and reported (`Event::FrameSkipped`).
- **H.264 limits.** 8 MiB per access unit, 4 MiB per NAL unit (the FU-A
  reassembly buffer), 256 NAL units per access unit.
- **Opus.** A timestamp gap with no sequence gap is DTX, one after a loss
  is loss; both are filled with silence shaped like the sender's last
  packet, up to `max_fill` (2 s). A longer gap, or a step back, is a
  discontinuity and is not filled. A gap under 2.5 ms (the smallest CELT
  frame) is left as a short timestamp step.

Each access unit, after a gap between one frame's marker and the next
packet, is kept only if its first NAL unit begins an access unit: an access
unit delimiter, an SPS, a slice with `first_mb_in_slice` 0, or an IDR slice
when the stream's IDRs do not carry an SPS. Otherwise it is reported
incomplete (`StartLost`), since the lost packets may have been its start.

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
- A1.2 (`src/receive/tests.rs`), on H.264 and Opus that FFmpeg sent as
  plain RTP, crossing the sequence wrap. Lossless, the access units are
  exactly the NAL units FFmpeg wrote as Annex B from the same encode. With
  a marker packet, a frame's first packet, an FU-A start, middle and end, a
  whole frame, both sides of the wrap and Opus packets dropped, plus
  reordering and duplicates: when every retransmission arrives (over RTX,
  or on the original SSRC as OBS sends it), the output is exactly the
  lossless output, every dropped packet was NACKed and nothing reordered
  inside the reorder delay was. With retransmissions withheld, loss is
  reported for exactly the withheld packets, incomplete frames are exactly
  those that lost some of their packets, every other frame is delivered
  unchanged or skipped until the next IDR, and Opus loss silence covers
  exactly the withheld packets' time.
- The Opus silence packets decode to exact zeros with both of FFmpeg's
  decoders, alone and after a tone (`tests/fixtures/silence-check.txt`).
- Fuzz targets `rtp_packet`, `rtcp_compound`, `srtp_unprotect`,
  `srtcp_unprotect`, `h264_depacketise`, `opus_depacketise` and
  `reorder_nack` (`fuzz/README.md`). A unit test replays every seed
  through the same entry functions.
