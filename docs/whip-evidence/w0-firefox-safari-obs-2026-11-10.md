# W0 evidence: Firefox, Safari (macOS), OBS and Opus playback

Status: evidence only, 10 November 2026. Nothing in Sapflow was built or
changed. This note extends
[the desktop Chrome run](w0-chrome-desktop-2026-11-10.md) and reports
observations against [the WHIP ingest design](../whip-ingest.md). It does not
edit the design. Two of the three new targets could not be run to the end, for
reasons that are host problems, not findings about the targets (see
"Blocked runs").

## Summary

| Target | A0.1 offer | A0.1 ClientHello | A0.5 H.264 | A0.3 TCP | A0.4 ramp | A0.7 Opus |
|---|---|---|---|---|---|---|
| Firefox 160.0 | captured | not captured (ICE could not complete on this host) | offered | no connection (host-blocked) | not run (host-blocked) | not run (host-blocked) |
| Safari 26.6.2 (macOS) | not run (automation session could not start) | not run | not run | not run | not run | not run |
| OBS 32.0.4 | captured | captured, two runs | offered | no TCP attempt at all (two runs) | not applicable | not applicable |

A0.2 (Opus playback on desktop) ran on Chrome and Firefox with hls.js and, for
the recording, with the native element. Safari did not run. A0.6 (OBS
keyframes) is not run here: it needs a receiver that completes DTLS, which
does not exist until W3. It moves to W3.

## Scope and setup

| Item | Value |
|---|---|
| Host | Apple M4, macOS 26.6.2, shared. Node v24.14.0, ffmpeg and ffprobe 8.1.2. Load averages (1 minute) are given with each run |
| Chrome (A0.2 and the TCP control) | Google Chrome 157, the installed app, headless, temporary profile, same install as the earlier run |
| Firefox | 160.0, release build downloaded from Mozilla, installed unprivileged at `~/Applications/Firefox.app`. `codesign --verify --deep --strict` passed, `spctl -a -vv` reported "accepted, Notarized Developer ID", authority "Developer ID Application: Mozilla Corporation". Run `-headless -no-remote -profile <temporary directory>` with `media.navigator.streams.fake` and `media.navigator.permission.disabled` set to true in `user.js` (plus autoplay and first-run prefs; the full list is in `scripts/whip-w0/browser.js`) |
| Safari | `/usr/bin/safaridriver`, "Included with Safari 26.6.2". No session could be created |
| OBS | 32.0.4 at `/Applications/OBS.app`, not running at the start. Dedicated profile and scene collection `W0-WHIP-test`, WHIP custom service, bearer token, one colour source, 1280x720 at 30 fps, x264 |
| hls.js | 1.7.4, from `npm pack hls.js@1.7.4` into scratch. Not in the repository |
| Endpoint | The same fake ICE-lite WHIP endpoint as before (`scripts/whip-w0/`), extended with a port range, a record of the `Authorization` scheme and PATCH handling. It never completes DTLS |
| Ports | 47000 to 47199 only, for every listener we opened. Browsers' own outgoing ICE sockets use ephemeral ports, which we cannot place |
| Clean room | Written from RFCs and wire observation. No WebRTC, ICE, STUN, DTLS or SRTP implementation source was read or installed. hls.js is a media player, not one of those |

Before any capture, `stun-rfc5769.test.js` passed (6 checks).

Commands (run on the M4 from the repository root; `<addr>` is the address the
browser can reach):

```sh
node scripts/whip-w0/run.js capture udp --browser firefox --range 47000-47199 --host <addr> --out <dir>
node scripts/whip-w0/run.js capture tcp --browser firefox --range 47000-47199 --port 47100 --host <addr> --out <dir>
node scripts/whip-w0/run.js capture tcp --browser chrome  --range 47000-47199 --port 47120 --host <lan-addr> --out <dir>   # control
node scripts/whip-w0/run.js obs udp --host <lan-addr> --range 47000-47199 --out <dir>
node scripts/whip-w0/run.js obs tcp --host <lan-addr> --range 47000-47199 --port 47121 --out <dir>
node scripts/whip-w0/run.js loopback --which ramp-b --browser firefox --range 47000-47199 --query "&ramp=6" --out <dir>
scripts/whip-w0/make-opus-assets.sh ~/whip-w0-assets
node scripts/whip-w0/run.js play --vendor <hls.js dir> --out <dir> --range 47000-47199 --browsers chrome,firefox
```

