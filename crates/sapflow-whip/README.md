# sapflow-whip

sapflow's own receive-only WHIP ingest (`docs/whip-ingest.md` at the
repository root), sans-IO: STUN and ICE-lite, SDP offer parsing and answer
building, RFC 4571 framing, the WHIP resource rules, and a session that
composes ICE, `sapflow-dtls`, SRTP and the `sapflow-rtp` receiver. MIT,
like the rest of sapflow.

Written from the RFCs only: RFC 8489 and RFC 5769 (STUN and its vectors),
RFC 8445 and RFC 7675 (ICE-lite and consent), RFC 7983 and RFC 9443
(demultiplexing), RFC 8866, RFC 8829, RFC 8839, RFC 8843 and RFC 8842 (SDP,
JSEP, ICE and BUNDLE in SDP, DTLS roles), RFC 8122 (fingerprints), RFC 4571
and RFC 6544 (TCP framing, passive ICE-TCP), RFC 6184 and RFC 7587 (H.264
and Opus SDP parameters), RFC 4585 and RFC 4588 (feedback, RTX) and RFC 9725
(WHIP). No source of another WebRTC, ICE, STUN or SDP implementation was
read. Browsers are run only as black boxes.

The HTTP listener, the sockets and the wiring into publications are W3c's;
PROXY protocol and the TCP listener are W3d's. Nothing here opens a socket,
except the test-only example harness.

## The rules

- **Nothing to a stranger.** A STUN request is answered only if its
  USERNAME is exactly `<our ufrag>:<their ufrag>`, its FINGERPRINT checks
  and its MESSAGE-INTEGRITY verifies under our password. Anything else gets
  no reply at all: no 400 or 401, unlike RFC 8489 section 9.1.3. 420 (with
  UNKNOWN-ATTRIBUTES) and 487 go only to authenticated requests. A response
  is never larger than its request; one that would be is not sent.
- **Roles.** A lite agent is always controlled: a request with
  ICE-CONTROLLED gets 487 Role Conflict (RFC 8445 section 7.3.1.1).
- **Nomination.** A successful check validates its tuple. The selected
  tuple is set and moved only by a successful check with USE-CANDIDATE
  (RFC 8445 section 8.2: a lite agent sends media only on a nominated
  pair). The first check is usually not a nominating one: W0 saw
  USE-CANDIDATE on 5 of 6 checks from Chrome and Safari and 2 of 3 from
  OBS.
- **DTLS only from the selected tuple.** `sapflow-dtls` is fed only
  datagrams from the selected tuple. Up to four that arrive from a
  validated tuple before its USE-CANDIDATE check are held and fed if that
  tuple is then selected; Chrome 157 did this in every gate 2 run (two
  ClientHello fragments). Everything else is dropped and counted.
- **SRTP from any validated tuple.** RFC 8445 section 12 has an agent
  receive media on any valid pair. Chrome 157 nominated a second pair in
  each gate 2 run and kept sending on the first for a moment; dropping
  that lost audio. SRTP authenticates every packet, so SRTP and SRTCP from
  a validated tuple are taken. Everything the session sends, DTLS and RTCP,
  goes only to the selected tuple.
- **Consent** expires 30 s after the last successful check on the selected
  tuple (RFC 7675).
- **Keys.** SRTP comes from the DTLS-SRTP exporter through
  `split_keying_material` as the DTLS server: the client's write keys
  unprotect what we receive, the server's protect our RTCP.
- **Streams** are matched by the offer's SSRCs and `ssrc-group:FID`, then
  by the MID header extension, then by payload type (the receiver binds by
  payload type). A packet naming the MID of a section answered with port 0,
  or an unknown MID, is dropped and counted.
- **Keyframes.** A PLI (or FIR) as soon as SRTP is up, then every
  `keyframe_interval` (2 s).
- **Ending.** No ICE and DTLS 10 s after creation, consent expired, DTLS
  failed or closed by the peer, or `close`: the session ends with a typed
  `EndReason` and sends nothing more.

## SDP

