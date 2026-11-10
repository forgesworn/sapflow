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
| `ports.js` | Binds inside a port range (`--range 47000-47199`), so a run stays out of other runs' ports |
| `browser.js` | Launchers: Firefox (headless, temporary profile, `user.js`), Safari through `safaridriver`, Chrome for playback |
| `obs-run.js` | OBS WHIP capture: backs up OBS's config, makes the `W0-WHIP-test` profile and scene collection, launches OBS, ends it by PID, restores the config and diffs it |
| `play-run.js`, `play-server.js` | A0.2 playback matrix and its static server (Range support) |
| `make-opus-assets.sh` | The A0.2 asset recipe: Chrome's `MediaRecorder` Opus muxed with `ffmpeg` into fMP4 HLS (audio and video, audio only) and a fragmented MP4 |
| `pages/play.html`, `pages/record-opus.html` | The playback page (analyser, decoded-byte count, click detector) and the Opus recorder |

Needs `openssl` on the path (a throwaway P-256 certificate is made per run) and
Google Chrome. Set `CHROME` or `FIREFOX` to use another binary. Run from the repository root.

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

# Other targets: --browser firefox|safari on capture and loopback (Safari uses synthetic media
# and needs `sudo safaridriver --enable` once); --range keeps every listener in one port range
node scripts/whip-w0/run.js capture udp --browser firefox --range 47000-47199 --host <addr> --out <dir>
node scripts/whip-w0/run.js obs udp|tcp --host <addr> --range 47000-47199 [--port N] --out <dir>

# A0.2 Opus playback: build the assets, then play them (hls.js fetched into scratch, never the repo)
scripts/whip-w0/make-opus-assets.sh ~/whip-w0-assets
node scripts/whip-w0/run.js play --vendor <dir with hls.min.js> --out <dir> --range 47000-47199 --browsers chrome,firefox

# Summaries
node scripts/whip-w0/clienthello.js <dir>/chrome-udp-clienthello.hex
node scripts/whip-w0/offer-summary.js <dir>/chrome-udp-offer.sdp
```

`--port` is the relay's listening port in TCP mode. Ports below 1024 bind on
all interfaces for the length of the run. See the evidence document for why
`8443` was used. Raw output holds addresses: scrub it before keeping any of it.
A `-ClientHello` `.hex` file holds one datagram per block, blocks separated by
a `--` line.
