# Receiver TV page and private preview

`web/tv/` is a standalone TV player for
`/tv/live/event-tv/index.m3u8`. It fetches no media before **Watch live**.
**Stop watching** aborts pending startup and disposes the media engine.
Off-air responses retry with bounded backoff while watching. **Back to live**
uses the engine's live position. The native video controls support pause,
volume, fullscreen and seeking within the available window.

There are no analytics, external fonts or remote scripts. Safari normally uses
native HLS; Chrome uses Sapflow's own MIT complete-fragment MediaSource player.
It consumes the receiver's single H.264/AAC fMP4 rendition, without a general
HLS transmuxer or hls.js dependency. The strict 64 KiB playlist parser refuses
external references and unsupported features; fragments are limited to 8 MiB.

Viewer RelaySwarm assistance is explicitly opt-in. Enabling it selects the
MediaSource engine and receiver-side `/media/` mirror. Peers supply only media
verified against the receiver's signed, playlist-bound commitments. HTTP remains
the fallback; peer waiting is capped at 500 ms with a three-second origin reserve.
Upload is capped at 250 kB/s and 128 MiB per viewing session. The page always
recommends a VPN covering browser and WebRTC traffic with a kill switch.
Peers and the configured STUN service may see the viewer's connection address.
No VPN or anonymity claim is inferred from a successful connection.

The private loopback Mac preview forcibly disables peer controls, never imports
the SDK and does not serve the peer module or SDK/WASM assets. It also skips the
edge IP lookup. The Mac's publishing route stays independent of viewer sharing.
See [the receiver adapter](tv-relayswarm.md) for setup and boundaries.

Chrome playback overlaps at most four complete-fragment downloads, appends them
in order and uses a ten-second live-edge target to
absorb jitter in the publisher's upload. Back to live retains that headroom.
This deliberately trades a short delay for continuity. A waiting decoder may
nudge across at most two seconds of already-buffered empty time, only at the end
of the preceding range; it never jumps directly over the older programme to a
newer buffered range. Transient playlist 404/429/503 responses retry at most twice
within the same ten-second request budget, and Stop cancels the backoff. It cannot recover
quality already lost during preparation or compensate for an upload that stays
slower than the encoded programme bitrate.

The stream/privacy panel reports media dimensions, advertised bitrate/codecs,
buffered video and estimated distance behind the receiver. It identifies
broadcaster fields that the player is not given and states that the publisher's
network route is not verifiable from this browser. It is not an anonymity
attestation. Images, audio and timing can still identify a broadcaster.

The footer links to the MIT player licence and the actual permissive dependency
notices. hls.js and its Apache licence are no longer distributed by this page.
Sunburst-generated standby and NOW STREAMING cards have saved prompts and digest
receipts. The on-air card appears only after an availability response while the
player starts. The user-controlled 1 kHz tone starts quietly, never automatically,
and stops when live playback starts or the viewer stops.

The same-origin edge endpoint `/tv/connection` returns that request's public IP,
approximate city/region/country and network provider. It uses Cloudflare's already
available request metadata, with no separate lookup, application storage or
per-viewer logging. This does not describe the platform's security logs. It does
not identify a street address or name, or establish whether a VPN is running.
Deploy `edge/tv-connection.mjs` as a Worker scoped to that exact route; missing
edge information displays unavailable without affecting playback.

## Serve the public page

Copy the contents of `web/tv/` into a separate public static directory. Do not
serve the private publisher directory, keys, configuration, logs or recordings.
Use the [Caddy fragment](examples/tv.Caddyfile) inside the existing receiver's
HTTPS site. Adapt the loopback upstream port to the deployment. Retain the
existing site's other services and keep Sapflow's listeners private.

Back up the existing configuration, validate the full candidate with
`caddy validate`, then reload. Read back the page, module and licence hashes,
CSP and MIME types through HTTPS. Test live playback and off-air recovery,
verify that other relay routes still work, and rehearse restoring the saved
configuration. If the existing site has a deferred, unconditional CSP header, turn that
header into a `?Content-Security-Policy` default so it respects the TV policy.
Check that other routes retain their existing CSP. The player needs
`media-src 'self' blob:` for Media Source Extensions. No inline scripts or JavaScript `eval` is required. The public peer-enabled policy
allows WASM compilation and same-host WSS; the private preview keeps both disabled.

