# sapflow-rtp

sapflow's own RTP, RTCP, SRTP and SRTCP, and the receive-side media
handling behind them, for receive-only WHIP ingest (`docs/whip-ingest.md`
at the repository root). MIT, like the rest of sapflow.

Written from the RFCs and the H.264 specification only: RFC 3550 and RFC
3551 (RTP and RTCP), RFC 4585 and RFC 5104 (feedback), RFC 5506
(reduced-size RTCP), RFC 8285 (header extensions), RFC 3711 (SRTP), RFC 7714
(AES-GCM), RFC 5764 section 4.2 (DTLS-SRTP keys), RFC 6184 (H.264 payload)
with ITU-T H.264 for the NAL unit types and the start of the slice header,
RFC 7587 and RFC 6716 sections 3.1 to 3.4 and 4.3 (Opus payload, framing and
the CELT silence flag), RFC 4588 (RTX) and
draft-holmer-rmcat-transport-wide-cc-extensions-01 (transport-cc). No source of
another RTP, RTCP or SRTP implementation, depacketiser, jitter buffer,
congestion controller or WebRTC stack was read.
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
| `rtcp::build` | What sapflow sends: RR, SDES CNAME, NACK (losses coalesced into PID and BLP pairs, across the sequence wrap), PLI, FIR, transport-cc, and a compound packet that starts with the RR |
| `srtp` | `SRTP_AES128_CM_HMAC_SHA1_80` and `SRTP_AEAD_AES_128_GCM`: RFC 3711 key derivation, DTLS-SRTP keying material split by role, unprotect of RTP and RTCP, protect of RTCP, ROC and index estimation, per-SSRC replay windows (1024 by default, configurable), the SRTCP E flag and 31-bit index. No MKI |
| `h264` | RFC 6184 packetisation mode 1: single NAL units, STAP-A and FU-A into access units by timestamp and marker, with 4-byte length prefixes, SPS and PPS taken out and an IDR flag. STAP-B, MTAP and FU-B refused. An access unit that lost anything is reported incomplete, never delivered with holes |
| `opus` | The RFC 6716 TOC byte and packet framing (codes 0 to 3, padding, the 120 ms bound), constant encoded-silence packets, and a depacketiser that fills DTX and loss with silence of the stream's channel layout, marked apart |
| `rtx` | RFC 4588 payloads: the OSN, and padding-only probes |
| `twcc` | Transport-wide congestion control: arrival times recorded once per transport-wide sequence number in a bounded window, encoded as RTPFB FMT 15 feedback (run-length and one- and two-bit vector chunks, 250 us deltas, 64 ms reference time) at a fixed cadence and split at a size cap |
| `timeline` | One 90 kHz timeline: a start hold for every stream's first sender report (1.5 s at most, bounded in bytes), then alignment by sender reports or by arrival time, timestamps that strictly increase and stay within a lead of arrival time, and later reports slewed in at up to 500 ppm |
| `receive` | The `Receiver`: per stream, a bounded reorder buffer, NACKs and RTT, RTX or same-SSRC retransmissions, keyframe requests (PLI or FIR), the depacketisers and receiver report statistics (RFC 3550 A.3 and A.8, each packet counted once); for the session, transport-cc and the timeline; giving events stamped on the timeline, and RTCP datagrams that start with the receiver report |

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