Fixtures, scrubbed to documentation values: `fixtures/whip/w0/firefox-160-desktop-macos/`,
`fixtures/whip/w0/obs-32.0.4-macos/` and `fixtures/whip/w0/opus-playback/`.

## Findings that contradict or add to the design

Numbered for the report. "Contradicts" means the design's text, as written,
would refuse or mishandle what was observed.

1. **OBS puts `a=setup`, `a=ice-ufrag`, `a=ice-pwd`, `a=ice-options` and
   `a=fingerprint` at session level, not in each media section. Firefox puts
   `a=fingerprint` at session level too.** The design's "an offer is accepted
   only if it has `a=ice-ufrag`, `a=ice-pwd` and a `sha-256` fingerprint" must
   read those as inheritable from the session level (RFC 8866 allows it). A
   parser that looks only inside each `m=` section rejects OBS, and would miss
   Firefox's fingerprint. Chrome does not show this.
2. **OBS's offer carries `a=group:LS 0 1` (lip sync) as well as
   `a=group:BUNDLE 0 1`.** "One BUNDLE group" must mean exactly one BUNDLE
   group and must ignore other `a=group` semantics.
3. **OBS offers no candidates at all, and sends no PATCH.** The offer has
   `a=ice-options:ice2,trickle` and no `a=candidate` line. Our endpoint never
   received a PATCH either. This agrees with the design (ICE-lite learns the
   address from the checks), and it means the design must never require
   candidates in the offer.
4. **OBS does not use ICE-TCP.** A TCP-only answer (one passive TCP candidate)
   produced no STUN request and no TCP connection in two runs of 40 s. OBS
   ended the run with its WebRTC state `Failed`. A0.3 as written ("each browser
   target and OBS reaches a passive TCP candidate through a layer-4 proxy on
   443") cannot pass for OBS. This agrees with the design's own remark that
   RTMPS stays the route for networks that only allow TCP 443. It should be
   said for OBS explicitly.
5. **OBS's DTLS ClientHello is a single 342-byte datagram, DTLS 1.2 only,
   with record and `client_version` 0xfefd and no `supported_versions` and no
   `key_share`.** It lists 104 cipher suites, including
   `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`. It does not match the design's
   Chrome-shaped profile in these ways:
   - `ecdsa_secp256r1_sha256` is fifth of six signature algorithms, not first
     (`ecdsa_secp521r1_sha512` is first);
   - there is no `renegotiation_info` extension, only the
     `TLS_EMPTY_RENEGOTIATION_INFO_SCSV` suite value (0x00ff), so a server must
     not require the extension;
   - it sends `encrypt_then_mac`, an empty `session_ticket`, and
     `ec_point_formats` (uncompressed), none of which a GCM-only server should
     echo;
   - `use_srtp` lists only `SRTP_AES128_CM_HMAC_SHA1_80`, so a server that
     supports only AEAD suites fails with OBS. The design's "otherwise
     `SRTP_AES128_CM_HMAC_SHA1_80`" is needed, not optional;
   - groups: `x25519`, `secp256r1`, `secp384r1`, `x448`, `secp521r1` and three
     codes the summariser does not name (0x001a to 0x001c, Brainpool).
   The design's statement that the single-datagram case "is not assumed" holds,
   but a second shape (one datagram, no 1.3 offer) is now seen.
6. **OBS's offer is narrower than the design's answer builder assumes.**
   - Video: one payload, H.264 `42e01f` mode 1, with `nack`, `nack pli` and
     `goog-remb`. There is **no `ccm fir`**, **no RTX**, no `transport-cc`, and
     no header extension of any kind (no `sdes:mid`, no `transport-wide-cc`).
   - Audio: Opus with `minptime=10;maxaveragebitrate=96000;stereo=1;sprop-stereo=1;useinbandfec=1`.
   - Consequences: the design's answer repeats `ccm fir` and `transport-cc`
     only if offered, so nothing breaks. But with OBS, `transport-cc` can never
     be negotiated, and OBS's own congestion signal is REMB. The design says
     REMB is not planned unless a target needs it and cannot use `transport-cc`;
     OBS is that target. Matching is by SSRC (the offer lists `a=ssrc` lines,
     two per section); the MID extension is never sent, so the "by MID"
     fallback does not apply to OBS. A0.6 for OBS cannot ask for a keyframe
     with FIR; PLI is the only request OBS accepts to receive.
7. **OBS is ICE-controlling with aggressive nomination and adds a STUN
   `SOFTWARE` attribute.** All three checks carried `ICE-CONTROLLING`,
   `PRIORITY`, `USERNAME`, `SOFTWARE`, `MESSAGE-INTEGRITY` and `FINGERPRINT`;
   two of three also carried `USE-CANDIDATE`. No `0xC057`. `SOFTWARE` is
   comprehension-optional (0x8022) and known to RFC 8489, so it needs no
   special handling.
8. **Firefox 160's offer.** `a=sendrecv` at session level with `sendonly` in
   each section; `setup:actpass`, `ice-options:trickle`, no `ice-lite`. Header
   extensions in the audio section include `csrc-audio-level` with a direction
   suffix (`a=extmap:2/recvonly`), so the extension parser must accept the
   `id/direction` form. Video offers VP8, VP9, H.264 (twice), AV1, `ulpfec`
   and `red`; all video payloads carry `nack`, `nack pli`, `ccm fir`,
   `goog-remb` and `transport-cc`. Firefox offers video orientation
   (`urn:3gpp:video-orientation`), which the design declines.
9. **Firefox's first H.264 payload in offer order is `42e01f` (payload 126,
   Constrained Baseline), then `42001f` (105).** Both are mode 1, both have an
   RTX payload. The design's rule picks 126. Firefox offers no mode-0 H.264.
