# W0 evidence: desktop Chrome 157 and the FFmpeg WHIP reference client

Status: evidence only, 10 November 2026. Nothing in Sapflow was built or
changed. This note reports observations against
[the WHIP ingest design](../whip-ingest.md) (A0.1, A0.3, A0.4, A0.5, A0.7). It
does not edit the design.

## Scope and setup

| Item | Value |
|---|---|
| Target | Google Chrome 157.0.8103.40, the installed app, headless (`--headless=new`), fresh temporary profile per run, fake camera and microphone (`--use-fake-device-for-media-stream --use-fake-ui-for-media-stream`) for the capture runs |
| Reference client (not a target) | FFmpeg 8.1.2, `-f whip` muxer (libx264, libopus, OpenSSL build). Labelled a non-target everywhere in this note |
| Host | Apple M4, macOS 26.6.2, shared and busy. Load averages (1 minute, at start and end of each run) are in each table |
| Endpoint | `scripts/whip-w0/`, a fake ICE-lite WHIP endpoint written from the RFCs. It answers STUN and saves the first DTLS flight. It never completes DTLS |
| Not run | Firefox, Safari (macOS and iOS), Chrome on Android, OBS. Nothing was substituted for them |

Before any browser run, `stun-rfc5769.test.js` passed (6 checks: the RFC 5769
request, IPv4 and IPv6 responses reproduced byte for byte, the long-term
credential request, and tamper detection).