// For each authenticated RTP and RTCP datagram, and whenever
// poll_timeout() says:
rx.handle_rtp(now, &packet)?;
rx.handle_rtcp(now, &compound);
rx.handle_timeout(now);
while let Some(event) = rx.poll_event() { /* Aligned, AccessUnit { pts, .. }, Audio, Lost, ... */ }
while rx.poll_feedback(our_ssrc, cname, &mut rtcp) { send(srtcp(&rtcp)); rtcp.clear(); }
```

Packets are routed by SSRC, or by payload type until an SSRC is bound.
Each stream keeps:

- **Reorder buffer.** Packets come out in sequence order. A gap waits up
  to `jitter` (200 ms, `SAPFLOW_WHIP_JITTER`) from when it was seen, then
  is declared lost (`Event::Lost`, with its sequence numbers). The first
  packets are held for `reorder_delay` (10 ms) in case earlier ones arrive
  late. Duplicates and packets older than what was released are dropped
  as late. A jump of more than `max_dropout` (3000, RFC 3550 A.1's
  `MAX_DROPOUT`) either way is a restart only once the next packet
  follows it; until then it is set aside, counted against the byte bound.
  A.1's `MAX_MISORDER` (100) is not used backward: late retransmissions
  are often further behind than that, and two in a row must not read as a
  restart. When
  the buffer is full (`max_packets` 1024, `max_bytes` 1 536 000) it
  releases from the front, declaring what is missing there lost, though
  never for a late packet, and a payload larger than `max_bytes` is
  refused.
- **NACK.** A gap is NACKed after `reorder_delay`, then every RTT and a
  quarter (at least 5 ms) until the deadline, at most `max_nacks` (4)
  times. All due numbers for a stream go
  in one NACK, oldest first, and NACK bytes for the whole receiver are held
  to `nack_bitrate` (64 kbit/s) with a `nack_burst` (1500 bytes)
  allowance. RTT starts at 100 ms and is measured from a NACK to the
  retransmission of a packet NACKed once, from RTX only where RTX was
  negotiated (a late original would read as a short RTT).
- **Keyframes.** A PLI (or FIR, when only `ccm fir` was negotiated) when
  the server calls `request_keyframe` as SRTP comes up, on the first
  packet as a fallback, every `keyframe_interval` (2 s), at once after
  unrecoverable loss, and every `keyframe_retry` (1 s) while waiting for an
  IDR, never closer than `keyframe_min_interval` (500 ms). Complete frames
  before the next IDR are dropped and reported (`Event::FrameSkipped`).
- **H.264 limits.** 8 MiB per access unit, 4 MiB per NAL unit (the FU-A
  reassembly buffer), 256 NAL units per access unit. Buffers grow by
  doubling but stop at the bound, so these are bounds on capacity, not
  only length.
- **Opus.** A timestamp gap with no sequence gap is DTX, one after a loss
  is loss; both are filled with silence shaped like the sender's last
  packet, up to `max_fill` (2 s). Since the sender chooses its
  timestamps, fill is also bounded by arrival time: a gap may exceed the
  time between its two packets' arrivals by at most `allowance` (200 ms),
  and total fill may run ahead of arrival time by at most `max_fill` and
  `allowance` together. A gap beyond any of these, or a step back, is a
  discontinuity and is not filled. A gap under 2.5 ms (the smallest CELT
  frame) is left as a short timestamp step.
- **Events.** At most `max_events` (8192) are queued at once, all
  streams together; beyond, they are dropped and counted
  (`StreamStats::events_dropped`), and a video stream that lost one waits
  for an IDR. Draining after every call never reaches it.
- **Report block.** RFC 3550 A.3 and A.8 for the media SSRC: each sequence
  number counted once within 4096 of the highest, so copies and needless
  resends never make loss negative; RTX not counted (RFC 4588 section 6.2),
  so loss is the network's before repair, which rate control needs (RFC
  4588 section 7), while OBS's same-SSRC resends count as late arrivals,
  so its loss is after repair; no blocks for RTX streams; jitter only from
  first arrivals never NACKed; LSR and DLSR from its last sender report.

For the session:

- **RTCP out.** `poll_feedback` writes datagrams of at most `max_datagram`
  (1200) bytes, a hard cap from 436 bytes (feedback is sized to fit after
  the largest report and CNAME): a receiver report first (with blocks every `interval`, 1 s
  fixed, the first at 0.5 s; empty between), the SDES CNAME, then NACK,
  PLI, FIR and transport-cc. With `reduced_size`, feedback between reports
  goes alone. A sender report for an SSRC not yet bound is kept until it is.
- **Transport-cc** (`transport_cc`, the `a=extmap` ID): every packet with
  the extension, RTX and probes included, recorded once within 4096
  sequence numbers; feedback every 50 ms, from where the last ended, in
  packets of at most `max_packet` (1000) bytes, at most `max_pending`
  (1024) packets waiting, at most 256 feedback packets queued.
- **Timeline.** Frames are held from the first media packet until every
  stream has a sender report, for at most `start_hold` (1.5 s) and
  `max_hold_bytes` (4 MiB) or half the event queue, then `Event::Aligned`
  says whether they were aligned by the reports or by arrival time and
  why. Every `AccessUnit` and `Audio` event carries `pts` on one 90 kHz
  timeline, which strictly increases and stays within `max_lead` (500 ms,
  plus 0.1% of elapsed time up to another 500 ms) of arrival time either
  way. Later sender reports are used in agreeing pairs at least
  `report_spacing` (500 ms) apart; errors within `max_lead` are slewed at
  up to `max_slew_ppm` (500 ppm), never backwards and with no event, so a
  sender's audio clock drifting against NTP is followed. Only a larger
  step forward, or a timestamp that jumps or falls behind arrival time by
  more than `max_lead`, is a discontinuity.

Each access unit, after a gap between one frame's marker and the next
packet, is kept only if its first NAL unit begins an access unit: an access
unit delimiter, an SPS, a slice with `first_mb_in_slice` 0, or an IDR slice
when the stream's IDRs do not carry an SPS. Otherwise it is reported
incomplete (`StartLost`), since the lost packets may have been its start.
The first access unit, and the first after a restart, is checked the same
way, with an IDR slice first never taken as a start: packets before the
first one received cannot be known or NACKed.

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
- Receiver report statistics worked by hand (fraction and cumulative
  loss, copies, the wrap, jitter, LSR and DLSR, the 24-bit clamp), and
  against the A1.2 impairments: over RTX the block reports exactly the
  originals dropped, on the original SSRC none.
- Transport-cc against the draft's chunk examples and hand-built cases:
  run-length against vectors, large and negative deltas, a delta past 16
  bits, the 24-bit reference wrap, the sequence wrap, reordering, copies,
  far jumps and splitting at the size cap.
- The timeline on the FFmpeg captures with sender reports synthesised
  from one clock (audio exactly 300 ms after video), the fallback to
  arrival time, audio only, hostile reports, and an hour of audio clock
  drift at 30 and 100 ppm either way (no discontinuity, A/V within 1 ms),
  forged alternating reports, a falling delay with reports, and a
  constant RTP timestamp.
- Fuzz targets `rtp_packet`, `rtcp_compound`, `srtp_unprotect`,
  `srtcp_unprotect`, `h264_depacketise`, `opus_depacketise`,
  `reorder_nack`, `rr_stats`, `twcc_feedback` and `sr_timeline`
  (`fuzz/README.md`). A unit test replays every seed through the same
  entry functions.
