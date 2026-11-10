# WHIP ingest (design)

Status: design only, 10 November 2026. Nothing here is built. WHIP stays off
unless configured, and it does not face the internet before the gates in
[Delivery and gates](#delivery-and-gates) pass.

WHIP (RFC 9725) lets a publisher send live media over WebRTC after a single
HTTP exchange. A browser can publish with no app installed, and OBS 30 and
later has a WHIP output. Sapflow would accept WHIP as a third way in, next to
SRT and RTMP, and serve the same Low-Latency HLS, recording and callbacks.

Why it is worth having:

- **No app on the phone.** A broadcaster opens a page, allows the camera and
  microphone and goes live. Camera, encoder and network handling are the
  browser's.
- **Networks that block non-web ports.** RTMP on 1935 and SRT on 8890 are
  often blocked on public Wi-Fi. WHIP signalling is HTTPS, and the media can
  use one fixed UDP port or TCP 443 (see [Connectivity](#connectivity)).
- **A shorter ingest wait.** The receiver waits a short bounded time for lost
  packets instead of SRT's latency window or RTMP's TCP buffering. Viewers
  still watch LL-HLS, so most of the delay a viewer sees is unchanged (see
  [Delay budget](#delay-budget)).

## Decision and how it fits the goals

On 10 November 2026 the user decided that sapflow carries its own WHIP
implementation under MIT, written as sans-IO crates like `sapflow-srt` and
`sapflow-rtmp`, instead of adopting an existing WebRTC stack or running a
separate gateway.

This departs from two lines in [GOALS.md](../GOALS.md), which this design
amends:

- "No RTSP, WebRTC, ..." becomes "WebRTC only as receive-only WHIP ingest".
- "Do not hand-roll hostile-input parsers without a reason": for SRT and RTMP
  the reason was that no maintained Rust choice existed. Maintained Rust
  WebRTC stacks do exist. The reasons here are different:
  - **Shape.** Sapflow's protocol code has no sockets, threads or clocks of its
    own, so the same crates run in the server, in tests driven packet by
    packet and, later, in the phone SDK. A general WebRTC stack brings its own
    runtime model.
  - **Narrow scope.** Receive-only, ICE-lite, one DTLS profile, two codecs.
    That is a small fraction of a general WebRTC stack, and everything outside
    it is refused at the door.
  - **No C or system crypto in the server.** The server today uses only
    RustCrypto primitives and denies `unsafe`. The design keeps both.
  - **Measured performance.** As with SRT, our own receiver can be profiled and
    tuned against references instead of carried as a black box.

The conditions that came with SRT and RTMP apply, and are gates here:

- **Clean room.** Written from the RFCs and wire observation only. Never read
  or copy the source of another WebRTC, DTLS or SRTP implementation (for
  example str0m, webrtc-rs, Pion, libwebrtc, libdatachannel or OpenSSL). Run
  them only as black boxes.
- **Maintained crates where parsing is not the protocol.** Cryptographic
  primitives, X.509/DER parsing, H.264 bitstream parsing (`h264-reader`) and
  MP4 boxes (`mp4-atom`) stay third-party.
- **A fuzz target for everything we parse**, before it faces the internet.
- **Differential tests against references**, run as black boxes (see W1 to W4).
- **An independent review of the DTLS and SRTP code**, by someone other than
  its author, before WHIP is enabled on a public deployment.

## Scope

In scope for the first version:

- Publish only (receive-only): one WHIP session feeds one stream path.
- ICE-lite (RFC 8445 section 2.5) with STUN (RFC 8489) and consent freshness
  (RFC 7675).
- One UDP port for every session, plus passive ICE-TCP candidates (RFC 6544,
  framed per RFC 4571) for a TCP listener that a front proxy can reach.
- DTLS 1.2 (RFC 6347) as the server only, in one cipher profile (see
  [The browser profile](#the-browser-profile)), with DTLS-SRTP key export
  (RFC 5764, RFC 5705).
- SRTP and SRTCP (RFC 3711) with AES-128-CM-HMAC-SHA1-80 and AEAD-AES-128-GCM
  (RFC 7714).
- RTP and RTCP (RFC 3550, RFC 4585): NACK with RTX (RFC 4588), PLI and FIR
  (RFC 5104), sender reports for timing, and transport-wide congestion control
  feedback if W0 shows it is needed (see [Congestion feedback](#congestion-feedback)).
- H.264 video (RFC 6184, packetisation mode 1) and Opus audio (RFC 7587).
  Video-and-audio, audio-only and video-only sessions, declared by the offer.
- WHIP `POST`, `DELETE` and `OPTIONS`.

Out of scope for the first version, and refused rather than half-supported:

- Playback over WebRTC (WHEP), simulcast, data channels.
- VP8, VP9, AV1, HEVC; any audio codec other than Opus.
- Trickle ICE and ICE restart: `PATCH` answers 405. A client that loses its
  path publishes again with a new `POST` (see [Reconnects](#reconnects)).
- TURN, full ICE, being the DTLS client, DTLS 1.3, RSA certificates.
- Transcoding. Sapflow packages what it receives.

## Architecture

```
publisher (browser, OBS)
  |  HTTPS POST offer / DELETE      front proxy (TLS)      SAPFLOW_WHIP_ADDR
  +-------------------------------->  ------------------>  signalling
  |                                                          | publish-auth (existing)
  |  UDP: STUN, DTLS, SRTP on one port                       v
  +---------------------------------------------------->  UDP owner task
  |  TCP 443: RFC 4571 frames                                |
  +--------------->  front proxy (layer 4) ------------>  TCP listener
                                                             |
                     session (sans-IO): ICE-lite -> DTLS -> SRTP -> RTP
                                                             |
                     depacketise -> EsIngest -> Muxer -> LL-HLS, recording
```

### Crates

| Crate | Holds | New or changed |
|---|---|---|
| `sapflow-rtp` | RTP and RTCP parsing and building, SRTP/SRTCP, H.264 and Opus depacketisers, RTX unwrapping, the reorder buffer, NACK, PLI/FIR and feedback generation, sender-report timing | New, sans-IO |
| `sapflow-dtls` | DTLS 1.2 server in the browser profile: records, handshake, retransmission, DTLS-SRTP key export, certificate fingerprints | New, sans-IO |
| `sapflow-whip` | ICE-lite and STUN, SDP offer parsing and answer building, the session that composes ICE, DTLS, SRTP and RTP, and the WHIP resource rules | New, sans-IO |
| `sapflow-hls` | An Opus audio track beside AAC (see [Opus](#opus)) | Changed |
| `sapflow-core` | `PublishRequest` and the protocol list gain WebRTC | Changed |
| `sapflow` (server) | `whip.rs`: signalling listener, UDP owner task, TCP listener, wiring into `Publication` and `EsIngest` | Changed |

Crate names are checked against crates.io before any publication, as in
[crates-io-plan.md](crates-io-plan.md). Nothing here changes which crates are
published.

STUN is small, and Spry has a STUN codec, but Spry is not published. So
`sapflow-whip` carries the subset it needs (Binding requests and responses,
USERNAME, MESSAGE-INTEGRITY, FINGERPRINT, XOR-MAPPED-ADDRESS, PRIORITY,
USE-CANDIDATE, ICE-CONTROLLING) and is tested with the RFC 5769 vectors. If
Spry's codec is later published as its own crate, switching to it is a
separate decision.

The sans-IO crates build for `wasm32` like the rest of the core. No WebAssembly
build is planned, because a browser publishes through its own WebRTC.

### Cryptographic primitives

The server today uses RustCrypto (`aes`, `ctr`, `sha1`, `sha2`, `hmac`).
`ring` and `rustls` appear in the workspace lockfile only through
`sapflow-bench` and `sapflow-signet-ffi`, not the server. The design adds
RustCrypto crates for the new primitives:

| Need | Crate |
|---|---|
| ECDHE and ECDSA on P-256 | `p256` with `ecdsa` |
| AES-128-GCM (DTLS records, SRTP GCM) | `aes-gcm` |
| AES-128 counter mode and HMAC-SHA1 (SRTP CM) | `aes`, `ctr`, `hmac`, `sha1` (already used) |
| PRF, transcript hashes, fingerprints | `sha2`, `hmac` (already used) |
| Peer certificate parsing, our self-signed certificate | `x509-cert` and `der` |
| Randomness | `getrandom` (already used) |

`ring` is the measured alternative if W4 shows AES-GCM or ECDSA cost matters.
Adopting it would add C and assembly to the server build, so it needs its own
decision.

## The browser profile

"The browser profile" means exactly what the target publishers offer, captured
from them in W0 and kept as fixtures, not recalled. The targets are Chrome on
desktop and Android, Safari on iOS and macOS, Firefox, and OBS's WHIP output.
The expected shape, to be confirmed by those captures:

- DTLS 1.2 with `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (RFC 5289), P-256
  key exchange (RFC 8422), `ecdsa_secp256r1_sha256` signatures, the extended
  master secret (RFC 7627) and the `use_srtp` extension.
- A peer certificate with a P-256 key, accepted only if its SHA-256 matches
  the offer's `a=fingerprint` (RFC 8122). Certificate dates and issuers are
  not checked, as in WebRTC generally.
- Sapflow answers `a=setup:passive` (RFC 8842) and acts as the DTLS server. An
  offer that can only be `passive` is refused.
- One BUNDLE group (RFC 9143) with `rtcp-mux`; `a=ice-lite`, `a=recvonly`.

An offer that does not fit is refused at `POST` with a plain reason (see
[Signalling](#signalling)), never accepted and then failed silently. If a
target's capture does not fit this profile, the profile changes in this
document before W2 starts.

A browser can stop offering DTLS 1.2. The record layer and handshake code are
kept separate so that adding DTLS 1.3 later does not mean rewriting SRTP key
handling. W0 records which DTLS versions each target offers.

## Signalling

### Where WHIP HTTP ends

Two designs were compared:

- **A. A WHIP listener in sapflow** (`SAPFLOW_WHIP_ADDR`), plain HTTP behind
  a TLS front proxy, separate from the viewer loop and the control API.
  Admission uses the existing publish-auth callback.
- **B. The app ends WHIP HTTP** and asks sapflow for a session over the
  loopback control API, as [scoped publish sessions](scoped-publish-sessions.md)
  do for SRT.

Recommended: **A**. It gives any sapflow deployment a standard WHIP endpoint
that OBS and a browser can use directly, reuses the publish-auth contract an
app already serves, and leaves the viewer loop unchanged. B stays possible
later for deployments that want the app in the signalling path.

The viewer loop stays as it is: `GET`, `HEAD` and `OPTIONS`, no request
bodies, no control routes. WHIP gets its own listener because it needs request
bodies.

### Resources

| Request | Answer |
|---|---|
| `POST /whip/live/<eventId>` with `Content-Type: application/sdp`, `Authorization: Bearer <token>` | `201 Created`, `Content-Type: application/sdp`, the answer, and `Location: /whip/session/<id>` |
| `DELETE /whip/session/<id>` with the same token | `200 OK`; the publication ends as an encoder leaving does today |
| `OPTIONS` on either | CORS preflight for configured origins; `Accept-Post: application/sdp` |
| `PATCH /whip/session/<id>` | `405 Method Not Allowed`: no trickle ICE or ICE restart in v1 |

- `<id>` is 128 random bits. It is not a credential on its own: `DELETE` also
  needs the token.
- The offer is at most 16 KiB. The headers and body share the absolute
  deadlines the control API already uses.
- Refusals, each with a short plain-text reason:
  - `400`: the SDP does not parse.
  - `401`: no token, or the app answered 401.
  - `403`: the app answered 403.
  - `409`: the path is live with another publisher and not held for a
    reconnect.
  - `422`: a well-formed offer outside the profile, for example no H.264.
  - `429`: throttled.
  - `503`: the app did not answer usably, or a resource cap is reached.
- A session that has not completed ICE and DTLS within 10 s of its `201` is
  removed and its path released. No `publish` callback has been sent by then,
  because that callback waits for the first servable segment, as today.

### Admission

Admission reuses the [publish authorisation](v4v-contract.md#publish-authorisation)
contract unchanged in shape:

```
{"user":"","password":"<bearer token>","ip":"<publisher address>","action":"publish",
 "path":"live/<eventId>","protocol":"webrtc","id":null,"query":""}
```

- `protocol` is `webrtc`, the name mediamtx uses for WHIP, so one app
  endpoint still serves both media servers.
- With `SAPFLOW_PUBLISH_USER` and `SAPFLOW_PUBLISH_PASS`, the token must equal
  the password. With `SAPFLOW_OPEN_PUBLISH=1`, any token is accepted.
- The token is a secret like the password: never logged or kept.
- The path is claimed at `POST`, before the answer is returned, so a second
  publisher gets `409` at once. Takeover of a stream held for a reconnect
  works as it does for SRT and RTMP.
- `ip` is the publisher's address. Behind a front proxy, it comes from
  `X-Forwarded-For` only when the request arrives from
  `SAPFLOW_WHIP_TRUSTED_PROXY`; otherwise it is the peer address. The
  existing throttle (10 failed attempts a minute per address) applies.

## Connectivity

Sapflow is ICE-lite. It sends no connectivity checks of its own. It answers
the publisher's checks and learns the publisher's address from them, so it
never needs the publisher's own candidates. Those are ignored and never
logged.

Candidates in the answer come from configuration, not from interface
discovery, because sapflow often runs in a container behind a published port:

```sh
SAPFLOW_WHIP_CANDIDATES="udp 203.0.113.5:8189, tcp 203.0.113.5:443"
```

- **UDP.** One socket (`SAPFLOW_WHIP_UDP_ADDR`) for every session. Packets
  are told apart by their first byte (RFC 7983, as updated by RFC 9443). A new
  5-tuple is accepted only through a STUN Binding request whose USERNAME names
  a live session and whose MESSAGE-INTEGRITY checks. After that, the tuple
  maps to its session.
- **TCP.** A passive candidate (`tcptype passive`) on a listener
  (`SAPFLOW_WHIP_TCP_ADDR`) that a layer-4 front proxy can forward to, so the
  media can use TCP 443 when UDP is blocked. When the proxy sends PROXY
  protocol v2 (`SAPFLOW_WHIP_TCP_PROXY_PROTOCOL=v2`), the header is accepted
  only from the trusted proxy address and gives the publisher's real address.
  RFC 4571 frames carry STUN, DTLS and SRTP exactly as datagrams do.
- **Consent.** A session whose consent checks stop for 30 s ends, as an
  encoder timing out does today.
- **Limits of TCP 443.** A network that only lets real TLS through on 443
  will block ICE-TCP framing. RTMPS remains the route for those networks.
  Media over TCP also stalls behind each lost segment, so it is the fallback,
  not the default.
- **No TURN.** Sapflow neither runs nor configures TURN. A deployment may
  offer its own, outside sapflow.

## Media

### H.264

- RTP packets are reordered per SSRC in a bounded buffer and depacketised
  (single NAL unit, STAP-A, FU-A). A frame ends at the marker bit.
- SPS and PPS arrive in band. They go to `EsIngest::parameter_sets`. Each
  access unit is rewritten with 4-byte length prefixes, which is what
  `EsIngest::video` takes.
- WebRTC senders do not use B-frames, so presentation and decode times are
  equal.

### Opus

`sapflow-hls` handles AAC only today. `AacConfig` and `AAC_FRAME_SAMPLES`
appear in `codec.rs`, `ingest.rs`, `fmp4.rs`, `muxer.rs` and `flv.rs`, in the
server's `stream.rs`, and in `sapflow-client` and `sapflow-ffi`. Opus support
means:

- An audio configuration that is AAC or Opus (channels, 48 kHz) throughout
  `EsIngest`, the muxer and the init segments, with Opus frame durations read
  from each packet's TOC byte (RFC 6716 section 3.1) instead of a fixed 1024
  samples.
- An Opus sample entry with a `dOps` box in the fMP4 init segment, using
  `mp4-atom`'s `Opus` and `Dops` (present in the pinned 0.16.2), and `opus` in
  the playlist's `CODECS`.
- The same in recordings, including the recording checkpoint and programme
  modules.
- `sapflow-client` and `sapflow-ffi` keep AAC. The native SDK does not publish
  over WebRTC.

Transcoding Opus to AAC inside sapflow was considered and rejected. It would
bring an Opus decoder and an AAC encoder into the server, against the
no-transcoding goal. A deployment whose players cannot take Opus can
transcode downstream.

Whether every target player decodes Opus in fMP4 LL-HLS is tested in W0, not
assumed. The results decide what a deployment needs downstream, not what
sapflow sends.

### One timeline

- Each SSRC's 32-bit RTP timestamps are unwrapped. Video uses a 90 kHz clock;
  Opus uses 48 kHz, converted to 90 kHz.
- RTCP sender reports map each stream's RTP clock to the sender's wall clock.
  At start, media is held until both streams' first sender reports arrive, for
  at most 1.5 s (proposed). After that, the stream falls back to aligning by
  arrival time and says so in its health.
- After start, timestamps advance by RTP deltas. Later sender reports check
  for drift. A jump beyond a bound is treated as a discontinuity, the way a
  resumed encoder is today.

### Keyframes

Browsers send a keyframe when asked, not on a schedule. `TargetDuration` and
`PartTiming` are fixed from the first segment. So:

- Sapflow sends a PLI as soon as SRTP is up, so the first keyframe comes at
  once.
- It then asks for a keyframe every `SAPFLOW_WHIP_KEYFRAME_INTERVAL`
  (proposed 2 s), sending PLI (or FIR where only FIR is negotiated). That keeps
  segments near the target that was fixed from the first one.
- A sender that ignores keyframe requests (OBS uses its own keyframe interval)
  needs that interval set to match. The ingest guidance says so.

### Format changes

A sender may change resolution mid-stream under CPU or bandwidth pressure. A
new SPS then reaches `EsIngest` as it would from an RTMP encoder, and starts a
new initialisation epoch (`restarted`) on the retained muxer. Pages that
publish from a browser should hold resolution and let frame rate drop instead
(`degradationPreference: "maintain-resolution"` where supported).

### Loss

- Gaps trigger NACKs, repeated within a bounded wait (`SAPFLOW_WHIP_JITTER`,
  proposed 200 ms). Retransmissions arrive on the RTX stream and are
  unwrapped. Round-trip time is estimated from NACK to retransmission, with
  100 ms assumed until measured.
- A frame still incomplete after the wait is dropped, along with every frame
  until the next keyframe. A PLI is sent at once. The loss is reported through
  the existing `PublicationLoss` path, so recordings are marked incomplete as
  they are for SRT.
- A lost Opus packet leaves a gap. Nothing is concealed, since sapflow does
  not decode.

### Congestion feedback

A browser sets its bitrate from the receiver's feedback. Without feedback it
may stay at a low starting rate. W0 measures, for each browser target, how
the sending rate ramps with no feedback and with transport-wide congestion
control feedback (`transport-cc`, draft-holmer-rmcat-transport-wide-cc-extensions-01):

- If any target needs it, sapflow negotiates `transport-cc` and sends feedback
  at a fixed cadence (proposed every 50 ms), recording each packet's arrival
  time. The sender runs the estimate, so sapflow does no rate estimation of
  its own.
- REMB is not planned unless W0 shows a target that needs it and cannot use
  `transport-cc`.

The existing per-publisher cap (`SAPFLOW_MAX_PUBLISH_MBPS`) counts SRTP bytes.

### Reconnects

There is no ICE restart in v1. When a publisher's path fails, for example
when a phone moves from Wi-Fi to mobile data, the client deletes its session
if it can and publishes again with a new `POST`. Within `SAPFLOW_RECONNECT_GRACE`,
that takes over the held stream, exactly as a reconnecting SRT or RTMP encoder
does.

## Concurrency and resource bounds

- One task owns the UDP socket and every session's sans-IO state, in the
  pattern of `srt_own.rs`. Depacketised media goes to each publication's
  ingest task over a bounded queue. A full queue is media loss for that
  publisher, never back-pressure on the socket.
- A panic while handling one session's packet ends that session only. Each
  call into a session runs under `catch_unwind` (safe Rust under
  `panic = "unwind"`), and the session is then dropped. This is an
  acceptance check, not a hope.
- Every cap is a stated setting with a refusal path:
  - pending (not yet connected) sessions in total and per address;
  - offer size;
  - packet size (1500 bytes for UDP, 1500 per RFC 4571 frame);
  - reorder buffer packets and bytes per stream;
  - DTLS handshake message size and number of flights;
  - TCP connections per address;
  - and the existing publisher and presentation caps.
- Nothing is sent to an address before it passes a STUN check with valid
  MESSAGE-INTEGRITY, and answers are never larger than the requests that
  caused them. So the UDP port cannot be used to reflect traffic at a third
  party.

## Privacy and security

- **Off by default.** WHIP runs only when `SAPFLOW_WHIP_ADDR` is set.
- **No new outbound contacts.** The only outbound calls are the existing
  publish-auth request and callback.
- **Not logged:**
  - Offers carry the publisher's addresses in their candidates. They are
    parsed in memory and never logged or stored.
  - The token is never logged. Neither are publisher addresses, as with RTMP
    and SRT today.
- **One server certificate per process.** It is generated at start (ECDSA
  P-256) and its fingerprint is put in every answer. It identifies the server,
  not the publisher.
- **Authenticity comes from signalling.** The DTLS peer is accepted only when
  its certificate matches the fingerprint in the offer that came with the
  admitted token. A forged DTLS peer from another address never gets past the
  ICE check, and a different certificate fails the fingerprint check.

## Configuration

All names are proposed and settle in W3.

| Variable | Meaning |
|---|---|
| `SAPFLOW_WHIP_ADDR` | Signalling listener. Unset means WHIP is off |
| `SAPFLOW_WHIP_UDP_ADDR` | Media UDP bind address |
| `SAPFLOW_WHIP_TCP_ADDR` | Passive ICE-TCP listener, for a front proxy. Optional |
| `SAPFLOW_WHIP_TCP_PROXY_PROTOCOL` | `v2` to accept PROXY protocol v2 from the trusted proxy |
| `SAPFLOW_WHIP_CANDIDATES` | Advertised candidates, as above |
| `SAPFLOW_WHIP_TRUSTED_PROXY` | Address whose `X-Forwarded-For` and PROXY headers are believed |
| `SAPFLOW_WHIP_ALLOW_ORIGINS` | Origins allowed by CORS for browser publishing |
| `SAPFLOW_WHIP_KEYFRAME_INTERVAL` | Keyframe request cadence, proposed 2 s |
| `SAPFLOW_WHIP_JITTER` | Longest wait for a lost packet, proposed 200 ms |

## Delay budget

The first three rows are unmeasured for WHIP. The last two are shared with
SRT and RTMP. W4 measures the WHIP rows on the same method as
[benchmarks.md](benchmarks.md).

| Stage | Bound by | Status |
|---|---|---|
| Capture and encode | The sender | Unmeasured |
| Network to the ingest | The path | Unmeasured |
| Reorder and loss wait | `SAPFLOW_WHIP_JITTER` | A setting; W4 measures the effect |
| Packaging | One LL-HLS part | Unchanged from SRT and RTMP |
| Viewer | The player's hold-back and fetches | Unchanged; measured for SRT and RTMP in [benchmarks.md](benchmarks.md) |

A shorter loss wait means less recovery on a poor network. W4 measures both
sides of that trade before a default is chosen.

## Delivery and gates

Each acceptance check is an assertion with the evidence it needs. A worker's
report or a green run is a claim until that evidence exists.

### W0: evidence before code

- **A0.1 Captured profile.** Capture offers and DTLS ClientHellos from every
  target in [The browser profile](#the-browser-profile) and keep them as
  fixtures (with addresses replaced). Evidence: the fixtures, and this
  document updated to match them. Fails if any target's capture is described
  from memory.
- **A0.2 Opus playback.** Package a captured browser Opus stream as fMP4
  LL-HLS and a recording, then play both on Safari for iOS (native player),
  Safari for macOS, Chrome and Firefox with hls.js, and Chrome on Android.
  Evidence: a table per player, playback and recording, with versions.
- **A0.3 TCP 443.** Check that each browser target, and OBS, reaches a passive
  TCP candidate through a layer-4 proxy on 443 when UDP is blocked. Evidence:
  the selected candidate pair from each client's statistics. Fails if
  inferred from documentation.
- **A0.4 Rate ramp.** Measure each browser target's sending rate over the
  first 30 s with no feedback and with `transport-cc`. Evidence: the rate
  series per target. This decides whether `transport-cc` is in v1.
- **A0.5 Missing H.264.** Record which targets, if any, offer no H.264. Those
  devices get `422` with a reason. Evidence: the offers.

### W1: `sapflow-rtp`

- **A1.1** SRTP and SRTCP protect and unprotect the RFC 3711 and RFC 7714
  test vectors exactly.
- **A1.2** Fed the captured browser streams with packets reordered, duplicated
  and dropped, the depacketiser emits the same access units as the lossless
  capture, or reports the loss.
- **A1.3** Fuzz targets for RTP, RTCP, SRTP unprotect and both depacketisers
  run in CI under the existing 60 s and nightly regime.

### W2: `sapflow-dtls`

- **A2.1** A handshake completes against a black-box OpenSSL DTLS 1.2 client
  (`s_client -dtls1_2 -use_srtp ... -keymatexport EXTRACTOR-dtls_srtp`), and
  both sides export the same SRTP keying material.
- **A2.2** Replayed captured ClientHellos from every target pass, and
  malformed, oversized, out-of-order and retransmitted flights are refused or
  handled within the caps.
- **A2.3** A peer certificate that does not match the offer's fingerprint is
  refused. The test fails if the check is bypassed by a test-only flag.
- **A2.4** Fuzz targets for records and each handshake message.

### W3: `sapflow-whip` and the server

- **A3.1** Headless Chrome with fake media publishes through `POST` to a local
  sapflow. The stream plays as LL-HLS in hls.js, and the recording opens.
  Evidence: the test log and a played frame.
- **A3.2** `DELETE` sends `unpublish`. A new `POST` within the grace period
  continues the same stream. Each refusal in [Resources](#resources) is
  observed with its status and reason.
- **A3.3** A panic injected while handling one session ends only that session.
  Another live session keeps publishing.
- **A3.4** Fuzz targets for STUN, SDP and the WHIP request parser.
- **A3.5** `scripts/smoke.sh` covers WHIP alongside SRT and RTMP.

### W4: qualification

- **A4.1** Under 1%, 5% and 10% random loss with added jitter (Linux `netem`),
  recovered and lost frames and the resulting delay are recorded per
  `SAPFLOW_WHIP_JITTER` value.
- **A4.2** CPU and memory per WHIP publisher are measured at the bitrates in
  [benchmarks.md](benchmarks.md), beside the RTMP and SRT publishers on the
  same machine.
- **A4.3** Physical devices: an iPhone (Safari) and an Android phone (Chrome)
  each publish for 30 minutes over Wi-Fi and mobile data, with a network
  switch. Evidence: the stream, the recording and the reconnects.
- **A4.4** An independent review of `sapflow-dtls` and SRTP in `sapflow-rtp`
  is recorded with its findings resolved. A review by the author, or by an
  agent of the same session, does not count.

### W5: documentation

The README's "When to choose mediamtx instead", [comparison.md](comparison.md)
and the [v4v contract](v4v-contract.md) change only when W4 passes. Until then
they describe what ships.

## Open decisions

| Decision | Settled by |
|---|---|
| `transport-cc` in v1 | A0.4 |
| Whether ICE-TCP is worth carrying | A0.3 |
| Default `SAPFLOW_WHIP_JITTER` | A4.1 |
| RustCrypto or `ring` | A4.2 |
| Accepting another video codec for phones without H.264 | A0.5, and would need transcoding downstream |
| ICE restart via `PATCH` | Physical results in A4.3 |