10. **Opus playback: no desktop player or asset failed to decode Opus in fMP4
    HLS or in a fragmented MP4 where it ran** (Chrome 157 with hls.js and
    native, Firefox 160 with hls.js and, for the MP4, natively). See A0.2. The
    design's pre-skip of 312 plays without a click in Chrome, **but in
    Firefox 160 with hls.js the pre-skip-312 asset produced two clicks within
    the first 1.7 s of playback in 4 of 4 runs, and the pre-skip-0 asset never
    did (4 of 4 runs).** The cause is not known. This bears directly on the
    design's "pre-skip 312 ... the value changes if it does not [play
    cleanly]".
11. **FFmpeg's `-c:a copy` from Chrome's `MediaRecorder` WebM writes dOps
    pre-skip 0, channels 2, input rate 48000, gain 0, mapping family 0.**
    Chrome's recorder wrote an `OpusHead` pre-skip of 0, so the spec'd assets
    (a), (b) and (c) test pre-skip 0. The design's rule value, 312, was tested
    by an extra asset (d).
12. **Firefox and OBS both start from `a=setup:actpass`.** The design's answer
    `passive` fits both. Nothing new beyond the FFmpeg `passive` finding
    already recorded.
13. **A host detail that affects every browser run on this machine.** See
    "Blocked runs". It is not a design finding.

## Blocked runs

### Firefox: ICE could not complete on this host

Firefox sent no STUN check to our endpoint in any run. Symptoms: `iceConnectionState` stayed `checking`; the one candidate pair was `in-progress`
with no responses; the loopback ramp (two connections in one page) sent zero
bits for 6 s.

Cause found, by observation: the M4's default route is a VPN tunnel interface.
Firefox gathers host candidates only on that interface (one address, in
the private 10.x range). On this machine, from a Node socket bound to that
address, a UDP datagram to `127.0.0.1` arrived, but datagrams to the tunnel
address itself, to the Wi-Fi address and to two other local addresses did not.
A TCP connection from that address to the tunnel address and to the Wi-Fi
address also never completed. Chrome gathered the Wi-Fi address and connected
(control run in the A0.3 table), so the host is fine for a browser that uses
that interface.