An offer is accepted only with one BUNDLE group, `a=mid`, `a=rtcp-mux`, ICE
credentials, one to four `sha-256` fingerprints (all passed to
`sapflow-dtls`; other hash functions are ignored, so an offer with only
those is refused), `a=setup:actpass` or `active`, and no `a=ice-lite`.
Session-level values are inherited and a section's own win; `a=group:LS`
is ignored; extmap direction suffixes are accepted; candidates are skipped
unread and the peer's `ice-pwd` is checked and dropped, so an `Offer` holds
no address and no password.

The answer accepts the first sending video and audio section in the
BUNDLE group: H.264 with `packetization-mode=1` and a Constrained Baseline,
Baseline, Main or High profile in offer order, its RTX by `apt`, and only
the `nack`, `nack pli`, `ccm fir` and `transport-cc` the offer lists;
Opus with `stereo=1`, `maxaveragebitrate` from configuration and
`usedtx=0`; only the `sdes:mid` and transport-wide-cc extensions, whose IDs
must agree across the bundle. Other sections get port 0. The answer has
`a=ice-lite`, our credentials, the configured candidates (UDP host and
`tcptype passive` TCP) in its tagged section, `a=setup:passive` and our
fingerprint.

Refusals are typed: `Refusal::Malformed` (does not parse, HTTP 400) or
`Refusal::Profile` (well formed, outside the profile, HTTP 422).

## Shape

Sans-IO: no sockets, clocks or threads. `unsafe` is denied, and outside
tests `unwrap`, `expect`, `panic!` and indexing are denied by lint.

```rust
use sapflow_whip::{Credentials, Event, Session, SessionConfig, Tuple};
use sapflow_whip::sdp::{self, AnswerConfig, Offer};

// POST: parse, answer, make the session.
let offer = Offer::parse(&body).map_err(sdp::Refusal::from)?;    // 400
let creds = Credentials::generate()?;
let (answer, negotiated) = sdp::answer(&offer, &AnswerConfig {
    credentials: &creds,
    fingerprint: identity.fingerprint(),
    candidates: &candidates,
    opus_max_bitrate: 128_000,
    origin,
})?;                                                              // 422
let mut s = Session::new(SessionConfig::default(), identity.clone(), creds, negotiated, now)?;

// The UDP owner: a check from a new address names the session by
// s.local_ufrag() (stun::request_username); afterwards s.accepts(&tuple).
s.handle_datagram(now, Tuple::udp(from), &datagram);
while let Some(t) = s.poll_transmit() { send(t.tuple, &t.bytes) }
while let Some(e) = s.poll_event() {
    // Nominated, Connected(profile), Media(sapflow_rtp::Event), Ended(reason)
}
if let Some(at) = s.poll_timeout() { /* call s.handle_timeout(at) then */ }
// s.state(), s.selected(), s.last_media() (for SAPFLOW_WHIP_STALE), s.close()
```

| Module | What |
|--------|------|
| `stun` | Message parsing and building; USERNAME, MESSAGE-INTEGRITY, FINGERPRINT (own CRC-32 table), XOR-MAPPED-ADDRESS (IPv4 and IPv6), PRIORITY, USE-CANDIDATE, ICE-CONTROLLING, ICE-CONTROLLED, ERROR-CODE, UNKNOWN-ATTRIBUTES; unknown comprehension-optional attributes skipped, comprehension-required ones listed |
| `ice` | Credentials, the lite agent, tuples (`Transport` and address; `Debug` leaves the address out) and the first-byte demultiplexer |
| `sdp` | `Offer::parse` within `Limits`, `answer`, `Negotiated` |
| `framing` | RFC 4571 `Decoder` and `encode`, 1500 bytes a frame |
| `resource` | Bearer token split at the first colon, 128-bit session IDs, `/whip/session/<id>`, `Location` from an absolute public base URL, every refusal and its status (400, 401, 403, 409, 415, 422, 429, 503) |
| `session` | `Session` |

## Bounds and defaults

