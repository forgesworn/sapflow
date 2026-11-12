# WHIP ingest (design)

Status: design, 10 November 2026, updated 11 November 2026. The WHIP
libraries `sapflow-rtp`, `sapflow-dtls` and `sapflow-whip` are built but not
yet wired into the server, so WHIP stays off. It does not face the internet
before the gates in [Delivery and gates](#delivery-and-gates) pass.

WHIP (RFC 9725) lets a publisher send live media over WebRTC after a single
HTTP exchange. A browser can publish with no app installed, and OBS 30 and
later has a WHIP output. Sapflow would accept WHIP as a third way in, next to
SRT and RTMP, and serve the same Low-Latency HLS, recording and callbacks.

Why it is worth having:

- **No app on the phone.** A broadcaster opens a page, allows the camera and
  microphone and goes live. Camera, encoder and network handling are the
  browser's.
- **Networks that block non-web ports.** RTMP on 1935 and SRT on 8890 are
  often blocked on public Wi-Fi. WHIP signalling is HTTPS. The media uses one
  fixed UDP port or, where clients support it, TCP 443 (see
  [Connectivity](#connectivity); client support is checked in A0.3).
- **A shorter ingest wait, expected.** The receiver waits a short bounded
  time for lost packets instead of SRT's latency window or RTMP's TCP
  buffering. W4 measures the effect. Viewers still watch LL-HLS, so most of
  the delay a viewer sees is unchanged (see [Delay budget](#delay-budget)).

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
    packet and, later, in the phone SDK. Our own crates keep that shape and
    plug straight into Sapflow's existing ingest types, resource limits and
    loss reporting.
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
| `sapflow-server` (package and binary `sapflow`) | `whip.rs`: signalling listener, UDP owner task; `whip_tcp.rs`: the ICE-TCP listener; wiring into `Publication` and `EsIngest`. `Protocol` in `publish_auth.rs` gains `Webrtc` | Changed |

`sapflow-core` is not expected to change: a WHIP request fills the existing
`PublishRequest` (path, user, password).

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
| Peer certificate parsing | `x509-cert` and `der` |
| Our self-signed certificate | Hand-encoded DER, signed with `p256` |
| Randomness | `getrandom` (already used) |

`ring` is the measured alternative if W4 shows AES-GCM or ECDSA cost matters.
Adopting it would add C and assembly to the server build, so it needs its own
decision.

## The browser profile

"The browser profile" means exactly what the target publishers offer, captured
from them in W0 and kept as fixtures, not recalled. The targets are Chrome on
desktop and Android, Safari on iOS and macOS, Firefox, and OBS's WHIP output.
Desktop Chrome 157 is captured
([evidence](whip-evidence/w0-chrome-desktop-2026-11-10.md), fixtures in
`fixtures/whip/w0/`). Safari on macOS and iOS, OBS, FFmpeg and Vanadium
(Chrome on Android) are captured too. Firefox's offer is captured but its
ClientHello is not.

What Chrome 157 sends:

- **A 1413-byte ClientHello in two handshake fragments**, in two datagrams of
  1200 and 263 bytes.
- **DTLS 1.3 and 1.2** in `supported_versions`, with a 1258-byte `key_share`.
- **Groups:** `X25519MLKEM768` first, then `x25519`, `secp256r1` and
  `secp384r1`.
- **14 cipher suites**, including `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`.
- **Signatures:** `ecdsa_secp256r1_sha256` first.
- **Extensions:** the extended master secret, `renegotiation_info`, and
  `use_srtp` with `SRTP_AES128_CM_HMAC_SHA1_80`, `SRTP_AEAD_AES_256_GCM` and
  `SRTP_AEAD_AES_128_GCM`, in that order.
- **Record version** `0xfeff` on the first flight.

What Sapflow does with it:

- **Reassembles handshake fragments** from the first message, within the
  handshake caps. A single-datagram ClientHello is not assumed.
- **Answers DTLS 1.2** to a ClientHello that also offers 1.3, as a 1.2 server
  does. It ignores `supported_versions` and `key_share`, and sends no TLS 1.3
  downgrade marker in its random.
- **Uses `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`** with P-256 key exchange
  (RFC 5289, RFC 8422), `ecdsa_secp256r1_sha256` signatures and the extended
  master secret (RFC 7627).
- **Picks `SRTP_AEAD_AES_128_GCM`** when the client lists it, and otherwise
  `SRTP_AES128_CM_HMAC_SHA1_80`. RFC 5764 lets the server choose from the
  client's list. `SRTP_AEAD_AES_256_GCM` is not in v1.
- **Refuses a non-empty `srtp_mki`** in the client's `use_srtp` extension.
  `sapflow-rtp` has no MKI support, so a peer that expects MKI fails at the
  handshake rather than on its first SRTP packet.

OBS 32.0.4 ([evidence](whip-evidence/w0-firefox-safari-obs-2026-11-10.md))
sends a 342-byte, single-datagram ClientHello. It differs from Chrome's in
these ways:

- DTLS 1.2 only.
- `use_srtp` offers only `SRTP_AES128_CM_HMAC_SHA1_80`.
- `ecdsa_secp256r1_sha256` is fifth of six signature algorithms.
- No `renegotiation_info` extension, only the
  `TLS_EMPTY_RENEGOTIATION_INFO_SCSV` value. Sapflow treats either as the
  RFC 5746 signal.

Firefox 160's offer is captured. Its ClientHello is not yet, because no ICE
check reached the test endpoint from the test host (see the evidence).

Safari 26.6 on macOS and Safari on iOS 27 send small single-datagram
ClientHellos: 153 and 216 bytes. Both are DTLS 1.2 only, with no hybrid
group. Safari on macOS lists the same `use_srtp` profiles as Chrome. Both
offer H.264 High (`640c1f`) first, and the iOS offer has no Baseline. Both
list video before audio, and their Opus offer has no `stereo=1`, which the
answer adds.

The FFmpeg 8.1 WHIP muxer, used only as a reference client, offers
`a=setup:passive` by default. Sapflow refuses that. With
`-whip_flags dtls_active` it offers `active` and fits.

For every target:

- A peer certificate with a P-256 key, accepted only if its SHA-256 matches
  the offer's `a=fingerprint` (RFC 8122). Certificate dates and issuers are
  not checked, as in WebRTC generally.
- Sapflow answers `a=setup:passive` (RFC 8842), as RFC 9725's example does,
  and acts as the DTLS server. RFC 9725 has clients offer `actpass`, or
  `active` if they implement only the DTLS client role. An offer that can only
  be `passive` is refused.
- One BUNDLE group (RFC 9143) with `rtcp-mux`; `a=ice-lite`, `a=recvonly`.

An offer that does not fit is refused at `POST` with a plain reason (see
[Signalling](#signalling)), never accepted and then failed silently. If a
target's capture does not fit this profile, the profile changes in this
document before W2 starts.

A browser can stop offering DTLS 1.2. Chrome 157 already offers 1.3 first. The
record layer and handshake code are kept separate so that adding DTLS 1.3
later does not mean rewriting SRTP key handling. W0 records which DTLS
versions each target offers. DTLS 1.3 support is a tracked risk, not v1
scope.

What `sapflow-dtls` does with the captures (W2, 11 November 2026):

- Each captured ClientHello gets a correct first flight: Chrome, Safari on
  macOS (both over UDP and TCP), Safari on iOS, Vanadium and OBS. So does
  FFmpeg with `dtls_active`. Chrome, Safari and Vanadium get
  `SRTP_AEAD_AES_128_GCM`; OBS and FFmpeg get `SRTP_AES128_CM_HMAC_SHA1_80`.
- OBS's SCSV-only hello is accepted.
- The hellos that offer DTLS 1.3 (Chrome, Vanadium, Safari on iOS) are
  answered with 1.2.
- Not yet covered: Firefox's ClientHello, which is still not captured.
  A browser's second flight was first covered in W3a: Chrome 157 completed
  the handshake with `sapflow-whip` on loopback (see W3a below).

## SDP answer and stream matching

The offer is accepted only if it has:

- one BUNDLE group covering every accepted media section. Other group
  semantics, such as OBS's `a=group:LS`, are ignored;
- `a=mid` on each section, `a=rtcp-mux`, `a=ice-ufrag` and `a=ice-pwd`, and
  a `sha-256` fingerprint;
- `a=setup:actpass` or `a=setup:active`;
- no `a=ice-lite` of its own, because two lite agents cannot connect.

Parsing rules:

- **Session level or per section.** ICE credentials, the fingerprint, the
  setup role and the direction may be given at session level, as OBS and
  Firefox do, or per section. A section's own value wins (RFC 8866).
- **Extension directions.** `a=extmap` lines may carry a direction suffix
  (`a=extmap:<id>/<direction>`), as Firefox's do.
- **No candidates needed.** An offer may carry no candidates at all, as
  OBS's does. Sapflow needs none (see [Connectivity](#connectivity)).

The answer is built as follows:

- **Sections.** At most one video and one audio section, each `sendonly` or
  `sendrecv`, is accepted and answered `recvonly`. Every other section (a
  second video, a data channel) is answered with port 0. An offer with
  neither video nor audio is refused.
- **Video payload.** The first H.264 payload type, in offer order, with
  `packetization-mode=1` and a `profile-level-id` whose profile is
  Constrained Baseline, Baseline, Main or High. The answer repeats that
  payload's `fmtp` line. The RTX payload whose `apt` names it is accepted.
  Every other video payload is left out.
- **Video feedback.** `nack`, `nack pli` and `ccm fir`, plus `transport-cc`
  if A0.4 puts it in v1.
- **Audio payload.** `opus/48000/2`, answered with `stereo=1`,
  `maxaveragebitrate` set from configuration (music needs more than a
  browser's speech default) and `usedtx=0`. Chrome 157 honours `stereo=1`
  (it sends mono without it) and `maxaveragebitrate`. DTX is off by default,
  so `usedtx=0` only states it. The other targets are pending (A0.7).
- **Header extensions.** Only `urn:ietf:params:rtp-hdrext:sdes:mid` is
  accepted, plus the transport-wide sequence number if `transport-cc` is
  used. Video orientation (`urn:3gpp:video-orientation`) is deliberately not
  accepted, because Sapflow has no way to carry a per-frame rotation into
  LL-HLS. A phone is then expected to rotate the pixels itself; A0.8 checks
  what each phone target does. Unknown extensions in received packets are skipped.
- **ICE and DTLS.** `a=ice-lite`, our credentials, the configured candidates,
  `a=setup:passive` and our fingerprint.

Packets are matched to sections:

- **By SSRC.** The offer's `a=ssrc` lines and its `ssrc-group:FID` lines pair
  each media SSRC with its RTX SSRC.
- **By MID.** When the offer names no SSRC, the MID header extension on the
  first packets maps SSRCs to sections.
- **By payload type**, which is unique per section under BUNDLE, as a last
  resort.

A packet that matches none is dropped and counted.

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
| `DELETE /whip/session/<id>` with the same token | `200 OK`; the publisher has left, handled as an encoder leaving is today (see [Reconnects](#reconnects)) |
| `OPTIONS` on either | CORS preflight for configured origins; `Accept-Post: application/sdp` |
| `PATCH /whip/session/<id>` | `405 Method Not Allowed` with `Allow: DELETE, OPTIONS`: no trickle ICE or ICE restart in v1 |

- `<id>` is 128 random bits. It is not a credential on its own: `DELETE` also
  needs the token, which RFC 9725 has clients send on every request.
- RFC 9725 makes trickle ICE and ICE restart optional. Its ICE sections (4.3
  and 4.3.1) require 422 only for a session that supports one of the two,
  and name no status for a session that supports neither. 405 is HTTP's
  answer for an unsupported method.
- `Location` is an absolute URL built from `SAPFLOW_WHIP_PUBLIC_URL`, the
  public base URL the front proxy serves. RFC 9725's examples use absolute
  URLs, and a client may not resolve a relative one.
- CORS, for configured origins only:
  - allowed methods `POST`, `DELETE` and `OPTIONS`;
  - allowed headers `Authorization` and `Content-Type`;
  - exposed header `Location`.

  Any other origin gets no CORS headers.
- The offer is at most 16 KiB. The headers and body share the absolute
  deadlines the control API already uses.
- Refusals, each with a short plain-text reason:
  - `415`: the content type is missing or not `application/sdp`.
  - `400`: the SDP does not parse.
  - `401`: no token, or the app answered 401.
  - `403`: the app answered 403.
  - `409`: the path is live with another publisher, is not held for a
    reconnect, and is not a stalled WHIP session (see [Reconnects](#reconnects)).
  - `422`: a well-formed offer outside the profile, for example no H.264.
  - `429`: throttled.
  - `503`: the app did not answer usably, or a resource cap is reached.
- A session that has not completed ICE and DTLS within 10 s of its `201` is
  removed. It never accepted media, so its path is simply released: no
  callback is sent and nothing is held (see [Reconnects](#reconnects)).

### Admission

Admission reuses the [publish authorisation](v4v-contract.md#publish-authorisation)
contract unchanged in shape:

```
{"user":"","password":"<bearer token>","ip":"<publisher address>","action":"publish",
 "path":"live/<eventId>","protocol":"webrtc","id":null,"query":""}
```

- `protocol` is `webrtc`, the name mediamtx uses for WHIP, so one app
  endpoint still serves both media servers. The
  [v4v contract](v4v-contract.md#publish-authorisation) lists `srt` or `rtmp`
  today; it gains `webrtc` in W5, when WHIP ships.
- The token carries the user and password in one string, because WHIP
  clients have a single token field:
  - if it contains a colon, the part before the first colon is the user and
    the rest is the password;
  - otherwise the user is empty and the whole token is the password.

  So with `SAPFLOW_PUBLISH_USER` and `SAPFLOW_PUBLISH_PASS`, the token is
  `<user>:<password>`, checked exactly as the shared publisher is today. An app
  using `SAPFLOW_PUBLISH_AUTH_URL` should issue colon-free tokens. With
  `SAPFLOW_OPEN_PUBLISH=1`, any token is accepted.
- The token is a secret like the password: never logged or kept.
- The path is claimed at `POST`, before the answer is returned, so a second
  publisher gets `409` at once. Takeover of a stream held for a reconnect
  works as it does for SRT and RTMP. A stalled WHIP session can also be
  superseded (see [Reconnects](#reconnects)).
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
  are told apart by their first byte (RFC 7983, as updated by RFC 9443). A
  Binding request goes to the session its USERNAME names, from any tuple; a
  new 5-tuple is accepted only through one whose MESSAGE-INTEGRITY checks.
  Everything else goes to the session that owns its tuple. A tuple belongs
  to one session at a time: a session that a new check validates from it
  takes it, unless its owner has had fresh SRTP or SRTCP from it within the
  last 2 s or has not connected yet (W3c; see the notes below).
- **Unknown STUN attributes.** Comprehension-optional attributes (0x8000 and
  above) that Sapflow does not know are ignored. Chrome 157 sends `0xC057` in
  every check. An unknown comprehension-required attribute gets `420 Unknown
  Attribute` (RFC 8489).
- **TCP.** A passive candidate (`tcptype passive`) on a listener
  (`SAPFLOW_WHIP_TCP_ADDR`) that a layer-4 front proxy can forward to, so the
  media can use TCP 443 when UDP is blocked. RFC 4571 frames carry STUN,
  DTLS, SRTP and SRTCP exactly as datagrams do, at most 1500 bytes each, and
  feed the same router and sessions as the UDP socket (W3d):
  - Each connection is its own tuple: the publisher's address and the
    listener's number for the connection, never reused. A UDP path, or
    another connection, from the same address is another tuple. A
    session's rules are the same on TCP, the per-tuple budget included
    (see below), counting packets, not their two-byte length prefixes.
  - When the proxy sends PROXY protocol v2
    (`SAPFLOW_WHIP_TCP_PROXY_PROTOCOL=v2`), each connection must start with
    the header, and only from `SAPFLOW_WHIP_TRUSTED_PROXY`: any other peer
    is dropped before a byte is read. The header's source, in canonical
    form, is then the publisher's address: it keys the per-address cap and
    is what a check's XOR-MAPPED-ADDRESS reports. Admission and the
    failed-attempt throttle stay at the `POST`, with its own address rules.
    The trust check compares addresses, not ports, so any process on the
    proxy's host is trusted.
  - A LOCAL header is the proxy's own connection, such as a health check,
    with no publisher behind it: once its header is in, the connection is
    closed and no frame on it is read. Taking the proxy as the publisher
    would put every such connection on the proxy's address.
  Chrome 157 did not start ICE-TCP to a private address on a port below 1024
  (80, 443 and 1023 were tried). It did at 1024 and 8443, and it started
  checks to a public-range address on 443. A public deployment advertises a
  public address, so 443 is expected to work, but A0.3 stays open until a
  run against a public address on 443 succeeds. Local tests use a port of
  1024 or above. Safari on macOS also connected over ICE-TCP in W0.
  Vanadium, a hardened Chromium on Android, gathered no UDP candidates at
  all and connected only over TCP. For such a browser, TCP is the only
  route, not a fallback.
- **Roles.** A lite agent is always the controlled one (RFC 8445 section
  6.1.1) and cannot switch. A request carrying ICE-CONTROLLED is answered
  with `487 Role Conflict`, so the full agent takes the controlling role
  (section 7.3.1.1). W3 tests check that each browser target recovers from it.
- **Nomination.** A valid Binding request with USE-CANDIDATE from a new
  5-tuple moves the selected tuple there; nothing else moves it. Until SRTP
  or SRTCP has arrived, sapflow sends to the selected tuple; from then on
  it sends to the tuple of the latest fresh SRTP or SRTCP with current
  consent, and nomination no longer moves its sends. SRTP is taken from
  any tuple that has passed a check (decided in W3a, below).
- **Consent.** A session whose consent checks stop for 30 s ends, as an
  encoder timing out does today. A connected session also ends 30 s after
  its last fresh SRTP or SRTCP (W3a). A replayed check whose ID the
  session has forgotten renews consent. A copy of SRTP counts as fresh
  only if it arrives before its original or the original was lost, so
  once the publisher stops sending nothing new can count, and the session
  ends 30 s after the last packet, or copy of one, arrived.
- **Limits of TCP 443.** A network that only lets real TLS through on 443
  will block ICE-TCP framing. RTMPS remains the route for those networks.
  OBS 32 made no TCP connection to a passive candidate in W0, so OBS on a
  network that blocks UDP uses RTMPS.
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

Before W3b, `sapflow-hls` handled AAC only. `AacConfig` and
`AAC_FRAME_SAMPLES` appear in `codec.rs`, `ingest.rs`, `fmp4.rs`, `muxer.rs`
and `flv.rs`, in the server's `stream.rs`, and in `sapflow-client` and
`sapflow-ffi`. Opus support means (W3b built it; see
[Opus as built](#opus-as-built)):

- An audio configuration that is AAC or Opus (channels, 48 kHz) throughout
  `EsIngest`, the muxer and the init segments, with Opus frame durations read
  from each packet's TOC byte (RFC 6716 section 3.1) instead of a fixed 1024
  samples.
- An Opus sample entry with a `dOps` box in the fMP4 init segment, using
  `mp4-atom`'s `Opus` and `Dops` (present in the pinned 0.16.2), and `Opus` in
  the playlist's `CODECS`. RTP carries no Opus header, so the `dOps` fields
  are set by rule:
  - channel count 2 when stereo was negotiated or the first packet's TOC
    byte is stereo, otherwise 1. An Opus decoder set up for that count
    decodes both mono and stereo packets, so a later change in the TOC
    stereo flag is not a format change. A0.2 includes a stream that
    switches;
  - input sample rate 48 000;
  - output gain 0;
  - channel mapping family 0;
  - pre-skip 0 (provisional). Chrome's own recorder writes 0. In W0, a
    312 asset clicked twice at the start in Firefox with hls.js, in four of
    four runs, and the 0 assets never did. The cause is unknown. Safari gave
    no comparison (see below). W3b rechecked with sapflow's own packaging:
    0 played without a click in four of four Firefox runs from the start,
    and 312 clicked once in four of four (see
    [Opus as built](#opus-as-built)).
- No gaps in the audio track. A gap in RTP timestamps with no gap in sequence
  numbers is the sender's silence suppression (DTX). A gap in sequence
  numbers that NACK did not recover is loss. Both are filled with a constant
  encoded-silence Opus frame of matching channel layout, so players see a
  continuous track. Only the loss counts as media loss. W1 verifies the
  silence frame against a reference decoder run as a black box. The
  timestamp is the sender's to choose, so fill is bounded by arrival time:
  a gap is filled only up to the time between the two packets' arrivals
  plus the jitter allowance, and in total never runs ahead of arrival time
  by more than that allowance and the fill limit. A timestamp advance
  beyond the bound is a discontinuity, which re-anchors the track and is
  not filled.
- The same in the server's recordings. The recording checkpoint and
  programme modules (`recording_checkpoint.rs`, `recording_programme.rs` and
  `recording_timing.rs` in `sapflow-hls`) serve only the native SDK's AAC
  recordings in `sapflow-ffi`, with 1024-sample AAC arithmetic. The native
  SDK stays AAC, so they stay AAC too; the server's recorder does not use
  them.
- `sapflow-client` and `sapflow-ffi` keep AAC. The native SDK does not publish
  over WebRTC.

**Apple players do not decode it.** W0 played Opus in fMP4 HLS, packaged by
FFmpeg as a stand-in for sapflow, in every target player:

| Player | Opus in fMP4 HLS | Opus in plain fragmented MP4 |
|---|---|---|
| Chrome 157 and Firefox 160, hls.js 1.7.4 | Plays and decodes | Plays |
| Vanadium on Android, hls.js 1.7.4 | Plays and decodes | Plays |
| Safari 26.6 on macOS, native HLS | `MEDIA_ERR_DECODE` | Plays, both channels |
| Safari on iOS 27, native HLS | `MEDIA_ERR_DECODE` | Plays (muted, decode not shown) |
| Safari 26.6 on macOS, hls.js 1.7.4 | Time advances, audio not shown | |

Safari also reports `audio/mp4; codecs="opus"` as unsupported to Media
Source Extensions. The test did not separate Opus in HLS from FFmpeg's
playlist and init segment, so W3 repeats it with sapflow's own packaging.
W3b did so for Chrome and Firefox; Safari and the phones need the user's
go-ahead and have not been rerun. Until then, a WHIP stream's LL-HLS is
treated as unplayable on Apple's players.

**Decision (user, 10 November 2026): sapflow keeps Opus and does not
transcode.** Transcoding Opus to AAC inside sapflow was considered and
rejected again with this evidence. It would bring an Opus decoder and an AAC
encoder into the server, against the no-transcoding goal. A deployment with
Apple viewers converts the audio to AAC downstream, before players see it.
The README and [comparison.md](comparison.md) say so when WHIP ships. If WHIP
is wanted without a downstream converter, this decision is revisited.

#### Opus as built

W3b added Opus to `sapflow-hls` and to the server's recordings. Nothing
feeds it yet; W3c connects `sapflow-whip` to it.

- **API, additive only.** `codec::OpusConfig`, `codec::AudioConfig` (AAC or
  Opus), `codec::opus_packet` and `codec::OpusPacket`; `EsIngest::opus_config`,
  `opus_at`, `opus` and `known_audio_config`; `Muxer::with_audio_config`,
  `reconfigure_with_audio` and `audio_config`; and
  `fmp4::init_segment_for_audio_tracks`. No existing signature changed.
  `Muxer::audio()` and `EsIngest::known_audio()` still return only an AAC
  configuration, so they give `None` for an Opus track. A test hashes the
  AAC output of the TS, elementary video-led and audio-only paths, with and
  without a fragment quota and across codec epochs and reconnects (init
  segments, parts, segments and playlists), and of every init builder, and
  holds it to digests taken from the 0.2.0-rc.1 code before the Opus change.
- **Durations.** Each packet's duration comes from its TOC configuration
  (2.5 to 60 ms frames) and frame-count code (0 to 3). A packet that breaks
  the framing rules of RFC 6716 section 3.4, including one over 120 ms, is
  refused with `InvalidAudio` before anything changes, as an unsupported AAC
  configuration is. So is Opus on an ingest that carries AAC, and AAC on one
  that carries Opus. The check agrees with `sapflow_rtp::opus::parse_packet`
  on validity, duration, frame count and stereo flag (fuzz target
  `opus_packaging`).
- **`CODECS` says `Opus`**, the sample entry's four-character code, which
  RFC 6381 section 3.3 makes the first element of an ISO BMFF codecs value
  and calls case sensitive. Headless Chrome 157 and Firefox 160 report both
  `opus` and `Opus` supported to `MediaSource.isTypeSupported` and
  `canPlayType`, and play sapflow's stream with either.
- **Timescale 48 000.** The encapsulation calls it typical (section 4.4).
  Every Opus frame is a whole number of 48 kHz ticks, and of 90 kHz ticks
  (120 samples are 225), so sample durations and the conversion to the
  shared timeline are exact.
- **Video-led audio.** As for AAC, a sample lasts until the next packet's
  timestamp, jitter and slew included, not for its TOC duration: the
  timeline slews audio by up to 500 ppm to keep it with the video, and TOC
  durations would throw that away. With exact timestamps every sample is
  exactly its TOC duration, and slew alone (500 ppm either way) keeps each
  within that slew plus one sample (at most 4 samples for a 120 ms packet).
  Other timestamp steps move it further: a timeline hold or a step by
  arrival time can make it anything from nothing (a repeated timestamp) to
  three packets' worth. A gap of more than three of the packet's own
  durations (1024 samples for AAC) gives it its own duration instead.
- **Audio-only.** As for AAC, decode time is an exact count of samples and
  timestamps only find breaks. Unlike AAC, which measures each frame against
  the run's origin and sample count, each Opus packet is compared with the
  previous timestamp plus that packet's TOC duration: more than 2 ms (180
  ticks) off starts a discontinuous run reported as media loss. A missing,
  repeated or non-increasing packet is a break, and so is the packet after
  a refused one, whose duration is missing. A timeline's slew is carried
  step by step: timestamps drifting smoothly at up to 500 ppm either way
  give no discontinuity in 120 s with 2.5 to 120 ms packets, where the
  run-origin rule gave the first at 20.1 s at 100 ppm and at 4.0 s at
  500 ppm (five and 29 in 120 s). The part target is fixed from the first packet's duration, as AAC's comes
  from its frame: 20 ms packets make 200 ms parts under a 234 ms target. It
  is never under 120 ms, the longest valid packet, so every packet fits a
  part of its own; a `part_min` that would give less gives 120 ms parts. A
  target inherited from an earlier epoch cannot change, so one under 120 ms
  refuses the audio-only Opus epoch up front (`InvalidAudio`). A later,
  longer packet that the open part has no room for closes only that part,
  short but `INDEPENDENT=YES`, which RFC 8216bis draft 19 section 4.4.4.9
  exempts from the 85% floor (`RuleCheck` allows that only where every part
  is independent, as in audio-only); the segment carries on. (Closing the segment
  too, as W3b first did, collapsed the count-limited window: after a 20 ms
  first packet, 120 ms packets made 0.12 s segments and a 2.88 s window. Now
  they make 1.08 s segments and a 25.9 s window.) Every refusal is made
  before anything changes, and the packet is copied only once admitted.
- **Channels** follow the rule above: `EsIngest::opus_config(OpusConfig::STEREO)`
  when stereo was negotiated, otherwise the first packet's TOC.
  `opus_config(OpusConfig::MONO)` records nothing, since mono is what a
  stream without negotiated stereo is, and any count but 1 or 2 is refused
  with `InvalidAudio`. A later
  change of the stereo flag keeps the configuration and the init segment. A
  reconnect whose first packet gives another count starts a codec epoch, as
  a changed AAC configuration does.
- **Bounds.** Opus goes through AAC's admission: the 64 MiB charged-segment
  limit (pending packet included), the 16,384 live-sample limit, the shared
  fragment quota and the audio-only segment limit, each refused before any
  change.
- **Recordings.** The server's recorder writes Opus with
  `init_segment_for_audio_tracks` and rebases audio at 48 kHz. Its test
  checks the file byte for byte against that layout (the recording init,
  then each part rebased), which the FFmpeg check below decodes.
  `sapflow-client`, `sapflow-ffi` and `sapflow-rtmp` are unchanged.
- **Server flags.** The health callback's `audio` and a fresh part's
  `has_audio` read `Muxer::audio_config()`, so an Opus stream reports its
  audio.

**Departures from the encapsulation.** Read against "Encapsulation of Opus
in ISO Base Media File Format", version 0.6.8:

- Pre-skip is 0. Section 4.3.2 says it "shall be at least 80 milliseconds'
  worth of PCM samples", and also that it is informative only, discarding
  priming samples being the edit list's job. W3b follows this design's
  provisional rule and Chrome's recorder, and the measurement below.
- The init segment has no edit list (`edts`, `elst`), as for AAC. Section
  4.4 asks for one to trim priming and padding.
- There are no `roll` sample groups (`sgpd` in the init segment, `sbgp` in
  each fragment), which section 4.3.6.2 requires for the 80 ms pre-roll
  after random access. Adding `sbgp` would also need
  `fmp4::rebase_part_for_file` changed for recordings: it accepts only
  `tfhd`, `tfdt` and `trun` in a track fragment.

FFmpeg and both browsers decoded the output without them. Whether to add
them is open.

Video-led sample durations also depart from section 4.3.4, which makes a
sample's duration its frames' total: they follow the timestamps (see
Video-led audio above). Slew alone moves them off the TOC duration by up to
the slew plus a sample; a timeline hold or a step by arrival time can move
one from nothing to three packets' worth. No player has been tried on such
a stream.

**FFmpeg decode.** sapflow's own init segment and parts, built in a test
(`write_opus_evidence` in `opus_real_tests.rs`) from real Opus packets, as
one file per stream and in the recorder's layout, decoded by FFmpeg 8.1.2 as
a black box: no errors from any file, a 48 kHz Opus stream with
`initial_padding=0`, and decoded samples per channel equal to the sum of
the packets' TOC durations.

| Stream | Packets | Channels | Duration | Decoded samples |
|---|---|---|---|---|
| libopus, 20 ms stereo, audio only | 101 | 2 | 2.020 s | 96 960 |
| libopus, 60 ms SILK mono, audio only | 17 | 1 | 1.020 s | 48 960 |
| libopus, 120 ms multi-frame stereo, audio only | 9 | 2 | 1.080 s | 51 840 |
| mono 60 ms, then stereo 20 ms, then stereo 120 ms (the TOC stereo flag changes) | 127 | 1 | 4.120 s | 197 760 |
| Chrome `MediaRecorder` (60 ms stereo), audio only | 332 | 2 | 19.920 s | 956 160 |
| Chrome Opus with 1280x720 H.264, also as a recording | 332 | 2 | 19.967 s | 950 400, video without errors |

The libopus sources declare a pre-skip of 312 and trim their last packet by
granule position. sapflow reads neither, so its output decodes those 312
priming samples and the final padding. Video-led output stops at the last
video frame, so the A/V file holds 950 400 of the 956 160 samples, as with
AAC. The audio-only recording the server's recorder wrote in its test
decodes the same way: 96 960 samples and no errors. Its A/V test recording
carries placeholder video, which FFmpeg opens but cannot decode, by design;
its audio decodes (94 080 samples).

**Playback.** sapflow's LL-HLS of 1280x720 30 fps H.264 (libx264) with the
Chrome recording's Opus (60 ms stereo packets) played in headless Chrome
157 and Firefox 160 with hls.js 1.7.4, through W0's play page (Web Audio
analyser and click detector). `scripts/hls-opus/replay-run.js` serves the
playlists sapflow wrote after each change, each from the media time it
appeared. It holds a blocking reload until a playlist satisfies it, and a
part or segment until a playlist lists it. Every playlist and media byte is
sapflow's; the harness adds only that timing, a view from the start (the
last playlist with `EXT-X-ENDLIST`, which hls.js starts at the first real
segment, past the leading `EXT-X-GAP` entries), and a copy with the `dOps`
pre-skip patched to 312.

| Browser | Stream | Runs | Result |
|---|---|---|---|
| Chrome 157 | Live, audio and video | 6 | Played 13.5 to 14 s of 14 s; 228 to 236 kB of audio decoded (`webkitAudioDecodedByteCount`); 445 Hz left, 879 Hz right |
| Chrome 157 | Live, audio only | 2 | Played and decoded, both tones |
| Chrome 157 | From the start, audio and video | 1 | Played and decoded, no click |
| Firefox 160 | Live, audio and video | 8 | Played 13.5 s of 14 s; both tones in the analyser |
| Firefox 160 | Live, audio only | 1 | Played, both tones |
| Firefox 160 | From the start, audio and video | 8 | Played 13.5 s of 14 s; see pre-skip below |

hls.js reported no fatal error. The audio-only and from-the-start runs report
the leading gaps as non-fatal `fragGap`. One early Chrome run stalled once
for 5.5 s and recovered, while the page's own 500 ms timer was also held up
under other load on the host. Four reruns at lower load did not stall.

After the review fixes, the evidence files came out byte for byte as
before, and a rerun gave the same results: Chrome live with video 2 of 2,
live audio only 1 of 1, from the start 1 of 1 without a click; Firefox live
with video 4 of 4 (two at each pre-skip), from the start 4 of 4 (pre-skip 0
without a click, 312 with one, two runs each), live audio only 1 of 1. A
copy whose multivariant playlist said `Opus` instead of `opus` played in
both browsers, with video and audio only, one run each.

Joining live with video, the first audible block jumps by 0.27 to 0.29 in 13
of 14 runs, against the test tone's largest step of 0.035, at pre-skip 0
and 312 alike. Audio-only live joins do not. The likely cause, not
confirmed, is the players starting the audio at the video's first frame, in
the middle of a continuous tone. It is the same at either pre-skip, so it
comes from the join, not the pre-skip.

**Pre-skip, measured.** Firefox 160 with hls.js 1.7.4, from the start of
sapflow's packaging, judged from decoded samples by W0's click detector (a
step over 0.07):

| `dOps` pre-skip | Runs | Click |
|---|---|---|
| 0, as sapflow writes | 4 | None. Largest step 0.0347 at onset, 0.0349 steady |
| 312, the same files with that field patched | 4 | One in each run: a step of 0.273 about 1.1 s after playback began, inside the first segment |

Chrome from the start, at pre-skip 0, had no click either. W0 saw two
clicks at the start with FFmpeg's packaging at 312; the count and position
differ, but the result is the same: 0 plays cleanly and 312 clicks in
Firefox. The cause is still unknown, so pre-skip stays 0.

**Open after W3b.**

- Settled after W3b: audio-only Opus carries a timeline's slew (see
  Audio-only above), and video-led Opus keeps timestamp durations (see
  Video-led audio above).
- Settled after W3b, with the user's agreement: RFC 8216bis 6.2.2's
  segment rule. A segment is no longer removed if the playlist left would
  last less than three target durations, as printed, up to 256 entries and
  64 MiB of media beyond `segment_count`; past either cap, removal goes on.
  Count-only eviction broke it for AAC: with `segment_count` 8 the epoch
  regression fell to 5.40 s against 6 s (now 6.27 s, nine entries at most),
  and at the default 24, a publisher reconnecting every 10 AAC frames left
  5.12 s (now 6.19 s, 29 entries at most). `RuleCheck` and
  `check-playlist.py` check it by default, and so do the `ts_ingest`,
  `flv_ingest`, `muxer_timing` and `opus_packaging` fuzz targets. A render
  shows the entry cap but not the byte cap, so `Muxer::window_byte_capped()`
  says when the byte cap cut the window short; the fuzz targets pass it to
  `RuleCheck::check_with_byte_cap`, and `check-playlist.py` takes
  `--byte-capped`. The `epochs` golden digest changed with it: that
  scenario now keeps one more 1 s segment. Opus output is pinned at 4ab0c12
  by `real_opus_output_is_byte_identical_to_4ab0c12`, which still matches.
- Open, and older than W3b: RFC 8216bis 6.2.2's part rule. A part must stay
  downloadable for three target durations after its `EXT-X-PART` tag leaves
  the playlist. Tags are listed while their segment ends within three
  target durations of the live edge (`playlist.rs`), but a part's data is
  freed with its segment (`Muxer::rotate_segment`), so a window shorter than
  about six target durations falls short. The live-window review measured
  how long a part stays downloadable after its tag leaves, the same at
  4ab0c12 and after the segment rule:

  | Case | Part held after its tag leaves |
  |---|---|
  | Steady stream, `segment_count` 7 | 1.0 s |
  | Default 24, reconnect every 10 frames | 0.000 s |
  | Default 24, reconnect every 20 frames | 4.05 s |
  | Steady stream, `segment_count` 12 | 6.02 s |
  | Steady stream, `segment_count` 24 | 18.05 s |
- The departures from the encapsulation listed above, and the cause of the
  312 click.
- Safari, iOS and Android with sapflow's packaging, which need the user's
  go-ahead.
- `sapflow-hls` and `sapflow-rtp` each have an Opus framing check. The fuzz
  target holds them to the same answers; one could move to `sapflow-core`.

### One timeline

- Each SSRC's 32-bit RTP timestamps are unwrapped. Video uses a 90 kHz clock;
  Opus uses 48 kHz, converted to 90 kHz.
- RTCP sender reports map each stream's RTP clock to the sender's wall clock.
  At start, media is held until both streams' first sender reports arrive, for
  at most 1.5 s (proposed), 4 MiB of media or half the event queue. After
  that, or if the reports disagree with arrival times by more than 500 ms,
  the streams are aligned by arrival time. `sapflow-rtp` says which, and why,
  in an `Aligned` event before the first frame.
- After start, timestamps advance by RTP deltas, strictly increasing, and
  stay within 500 ms (`max_lead`) of arrival time either way, plus 0.1% of
  elapsed time up to another 500 ms. A little above that bound a timestamp is
  held to it; a timestamp that does not advance continues by arrival time.
- Later sender reports are used only in agreeing pairs (same rate within
  0.1% and 1 ms) at least 500 ms apart, so forged or alternating reports are
  ignored. An error within 500 ms, or a stream ahead by any amount, is
  slewed at up to 500 ppm, never backwards and with no discontinuity; this
  follows a sender's audio clock drifting against its NTP clock. Video
  slews the part of its error audio does not share at up to 5%, so a video
  RTP jump of 900 ms is back in sync within 20 s. Only a step forward of
  more than 500 ms, or a timestamp that jumps back, jumps ahead of or falls
  behind arrival time by more than that, is treated as a discontinuity,
  the way a resumed encoder is today.
- A sender whose timestamps crawl can cause about one discontinuity a
  second, so the server merges or rate-limits discontinuity epochs rather
  than starting one per event.

### Keyframes

Browsers send a keyframe when asked, not on a schedule. `TargetDuration` and
`PartTiming` are fixed from the first segment. So:

- Sapflow sends a PLI as soon as SRTP is up, so the first keyframe comes at
  once. The server asks `sapflow-rtp`'s receiver for it then
  (`Receiver::request_keyframe`); the first media packet asks too, as a
  fallback, and so does any stream whose SSRC is not yet known.
- It then asks for a keyframe every `SAPFLOW_WHIP_KEYFRAME_INTERVAL`
  (proposed 2 s), sending PLI (or FIR where only FIR is negotiated). That keeps
  segments near the target that was fixed from the first one.
- A sender may not act on keyframe requests. Whether OBS does is checked in
  A3.10. OBS's offer has no `ccm fir`, so only PLI is used with it. Until it is shown to, OBS guidance sets its own keyframe interval to
  match.

### Format changes

A sender may change resolution mid-stream under CPU or bandwidth pressure,
and a browser often starts low and ramps up (Chrome 157 went from 480x270 to
640x360 a few seconds in). A new SPS or PPS reaches `EsIngest` as an RTMP
sequence header or an SRT encoder's in-band sets do, and the same rule
applies to all three:

- At the next IDR whose parameter sets differ from the current epoch's (byte
  for byte, ignoring trailing zero bytes), the ingest starts a new codec
  epoch on the same muxer: a new init segment with the new avcC, after an
  `EXT-X-DISCONTINUITY` and a new `EXT-X-MAP`, with `TARGETDURATION` and
  `PART-TARGET` unchanged and the timeline carried straight on. The update
  says `restarted`, so the recording rolls to a new file there.
- A PPS change alone starts an epoch too, since samples carry no parameter
  sets of their own. Sets repeated unchanged at every IDR start nothing. A
  changed SPS that does not parse keeps the current epoch, and is logged once.
- Before the recording rolls, the old file takes its epoch's last parts,
  which the change sealed. Media that was served is recorded, or the
  recording is marked:
  - Once part timing is known, the change seals the open part, so even an
    epoch shorter than one part is served (its own `EXT-X-MAP` and a
    segment). Its recording never started; it is started at the roll, with
    the tracks the stream noted when the epoch began, and fed the epoch's
    parts.
  - An epoch that began and ended inside one ingest batch left no tracks to
    start a recording with; the next recording is then marked incomplete
    (`epoch-unrecorded`).
  - While part timing is still being learnt, at the start of a stream, the
    change seals nothing and that epoch is never served or recorded.
    Chrome 157 did this at the start of some runs: its first IDR at
    640x360, then 480x270 at once, then 640x360.
  - The log line gives the ended epoch's parts after the change sealed them
    (`previous_parts`); zero means it was never served.

Before this, the first init stayed while frames of the new size were
written under it, so players failed to decode them (hls.js stalled after a
few frames) and the recording, though marked complete, did not decode past
the change.

Left open:

- At the change the muxer drops its pending video frame and pending audio
  (in the fixture test, 1 video frame and 3 AAC frames of 60 and 96), as it
  does at any interrupt, and the recording is not marked for it.
- Two writers a path (`PATH_WRITERS`): epochs that change faster than a
  finished file can be written out leave a later recording without a
  writer, which fails it. In real time the short epoch and the next part
  give the writer that time; nothing bounds the epoch rate but the IDR
  cadence (for the bounds work in W3c part 2b).
- Observed stalls of hls.js 1.7.1 at the boundary in the end-to-end runs:
  0.10 to 0.82 s, and 0.52 s in the review. Once, in the diagnosis, it
  stalled about 7 s on a 0.37 s buffer hole before stepping over it; that
  was not seen again.

Pages that publish from a browser should still hold resolution and let frame
rate drop (`degradationPreference: "maintain-resolution"` where supported):
an epoch change costs viewers a discontinuity. In one run with it set,
Chrome held 640x360 throughout, but a run without it did not change
resolution either, so that is not proof that it prevents one.

### Loss

- Gaps trigger NACKs, repeated within a bounded wait (`SAPFLOW_WHIP_JITTER`,
  proposed 200 ms). Retransmissions arrive on the RTX stream and are
  unwrapped. A sender that negotiated no RTX, such as OBS, resends on the
  original SSRC and sequence number. The reorder buffer takes either. Round-trip time is estimated from NACK to retransmission, with
  100 ms assumed until measured.
- A frame still incomplete after the wait is dropped, along with every frame
  until the next keyframe. A PLI is sent at once. The loss is reported through
  the existing `PublicationLoss` path, so recordings are marked incomplete as
  they are for SRT.
- A lost Opus packet is replaced by silence, as described under [Opus](#opus).
  Nothing else is concealed, since sapflow does not decode.
- **SRTP replay window (implementation note).** RFC 3711 requires a window
  of at least 64 packets. A sender without RTX, such as OBS, resends a NACKed
  packet on its original SSRC and index, up to the 200 ms wait later; at
  5 Mbit/s that is a hundred or more packets back. So `sapflow-rtp` keeps a
  configurable window per SSRC, 1024 packets by default. An index never
  received inside the window is accepted; a duplicate of a received index is
  refused.

### Congestion feedback

Sapflow sends RTCP receiver reports (RFC 3550) for each stream it receives,
every second, fixed rather than RFC 3550's randomised interval (a WHIP
session has one sender and one receiver, so there is nothing to fall into
step with), and in compound packets with its NACK, PLI and any other
feedback, at most 1200 bytes each. Senders use them for loss-based rate
control. Each sequence number counts once. Packets repaired over RTX still
count as lost, so the reports give loss before repair, as rate control needs
(RFC 4588 section 7) and as transport-cc does; OBS, which resends on the
original SSRC, gets loss after repair. RTX streams get no report blocks of
their own.

A browser sets its bitrate from the receiver's feedback. Without feedback it
may stay at a low starting rate. W0 measures, for each browser target, how
the sending rate ramps with no feedback and with transport-wide congestion
control feedback (`transport-cc`, draft-holmer-rmcat-transport-wide-cc-extensions-01):

- If any target needs it, sapflow negotiates `transport-cc` and sends feedback
  at a fixed cadence (proposed every 50 ms), recording each packet's arrival
  time. The sender runs the estimate, so sapflow does no rate estimation of
  its own.
- REMB is not planned unless a target needs it and cannot use
  `transport-cc`. OBS 32 offers REMB and not `transport-cc`. Whether OBS
  changes its rate on feedback at all is checked in A3.10.

Provisional result: desktop Chrome 157, in-page loopback, with receiver
reports still present.

- With `transport-cc`, the target rate reached 5 Mbit/s in all three runs,
  after 1, 2 and 19 s.
- Without it, two of three runs were still near 2.6 Mbit/s after 30 s.

So `transport-cc` is planned for v1 unless the other targets contradict it.

The existing per-publisher cap (`SAPFLOW_MAX_PUBLISH_MBPS`) counts SRTP bytes.

### Reconnects

There is no ICE restart in v1.

- **A session that never accepted media** releases its path with no callback
  and nothing held. That covers a failed or timed-out handshake, and a
  `DELETE` before any media.
- **A session that has accepted media and then ends**, through `DELETE`,
  consent expiry or being superseded, leaves by the existing path for an
  encoder that goes (`publish.rs:481-486`). `unpublish` is sent at once and
  the stream is held for `SAPFLOW_RECONNECT_GRACE`. A new `POST` within the
  grace takes over the held stream.
- **A stalled session can be superseded.** When a phone moves from Wi-Fi to
  mobile data, its old session can look alive until consent expires, up to
  30 s. So a new admitted `POST` for a path whose publisher is a stalled WHIP
  session ends that session and takes over at once, as if it had left. A
  session is stalled when `SAPFLOW_WHIP_STALE` (proposed 2 s) has passed
  since its last accepted media packet.
- **A session that has accepted no media yet** is replaced by any new
  admitted `POST` for its path, at once. Nothing is live yet, so a retried or
  double-clicked `POST` simply wins.
- **Ending a gig.** A deliberate `DELETE` is handled like a drop, so the
  stream is held and an app may show "reconnecting" until the grace runs out.
  That is intended and matches encoders today. An app that knows the gig has
  ended says so from its own state. If the current
  WHIP session is still delivering media, or the current publisher is SRT or
  RTMP, the new `POST` gets `409`, so two encoders never fight over one
  stream.
- **Clients** recover by sending a new `POST`. A `DELETE` first is a
  courtesy, and is not needed when the old network is gone.

## Concurrency and resource bounds

- One task owns the UDP socket and every session's sans-IO state, in the
  pattern of `srt_own.rs`. Depacketised media goes to each publication's
  ingest task over a bounded queue. A full queue is media loss for that
  publisher, never back-pressure on the socket.
- The ICE-TCP listener (`whip_tcp.rs`) runs one task per connection. Their
  frames reach the UDP owner over one queue of 1024; a full queue makes the
  readers wait, which TCP's flow control carries back to the publishers.
  What a session sends to a TCP tuple goes into that connection's queue
  without waiting: a full queue drops the packet, as a lost datagram would
  be, and never holds up the owner.
- A panic while handling one session's packet ends that session only. Each
  call into a session runs under `catch_unwind` (safe Rust under
  `panic = "unwind"`), and the session is then dropped. This is an
  acceptance check, not a hope.
- Every cap is a stated setting with a refusal path:
  - pending (not yet connected) sessions in total and per address;
  - offer size;
  - packet size (1500 bytes for UDP, 1500 per RFC 4571 frame);
  - reorder buffer packets and bytes per stream;
  - the start hold's bytes and time, and the event queue;
  - RTCP datagram size, and queued feedback (256 packets);
  - transport-cc packets awaiting feedback (1024) and feedback packet size
    (1000 bytes);
  - DTLS handshake message size and number of flights;
  - ICE-TCP connections in total (128) and per publisher address (32),
    the PROXY header's time (5 s), the time to a first frame sent back
    (10 s) and between frames sent back (30 s), and each connection's send
    queue (256 frames, 256 KiB);
  - and the existing publisher and presentation caps.
- Nothing is sent to an address before a STUN check from it passes
  MESSAGE-INTEGRITY, and STUN answers are no larger than the requests that
  caused them. MESSAGE-INTEGRITY does not prove the source address (RFC
  8445 section 19.2), so a check captured on the way and re-sent with a
  third party's address as its source verifies too. W3a limits what that
  gains:
  - a check whose transaction ID the session has seen (the last 512) is
    answered only from the address that first sent it, and changes
    nothing; from any other address it gets nothing;
  - SRTP authentication does not cover the source address either, so a
    copy of an SRTP or SRTCP packet that arrives before its original, or
    whose original was lost, is fresh too. Neither kind of proof lifts
    the limit below;
  - **the bound:** for each address, over the life of a session, sapflow
    sends at most four times the bytes it has received from that address,
    counting STUN, DTLS, SRTP and SRTCP and including its STUN responses.
    A STUN response to an address that has not passed a check (a 487 or
    a 420) is no larger than its request. A datagram over the limit is
    dropped and counted, and the session goes on;
  - DTLS and RTCP go to the selected address until SRTP or SRTCP has
    arrived, then to the address of the latest fresh SRTP or SRTCP with
    current consent;
  - a full table of validated addresses evicts one that is neither
    selected nor sent to: first one that has never delivered fresh media,
    then the one whose last fresh media is oldest;
  - a connected session ends 30 s after its last fresh SRTP or SRTCP.

  So a re-sent check that arrives after its original draws nothing while
  its ID is remembered, and any amount of re-sending, copying or racing
  draws at most four times what the attacker sent from that address. A
  full table evicts an address that is neither selected nor the send
  address: one that has never delivered fresh media first, then the one
  whose last fresh media is oldest, the earliest validated breaking ties.
  An address validated again starts with no media. So checks alone,
  replayed or not, push out an address that has delivered fresh media
  since it was validated only once every address that could go has
  delivered some. Pushing the publisher's address out takes
  `max_tuples` - 2 other addresses (6 by default) each with fresh media at
  least as new as the publisher's last, and a selected address other than
  the publisher's. An observer that copies and spoofs gets them from
  checks whose IDs are forgotten or whose originals were lost, one of them
  carrying USE-CANDIDATE, and that many
  of the publisher's packets that the session never received on their
  original index while each was inside the replay window (1024 packets per
  SSRC, about 20 s of Opus at 50 a second): packets lost after the
  observer saw them, or beaten by its copies. No race is needed: it can
  keep lost packets and send them, with one such check from a new
  address, in one burst between two of the publisher's packets, and RTX
  resends go on their own SSRC, leaving the original index open. Short of
  that, after a won race or a copy of a lost packet, the publisher loses
  no media and sends are back with it at its next packet that arrives
  before any copy of it. Once pushed out, the publisher's media is dropped
  until one of its own checks arrives before any copy of it (a check whose
  copy came first is refused as a replay). That check brings its address
  back with no media, below every address that has some, so one more
  such check from a new address that lands before the publisher's
  next packet pushes it out again: that needs no race, only the gap. The
  exception is a check of the publisher's that carries USE-CANDIDATE: its
  address becomes the selected one and is kept while it stays selected. An
  attacker who wins every race, or drops or delays the publisher's packets
  (RFC 8445 section 19.2), can also keep them from ever counting as fresh
  and keep sapflow's sends, PLIs and NACKs among them, away from the
  publisher. That disrupts delivery, though still not beyond four times
  what the attacker sends.
  This is shown in process (W3a); no real browser has yet been seen to
  move pairs mid-handshake.

## Privacy and security

- **Off by default.** WHIP runs only when `SAPFLOW_WHIP_ADDR` is set.
- **No new outbound contacts.** The only outbound calls are the existing
  publish-auth request and callback.
- **Not logged:**
  - Offers carry the publisher's addresses in their candidates. They are
    parsed in memory and never logged or stored.
  - The token is never logged, and neither are publisher addresses. A3.6
    checks both. The address is used only for the throttle, in memory, and
    on the ICE-TCP listener for the per-address cap and the check's
    XOR-MAPPED-ADDRESS. Behind PROXY protocol v2 that is the header's
    source; the ICE-TCP listener logs a connection only by its number.
- **One server certificate per process.** It is generated at start (ECDSA
  P-256) and its fingerprint is put in every answer. It identifies the server,
  not the publisher.
- **Authenticity comes from signalling.** The DTLS peer is accepted only when
  its certificate matches the fingerprint in the offer that came with the
  admitted token. A forged DTLS peer from another address never gets past the
  ICE check, and a different certificate fails the fingerprint check.

## Configuration

W3c built `SAPFLOW_WHIP_ADDR`, `SAPFLOW_WHIP_PUBLIC_URL`,
`SAPFLOW_WHIP_UDP_ADDR` and `SAPFLOW_WHIP_CANDIDATES` (required together),
and `SAPFLOW_WHIP_ALLOW_ORIGINS`, `SAPFLOW_WHIP_TRUSTED_PROXY`,
`SAPFLOW_WHIP_KEYFRAME_INTERVAL` (seconds, 0 for none) and
`SAPFLOW_WHIP_OPUS_MAX_BITRATE`. W3d built `SAPFLOW_WHIP_TCP_ADDR` and
`SAPFLOW_WHIP_TCP_PROXY_PROTOCOL`, and `tcp` candidates. Refused at start:
any WHIP setting without `SAPFLOW_WHIP_ADDR`; WHIP with the scoped control
API; a `tcp` candidate without `SAPFLOW_WHIP_TCP_ADDR`, and
`SAPFLOW_WHIP_TCP_ADDR` with no `tcp` candidate or on the signalling
address; `SAPFLOW_WHIP_TCP_PROXY_PROTOCOL` other than `v2`, or `v2` without
`SAPFLOW_WHIP_TCP_ADDR` or `SAPFLOW_WHIP_TRUSTED_PROXY`. The UDP socket is
bound even when only `tcp` candidates are advertised.
`SAPFLOW_WHIP_JITTER` and `SAPFLOW_WHIP_STALE` are still proposals.

| Variable | Meaning |
|---|---|
| `SAPFLOW_WHIP_ADDR` | Signalling listener. Unset means WHIP is off |
| `SAPFLOW_WHIP_PUBLIC_URL` | Public base URL of the signalling listener, for `Location` |
| `SAPFLOW_WHIP_UDP_ADDR` | Media UDP bind address |
| `SAPFLOW_WHIP_TCP_ADDR` | Passive ICE-TCP listener, for a front proxy. Optional; needs a `tcp` candidate |
| `SAPFLOW_WHIP_TCP_PROXY_PROTOCOL` | `v2`: every ICE-TCP connection starts with a PROXY protocol v2 header from `SAPFLOW_WHIP_TRUSTED_PROXY`, whose source is the publisher; any other peer is dropped. Unset: no header, and the TCP peer is the publisher |
| `SAPFLOW_WHIP_CANDIDATES` | Advertised candidates, as above: `udp <ip>:<port>` and `tcp <ip>:<port>`, 1 to 8 |
| `SAPFLOW_WHIP_TRUSTED_PROXY` | Address whose `X-Forwarded-For` and PROXY headers are believed |
| `SAPFLOW_WHIP_ALLOW_ORIGINS` | Origins allowed by CORS for browser publishing |
| `SAPFLOW_WHIP_KEYFRAME_INTERVAL` | Keyframe request cadence, proposed 2 s |
| `SAPFLOW_WHIP_JITTER` | Longest wait for a lost packet, proposed 200 ms |
| `SAPFLOW_WHIP_STALE` | Media silence after which a new `POST` may supersede a session, proposed 2 s |
| `SAPFLOW_WHIP_OPUS_MAX_BITRATE` | `maxaveragebitrate` offered for Opus |

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

W0 gathers facts. Each check fails if a target is skipped, tested only in
emulation where a device is named, or described from documentation or memory
instead of observed.

- **A0.1 Captured profile.** Capture offers and DTLS ClientHellos from every
  target in [The browser profile](#the-browser-profile) and keep them as
  fixtures, with addresses replaced. Evidence: the fixtures, and this
  document updated to match them.
- **A0.2 Opus playback.** Package a captured browser Opus stream, with the
  `dOps` values above, as fMP4 LL-HLS and as a recording. Play both on:
  - Safari for iOS on a physical iPhone, in the native player;
  - Safari for macOS;
  - Chrome and Firefox with hls.js;
  - Chrome on a physical Android phone.

  Evidence: a table per player for live and recording, with versions, noting
  any click at the start (pre-skip).
- **A0.3 TCP 443.** With UDP blocked, each browser target and OBS reaches a
  passive TCP candidate through a layer-4 proxy on 443. Evidence: the
  selected candidate pair from each client's statistics.
- **A0.4 Rate ramp.** Measure each browser target's sending rate over the
  first 30 s, with receiver reports only and with receiver reports plus
  `transport-cc`. Evidence: the rate
  series per target. This decides whether `transport-cc` is in v1.
- **A0.5 Missing H.264.** Record which targets, if any, offer no H.264.
  Evidence: their offers.
- **A0.6** moved to W3 as A3.10, because it needs a receiver that completes
  DTLS.
- **A0.7 Opus parameters.** Record whether each browser target honours
  `stereo=1`, `maxaveragebitrate` and `usedtx=0`. Evidence: the TOC stereo
  flag and bitrate of received packets.
- **A0.8 Rotation.** Rotate each phone target mid-stream, with video
  orientation not negotiated. Record whether it sends rotated pixels at new
  dimensions, or something else. Evidence: the SPS dimensions before and
  after, and a decoded frame from each.

### W1: `sapflow-rtp`

- **A1.1 Vectors.** SRTP and SRTCP protect and unprotect the RFC 3711 and
  RFC 7714 test vectors exactly.
- **A1.2 Recovery.** Feed the captured streams with packets reordered,
  duplicated and dropped, where every dropped packet's retransmission arrives
  within the wait. The depacketiser must emit exactly the access units of the
  lossless capture. With retransmissions withheld, it must report loss for
  exactly the withheld packets' frames. Fails if any access unit differs, or
  if loss is reported for a delivered packet.
- **A1.3 Fuzzing.** RTP, RTCP, SRTP unprotect, both depacketisers and the
  Opus TOC parser each have a fuzz target, as do the receiver
  (`reorder_nack`), its report statistics (`rr_stats`), transport-cc
  (`twcc_feedback`) and the timeline (`sr_timeline`). Each target:
  - starts from a seed corpus built from the W0 captures;
  - runs in CI under the existing 60 s and nightly regime;
  - has a coverage report showing it reaches the parser past its length
    checks.

  Fails if a target only ever rejects at the first byte.

### W2: `sapflow-dtls`

- **A2.1 Key export.** A handshake completes against a black-box OpenSSL DTLS
  1.2 client (`s_client -dtls1_2 -use_srtp ... -keymatexport
  EXTRACTOR-dtls_srtp`). Both sides export byte-identical SRTP keying
  material.
- **A2.2 Captures and abuse.** Every captured ClientHello, replayed, gets a
  correct first flight. A full handshake with each target needs a live peer
  and is part of A3.1. Firefox's ClientHello is not captured yet, so it is
  pending. Malformed, oversized, out-of-order and retransmitted flights are
  each refused or handled within the caps, with one test per case.
- **A2.3 Fingerprint.** A peer certificate that does not match the offer's
  fingerprint is refused. Fails if any build or test flag can skip the check.
- **A2.4 Fuzzing.** Records and each handshake message have fuzz targets, held
  to A1.3's corpus and coverage rules.

Implementation notes (11 November 2026):

- **Crate.** `crates/sapflow-dtls`, sans-IO, with RustCrypto only: `p256`
  0.14, `ecdsa` 0.17, `aes-gcm` 0.11, `sha2` 0.11, `hmac` 0.13, `x509-cert`
  0.3 and `der` 0.8.
- **Our certificate.** It is DER written by sapflow and signed with `p256`.
  `x509-cert` parses only the peer's. Ours is valid from a day before
  process start to a year after it, and W3 makes a new one at each start.
- **Expected fingerprints.** `Server::new` takes one to four `sha-256`
  fingerprints from the offer (RFC 8122 allows several lines), and any one
  may match.
- **Bounds and defaults.** These are listed in the crate README.
  - HelloVerifyRequest is off by default, because ICE has already proved
    return routability.
  - Flights are re-recorded for every send.
  - A peer's retransmission triggers a resend only when it repeats the
    start of the hello we answered (the version and random included), or
    when an authenticated Finished arrives again after the handshake.
- **A2.1.** OpenSSL 4.0.3 `s_client` matched our exported keying material
  byte for byte in eight runs. Both profiles ran, with 300-byte MTUs, a
  dropped first flight, a dropped final flight and the cookie exchange. The
  datagrams and OpenSSL's key log are fixtures. Unit tests replay them offline, derive the exporter
  and record keys from OpenSSL's master secret, and feed both halves of the
  material to `sapflow-rtp`.
  - That last step runs `sapflow-rtp` on both sides. GCM SRTP stayed
    unchecked against a real peer's packets until W3a, where Chrome 157's
    SRTP and SRTCP under `SRTP_AEAD_AES_128_GCM` authenticated, live and in
    an offline replay (see W3a below). `SRTP_AES128_CM_HMAC_SHA1_80` has
    met a real peer only through FFmpeg in W1.
- **A2.4.** The fuzz targets are `dtls_record`, `dtls_handshake_messages`
  and `dtls_server`. Their seeds come from the W0 captures, the OpenSSL runs
  and scripted handshakes.
  - CI's 60 s runs and a coverage report are still to come.
  - A seed replay test and a fixed-seed mutation pass run with the unit
    tests.
- **Review.** On 11 November 2026 a review of this crate found no defect in
  the protocol or crypto code, and asked for fixes. Those fixes are made.
  That review was by an agent of the same session as the crate's author,
  which A4.4 excludes, so it does not count towards A4.4. A4.4 still needs
  an independent review of `sapflow-dtls` and of SRTP in `sapflow-rtp`,
  with every finding recorded.
- **For W3.**
  - **Handshake deadline.** W3 needs its own. With the defaults (a 1 s
    timer doubling to 60 s, six sends), an unanswered flight 4 runs to
    about 63 s before `Error::Timeout`.
  - **Epoch 0 injection.** An unauthenticated epoch 0 packet, such as a
    forged fatal alert, can end a handshake. W3 limits the damage:
    - each `Server` receives only datagrams from its selected ICE-validated
      5-tuple;
    - handshake failures are counted by reason, without addresses;
    - the publisher recovers with a new WHIP `POST`.

### W3: `sapflow-whip` and the server

- **A3.1 End to end.** Headless Chrome with fake media publishes through
  `POST` to a local sapflow. The stream plays as LL-HLS in hls.js, and the
  recording opens. Evidence: the test log and a decoded frame. The DTLS
  handshake also completes with every target in
  [The browser profile](#the-browser-profile) (moved from A2.2), Firefox
  included once its ClientHello is captured.
- **A3.2 Leaving and returning.**
  - `DELETE` after media sends `unpublish` at once and holds the stream.
  - A new `POST` within the grace continues the same stream, and one after
    the grace starts a new one.
  - A `DELETE`, or a handshake timeout, before any media releases the path
    with no callback.
  - Every refusal in [Resources](#resources) is observed with its status and
    reason.
- **A3.3 Supersede.**
  - A second `POST` while the first session is still delivering media gets
    `409`.
  - Once the first session has delivered nothing for `SAPFLOW_WHIP_STALE`, a
    second `POST` takes over without waiting for consent expiry.
  - A second `POST` during the first session's handshake replaces it at once.
- **A3.4 Panic isolation.** A panic injected while handling one session ends
  only that session. A second live session keeps publishing without a gap.
- **A3.5 Fuzzing.** STUN, SDP and the WHIP request parser have fuzz targets,
  held to A1.3's rules.
- **A3.6 Privacy and default.**
  - With `SAPFLOW_WHIP_ADDR` unset, no WHIP socket is bound (checked with the
    process's open sockets).
  - With trace logging on, an A3.1 run's logs contain neither the token, nor
    any address from the offer, nor the publisher's address.
- **A3.7 Bounds and reflection.**
  - Each cap in [Concurrency and resource bounds](#concurrency-and-resource-bounds)
    refuses at its limit.
  - A session with no completed handshake is gone 10 s after its `201`, and
    one without consent checks is gone 30 s after the last.
  - Unauthenticated STUN, DTLS and random datagrams sent to the UDP port from
    a fresh address produce zero bytes in reply (packet capture).
  - A captured check re-sent from a fresh address draws zero bytes once
    its original has arrived, while its ID is remembered; any mix of
    re-sent checks and copied SRTP from an address draws at most four
    times what that address sent (packet capture).
- **A3.8 Audio and video sync.** A browser publishing from a test file with a
  click and a flash at known times produces HLS whose audio is no more than
  40 ms early or 60 ms late against the video (the EBU R37 range).
- **A3.9 Smoke.** `scripts/smoke.sh` covers WHIP alongside SRT and RTMP.
- **A3.10 OBS keyframes and rate.**
  - Record whether OBS's WHIP output answers PLI with a keyframe, and whether
    it changes its sending rate on REMB or receiver reports.
  - Evidence: keyframe arrival times after requests sent at known times, and
    the rate series.

Implementation notes (W3a, 11 November 2026):

- **Crate.** `crates/sapflow-whip`, sans-IO, with the same lints as W1
  and W2: STUN, the ICE-lite agent, SDP offer parsing and answer building,
  RFC 4571 framing, the WHIP resource rules (token split, 128-bit session
  IDs, refusal statuses, `Location`) and the session that composes ICE,
  `sapflow-dtls`, SRTP and the `sapflow-rtp` receiver. It adds no crate the
  workspace did not already pin: `hmac` 0.13.0 and `sha1` 0.11.0, as
  `sapflow-rtp` uses them, for MESSAGE-INTEGRITY; CRC-32 is our own table.
  The details and bounds are in the crate README.
- **Decided in W3a.**
  - Unauthenticated STUN gets no answer at all, not 400 or 401; 420 and
    487 go only to requests that passed MESSAGE-INTEGRITY. A response that
    would be larger than its request is not sent.
  - The selected tuple is first set, as well as moved, only by a
    successful check with USE-CANDIDATE. Up to four DTLS datagrams from a
    validated tuple are held until it is nominated. Chrome 157 sent its
    two ClientHello fragments that way in every gate 2 run.
  - SRTP and SRTCP are taken from any validated tuple, not only the
    selected one. RFC 8445 section 12.2 says an agent should be prepared
    to receive data on any candidate from the most recent exchange; a lite
    agent learns the publisher's only from its checks, so the validated
    tuples stand in for them. Consent (RFC 7675) governs sending only. Chrome 157 nominated a second pair in
    every gate 2 run and kept sending on the first for a moment. The two
    runs that took SRTP only from the selected tuple lost 3 Opus packets
    each (one counted 10 packets dropped); the three after the change lost
    none. DTLS input stays on the selected tuple.
  - After review, replayed checks (see Concurrency and resource bounds):
    - the agent remembers the transaction IDs of its last 512
      authenticated requests;
    - every tuple is held to four times what it sent, for the whole
      session; a second review showed that a copied SRTP packet, fresh
      if it beats its original, had lifted the limit in an earlier
      version that exempted tuples proven by SRTP (45384 bytes for 224
      over 60 s in a test, now 148). One check
      and the smallest ClientHello in the profile (about 120 bytes) must
      draw the check's 64-byte response and our 637-byte first flight. At
      one times, Safari's 153-byte hello would have waited for
      retransmissions. QUIC's limit is three (RFC 9000 section 8.1), but
      its clients pad their first datagrams to 1200 bytes. The DTLS cookie
      exchange would not help: it proves the address once, at the hello,
      and is not bound to the address;
    - sends follow the selected tuple until SRTP or SRTCP arrives, then the
      tuple of the latest fresh SRTP or SRTCP with current consent. Short
      of a push-out (see the eviction paragraph under Concurrency and
      resource bounds), after a won race they are back with the publisher
      at its next packet that arrives before any copy of it (20 ms of
      audio in the test). A push-out, or an attacker that wins every race,
      can keep sends away from the publisher, within the per-tuple bound;
    - eviction takes a tuple that has never delivered fresh media, then
      the one with the stalest, never the selected or send tuple. A third
      review showed that eviction by age alone let forgotten copies push
      the publisher's tuple out after one copied packet (2 s of its audio
      dropped in a test, now none);
    - consent is kept per tuple and read for the send tuple;
    - a connected session ends 30 s after its last fresh SRTP or SRTCP.
  - The answer puts the configured candidates in its tagged section and
    our credentials, fingerprint and `setup:passive` in every accepted
    section, as W0's answer did for every target. It does not negotiate
    `rtcp-rsize`. It answers `transport-cc` for Opus too when the offer
    lists it there.
  - The handshake deadline is the session's (10 s), so `sapflow-dtls`'s
    63 s timeout never runs. A DTLS failure ends the session with
    `EndReason::Dtls` and its reason, for W3c to count.
- **Gate 1, in process.** For both SRTP profiles a scripted publisher (the
  test STUN client, the `sapflow-dtls` test client, `sapflow-rtp`'s SRTP
  protect) sends H.264 in STAP-A and FU-A and Opus. The session emits
  exactly the access units and audio sent. The client unprotects the PLI
  sent as SRTP comes up, receiver reports, SDES and transport-cc. Tests
  also cover DTLS from an unvalidated tuple, unauthenticated STUN (zero
  bytes), 487, USE-CANDIDATE moving the tuple, consent expiry, the
  handshake deadline, a fingerprint mismatch, MID matching and RTX repair.
- **Gate 2, a real browser.** This is not A3.1: no server, LL-HLS or
  recording is involved.
  - Headless Google Chrome 157 with fake media, on the test host, published
    through `POST` to `examples/whip_harness.rs` on loopback, in five
    runs. Every run reached every milestone:
    - ICE nominated at about 120 ms after the `201`;
    - DTLS connected 4 to 6 ms later, with `SRTP_AEAD_AES_128_GCM`;
    - first SRTP packet decrypted;
    - first Opus at about 1.65 s, after the 1.5 s start hold, and the
      first H.264 keyframe at 1.64 to 3.14 s. The hold ended by arrival
      time (`Fallback::Timeout`): not every stream had a sender report
      within it;
    - feedback sent.
  - Chrome's own statistics showed DTLS 1.2 as the client, 6 to 8 PLIs
    received and 5 to 7 keyframes encoded in 10 s, and its target rate
    rising from 30 kbit/s to between 455 kbit/s and 1.7 Mbit/s.
  - Chrome sends RTX while its rate ramps up. In the two runs that counted
    payload types, 43 of 182 and 33 of 196 video-section packets were RTX.
    10 and 9 of them repeated packets already delivered, which
    `sapflow-rtp` counts as late; the rest were padding only. Another run
    counted 628 of 819 video packets late. Bandwidth probing is the likely
    cause; it was not confirmed.
  - One run is kept in `crates/sapflow-whip/tests/fixtures/chrome-157/`:
    - the offer without candidates or addresses;
    - every DTLS datagram both ways;
    - the leaf certificate;
    - the first SRTP and SRTCP packets, from Chrome's built-in fake camera
      and microphone (generated, not a camera).

    A unit test replays it with the same seeded session. Our flights match
    byte for byte, the leaf matches the offer's fingerprint, and Chrome's
    GCM SRTP and SRTCP authenticate.
  - Firefox was not run. It is no longer installed on the test host, and
    W0 found that its ICE could not complete on that host. So Firefox's
    ClientHello is still not captured.
- **Fuzzing.** `stun_message`, `sdp_offer`, `rfc4571_frames` and
  `whip_session`. The session target scripts honest checks and DTLS
  flights so it reaches the authenticated paths. With an ID cache of 16 so
  that checks are forgotten, and room for 3 validated tuples of the 6 it
  uses so that tuples are evicted, it delivers copies of captured checks
  and of fresh SRTP and SRTCP from five other tuples, ahead of and after
  their originals. After every datagram in and every one out it checks
  that every tuple has been sent at most four times what it sent, STUN
  responses included, that a tuple that never authenticated gets only a
  STUN error response no larger than its request, that DTLS and RTCP go
  only to the send tuple, and that an evicted tuple had the stalest media
  of those that could go; after every step, that output is bounded and the
  selected and send tuples are validated. Seeds come from the RFC 5769
  vectors, the W0 offers and TCP captures, the recorded Chrome offer and
  scripted sessions; a seed replay test and a fixed-seed mutation pass run
  with the unit tests. CI's fuzz job picks the targets up from `cargo fuzz
  list`. A coverage report is still to come. These cover A3.5's STUN and
  SDP parsers; `whip_request` (W3c) covers the WHIP request rules.
Implementation notes (W3c part 1, 11 November 2026):

- **Shape.** `sapflow_whip::router::Router` is the UDP owner's sans-IO
  core: every session on one socket, checks routed by USERNAME, everything
  else by the tuple's owner, with a session cap
  (`SAPFLOW_MAX_PUBLISHERS`). In the server, `whip.rs` binds both sockets
  at start, serves signalling on tokio with `http_lite`, and runs one task
  for the media socket and the router. Each session's access units and
  Opus packets go to its own ingest task over a queue bounded at 1024
  items and 16 MiB; that task owns the `Publication` and feeds a new
  elementary-stream source (`EsIngest`, stereo Opus as answered), so the
  stream is packaged and recorded by the same code as SRT's and RTMP's. A
  full queue marks the recording, asks the publisher for a keyframe at
  once, and makes video wait for the next IDR. Only SRTP and SRTCP that
  the session authenticated count against the bitrate cap, so bytes
  spoofed from a publisher's tuple cannot push it over. Health and publish
  callbacks go through the same `Publication`, untested for WHIP. A failed
  receive on the media socket, and a failed accept on the signalling
  listener (retried after 100 ms), are logged and passed over, as for SRT.
  The trusted proxy is compared by canonical address, so a dual-stack
  listener that sees it IPv4-mapped still believes it. The request rules
  are a pure module, `whip_request.rs`, which the `whip_request` fuzz
  target includes.
- **Tuple ownership.** The rule written for W3c, "the session whose check
  from it came last", fails: a valid check for session X, copied or
  spoofed from the tuple Y's publisher is sending media on, gave X that
  tuple, and every one of Y's packets after it went to X and was refused
  (50 of 100 in the test). So a session that a check it takes as new
  validates from a tuple takes that tuple unless its owner still has it
  validated and either has had fresh SRTP or SRTCP from it within 2 s or
  has not connected yet. A retransmission, a replayed ID or a request that
  fails authentication moves nothing. Left open:
  - after a publisher is silent from its tuple for more than 2 s, one
    valid check for another session from it, copied or spoofed, takes it,
    and the publisher's media is dropped. If the session that took it has
    connected, the publisher's own next check takes it back (that session
    has no fresh media from it). If it has not connected, it keeps the
    tuple while it checks or handshakes, and the publisher's media is
    dropped until it connects or ends at its handshake deadline, up to 10 s
    after it was created, and then until the publisher's next check from
    the tuple takes it back (once that session has ended, the tuple has no
    owner and the publisher's media from it goes nowhere until that
    check);
  - an admitted publisher that can spoof addresses can take a tuple
    whenever its owner may lose it: before the owner's first check from
    it, or after the owner has been silent from it for more than 2 s. If
    it then sends its own SRTP from the tuple, it keeps it for as long as
    it does, and the victim cannot connect on it, or its media from it is
    dropped.

  Letting the publisher's check take a tuple back from a session that has
  not connected would close the second case of the first item, but checks
  replayed for a session whose publisher has gone (with their IDs
  forgotten) could then break the handshake of a new publisher given the
  same port by a NAT, so the rule is unchanged.
- **Admission.** A `POST` goes through `Publication::begin_authorised`
  with protocol `webrtc`: the publish-auth request, the failed-attempt
  throttle and the global publisher, as for SRT and RTMP. Only the global
  publisher's refusal and `409` are tested for WHIP. A WHIP claim never
  takes over an attached publisher, so a second `POST` gets `409` even
  while the first is handshaking (superseding is not built yet). By the
  code, and untested: an SRT or RTMP publisher still takes over a live
  WHIP stream in global-publisher mode, and an app's 403 answers `401`
  like its 401.
- **`DELETE`** needs the bearer token again (compared as SHA-256 digests)
  and answers `200` once the publication has been released, so a `POST`
  that follows at once is admitted rather than refused `409`.
- **Loss.** A recording is marked incomplete from the `sapflow-rtp`
  receiver and the session, never from the muxer's own discontinuities: a
  lost Opus packet is filled with silence and the timeline can absorb its
  timestamp jump, so audio-only Opus can look continuous to the muxer.
  Every path by which media goes missing marks it:
  - `Lost`: sequence numbers never received within the jitter wait, after
    NACK and RTX repair;
  - `IncompleteFrame`, and `FrameSkipped` after loss: video frames dropped;
  - `Discontinuity` for a confirmed sequence restart (a jump of more than
    3000, reported with no `Lost`) or an Opus timestamp jump too big to
    fill;
  - `Refused`, and Opus silence filled for loss (`FrameKind::Loss`), which
    is how a refused Opus payload shows, with no sequence gap;
  - the session's `MediaDropped`: events the receiver's queue had no room
    for, and, when the session ends, packets still held behind a gap or in
    the start hold (`Receiver::unreleased`);
  - a full queue to the ingest task, by items or by bytes;
  - an end that is not orderly (consent or media timing out, DTLS
    failing, keys spent), since the last media may be missing with no
    later packet to show a gap.

  Frames skipped before the first IDR lose nothing and do not mark, nor
  does an orderly end (`DELETE`, our close, the publisher's
  `close_notify`); loss just before an orderly end cannot be seen. A run
  that loses nothing stays complete. Each session's lost packets are
  logged when it ends.
- **A re-anchor with no loss is not a mark.** When the `sapflow-rtp`
  timeline re-anchors on arrival time (`Discontinuity::Arrival`: the
  publisher's packets have fallen more than 500 ms behind their own RTP
  clock, as a delivery delay that ramps up does) or moves forward to a
  sender report, no media is missing. For an audio-only stream the server
  then ends the muxer's run deliberately (`Muxer::interrupt`, as a
  reconnect does) before the jumped packet, so the packet starts a new run
  after an HLS discontinuity, which keeps live latency bounded, and the
  recording stays complete. Fed straight to the muxer, the same jump reads
  as a break, which it reports as loss. A video-led stream's muxer takes
  the jump without reporting loss, and an interrupt there would drop its
  pending frame and wait for a keyframe, so it is left alone. Re-anchors
  are counted per session (`reanchors`) and logged at debug. A re-anchor
  after an outage that lost packets is still marked, by the loss.
- **Opus timing.** Audio-only Opus discontinuities under a slewed timeline
  (Open after W3b, above) belong to `sapflow-hls`; the server does not
  work around them. For an audio-only stream, `EXT-X-PROGRAM-DATE-TIME`
  drifts from wall-clock time by the sender's clock skew (about 0.36 s an
  hour at 100 ppm), as AAC from SRT and RTMP already does. That is
  expected, and the server leaves program dates alone.

| Claim | Test | Status |
|---|---|---|
| A3.1: Chrome publishes through `POST`, LL-HLS plays in hls.js, the recording opens | `scripts/whip-e2e.js` | Partial: headless Chrome 157 on the test host, the gate run: 10.1 s played at 640x360 (206 frames decoded, none dropped); the recording decoded 251 H.264 frames at 640x360 and 625 stereo Opus frames, and was finished complete, and a frame from each was saved. Other browsers, and the DTLS handshake with every target, are later gates |
| A3.1 in process: `POST`, ICE, DTLS, SRTP over UDP, media in the playlist | `whip::tests::a_publisher_reaches_the_stream_through_post_and_udp` | Met, with the scripted publisher |
| A3.2 (basic): `DELETE` needs the token, answers `200`, the session is gone, and an immediate `POST` for the path is admitted (`201`) | the same test | Partial: the callbacks, the grace and a handshake timeout at server level are not observed |
| Refusals before admission: 404, 405 with `Allow`, 401, 415, 413 by `content-length`, 400, 422; 409 and 401 from admission | `whip::tests::refusals_carry_their_statuses`, `whip_request::tests` | Met for these. 403, 429 and 503 from the app are not tested |
| `503` past `SAPFLOW_MAX_PUBLISHERS` sessions, its path released | `whip::tests::past_the_session_cap_a_post_gets_503` | Met |
| CORS only for configured origins, refusals included; preflight methods, headers and `Accept-Post` | `whip::tests::cors_only_for_configured_origins`, `whip_request::tests::cors_only_for_configured_origins` | Met |
| `X-Forwarded-For` only from the trusted proxy, its own entry | `whip_request::tests::forwarded_for_only_from_the_trusted_proxy` | Unit tests only |
| A3.5: the WHIP request parser has a fuzz target | `whip_request`; its six seeds replayed by `whip_request::tests::seeds_hold_the_fuzz_checks`, and 12,000 fixed-seed mutations of them by `whip_request::tests::mutated_seeds_hold_the_fuzz_checks` | Met for 60 s runs: on the test host, built with `-s none` and `RUSTFLAGS="-Clink-arg=-undefined -Clink-arg=dynamic_lookup"` (`sapflow-signet-ffi`'s library otherwise fails to link the coverage hooks), it ran 8,338,071 times in 60 s in the review and 7,172,707 times in 61 s in the final gate, with no crash. CI's fuzz job, which lists targets with `cargo fuzz list`, has not run it yet |
| A3.6: nothing bound with `SAPFLOW_WHIP_ADDR` unset, and the media address alone refused | `scripts/whip-e2e.js` (lsof on the process), `config::tests::whip_is_off_unless_its_address_is_set` | Met |
| A3.6: an A3.1 run's trace log holds no token, offer address, port or mDNS name, or publisher address | `scripts/whip-e2e.js` | Met for that run (44 literals); the check first finds a planted token |
| A check for another session from a media tuple does not cost its publisher media | `router::tests::a_check_for_another_session_cannot_take_a_media_tuple` (with and without USE-CANDIDATE) | Met |
| The first session keeps a tuple through its handshake | `router::tests::the_first_session_keeps_a_tuple_through_its_handshake` | Met |
| A silent tuple passes to the session that checks from it (NAT reuse) | `router::tests::a_silent_tuple_passes_to_the_session_that_checks_from_it` | Met in process |
| A replayed or forged check moves nothing | `router::tests::a_replayed_or_forged_check_moves_nothing` | Met |
| The two limits above | `router::tests::a_check_that_beats_the_first_one_owns_the_tuple`, `router::tests::a_publisher_back_after_a_gap_reclaims_its_tuple_at_its_next_check`, `router::tests::an_unconnected_session_keeps_a_taken_tuple_until_its_deadline`, `router::tests::spoofed_media_keeps_a_tuple_taken_after_a_silence` | Shown |
| Datagrams for no session draw nothing | `router::tests::unroutable_datagrams_draw_nothing` | Met in process; the packet-capture check is A3.7 |
| A full queue marks the recording, asks for a keyframe once, and video waits for an IDR | `whip::tests::a_full_queue_skips_video_to_the_next_idr` | Met |
| The queue is bounded in bytes (16 MiB a session) as well as items, and past it is a full queue | `whip::tests::the_queue_is_bounded_in_bytes` | Met |
| Only authenticated SRTP and SRTCP count against the bitrate cap | `whip::tests::only_authenticated_media_counts_against_the_cap` | Met |
| A lost packet marks the recording from the receiver; the muxer alone does not see it; a run without loss is not marked; lost packets are counted | `whip::tests::a_lost_packet_is_marked_from_the_receiver_not_the_muxer`, `whip::tests::a_lost_packet_marks_the_recording_from_the_receiver` (over UDP), `whip::tests::receiver_loss_is_counted_and_marks_the_recording` | Met |
| A confirmed sequence restart (no `Lost`) marks the recording | `whip::tests::a_sequence_restart_marks_the_recording` | Met |
| A refused Opus payload (silence filled for loss, no `Lost`) marks the recording | `whip::tests::a_refused_opus_payload_marks_the_recording` | Met |
| Packets held behind a gap when the session ends mark the recording | `whip::tests::media_held_when_the_session_ends_marks_the_recording` | Met |
| Receiver events dropped for want of queue room mark the recording | `whip::tests::receiver_events_dropped_for_room_mark_the_recording` | Met |
| The muxer alone, given the same access units and audio, marks none of the four paths above | the same four tests, through `marked_only_by_the_server` | Met |
| An end that is not orderly marks the recording; an orderly one does not | `whip::tests::an_abnormal_end_marks_the_recording` | Met |
| A run that loses nothing stays complete | `whip::tests::a_clean_close_leaves_the_recording_complete`; `whip::tests::a_clean_run_over_udp_stays_complete` (the test publisher paced by its media clock); `scripts/whip-e2e.js` (session ended with `lost=0`, recording finished complete) | Met. On the test host, the clean UDP run stayed complete in 100 of 100 runs of the whole server test binary under a 16-process CPU soak, and in 100 of 100 runs alone under a 32-process soak. Before the publisher was paced by its media clock, 61 of 100 whole-binary runs marked it: each time its packets had fallen more than 500 ms behind their RTP clock, the timeline re-anchored (+501 to +512 ms), and the muxer read the jump as a break |
| A re-anchor with no loss leaves the recording complete, gives one HLS discontinuity and is counted; the muxer alone would have marked it | `whip::tests::a_delay_ramp_with_no_loss_reanchors_without_marking` | Met in process (audio-only) |
| An outage that loses packets and then re-anchors is still marked | `whip::tests::an_outage_that_loses_packets_and_reanchors_is_marked` | Met in process |
| Each mark says which path set it | the session's loss counts (`LossCounts`), logged when it ends and given in the UDP tests' failure messages | Met |
| A full queue's keyframe request sends a PLI at once, with no keyframe interval | `tests::request_keyframe_sends_a_pli_at_once` (sapflow-whip) | Met at the session; the server's call is not tested end to end |
| A dual-stack listener believes `X-Forwarded-For` from an IPv4 proxy seen IPv4-mapped | `whip_request::tests::forwarded_for_only_from_the_trusted_proxy` | Unit tests only |
| WHIP settings | `config::tests::whip_is_off_unless_its_address_is_set` | Met |

- **Left to W3c part 2.**
  - The throttle as a WHIP refusal path of its own, and pending-session
    caps, in total and per address.
  - Supersede (A3.3), from `Session::last_media`.
  - Running each session call under `catch_unwind` (A3.4).
  - Counting end reasons.
  - A3.2 beyond the basic `DELETE`: the callbacks and the grace, and every
    refusal observed.
  - A3.7's bounds and the packet-capture tests, as invariants on the
    router and session harness.
  - The UDP owner scans every session for due timers on each datagram
    (`Router::poll_timeout` and `Router::handle_timeout`); a deadline
    index would make that cost independent of the session count.
  - Slow signalling clients: 32 of them can hold every slot for about
    20 s each (5 s for the head, then 10 s and the drain), and a body
    that arrives too slowly gets 503 rather than 408.
  - The end-to-end privacy check (`scripts/whip-e2e.js`) does not look for
    127.0.0.1 on its own, since the server's listeners log it; it checks
    the publisher's address with its port. A run from a non-loopback
    address would check the address alone.
  - `rtmp.rs` and `publish_control.rs` still end on an accept error, as
    `whip.rs` did; they need the same fix.
  - `publish_auth::tests::neither_the_password_nor_the_url_reaches_logs_or_refusals`
    sometimes misses its log line when the server's tests run in
    parallel (3 times in about 95 runs of the whole test binary on a
    loaded host), probably because its subscriber is thread-local
    (`set_default`). It predates W3c.
  - WHIP's recording marks are reported under the SRT- and RTMP-named
    callback codes (`srt-unrecovered-loss` is not used, but
    `srt-ingest-queue-full`, `srt-timeout` and `rtmp-protocol-error` are,
    beside `media-loss`). The codes are part of the v4v contract, so they
    stay as they are until the v4v integration.
  - `SAPFLOW_SEGMENT_COUNT`: since the live-window fix in `sapflow-hls`
    (#71), the playlist keeps at least three target durations and can
    hold more than the count (up to 256 entries, or 64 MiB of media
    beyond it); `--help` and any server doc that states the count as
    exact should say so.
  - A muted video section draws a PLI every second: `sapflow-rtp` asks
    again every `keyframe_retry` while it waits for an IDR. That case has
    no test yet. The per-tuple bound holds whatever happens, since a send
    over budget is dropped; what the budget decides is whether each PLI
    goes out, which depends on the publisher's sender reports and checks
    keeping its tuple's budget up.
  - Eviction memory. A tuple validated again starts with no media, so a
    publisher that comes back after being pushed out is first in line to
    go again. Consider keeping a tuple's last media time across
    re-validation, in a small bounded memory of evicted tuples. No test
    yet covers an honest NAT rebinding or an honest pair move with the
    table full; add both.
  - A publisher's `close_notify` can be lost. DTLS is taken only from the
    selected tuple but sent to the send tuple, so once they differ a
    `close_notify` from the media tuple is dropped. A graceful end should
    rely on `DELETE` and the media timeout, not on it.
- **Left to W3d.** PROXY protocol v2 and the TCP listener: built in W3d,
  below, with the partial-header timeout, a decision on LOCAL, the note on
  ports, and `parse_trusted` on every connection.

Implementation notes (W3d, 11 November 2026):

- **Tuples.** `sapflow_whip::Tuple` gained `connection`: 0 for UDP, and for
  TCP the listener's number for the connection, never reused while the
  process runs. A TCP tuple is `Tuple::tcp(publisher address, number)`.
  The address alone could not name a connection: a PROXY header can repeat
  one, and a port is reused after a close. Tuples compare transport,
  address and number, so a TCP tuple never equals a UDP one.
- **Session and router unchanged.** TCP has its own handshake, so a source
  cannot be spoofed on it except through the trusted proxy; the rules are
  kept anyway, the 4x per-tuple budget included. A copy of a check or of
  SRTP sent on another connection lands on that connection's own tuple, so
  it cannot take or pollute the publisher's. The budget counts packets on
  both sides, not their length prefixes.
- **The listener** (`whip_tcp.rs`, with the sans-IO front in
  `whip_tcp_front.rs`):
  - one task accepts; past 128 connections a new one is closed at once;
  - one task per connection reads 4 KiB at a time. With PROXY protocol v2,
    a peer other than the trusted proxy is closed before a read, and
    `parse_trusted` reads the header within 5 s of accept. A PROXY header
    gives the publisher's address; LOCAL is closed. Then the per-address
    cap (32) applies, keyed on that address. Frames (at most 1500 bytes; a
    longer one ends the connection) go to the UDP owner as
    `Tuple::tcp(address, number)`;
  - the UDP owner takes them through `whip.rs`'s own receive path, so only
    authenticated SRTP and SRTCP count against the bitrate cap, as for UDP.
    Its sends to a TCP tuple go into that connection's queue (256 frames,
    256 KiB, framed) with `try_send`; a full queue, or a connection that
    has gone, drops the packet and counts it. A TCP tuple never falls
    through to the UDP socket;
  - a connection closes when nothing has been sent back to it within 10 s
    of its start, or within 30 s of the last frame sent back. Sapflow sends
    nothing to a tuple whose check has not passed MESSAGE-INTEGRITY, so
    these judge by what the session answered, not by what the peer sent.
    One write may take 5 s;
  - a failed accept is logged and retried after 100 ms, as on the
    signalling listener.

  The hook in `whip.rs` is one `select!` arm, the send dispatch and the
  bound listener in `Sockets`. The limits are constants (`whip_tcp::Limits`),
  not settings; tests set them small.
- **LOCAL.** Closed once its header is in, no frame read (see Connectivity).
- **Fuzzing.** `whip_tcp_front`: a mode byte picks a direct connection or
  PROXY v2 from the trusted proxy (as itself or IPv4-mapped) or another
  peer, and the stream is cut two ways in pushes of at most 4096 bytes.
  Both must give the same frames, address and close; another peer is
  refused unread; frames are non-empty and at most 1500 bytes; a direct
  connection's frames are exactly the RFC 4571 decoder's; behind the proxy,
  frames come only after a PROXY header, whose canonical source is the
  address, and LOCAL or a refused header gives none. Eight synthetic seeds;
  a seed replay test and a fixed-seed mutation pass run with the unit
  tests. On the test host, built with `-s none` and
  `RUSTFLAGS="-Clink-arg=-undefined -Clink-arg=dynamic_lookup"`, it ran
  7,474,458 times in 121 s from the seeds with no crash. Every fuzz
  target builds that way (38).
- **End to end** (`scripts/whip-e2e.js --transport tcp|proxy`, headless
  Google Chrome 157 on the test host, loopback, ports 47400 to 47447). The
  answer advertised only a TCP candidate, so ICE-TCP was the only way in:
  - **Direct** (`tcp 127.0.0.1:47406`): the nominated pair was Chrome's
    active TCP candidate (peer-reflexive) to our passive one. Chrome opened
    5 connections, one per local candidate. hls.js played 10.1 s at
    640x360 (206 frames decoded, none dropped). The recording decoded 246
    H.264 frames at 640x360 and 609 stereo Opus frames, and was finished
    complete; the session ended `Closed` with `lost=0`.
  - **Longer than the timeouts** (`--seconds 60`, direct): the session ran
    about 79 s (3,944 Opus frames recorded) and ended `Closed`, `lost=0`,
    complete. All 5 connections stayed open until Chrome closed them: none
    closed idle, so Chrome kept checking its spare pairs and they were
    answered. hls.js stalled in that run on the SPS change below.
  - **Through a PROXY v2 relay** (`tcp 127.0.0.1:47427`): a test-only Node
    relay, written from the specification, prefixed each connection with a
    PROXY v2 header and forwarded it from `::1` to the listener on
    `[::1]:47426`, which trusted only `::1`. The browser's `POST` came from
    127.0.0.1, so it was not taken as the proxy's. The pair was again
    active to passive TCP. 10.1 s played at 640x360 (205 decoded, none
    dropped); the recording decoded 247 H.264 frames at 640x360 and 613
    Opus frames, complete, `lost=0`.
  - **Privacy** in both: the trace log held none of 54 literals: the
    token, the offer's addresses, ports and mDNS names, and the
    publisher's 5 real TCP ends (address and source port, from `lsof` on
    sapflow or from the relay; the run fails without them, since the
    offer's active candidates say port 9). The check first caught a
    planted token.
  - **UDP** still passes after the change: 10.0 s at 640x360, 205 frames
    decoded, none dropped; recording 243 H.264 and 609 Opus frames.

  These figures are from the final build; earlier builds of the branch
  gave the same results.
  - **Runs that failed.** Chrome changed its SPS mid-stream to 480x270 in
    3 of 6 direct runs, 2 of 4 relay runs and 1 of 4 UDP runs, mostly
    while the test host's load average was up to 331 from other jobs, once
    at about 8. `sapflow-hls` keeps the first SPS (its warning says so),
    and hls.js decoded 4 frames and stalled. The recordings still decoded
    (249 to 584 frames at 480x270) and were complete. It happened over UDP
    too, so it is not the transport's, and it belongs to `sapflow-hls`;
    whether TCP makes Chrome lower its resolution more often is not
    settled by so few runs.
  - **Loopback artefact.** Over UDP on loopback, about 200 sends a run fail
    with `Can't assign requested address`: Chrome's checks from its
    non-loopback host candidates reach the socket bound to 127.0.0.1,
    which cannot answer them. This predates W3d; the debug line now gives
    the error.

| Claim | Test | Status |
|---|---|---|
| A TCP tuple names its connection: a UDP path or another connection from the same address is another tuple, is not validated, and its media reaches no session | `tcp_tests::a_session_over_framed_tcp_aead_aes_128_gcm` (sapflow-whip), `tcp_tests::a_second_connection_is_its_own_pair` | Met in process |
| A whole session over RFC 4571 frames cut at awkward offsets both ways, both SRTP profiles: exactly the access units and audio sent, feedback back over the connection, the check's XOR-MAPPED-ADDRESS the connection's address, the budget dropping nothing | `tcp_tests::a_session_over_framed_tcp_aead_aes_128_gcm`, `tcp_tests::a_session_over_framed_tcp_aes_cm_hmac_sha1_80` | Met |
| The server over ICE-TCP: `POST`, ICE, DTLS and SRTP on one connection, media in the playlist, `DELETE` 200, and nothing sent over UDP to the publisher's address | `whip::tcp_tests::a_publisher_reaches_the_stream_over_ice_tcp` | Met in process (GCM) |
| PROXY v2 from the trusted proxy: the header's source is the publisher's address, canonical, and the session connects | `whip::tcp_tests::a_proxy_v2_header_from_the_trusted_proxy_gives_the_publisher_address`, `whip_tcp_front::tests::the_header_source_is_the_client_and_frames_follow_it`, `whip_tcp_front::tests::an_ipv4_mapped_proxy_is_still_the_proxy_and_its_source_canonical` | Met |
| Any other peer is dropped before a byte is read | `whip::tcp_tests::any_other_peer_is_dropped_unread`, `whip_tcp_front::tests::any_other_peer_is_refused_unread`, the `whip_tcp_front` target | Met |
| LOCAL is closed and no frame on it read | `whip::tcp_tests::a_local_header_is_closed_without_reading_frames`, `whip_tcp_front::tests::local_is_refused_once_its_header_is_in` | Met |
| A partial header closes at the header timeout (5 s) | `whip::tcp_tests::a_partial_header_times_out` (at 300 ms) | Met |
| Past the connection cap (128) a connection is closed at accept, and a slot frees when one closes | `whip::tcp_tests::past_the_connection_cap_a_connection_is_closed` (at 2) | Met |
| Past the per-address cap (32), keyed on the PROXY header's source, a connection is closed; another address is not. A direct connection is keyed on its TCP peer by the same code, untested | `whip::tcp_tests::past_the_per_address_cap_a_connection_is_closed` (at 1) | Met |
| A connection never answered closes at 10 s, whatever it sends; one answered then left quiet, at 30 s | `whip::tcp_tests::unanswered_and_idle_connections_close` (at 300 ms each) | Met |
| A frame over 1500 bytes ends the connection | `whip::tcp_tests::a_frame_over_the_cap_ends_the_connection`, `whip_tcp_front::tests::a_frame_over_the_cap_ends_the_connection` | Met |
| A connection's send queue is bounded in frames (256) and bytes (256 KiB); past either, and for a connection that has gone or a packet over 1500 bytes, the packet is dropped and counted | `whip_tcp::tests::the_send_queue_is_bounded_in_bytes_and_frames` (at 4 frames, 3006 bytes) | Met. That the owner never waits on a connection follows from `try_send`; no test stalls a reader to show it |
| The front's outcome does not depend on how the stream is cut | the `whip_tcp_front` target; `whip_tcp_front::tests::seeds_hold_the_fuzz_checks` (6 inline seeds, 500 mutations each), `whip_tcp_front::tests::committed_seeds_hold_the_fuzz_checks` | Met for a 121 s run (7,474,458 inputs, no crash); CI's fuzz job has not run it yet |
| Configuration refusals and the TCP settings | `config::tests::whip_is_off_unless_its_address_is_set` | Met |
| A3.1 over ICE-TCP: headless Chrome publishes with only a TCP candidate, directly and through a PROXY v2 relay; hls.js plays; the recording opens | `scripts/whip-e2e.js --transport tcp`, `--transport proxy` | Partial: Chrome 157 on loopback, figures above. Other browsers, and A0.3 (a public address on 443, UDP blocked), are open |
| A3.6 over ICE-TCP: the trace log holds no token, offer address, port or mDNS name, or the publisher's real TCP ends | the same runs | Met for those runs (54 literals each) |

Not claimed, because nothing tests them: the 5 s write timeout, the retry
after a failed accept, the per-address cap on a direct connection, and
that the inbound queue's back-pressure stays with the slow reader.

- **Left after W3d.**
  - A0.3: a run against a public address on 443 with UDP blocked, for each
    browser target and OBS.
  - A closed connection is not reported to its session, which sees a path
    gone quiet and ends by consent or the media timeout (30 s) unless
    another path carries on. Ending, or failing over, at once when the
    session's send tuple closes would be quicker.
  - Without PROXY protocol, a proxy in front makes every connection the
    proxy's, under one per-address cap: use v2 behind a proxy.
  - The limits are constants. The total does not follow
    `SAPFLOW_MAX_PUBLISHERS`; at 5 connections a Chrome publisher, 128
    serve about 25 at once.
  - The `whip_session` target still uses UDP tuples. The session treats
    tuples as opaque, so it reaches no new state through TCP, but a TCP
    mix there would show it.
  - CI's fuzz job has not run `whip_tcp_front` yet.
  - The mid-stream SPS change above, in `sapflow-hls`.

### W4: qualification

- **A4.1 Loss.** Apply 1%, 5% and 10% random loss with added jitter (Linux
  `netem`). At the chosen default `SAPFLOW_WHIP_JITTER`, WHIP must leave no
  more frames unrecovered than the SRT receiver with its latency set to the
  same value, on the same loss profile
  ([srt-loss-method.md](srt-loss-method.md)). Fails otherwise, unless the user
  accepts the difference in writing.
- **A4.2 Cost.** CPU and memory per WHIP publisher, measured at the bitrates
  in [benchmarks.md](benchmarks.md) beside the RTMP and SRT publishers on the
  same machine, must be no higher than the SRT publisher's. Fails otherwise,
  unless the user accepts the difference in writing. DTLS, SRTP and ICE add
  work that SRT does not do, so a failure here is a decision point, not
  necessarily a defect.
- **A4.3 Physical devices.** An iPhone (Safari) and an Android phone (Chrome)
  each publish for 30 minutes over Wi-Fi and mobile data, switching network
  at least twice. Fails on:
  - any stop the publisher did not ask for;
  - a switch that is not back live within `SAPFLOW_RECONNECT_GRACE`;
  - a recording missing anything other than marked loss.
- **A4.4 Review.** An independent review of `sapflow-dtls` and of SRTP in
  `sapflow-rtp` is recorded, with every finding resolved or accepted in
  writing. A review by the author, or by an agent of the same session, does
  not count.

### W5: documentation

The README's "When to choose mediamtx instead", [comparison.md](comparison.md)
and the [v4v contract](v4v-contract.md) change only when W4 passes. Until then
they describe what ships.

## Open decisions

| Decision | Settled by |
|---|---|
| `transport-cc` in v1 | Provisionally yes from desktop Chrome; A0.4 on the other targets |
| Whether ICE-TCP is worth carrying | A0.3 against a public address on 443 |
| Default `SAPFLOW_WHIP_JITTER` | A4.1 |
| RustCrypto or `ring` | A4.2 |
| Accepting another video codec for phones without H.264 | A0.5, and would need transcoding downstream |
| Opus to AAC inside sapflow | Decided no (10 November 2026). Revisit if WHIP is used without a downstream converter |
| ICE restart via `PATCH` | Physical results in A4.3 |
| Defaults for `SAPFLOW_WHIP_STALE` and `SAPFLOW_WHIP_OPUS_MAX_BITRATE` | A3.3, A0.7 and A4.3 |