Attempts (more than the two the brief allows, so this is the stop point):

| Attempt | Prefs added | Result |
|---|---|---|
| Wi-Fi address advertised, default prefs | none | candidates were mDNS names for the tunnel address; 0 checks |
| Loopback address advertised | `ice.loopback` | no pairs formed; ICE `failed` |
| Tunnel address advertised | none; then `obfuscate_host_addresses` false | 0 checks |
| In-page loopback | `ice.loopback`, `obfuscate_host_addresses` false, `default_address_only` false, `link_local` true | still one local address; 0 bits |
| Wi-Fi address advertised | `ice.force_interface` `en0`, obfuscation off | the gathered address stayed the tunnel address; 0 checks |
| Loopback advertised | `ice.force_interface` `lo0`, `ice.loopback`, obfuscation off | the gathered address stayed the tunnel address; 0 checks |

Nothing was changed on the machine's network, and no VPN was touched. A run
with the tunnel down, or on another host, is needed for Firefox A0.1
ClientHello, A0.3, A0.4 and A0.7.

### Safari: no automation session

Two attempts with `safaridriver` (one with `--diagnose`) returned, after 30 s:

> session not created: Could not create a session: The session timed out while
> connecting to a Safari instance. The following asynchronous operation timed
> out: Finding or launching a compatible local Safari and waiting for its
> RWIApplication to appear and become ready.

No Safari process was running before or after, no automation window opened, and
the `AllowRemoteAutomation` preference does not exist for Safari on this
account. The driver's own log shows only the request and the 500. That matches
automation not being enabled; it could also be a launch from a non-GUI session.
I cannot tell the two apart. Enabling it needs `sudo safaridriver --enable`,
which is the owner's job and was not attempted. No session was ever created,
so there was nothing to delete. Every Safari cell in this note is pending.

### Safari: second session, after the owner enabled remote automation

Two further attempts, again with no session:

1. `safaridriver` started over ssh: the same 500, "The session timed out while
   connecting to a Safari instance ... waiting for its RWIApplication to appear
   and become ready", after 30 s. No Safari process was running.
2. The driver started in the owner's GUI session with `launchctl asuser`
   (`W0_SAFARI_ASUSER=1`): the driver never answered (`fetch failed`).
   `launchctl asuser <uid> /usr/bin/true` itself fails over ssh with "Could not
   switch to audit session ... Operation not permitted", so this route needs
   root or a process already in the GUI session.

The `AllowRemoteAutomation` key is still absent from Safari's preferences
domain for this account, which is consistent with automation not being
effective for the ssh login, though the owner reports enabling it. I did not
try anything else. A session needs either a driver started from a process inside
the owner's GUI session (for example Terminal there, running
`node scripts/whip-w0/run.js ... --browser safari`) or a check of what
`sudo safaridriver --enable` changed. No session was created, so none needed
ending; no Safari process was started by me. All Safari cells stay pending.

## A0.1 Captured profile