| Setting | Default | Range |
|---|---|---|
| `SessionConfig::handshake_timeout` | 10 s | 1 to 120 s |
| `SessionConfig::keyframe_interval` | 2 s | 100 ms and up, or none |
| `SessionConfig::max_datagram` (datagram or frame taken) | 1500 | 256 to 65535 |
| `SessionConfig::max_early_dtls` | 4 | 0 to 16 |
| `SessionConfig::max_queued` (output) | 64 | 16 to 1024 |
| `IceConfig::max_tuples` (validated) | 8 | 1 to 64 |
| `IceConfig::consent_timeout` | 30 s | 1 to 300 s |
| `sdp::Limits` | 16 KiB offer, 1024 lines of 1024 bytes, 16 sections, 64 formats, 16 SSRCs a section | |
| `Framing` | 1500 bytes a frame, 64 KiB buffered | frame 1 to 65535, buffer to 1 MiB |
| Opus `maxaveragebitrate` | as configured | held to 6000 to 510000 |

Fixed: 32 attributes a STUN message, 8 unknown types listed, 513-byte
USERNAME, 8 candidates in an answer, 16 SSRCs remembered from MID matching,
32 session events queued (media events wait in the receiver, within its
own bound). `SessionConfig` also carries `sapflow-dtls`'s `Config`,
`sapflow-rtp`'s SRTP `Config` (1024-packet replay windows, 8 streams) and
the `ReceiverConfig` (reorder, NACK, start hold, reports, transport-cc and
events), each with its own stated bounds.

## Dependencies

No crate that the workspace did not already pin: `hmac` 0.13.0 and `sha1`
0.11.0 for MESSAGE-INTEGRITY (`sapflow-rtp` pins both for SRTP's
HMAC-SHA1; `sha1` 0.11 is the release on `digest` 0.11, which `hmac` 0.13
uses), `subtle` 2.6.1 for the constant-time comparison, `zeroize` 1.9.1 and
`getrandom` 0.4.3, all exact. CRC-32 is a table of our own.

## Evidence

- RFC 5769: all four vectors at codec level. The sample request carries
  ICE-CONTROLLED, and the long-term one REALM and NONCE, so neither goes
  through the agent; the long-term key, MD5 of the vector's username, realm
  and password, was worked out once and is a constant.
- SDP: a test per W0 offer (Chrome, Firefox, Safari on macOS and iOS, OBS,
  FFmpeg with and without `dtls_active`, Vanadium), checking every accepted
  value and the answer line by line, plus synthetic refusals for each
  `ParseError` and `ProfileError`.
- Gate 1, in process, for `SRTP_AEAD_AES_128_GCM` and
  `SRTP_AES128_CM_HMAC_SHA1_80`: the test STUN client, the `sapflow-dtls`
  test client and `sapflow-rtp`'s SRTP protect publish H.264 (STAP-A and
  FU-A) and Opus; the session emits exactly the access units and audio
  sent, and the client unprotects its PLI, receiver reports, SDES and
  transport-cc. Also: DTLS from an unvalidated tuple ignored, unauthenticated
  STUN drawing zero bytes, 487, USE-CANDIDATE moving the tuple, consent
  expiry, the handshake deadline, a fingerprint mismatch, held DTLS, MID
  matching, RTX repair, close.
- Gate 2: headless Google Chrome 157 with fake media published to
  `examples/whip_harness.rs` on loopback and reached every milestone
  (`tests/browser_gate.js`). `tests/fixtures/chrome-157/` keeps that run's
  offer (no candidates or addresses), its DTLS datagrams both ways, the
  peer's leaf certificate and its first SRTP and SRTCP packets; a unit test
  replays them offline, our flights byte for byte, and authenticates
  Chrome's GCM SRTP. Firefox was not run: it is not installed on the test
  host.
- Fuzz targets `stun_message`, `sdp_offer`, `rfc4571_frames` and
  `whip_session` (`fuzz/`), seeded from the RFC 5769 vectors, the W0 offers
  and TCP captures, the recorded Chrome offer and scripted sessions. A unit
  test replays every seed to its stage, and a fixed-seed mutation pass runs
  with the unit tests.

`bash crates/sapflow-whip/tests/check.sh [target]` runs formatting, clippy
three ways, the tests and the fuzz crate check. The browser gate:

```sh
cargo build -p sapflow-whip --features testing --example whip_harness
node crates/sapflow-whip/tests/browser_gate.js \
  --bin target/debug/examples/whip_harness --http 47201 --udp 47202 [--record <dir>]
```
