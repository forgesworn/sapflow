# W0 evidence tool

Dependency-free Node (24) tools that gather the W0 evidence for
[WHIP ingest](../../docs/whip-ingest.md). Nothing here is part of Sapflow. It
was written from the RFCs and from observed traffic only: RFC 8489 and 5769
(STUN), 8445 (ICE), 6544 and 4571 (ICE-TCP framing), 7983 (demultiplexing),
6347 (DTLS 1.2), 5246 and 8446 (ClientHello), 9725 (WHIP), 8866 (SDP).

| File | Purpose |
|---|---|
| `stun.js` | STUN codec: MESSAGE-INTEGRITY (HMAC-SHA1), FINGERPRINT (CRC-32), XOR-MAPPED-ADDRESS |
| `stun-rfc5769.test.js` | The RFC 5769 vectors. Run this first |
| `sdp.js` | Offer reader and ICE-lite answer builder |
| `fake-whip.js` | The fake endpoint: HTTP `POST` of `application/sdp`, UDP and RFC 4571 TCP, saves the offer and the ClientHello fragments, never completes DTLS |
| `l4-relay.js` | Byte relay that stands in for the TCP 443 front proxy |
| `clienthello.js` | ClientHello summariser (reassembles fragments) |
| `offer-summary.js` | Offer summariser for A0.1 and A0.5 |
| `run.js` | Runs one mode and cleans up its own child processes and temp profile |
| `pages/capture.html` | Page that publishes fake media to the endpoint and samples `getStats` |
| `pages/loopback.html` | A0.4 and A0.7 loopback tests (`--which ramp`, `ramp-a`, `ramp-b`, `opus`) |

Needs `openssl` on the path (a throwaway P-256 certificate is made per run) and
Google Chrome. Set `CHROME` to use another binary. Run from the repository root.

```sh
# 1. STUN vectors
node scripts/whip-w0/stun-rfc5769.test.js

# 2. Chrome offer and ClientHello over UDP (A0.1)
node scripts/whip-w0/run.js capture udp --host <lan-ip> --out <dir>

# 3. TCP only (A0.3): advertises one passive TCP candidate, behind the relay
node scripts/whip-w0/run.js capture tcp --host <lan-ip> --port 8443 --out <dir>

# 4. Loopback rate ramp (A0.4) and Opus parameters (A0.7), about 3 minutes
node scripts/whip-w0/run.js loopback --out <dir> [--which ramp|opus]

# 5. FFmpeg reference client (not a target)
node scripts/whip-w0/run.js ffmpeg udp --host <lan-ip> --out <dir>
node scripts/whip-w0/run.js ffmpeg udp --dtls-active --host <lan-ip> --out <dir>

# Summaries
node scripts/whip-w0/clienthello.js <dir>/chrome-udp-clienthello.hex
node scripts/whip-w0/offer-summary.js <dir>/chrome-udp-offer.sdp
```

`--port` is the relay's listening port in TCP mode. Ports below 1024 bind on
all interfaces for the length of the run. See the evidence document for why
`8443` was used. Raw output holds addresses: scrub it before keeping any of it.
A `-ClientHello` `.hex` file holds one datagram per block, blocks separated by
a `--` line.
