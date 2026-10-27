# Receiver TV page and private preview

`web/tv/` is a standalone, origin-only TV player for
`/tv/live/event-tv/index.m3u8`. It fetches no media before **Watch live**.
**Stop watching** aborts pending startup and disposes the media engine.
Off-air responses retry with bounded backoff while watching. **Back to live**
uses the engine's live position. The native video controls support pause,
volume, fullscreen and seeking within the available window.

There are no analytics, external fonts, remote scripts, signalling or peer
connections. Safari uses native HLS; other supported browsers use the local,
pinned hls.js light module. A Chromium native HLS capability hint alone does
not select native playback. No RelaySwarm integration is enabled here.

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
`media-src 'self' blob:` for Media Source Extensions. No inline scripts, `eval`, worker or external origin is required.

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
limits concurrent transfers to four, with time and response-size bounds.
Stopping the process cancels outstanding transfers and closes the pool.

The preview accepts only Sapflow's current, bounded playlist subset. Absolute
URLs, encryption key URLs, content steering, variable definitions, unknown
playlist features and unexpected paths are refused. Media always returns
through the local gateway. The local CSP further restricts network access.
The preview uses complete segments with an approximately six-second start
offset to reduce Tor round trips; public playback retains LL-HLS parts.
Tor capacity and latency still vary. This is not a guarantee of uninterrupted
playback or complete anonymity against traffic correlation, device compromise,
identifying programme content or other applications on the Mac. Metadata
stripping and prepared publishing are covered in the
[private publishing guide](private-tv-publishing.md).

## Verification and third-party software

```sh
node --test tests/tv-player.test.mjs
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

hls.js light 1.7.3 is vendored under Apache 2.0, including its upstream notice
and complete licence. `web/tv/vendor/hls.build.json` records upstream package
integrity and the local file hash. Only the source map comment was removed.
The preview optionally installs PycURL 7.49.0, with distribution hashes pinned
in `scripts/requirements-tv-preview.txt`, using the MIT option of its dual
licence. PycURL and its bundled libcurl dependencies are installed separately;
they are not linked into Sapflow or bundled with the public player.