The public URL is for ordinary viewers. Opening it normally reveals that
browser's connection address to the HTTPS endpoint and any intermediary.
Broadcasters who also want to hide their viewing connection should use the
private local preview below.

## Preview on the publishing Mac through Tor

Keep a trusted local copy of this repository's `scripts/` and `web/tv/`.
The preview serves the local assets, never receiver-supplied HTML or JavaScript.
Only the allowed TV media routes are fetched from the configured receiver.
Install the hash-pinned transport in a private virtual environment (Python
3.10 or later, a supported PycURL wheel, and an existing Tor SOCKS listener):

```sh
python3 -m venv /private/sapflow-preview-venv
/private/sapflow-preview-venv/bin/python -m pip install \
  --only-binary=:all: --require-hashes --no-deps \
  -r scripts/requirements-tv-preview.txt
/private/sapflow-preview-venv/bin/python -B scripts/tv-preview.py \
  --receiver https://media.example --socks-port 9050
```

Open the printed `http://127.0.0.1:PORT/tv/` address. The server binds only to
loopback and checks Host, Origin and cross-site fetch headers. Requests use
SOCKS5 with remote hostname resolution, mandatory certificate verification,
no redirects and no direct fallback. The pool reuses HTTPS connections and
limits concurrent transfers to four, with time and response-size bounds. Idle local browser sockets are closed
after five seconds so they cannot hold shutdown open.
Stopping the process cancels outstanding transfers and closes the pool.

The preview accepts only Sapflow's current, bounded playlist subset. Absolute
URLs, encryption key URLs, content steering, variable definitions, unknown
playlist features and unexpected paths are refused. Media always returns
through the local gateway. The local CSP further restricts network access.
The preview uses complete segments with an approximately ten-second start
offset to reduce Tor round trips; the receiver can still serve LL-HLS parts to other clients.
Tor capacity and latency still vary. This is not a guarantee of uninterrupted
playback or complete anonymity against traffic correlation, device compromise,
identifying programme content or other applications on the Mac. Metadata
stripping and prepared publishing are covered in the
[private publishing guide](private-tv-publishing.md).

See the [bounded browser and deployment qualification](tv-player-qualification.md)
for measured results and the remaining Safari acceptance gate.

## Verification and third-party software

```sh
node --test tests/tv-player.test.mjs tests/tv-playlist.test.mjs tests/tv-tone.test.mjs tests/tv-connection.test.mjs tests/tv-peers.test.mjs
node fuzz/tv_playlist_fuzz.mjs 20000
python3 -B tests/test_tv_preview.py
python3 -B scripts/check-tv-assets.py
python3 -B fuzz/tv_preview_fuzz.py 20000
# After building Sapflow and installing Playwright 1.63.0:
python3 -B tests/tv_browser_fixture.py
```

The browser fixture creates its own generated H.264/AAC programme, loopback
Sapflow and explicit test-only direct transport. Production preview has no
direct mode. It checks decoded frames, audio signal, playback progression,
network isolation and Stop, retaining generated evidence in `target/tv-browser`.
The Python mutation gate is finite and is not coverage-guided fuzzing. A
WebKit run is distinct from physical Safari or iPhone acceptance.

RelaySwarm 0.4.4 browser and core assets are pinned by SHA-256 in
`tools/tv-relayswarm/pins.json`. Runtime dependency notices are distributed under
`web/tv/vendor/THIRD-PARTY-LICENCES.txt`, alongside the MIT player licence.
The preview optionally installs PycURL 7.49.0, with distribution hashes pinned
in `scripts/requirements-tv-preview.txt`, using the MIT option of its dual
licence. PycURL and its bundled libcurl dependencies are installed separately;
they are not linked into Sapflow or bundled with the public player.