| Item | Firefox 160.0 | OBS 32.0.4 |
|---|---|---|
| Offer file | `firefox-160-desktop-macos/offer.sdp` (summary beside it) | `obs-32.0.4-macos/offer.sdp`, `offer-tcp-run.sdp` |
| DTLS setup in offer | `actpass`, per section | `actpass`, session level |
| Fingerprint | `sha-256`, session level | `sha-256`, session level |
| ICE credentials | per section | session level |
| ICE options | `trickle` | `ice2,trickle` |
| Candidates in the offer | 2 UDP host and 2 TCP host active per section (mDNS names under default prefs) | none; no PATCH sent |
| Sections | audio mid 0, video mid 1, BUNDLE `0 1`, `rtcp-mux` | audio mid 0, video mid 1, BUNDLE `0 1` and `group:LS 0 1`, `rtcp-mux` |
| Direction | session `sendrecv`, sections `sendonly` | `sendonly` |
| Header extensions (video) | `sdes:mid`, `abs-send-time`, `toffset`, `playout-delay`, transport-wide-cc, `video-orientation`, AV1 dependency descriptor | none |
| Feedback (H.264) | `nack`, `nack pli`, `ccm fir`, `goog-remb`, `transport-cc` | `nack`, `nack pli`, `goog-remb` |
| RTX for H.264 | yes | no |
| Audio | Opus 109 `maxplaybackrate=48000;stereo=1;useinbandfec=1`, G722, PCMU, PCMA, telephone-event; `transport-cc` | Opus 111 `minptime=10;maxaveragebitrate=96000;stereo=1;sprop-stereo=1;useinbandfec=1` only |
| WHIP HTTP | `POST`, `Content-Type: application/sdp`, browser `Origin` | `POST`, `Authorization: Bearer ...` (scheme seen, value not kept), `Content-Type: application/sdp`, no `Origin`; `DELETE` attempted at stop; no `PATCH` |
| ClientHello | not captured | `obs-32.0.4-macos/clienthello.hex`, summary beside it |
| DTLS versions offered | unknown | 1.2 only |
| ClientHello size | unknown | 317 bytes of handshake in one 342-byte datagram; the datagram was sent 4 times (retransmits) |
| TLS suites | unknown | 104 listed; ECDHE-ECDSA-AES128-GCM-SHA256 present; `renegotiation_info` SCSV, no extension |
| Groups | unknown | x25519, secp256r1, secp384r1, x448, secp521r1, plus three Brainpool codes |
| Signature algorithms | unknown | `ecdsa_secp521r1_sha512` first; `ecdsa_secp256r1_sha256` fifth of six |
| `use_srtp` | unknown | `AES128_CM_HMAC_SHA1_80` only; MKI length 0 |
| Other extensions | unknown | `extended_master_secret`, `encrypt_then_mac`, `session_ticket`, `ec_point_formats` |
| STUN checks | none arrived | 3 per run, all valid; `ICE-CONTROLLING`, `PRIORITY`, `USERNAME`, `SOFTWARE`, `MESSAGE-INTEGRITY`, `FINGERPRINT`; `USE-CANDIDATE` on 2 of 3 |
| Load average (start / end) | 10.0 / 11.5 | run 1 9.8 / 10.1; run 2 14.2 / 16.2 |

OBS's second UDP run reproduced the first: 342 bytes, 4 datagrams, 3 valid
checks, 2 with `USE-CANDIDATE`. The two hex files differ first inside the
client random, as expected.

OBS runs: the process was ended by PID. SIGTERM once stops the stream and OBS
stays open if the stream is live, so the UDP runs needed a second SIGTERM. On
every run OBS then exited with SIGABRT, and macOS wrote an OBS crash report in
`~/Library/Logs/DiagnosticReports` for each (four reports, 22:25 to 22:44; they
were left in place). The OBS log shows a normal "Shutting down"
before the abort. I did not investigate it.

OBS config handling, for every OBS run: the whole `obs-studio` config
directory was copied to scratch first. After OBS ended, the modified directory
was moved to `~/.Trash/` and the backup was copied back with `rsync -a`. The
result was compared with `diff -r` (exit 0, empty) and an `rsync -an
--checksum --delete` dry run (empty). The proofs for three runs are
in `fixtures/whip/w0/obs-32.0.4-macos/run-udp2.json`, `run-tcp1.json` and
`run-tcp2.json`; the first UDP run was restored by hand with the same steps and
printed `diff exit=0`. The backup directories were then moved to `~/.Trash/`.

## A0.5 Missing H.264

| Target | Video codecs offered (excluding rtx, red, ulpfec) | H.264 `profile-level-id` / `packetization-mode` |
|---|---|---|
| Firefox 160.0 | VP8, VP9, H.264, AV1 | `42e01f`/1 (payload 126), `42001f`/1 (105), each with RTX. No mode 0 |
| OBS 32.0.4 | H.264 | `42e01f`/1 (payload 96), `level-asymmetry-allowed=1`, no RTX |
| Safari macOS | not run | not run |