Commands, run from the repository root (`<lan-ip>` is the M4's LAN address):

```sh
node scripts/whip-w0/stun-rfc5769.test.js
node scripts/whip-w0/run.js capture udp --host <lan-ip> --out <dir>
node scripts/whip-w0/run.js capture tcp --host <lan-ip> --port 8443 --out <dir>
node scripts/whip-w0/run.js capture tcp --host <lan-ip> --port 443  --out <dir>
node scripts/whip-w0/run.js capture tcp --host <lan-ip> --advertise 203.0.113.5 --port 443 --out <dir>
node scripts/whip-w0/run.js ffmpeg udp [--dtls-active] --host <lan-ip> --out <dir>
node scripts/whip-w0/run.js loopback --which ramp-a|ramp-b|opus --out <dir>
```

Fixtures (addresses and ports scrubbed to documentation values) are in
`fixtures/whip/w0/chrome-157-desktop-macos/` and
`fixtures/whip/w0/ffmpeg-8.1-reference/`.

## Findings that contradict or add to the design's expected profile

1. **Chrome's ClientHello is not a plain DTLS 1.2 hello, and it does not fit
   one datagram.** It is 1413 bytes, sent as two handshake fragments
   (message_seq 0, offsets 0 and 1175) in two datagrams of 1200 and 263 bytes.
   It offers `supported_versions` DTLS 1.3 (`0xfefc`) and DTLS 1.2 (`0xfefd`),
   carries a 1258-byte `key_share`, and lists `X25519MLKEM768` first in
   `supported_groups`. The record version is `0xfeff`. The design says W0
   records which DTLS versions each target offers; this is the record, and
   W2's server therefore needs ClientHello fragment reassembly
   (RFC 6347 section 4.2.3) and must answer with a DTLS 1.2 ServerHello to an
   offer that includes 1.3. The design text does not mention either.
2. **The expected DTLS 1.2 elements are present** in Chrome's hello:
   `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (offered fourth, after three TLS
   1.3 suites), `secp256r1`, `ecdsa_secp256r1_sha256`, `extended_master_secret`
   and `use_srtp`. No contradiction there. The `use_srtp` list is wider than the
   design's table assumes: `SRTP_AES128_CM_HMAC_SHA1_80` first, then
   `SRTP_AEAD_AES_256_GCM` and `SRTP_AEAD_AES_128_GCM`. FFmpeg offers only
   `SRTP_AES128_CM_HMAC_SHA1_80`. A server that supports CM-80 and
   AEAD_AES_128_GCM covers both.
3. **FFmpeg 8.1's default offer is `a=setup:passive`.** RFC 9725 has clients
   offer `actpass` (or `active`). The design refuses an offer that can only be
   passive, so under the design FFmpeg's default would be refused at `POST`. It
   offers `active` only with `-whip_flags dtls_active`. Our fake endpoint
   answered the passive offer `passive` regardless (it does not enforce the
   rule), and FFmpeg then waited for a ClientHello that never came. FFmpeg is a
   reference client, not a target. Whether OBS offers `passive` is unknown and
   matters for the same rule.
4. **TCP 443 could not be verified on a LAN, and Chrome behaved unexpectedly.**
   Chrome 157 never started ICE (no `checking`, no TCP connection, no error)
   when the answer's only candidate was a passive TCP candidate at a private
   address on port 80, 443 or 1023. The same setup worked at 1024 and 8443.
   With an unroutable documentation address instead, Chrome did start ICE and
   paired the candidate on 443 and on 8443 alike. So the observed boundary is
   private address plus port below 1024, not 443 alone, and it is an
   observation of Chrome's behaviour, not a documented rule. No test used a
   reachable public address on 443, so the design's A0.3 premise (selected pair
   over TCP 443) remains unproven for desktop Chrome. UDP was never blocked in
   these runs; it was simply not advertised.
5. **Chrome adds an attribute the RFCs do not define to every connectivity
   check:** `0xc057`, in the comprehension-optional range. A server must ignore
   unknown attributes in that range. Our endpoint did, and responded normally.
6. **Chrome offers H.265 and AV1 as well as VP8, VP9 and H.264.** The design's
   rule (first H.264 payload with `packetization-mode=1` and an accepted
   profile) selects payload 102 (`42001f`, Baseline, since the constraint flags
   are zero), after VP8 in offer order. No contradiction, but the first video
   codec in the offer is VP8, not H.264.
7. **Chrome sends mono Opus unless the answer carries `stereo=1`** (A0.7), which
   supports the design's answer.

No finding contradicts the design's `setup:passive` answer, `ice-lite`, BUNDLE
or `rtcp-mux` rules for Chrome: Chrome offered `actpass`, BUNDLE `0 1`,
`rtcp-mux`, and a `sha-256` fingerprint, and accepted our answer on both UDP
and TCP.

## A0.1 Captured profile (Chrome 157 and FFmpeg 8.1, UDP)

Load average at the start and end of each run in the last column.

| Item | Chrome 157 desktop | FFmpeg 8.1 (non-target) |
|---|---|---|
| Offer file | `chrome-157-desktop-macos/offer.sdp` | `ffmpeg-8.1-reference/offer-default.sdp`, `offer-dtls-active.sdp` |
| DTLS setup in offer | `actpass` | `passive` (default), `active` with `dtls_active` |
| ICE | trickle option; 6 UDP host and 6 TCP active host candidates per section; no `ice-lite` | no candidates in the offer; one STUN check seen |
| Sections | audio mid 0, video mid 1, BUNDLE `0 1`, `rtcp-mux`, `rtcp-rsize` | same layout, `rtcp-mux` |
| Direction | `sendonly` | `sendonly` |
| Header extensions (video) | toffset, abs-send-time, video-orientation, transport-wide-cc, playout-delay, video-content-type, video-timing, color-space, sdes:mid, rtp-stream-id, repaired-rtp-stream-id | none |
| Feedback (H.264) | goog-remb, transport-cc, ccm fir, nack, nack pli | nack only |
| ClientHello | `chrome-157-desktop-macos/clienthello.hex`, summary beside it | `ffmpeg-8.1-reference/clienthello-dtls-active.hex` (only the `active` variant sends one) |
| DTLS versions offered | 1.3 and 1.2 (`supported_versions`), `client_version` 1.2 | 1.2 only (no `supported_versions`) |
| ClientHello size | 1413 bytes in two datagrams (1200 and 263) | 190 bytes in one datagram (215 with headers) |
| TLS 1.2 suites | 11 listed (ECDHE and RSA families, GCM, CHACHA20, CBC) plus 3 TLS 1.3 suites; ECDHE-ECDSA-AES128-GCM-SHA256 present | 27 listed; ECDHE-ECDSA-AES128-GCM-SHA256 present |
| Groups | X25519MLKEM768, x25519, secp256r1, secp384r1 | x25519, secp256r1, x448, secp384r1, secp521r1 |
| Signature algorithms | ecdsa_secp256r1_sha256 first; 9 listed | ecdsa_secp256r1_sha256 first; 20 listed |
| `use_srtp` | AES128_CM_HMAC_SHA1_80, AEAD_AES_256_GCM, AEAD_AES_128_GCM; MKI length 0 | AES128_CM_HMAC_SHA1_80; MKI length 0 |
| `extended_master_secret`, `renegotiation_info` | both present | both present (plus `encrypt_then_mac`, `session_ticket`) |
| Cookie in first hello | none | none |
| STUN checks | ICE-CONTROLLING, PRIORITY, USERNAME, MESSAGE-INTEGRITY, FINGERPRINT, unknown `0xc057`; USE-CANDIDATE on 5 of 6 | ICE-CONTROLLING, USE-CANDIDATE, PRIORITY, USERNAME, MESSAGE-INTEGRITY, FINGERPRINT |
| Load average (start / end) | 19.3 / 17.8 | default 13.4 / 12.4; `dtls_active` 11.8 / 7.1 |

Our endpoint answered every Chrome check with a valid XOR-MAPPED-ADDRESS,
MESSAGE-INTEGRITY and FINGERPRINT (6 of 6 accepted, no integrity failures).
Chrome reached `iceConnectionState: connected` about 120 ms after the answer was
applied, and its `connectionState` stayed `connecting` because DTLS never
completed, as expected. The `dtlsRole` Chrome reported was `client`.

The FFmpeg default run sent one STUN check, never a ClientHello, and gave up
after its 5 s handshake timeout. The `dtls_active` run sent one ClientHello
and then waited; FFmpeg was still running at the run's 40 s bound and was
stopped by its PID.

## A0.3 TCP (RFC 4571 framing behind a layer-4 relay)

The endpoint advertised only `a=candidate:... tcp ... typ host tcptype passive`.
A small byte relay of our own listened in front of the RFC 4571 endpoint.
Port 443 was free on the M4, but the relay could only bind it on all
interfaces (binding 443 on one address needs privilege), so it did for the 20
seconds or so of each run. It carried no traffic other than ours.

| Run | Port, address advertised | ICE states | Selected pair (Chrome `getStats`) | DTLS over TCP | Load (start / end) |
|---|---|---|---|---|---|
| tcp 8443 | 8443, LAN address | checking, connected | `succeeded`, nominated, writable. Local: tcp, prflx, `active`. Remote: tcp, host, `passive` | ClientHello arrived as two RFC 4571 frames (1200 and 263 bytes); 6 valid STUN checks over TCP; 4 TCP connections to the relay | 17.8 / 15.7 |
| tcp 443 | 443, LAN address | none (stayed `new`) | none; no pairs listed | none; no TCP connection | 15.7 / 13.4 |
| tcp 80, 1023, 1024 (earlier runs, same code) | LAN address | 80 and 1023: none. 1024: checking, connected | 1024: `succeeded`, tcp prflx `active` to tcp host `passive` | 1024: yes. 80 and 1023: no | 80: 24.8 / 23.5. 1023: 21.8 / 21.3. 1024: 21.2 / 19.9 |
| unroutable 443 | 443, `203.0.113.5` | checking (not connected) | none selected; 3+ pairs `waiting`, tcp host `active` to tcp host `passive` | none (unreachable address) | 86.7 / 92.9 |
| unroutable 8443 | 8443, `203.0.113.5` | checking | as above | none | 90.4 / 86.9 |
| unroutable UDP 443 | UDP port 443, `203.0.113.5` | checking | pairs `in-progress`, udp host to udp host | none | 86.9 / 78.1 |
| unroutable UDP, other port | ephemeral, `203.0.113.5` | checking | as above | none | 78.1 / 67.0 |

What this shows and does not show:

- Evidence of a TCP candidate pair carrying a DTLS ClientHello in RFC 4571
  framing exists for port 8443 (selected pair from `getStats`, plus the
  ClientHello bytes arriving through the relay). ICE state alone was not used.
- It does not show TCP 443. On a private address Chrome did not try. On an
  unroutable address it paired but could not connect. A reachable public
  address on 443 is the missing test, so it stays pending.
- UDP was not blocked. The TCP runs advertised no UDP candidate, so Chrome had
  nothing else to pick.
- The selected local candidate was a peer-reflexive TCP `active` candidate.
  Chrome offered six TCP `active` host candidates on port 9 (a placeholder, as
  RFC 6544 uses it) and connected from an ephemeral port.
- Not tested: whether the low-port behaviour also applies to UDP candidates at a
  private address on a low port.

## A0.4 Rate ramp (loopback, provisional)

Setup: two `RTCPeerConnection`s in one Chrome page, the answer edited and
applied to both sides. A 1280x720, 30 fps animated canvas (random colour
blocks redrawn every frame, so the content is deliberately hard to compress),
H.264 preferred, `maxBitrate` 5 Mbit/s. Outbound bitrate (from
`bytesSent + headerBytesSent`), `targetBitrate`, the candidate pair's
`availableOutgoingBitrate` and `qualityLimitationReason` were read every second
for 30 s, with the 1-minute load average read beside each sample. Chrome chose
H.264 (`video/H264`) in every arm.

Feedback left in each arm, from the full answer SDP:

| | Arm (a) | Arm (b) |
|---|---|---|
| Removed | `transport-cc` (every payload), `goog-remb` (every payload), the transport-wide sequence-number extension | `goog-remb` (every payload) |
| Still negotiated | `nack`, `nack pli`, `ccm fir`, `rtcp-rsize`, `rtcp-xr:rcvr-rtt=all`, the `abs-send-time` and `toffset` extensions | the same, plus `transport-cc` and the transport-wide sequence-number extension |
| Still sent by the receiver | RTCP receiver reports (`remote-inbound-rtp` was present, round trip about 1 ms), NACK and PLI when needed | same, plus transport-wide feedback |
| Not present | transport-wide feedback, REMB | REMB |

This is why the result is provisional: the receiver is a browser and still
sends receiver reports, which Chrome's sender can use for loss-based
estimation. A server that sent no RTCP at all would be a harsher case, and a
real network adds delay and loss that loopback lacks. All arms used the same
machine, which was loaded by other users (see load per run).

Summary (30 s each; targets in kbit/s):

| Run | Arm | targetBitrate at 1 s | First second with target at or above 4000 | Target at 30 s | Mean outbound s21 to s30 | Load range | `qualityLimitationReason` |
|---|---|---|---|---|---|---|---|
| Run 0 | a | 0 | 22 | 4995 | 4812 | 10.7 to 16.7 | bandwidth 27.0 s of 30.1 s |
| Run 0 | b | 4167 | 1 | 5000 | 5126 | 11.8 to 14.3 | bandwidth 28.1 s of 30.1 s |
| Run 1 | a | 571 | not in 30 s | 2495 | 1730 | 4.4 to 4.9 | bandwidth 27.3 s of 30.1 s |
| Run 1 | b | 1314 | 2 | 5000 | 5123 | 4.8 to 6.6 | bandwidth 27.3 s of 30.1 s |
| Run 2 | a | 604 | not in 30 s | 2661 | 1840 | 25.4 to 38.7 | bandwidth 27.2 s of 30.1 s |
| Run 2 | b | 1321 | 19 | 5000 | 5126 | 41.7 to 61.8 | bandwidth 28.1 s of 31.0 s |
Run 0 reported a target of 0 at 1 s because the sample was taken before the first
estimate. Run 0 had both arms in one page, so the second arm may carry state
from the first, and it is the only run in which arm (a) reached the cap. Runs 1
and 2 used a fresh Chrome and page per arm and are the primary evidence.

Per-second series (index 1 to 30; `-` means no value yet):

**Run 0 (one page: arm a, then arm b; load 10.7 to 14.4)**

```text
arm a targetBitrate kbit/s : 0 1343 1443 1562 1599 1599 1599 2059 1852 1881 1881 2222 2354 2310 1973 2176 2434 2445 3083 3434 3786 4256 4718 5000 5000 5000 4984 4969 4991 4995
arm a outbound kbit/s      : - 1393 1193 2689 828 848 1308 1710 2688 794 1664 1726 2239 2522 2346 2207 2574 2820 3029 3468 3743 3968 4522 5010 5163 5200 5120 5068 5180 5142
arm a availableOutgoing    : 300 1614 1735 1878 1922 1922 1922 2062 2227 2261 2261 2380 2577 2776 2982 3196 3284 3466 3704 4034 4312 4693 5107 5460 5927 6365 6911 7407 7407 7449
arm b targetBitrate kbit/s : 4167 4167 4167 4167 3543 5000 5000 4167 4167 5000 5000 5000 5000 5000 5000 5000 5000 4167 4167 5000 5000 5000 5000 5000 5000 5000 5000 5000 5000 5000
arm b outbound kbit/s      : - 4269 5646 2927 3686 4137 4387 4539 4013 5081 4949 5147 4935 5313 4951 5153 4945 4332 4484 4777 5211 5486 4614 5152 5349 5051 5121 5154 5431 4691
arm b availableOutgoing    : 5414 5674 6142 6410 6410 6512 6966 7569 7608 7709 7954 8128 8173 8173 8173 8173 8173 8251 8251 8291 8452 8628 8754 8841 8841 8882 8966 9053 9053 9087
```

**Run 1 (fresh page per arm; load 4.4 to 6.6)**

```text
arm a targetBitrate kbit/s : 571 594 629 682 697 697 697 708 758 780 780 780 799 864 919 1008 929 1120 1216 1022 1184 1282 1324 1406 1601 1796 1935 2103 2281 2495
arm a outbound kbit/s      : - 897 785 997 365 482 584 831 1116 321 378 682 923 1021 1002 926 996 1349 1050 1313 1011 1368 1268 1423 1508 2033 1628 2025 2362 2678
arm a availableOutgoing    : 686 716 758 820 837 837 837 852 911 937 937 937 961 1039 1105 1212 1282 1346 1461 1583 1665 1726 1850 1961 2065 2252 2420 2594 2787 3040
arm b targetBitrate kbit/s : 1314 4167 4167 4167 3421 3863 5000 4167 4167 4824 5000 5000 5000 5000 5000 5000 5000 4167 4167 5000 5000 5000 5000 4920 5000 5000 5000 4938 4997 5000
arm b outbound kbit/s      : - 2290 4277 4797 3499 3420 4203 4786 4197 4117 4792 5107 4960 5311 4785 5147 5116 5345 3935 4500 5323 4844 5192 5268 5211 4752 5209 5441 4823 5163
arm b availableOutgoing    : 1701 5835 6060 6591 6718 6718 6718 7136 7635 7635 7674 8007 8210 8210 8261 8278 8305 8305 8385 8385 8385 8663 8781 8860 8860 8933 8978 8978 9063 9112
```

**Run 2 (fresh page per arm; load 25 to 62)**

```text
arm a targetBitrate kbit/s : 604 650 662 716 729 729 729 755 800 823 823 823 879 948 1023 1083 1107 1283 1374 1138 1349 1411 1538 1632 1775 1858 1990 2203 2384 2661
arm a outbound kbit/s      : - 975 700 1050 301 487 746 885 1165 402 349 761 984 1213 1037 1105 1129 1477 1200 1387 1134 1476 1509 1939 1500 1809 2080 2038 2449 2470
arm a availableOutgoing    : 726 782 796 861 876 876 876 908 963 989 989 989 1057 1140 1229 1302 1439 1543 1651 1797 1834 1920 2026 2213 2388 2507 2644 2845 3050 3255
arm b targetBitrate kbit/s : 1321 1406 1546 1658 1667 1696 2192 2017 2113 1308 1709 2621 2506 2167 2440 2679 3070 3632 4198 4581 5000 5000 5000 4959 4986 4997 4986 5000 5000 5000
arm b outbound kbit/s      : - 1349 2049 1329 861 1569 1694 2679 1057 1593 1658 2499 2636 2733 2411 2638 3083 3584 4098 4617 5005 5239 5140 5137 5187 5016 5139 5127 5225 5044
arm b availableOutgoing    : 1689 1796 1978 2118 2128 2167 2372 2578 2697 2783 2809 2993 3201 3508 3711 4045 4335 4639 5038 5391 5872 6287 6880 7367 7884 8243 8367 8415 8415 8418
```

Observations:

- With `transport-cc` (arm b) the target reached the 5 Mbit/s cap in all three
  runs, after 1 s, 2 s and 19 s. The 19 s run had the highest load (42 to 62).
- Without it (arm a) the target climbed in small steps from about 0.6 Mbit/s.
  In runs 1 and 2 it was still at 2.5 to 2.7 Mbit/s after 30 s. In run 0 it
  reached the cap after 22 s. Three runs of one arm do not give a rate, but
  the direction is the same one the design guessed: without transport-wide
  feedback, Chrome's ramp is slow and uneven.
- `qualityLimitationReason` was `bandwidth` for about 27 of 30 s in every arm.
  Chrome scaled the picture down while the estimate was low (1280 to 960 to 640
  to 480 to 320 pixels wide over the ramp), and it had not scaled back up by 30
  s, so the sender's output at the cap is a 320-pixel-wide picture in these
  runs. The blocks-of-noise content makes the encoder spend bits freely, so
  outbound bitrate follows the target closely. A compressible animation was not
  run; it would show lower outbound rates for the same target. `targetBitrate`
  and `availableOutgoingBitrate` do not depend on the content.
- The load average varied from 4 to 62 across runs. `cpu` limitation duration
  was 0 in every arm, so CPU starvation did not limit the encoder. The run with
  the highest load (run 2, arm b) was the slowest `transport-cc` ramp, which
  could be load or chance. It was not repeated.
- Decision input: for desktop Chrome, `transport-cc` made the ramp reach the
  cap sooner in 3 of 3 runs. This is one browser, loopback, and no network
  delay or loss.

## A0.5 Missing H.264

Both captured offers contain H.264. Neither target is missing it.

| Offer | Video codecs offered (excluding rtx, red, ulpfec) | H.264 `profile-level-id` / `packetization-mode` |
|---|---|---|
| Chrome 157 | VP8, H.264, AV1, VP9 (`profile-id` 0 and 2), H.265 | `42001f`/1, `42001f`/0, `42e01f`/1, `42e01f`/0, `4d001f`/1, `4d001f`/0, `64001f`/1. All carry `level-asymmetry-allowed=1`. Payload types 102, 104, 108, 114, 116, 39 and 118 in that order |
| FFmpeg 8.1 (non-target) | H.264 only | `42001f`/1 (payload type 106, with RTX 105) |

H.264 `packetization-mode=1` is offered by Chrome at four profiles (Baseline,
Constrained Baseline, Main, High) and by FFmpeg at one. Every `profile_idc`
that appears (`42`, `4d`, `64`) is in the design's accepted set. The first
mode-1 payload in offer order is `42001f`.

Pending targets for A0.5: Firefox, Safari on macOS and iOS, Chrome on Android,
OBS. Safari and Firefox in particular may offer fewer H.264 profiles, and
Chrome on Android may differ if hardware encoders are absent. None of this was
observed.

## A0.7 Opus parameters (loopback, provisional)

Setup: a stereo WebAudio source with a 440 Hz tone on the left and 880 Hz on
the right, sent by one connection to another in the same page. The answer's
Opus `fmtp` was edited and applied to both sides; the unedited answer's `fmtp`
was `minptime=10;useinbandfec=1`. The receiver read each encoded frame before
decoding and recorded the first byte (the TOC, RFC 6716 section 3.1). Bitrate is
payload bitrate over 12 s from `getStats`, and it matches the bitrate computed
from the received encoded frames in every arm. Load average (start of run) was
21 to 30 during these arms.

| Arm | Answer `fmtp` additions | Payload kbit/s | Packets/s | TOC stereo flag set | TOC mode (RFC 6716 table 2) |
|---|---|---|---|---|---|
| default (tones) | none | 32.4 | 50.5 | 0 of 600 | config 27, CELT SWB 20 ms |
| `stereo=1;maxaveragebitrate=128000;usedtx=0` | all three | 128.4 | 50.5 | 600 of 600 | config 31, CELT FB 20 ms |
| `stereo=1` only | stereo | 64.4 | 50.5 | 600 of 600 | config 27 |
| `maxaveragebitrate=128000` only | bitrate | 128.4 | 50.7 | 0 of 600 | config 31 |
| silence, `usedtx=0` | usedtx | 14.0 | 50.5 | 0 | config 15, hybrid FB 20 ms |
| silence, `usedtx=1` | usedtx | 0.7 | 5.2 | 0 | config 15 (56 frames), plus short frames |

Reading:

- `stereo=1` is honoured: the TOC `s` bit was set on every full frame, and
  without it Chrome sent mono even from a stereo source. This supports the
  design's answer.
- `maxaveragebitrate=128000` is honoured as a bitrate: 128 kbit/s against 32
  by default. Without `stereo=1` it gave mono at 128 kbit/s.
- `usedtx=0` changed nothing, because Chrome's default is no DTX (silence still
  sent 50 packets/s). `usedtx=1` is honoured: packets fell to about 5 per
  second. The `usedtx=0` in the design's answer is harmless but only guards
  against a sender that turns DTX on by default.
- About 6 to 8 frames at the start of each 12 s window had a payload of 3 bytes
  or fewer (some empty). They carry no usable TOC and are left out of the
  stereo counts above (the totals are 600 full frames per arm).
- Not examined: RTP timestamp and sequence-number gaps under DTX, which the
  design relies on to tell silence from loss. Only encoded frame counts were
  recorded.
- `RTCRtpScriptTransform` (the worker API) and the older `createEncodedStreams`
  were both used and gave the same counts for the two arms where both ran
  (default, and the all-three-parameters arm). In an earlier run of this page
  the worker API returned zero frames in every arm with `stereo` or
  `maxaveragebitrate`. That was a fault in our harness, not a Chrome result.
  The likely cause is our worker formatting the first byte of a zero-length
  frame (`undefined`), which throws and kills the pipe. That was not confirmed
  by a separate test; the worker was fixed, given error capture, and those arms
  then ran as shown above.
- `totalSamplesReceived` stayed 0 in every arm because headless Chrome never
  played the audio, so this run has no playback evidence.

## What is provisional, and why

- **A0.4 and A0.7** are loopback with a browser receiver. The receiver still
  sends RTCP receiver reports, there is no network delay or loss, and the host
  was loaded by other users.
- **A0.4 sample size.** Three runs per arm, one browser build, one machine. The
  `transport-cc` direction was consistent; the size of the effect was not.
- **A0.3** has one selected-pair capture, for port 8443 on a private address.
  The 443 premise is unproven.
- **A0.1 and A0.5** are one capture each per target, from headless Chrome with a
  fake camera and microphone, not a user-facing build with a real camera. A
  headless run uses the same WebRTC stack but could offer different codecs when
  hardware encoders are present or absent.

## Pending, per check

| Check | Chrome 157 desktop | FFmpeg 8.1 | Firefox | Safari macOS | Safari iOS | Chrome Android | OBS |
|---|---|---|---|---|---|---|---|
| A0.1 profile | done | done (non-target) | pending | pending | pending | pending | pending |
| A0.3 TCP 443 | 8443 done; 443 on a public address pending | not run | pending | pending | pending | pending | pending |
| A0.4 ramp | loopback done, provisional | not run | pending | pending | pending | pending | not applicable to a browser loopback; needs a real receiver |
| A0.5 H.264 | done | done (non-target) | pending | pending | pending | pending | pending |
| A0.7 Opus | loopback done, provisional | not run | pending | pending | pending | pending | pending |

Also not started, and outside this run: A0.2 (Opus playback), A0.6 (OBS
keyframes on PLI and FIR) and A0.8 (phone rotation). Each needs a device or a
target this run did not have or was told not to touch.