Both new targets offer H.264 with `packetization-mode=1` in an accepted
profile. For Firefox this is the offer, not evidence that its encoder sends
H.264: the headless profile has no downloaded OpenH264 module, and no media
flowed. That part is provisional.

## A0.3 TCP

| Run | Advertised | ICE states and pairs | Evidence | Load (start / end) |
|---|---|---|---|---|
| Chrome control, port 47120 | LAN address, one passive TCP candidate, relay in front of the RFC 4571 endpoint | `connected`; selected pair succeeded, nominated, local tcp prflx `active` to remote tcp host `passive` | 6 valid STUN checks and the ClientHello (1200 and 263 byte frames, same two fragments as UDP) arrived over RFC 4571 framing; 4 TCP connections to the relay | 16.4 / 15.1 |
| Firefox, port 47100 | tunnel address (the only one it gathers) | `checking`; one pair, tcp host `active` to tcp host `passive`, `in-progress` | no TCP connection reached the relay; 0 checks. Host-blocked (above) | 15.7 / 18.9 |
| OBS run 1, port 47110 | LAN address, passive TCP | WebRTC `Connecting` then `Failed` after about 40 s | 0 STUN checks, 0 TCP connections | 280.4 / 394.9 |
| OBS run 2, port 47121 | LAN address, passive TCP | `Connecting` then `Failed` again | 0 STUN checks, 0 TCP connections | 15.1 / 27.0 |
| Safari | not run | | | |

The Chrome control re-proves the TCP path after the tool's port-range change,
so the OBS result is not a fault in the relay. OBS run 1 was at an extreme
load (280); run 2 repeated it at 15 with the same result. Only the Chrome
control and OBS runs bear on A0.3. Port 443 on a public address is still
untested for every target.

## A0.4 Rate ramp and A0.7 Opus parameters

Not run for Firefox (blocked: no ICE completes in-page on this host) or Safari
(no session). The page changes for them are in place and tested only as far as
Firefox's failed attempt: `--browser firefox|safari` on `loopback`, reading
Opus frames with `RTCRtpScriptTransform` (`?xform=script`, because neither has
`createEncodedStreams`), a click gate for Safari's suspended audio context,
`null` instead of `0` for an absent `targetBitrate`, `not exposed` for an absent
`qualityLimitationReason`, and the transport-wide extension strip fixed to
match `a=extmap:N/direction` as well as `a=extmap:N`. The loopback series for
Chrome in the earlier note used the older version of the strip; it matched
Chrome's `a=extmap:N` lines, so those results stand.

| Check | Firefox | Safari |
|---|---|---|
| A0.4 ramp, both arms, three runs each | not obtained | not obtained |
| A0.7 stereo, bitrate, DTX | not obtained | not obtained |

## A0.2 Opus playback on desktop

Real FFmpeg HLS is not LL-HLS. This test answers the codec question only: does
Opus in fMP4, with the `dOps` box, decode and play. It says nothing about
part delivery, blocking reload or latency.

### Assets

Built by `scripts/whip-w0/make-opus-assets.sh` into `~/whip-w0-assets/` on the
M4 (not committed). Source: 20 s from headless Chrome `MediaRecorder`
(`audio/webm;codecs=opus`, 128 kbit/s, 319 245 bytes), a stereo WebAudio source,
440 Hz left and 880 Hz right at 0.3 gain. Video: `testsrc2`, 1280x720, 30 fps,
libx264 Main level 3.1, keyframe every 60 frames (2 s), no B-frames. Audio is
muxed with `-c:a copy`.

| Asset | Path under `~/whip-w0-assets/` | Packaging |
|---|---|---|
| (a) audio and video | `av/master.m3u8` | fMP4 HLS, 2 s segments, 10 segments, `init.mp4`, master `CODECS="avc1.4d401f,opus"` |
| (b) audio only | `audio/master.m3u8` | fMP4 HLS, 2 s segments, master `CODECS="opus"` |
| (c) recording | `recording/recording.mp4` | fragmented MP4 (`frag_keyframe+empty_moov+default_base_moof`) |
| (d) extra | `av-preskip312/master.m3u8` | (a) with only the `dOps` pre-skip patched to 312 |

What ffmpeg wrote (`fixtures/whip/w0/opus-playback/assets-info.txt`): codec tag
`Opus`, 2 channels, stereo layout, 48000 Hz. The `dOps` box bytes for (a) and
(b) are `00 02 0000 0000bb80 0000 00`: version 0, channels 2, **pre-skip 0**,
input rate 48000, gain 0, mapping family 0. ffprobe reports `initial_padding=0`.
For (d), pre-skip 312 (`0138`).

### Players, versions and method

| Player | Version |
|---|---|
| Chrome, hls.js | Chrome 157, hls.js 1.7.4, default config |
| Chrome, native | Chrome 157. Chrome 157 plays HLS natively (`canPlayType` "maybe"); not a design target, run for information |
| Firefox, hls.js | Firefox 160.0, hls.js 1.7.4, default config |
| Firefox, native | Firefox 160.0, `canPlayType` for HLS is empty |
| Safari, native and hls.js | not run |

Served by our own server (`play-server.js`, with Range support). Each case is a
fresh headless browser and a fresh profile, playing for 14 s. Audio decoding is
checked two ways, and each row says which:

- `webkitAudioDecodedByteCount` on the element, where it exists (Chrome);
- a Web Audio tap on the element (`createMediaElementSource` and a
  `ScriptProcessorNode` of 4096 samples), which reads the decoded samples for
  both channels: level, tone frequency by zero crossings, and the largest
  sample-to-sample step. A 0.3-amplitude 880 Hz tone cannot step by more than
  0.035 at 48 kHz; anything much larger is a click or dropout. Firefox has no
  decoded-byte property, so the tap is its evidence.

### Results (14 s of playback each, one run unless stated)

`t end` is `currentTime` at 14 s of wall time. Hz are the left and right tone.
`step start` is the largest step in the first blocks after audio begins
(expected maximum 0.035). Load is the 1-minute average at start and end.

| Player | Asset | t end | Advanced 10 s or more | Media error | Decode evidence | Hz L / R | Step at start | Later clicks | Load |
|---|---|---|---|---|---|---|---|---|---|
| Chrome hls.js | (a) av | 14.14 | yes | none | 231 119 bytes; tap | 445 / 879 | 0.035 | none | 33.2 / 26.7 |
| Chrome hls.js | (b) audio | 14.07 | yes | none | 229 201 bytes; tap | 445 / 879 | 0.035 | none | 24.7 / 20.0 |
| Chrome hls.js | (d) pre-skip 312 | 14.13 | yes | none | 230 160 bytes; tap | 445 / 879 | 0.035 | none | 20.0 / 16.0 |
| Chrome native element | (c) recording | 14.10 | yes | none | 230 160 bytes; tap | 445 / 879 | 0.035 | none | 20.8 / 33.8 |
| Chrome native HLS | (a) av | 14.05 | yes | none | 229 201 bytes; tap | 445 / 879 | 0.035 | none; two `stalled` events | 33.8 / 36.3 |
| Chrome native HLS | (b) audio | 14.07 | yes | none | 229 201 bytes; tap | 445 / 879 | 0.035 | none; two `stalled` events | 36.2 / 35.3 |
| Firefox hls.js | (a) av | 14.05 | yes | none | tap only | 445 / 879 | 0.035 | none; 3 of 421 frames dropped | 35.0 / 31.4 |
| Firefox hls.js | (b) audio | 13.97 | yes | none | tap only | 445 / 879 | 0.035 | none | 31.4 / 40.1 |
| Firefox hls.js | (d) pre-skip 312 | 13.98 | yes | none | tap only | 445 / 879 | 0.035 | **two clicks, 1188 and 1615 ms after play, steps 0.23 and 0.30** | 40.1 / 49.0 |
| Firefox native element | (c) recording | 13.98 | yes | none | tap only | 445 / 879 | 0.035 | none | 48.3 / 46.6 |
| Firefox native HLS | (a) av | 0 | no | code 4, "Failed to init decoder" | none | none | none | none | 46.6 / 44.5 |
| Firefox native HLS | (b) audio | 0 | no | code 4, "Failed to init decoder" | none | none | none | none | 41.9 / 34.9 |
| Safari native, Safari hls.js | all | not run | | | | | | | |

Reading:

- Opus in fMP4 HLS, audio only and with H.264, and in a fragmented MP4, decoded
  with both channels correct (440 Hz left, 880 Hz right) in Chrome and Firefox
  with hls.js, and for the recording in both natively. hls.js reported no error
  and `MediaSource.isTypeSupported('audio/mp4; codecs="opus"')` was true in
  both. The master playlist's `avc1.4d401f,opus` was read by hls.js as
  `avc1,opus`.
- Firefox's native HLS failure is expected (it has no native HLS) and is shown
  only for completeness. The native HLS rows for Chrome are for information.
- **Start.** In every case that played, the first audible block had a largest
  step of 0.035, the tone's own maximum, so there was no click at the start. The
  first audible block of some rows is partial (lower level), which is the cut
  point of a block, not the decoder.
- **The Firefox pre-skip-312 clicks.** Asset (d) differs from (a) only by the
  `dOps` pre-skip field. In Firefox with hls.js it produced two clicks at
  1188 ms and 1615 ms after play in the final run, and a steady-state step of
  0.2986 in three earlier runs of an earlier version of the page (the clicks'
  times were not recorded then), so 4 of 4 runs. Assets (a) and (b) never
  clicked in the same four runs. The clicks are not on a 2 s segment boundary
  and not at the start. I do not know why. It is a signal that the 312 value
  is not neutral in Firefox, not a diagnosis. Chrome showed no click with
  either value.
- Whether pre-skip 0 is also audibly clean cannot be judged from a tap alone;
  the tap shows no discontinuity.

## What is provisional

- **A0.2** is one run per case (four for Firefox with (d)), 14 s, headless
  browsers with autoplay allowed, an analyser tap that is not a listening test,
  and a busy machine (load 16 to 49 during the final matrix). It is also FFmpeg
  HLS, not LL-HLS. The design asks for a captured browser Opus stream, "with
  the `dOps` values above"; this used Chrome's recorder, whose pre-skip is 0,
  and patched 312 into an extra asset.
- **A0.1 for Firefox** is the offer only, from a headless profile with fake
  devices, and has no ClientHello. The mDNS candidate names depend on prefs.
- **A0.5 for Firefox** is "offered", not "sent".
- **A0.3** has the Chrome control and two OBS runs. None is on 443, and none
  uses a public address.
- **OBS A0.1** is two runs on one configuration (x264, 1280x720 at 30 fps,
  colour source). Another encoder setting could change the offer's video
  `fmtp`.

## What is pending

| Check | Firefox | Safari macOS | OBS |
|---|---|---|---|
| A0.1 ClientHello | needs ICE: tunnel down or another host | needs `sudo safaridriver --enable` (owner) | done |
| A0.1 offer | done | needs automation | done |
| A0.3 | needs ICE | needs automation | no TCP attempt; nothing pending except a 443 run if wanted |
| A0.4 ramp | needs ICE | needs automation | not applicable |
| A0.5 | offer only | needs automation | done (offer) |
| A0.7 Opus | needs ICE | needs automation | not run (no browser page); the offer states `stereo=1` and a 96 kbit/s maximum |
| A0.2 | done (hls.js and MP4); the pre-skip click needs a second look | needs automation | not applicable |
| A0.6 keyframes | not applicable | not applicable | moved to W3 (needs a receiver that completes DTLS) |

Also not started here: A0.2 on a physical iPhone and a physical Android phone,
A0.8 rotation, and Safari on iOS. The phone run is separate.

## Left on the M4

- `~/Applications/Firefox.app` (Firefox 160.0, Mozilla Developer ID), kept for
  later runs.
- `~/whip-w0-assets/` (assets above, plus `src/opus-20s.webm` and the ffmpeg
  logs), kept for the phone run. Serve it with `play-server.js`.
- Four OBS crash reports in `~/Library/Logs/DiagnosticReports`, from the
  SIGABRT exits above.
