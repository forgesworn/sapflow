# A linear TV channel

For publishing from a Mac while hiding its network address from the receiver,
see [private TV publishing](private-tv-publishing.md). It uses onion-only SSH
through Tor with mandatory metadata stripping and no direct fallback. Ordinary
SRT/RTMP publishing below does not hide the publisher's address from the receiver.

Prepare a folder once, then publish it continuously to one Sapflow path. Sources
stay untouched. Live gigs from phones or OBS use separate paths and credentials:
their encoder formats need not match the channel. Switching a single player
between TV and a gig belongs to the host application; these scripts do not
schedule gigs or change a public stage.

## Prepare

Requires Bash, FFmpeg with libx264/AAC, FFprobe and the usual Unix tools. Examples
use generated or operator-owned media; no media is included in the repository.

```sh
scripts/tv-prepare.sh /srv/tv/sources /srv/tv/prepared
scripts/tv-prepare.sh --verify /srv/tv/prepared
```

MP4, MOV and MKV inputs are sorted by filename. The default output is 1920x1080,
square pixels, 25 fps, H.264 High level 4.1, yuv420p, BT.709 limited range, a
closed 25-frame GOP, 4.5 Mbit/s video with 5 Mbit/s maximum and a 10 Mbit buffer.
AAC-LC is stereo, 48 kHz, 160 kbit/s. Smaller or differently shaped pictures are
scaled to fit and padded with black. Silent sources receive an audio track.
MP4s use faststart. `TV_WIDTH`, `TV_HEIGHT`, `TV_FPS`, `TV_VIDEO_BITRATE`,
`TV_MAXRATE`, `TV_BUFSIZE`, `TV_AUDIO_BITRATE` and `TV_PRESET` override defaults.
`TV_ENCODER=videotoolbox` selects Apple's hardware encoder; verification still
decides whether its outputs are compatible. Changing settings requires preparing
every clip again. Fractional frame rates use a GOP rounded to the nearest frame.

Existing newer outputs are skipped. Older outputs are reported as stale and
left alone; `--force` explicitly replaces them. Duplicate output names and
source/output directory equality are refused. A successful FFmpeg encode is
moved into place from a temporary file. Verification compares stream fields,
H.264 SPS/PPS bytes and AAC configuration across every selected output. One
single-rendition live stream requires these to match its init segment. Matching
filenames or resolutions alone is insufficient.

## Publish

Use a dedicated `live/event-tv` path. Obtain credentials through the existing
publish-session control API, or the server's legacy configured publisher
credentials. Local generated-media tests may use `SAPFLOW_OPEN_PUBLISH=1`.

```sh
export TV_PUBLISH_URL='srt://media.example:8890?streamid=publish:live/event-tv:USER:PASS&latency=2000'
scripts/tv-playout.sh /srv/tv/prepared
# Alternative transport:
export TV_PUBLISH_URL='rtmp://media.example:1935/live/event-tv?user=USER&pass=PASS'
scripts/tv-playout.sh --shuffle /srv/tv/prepared
```

Protect real URL files from other users. SRT media encryption additionally
requires the server's configured passphrase and `passphrase` URL parameter;
stream IDs are visible on the wire. Raw RTMP has no transport encryption.
RTMPS requires a separately configured TLS ingest front. URL secrets are
redacted from script logs but remain visible to local process inspection.

Playout verifies the prepared folder, writes a safely quoted concat list and
uses `-re -stream_loop -1` with copied video. Audio is re-encoded to avoid
overlapping AAC priming/padding timestamps at file boundaries. `--audio copy`
opts into those timestamp warnings; it is not the validated default. `--once`
plays the list once for diagnostics. `--shuffle` chooses an order each time
FFmpeg starts, not at each loop. `--no-verify` is a diagnostic escape hatch.
If FFmpeg lacks SRT, the script uses `srt-live-transmit` when available.

After failure, playout restarts at `TV_BACKOFF_MIN` (default 2 s), doubling to
`TV_BACKOFF_MAX` (60 s). A run lasting `TV_STABLE_SECONDS` (60 s) resets the
delay. A restart inside `SAPFLOW_RECONNECT_GRACE` can retain the stream with a
discontinuity; it is not a seamless encoder handover. SIGTERM stops the child
process tree and removes the temporary concat list.

## Run as a service

Keep one supervisor: the script retries media failures; systemd or launchd
restarts the script itself after a fatal error. Example systemd unit, with an
operator-created unprivileged `sapflow-tv` account and a mode-0600 environment
file containing `TV_PUBLISH_URL`:

```ini
[Unit]
Description=Prepared video channel
After=network-online.target
Wants=network-online.target

[Service]
User=sapflow-tv
EnvironmentFile=/etc/sapflow/tv.env
ExecStart=/opt/sapflow/scripts/tv-playout.sh /srv/tv/prepared
Restart=on-failure
RestartSec=5
KillMode=control-group
TimeoutStopSec=15
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

For launchd, use an operator-owned mode-0600 wrapper that exports the URL from a
private file, then `exec`s playout. Never put credentials in a committed plist.
Set PATH explicitly to the operator's installed tools in that wrapper.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.example.tv-playout</string>
  <key>ProgramArguments</key><array><string>/opt/sapflow/tv-run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>StandardOutPath</key><string>/var/tmp/tv-playout.stdout.log</string>
  <key>StandardErrorPath</key><string>/var/tmp/tv-playout.stderr.log</string>
</dict></plist>
```

## TLS front and capacity

[The HAProxy example](examples/haproxy.cfg) exposes only `/live-hls/live/`,
strips `/live-hls/` on the upstream request and preserves LL-HLS query strings,
CORS and cache headers. It advertises HTTP/2 and HTTP/1.1 over TLS and does not
add caching or access logs. Keep Sapflow's viewer and control listeners private.
Validate the config with real certificate paths, then separately test blocking
reloads, preload hints, browser playback and rollback before changing a host.

Certificates are supplied by the operator; this sample does not provision or
renew them. Wire certificate renewal to a validated HAProxy reload. The
[HAProxy 3.2 manual](https://docs.haproxy.org/3.2/configuration.html) describes
timeouts and connection reuse. HAProxy's core uses GPL version 2, with LGPL
exportable headers and an OpenSSL exception, per its
[licence file](https://github.com/haproxy/haproxy/blob/master/LICENSE).
It runs as a separate proxy; it is not linked into Sapflow.

At 4.5 Mbit/s video plus 160 kbit/s audio, 200 viewers consume about 932 Mbit/s
before protocol overhead. A 1 Gbit/s link therefore supports fewer than the
idealised 214 viewers, with operational headroom required. This is arithmetic,
not a measured audience capacity. A faster proxy alone cannot raise the link
capacity. A lower rendition needs a separate encode and multivariant packaging;
these scripts currently prepare one rendition.

## RelaySwarm integration boundary

RelaySwarm already provides an hls.js loader with origin fallback, signed media
commitments, explicit relay/ICE policy and optional peer uploads. Its current
`createHlsPublisher` reads a local HLS directory. Sapflow keeps live parts in
memory and serves them over HTTP; an existing MP4 recording is not that HLS
directory. Merely attaching a loader does not provide signed live commitments
or prove reduced server bandwidth.

The TV host must supply a stable channel/swarm identity, trusted stream event,
bounded commitment endpoint and byte-identical origin/range identity including
init segment, media sequence, discontinuity and part index. Build the adapter
at that boundary, without giving a browser the signing key or accepting hashes
chosen by viewers. Use the deployment's tested relays and ICE policy explicitly.
Playlist reloads and preload hints continue to use Sapflow.

Start with origin-only playback, then shadow measurement; shadow downloads
additional peer bytes and saves no origin bandwidth. Peer-first requires the
authorised commitments and measured recovery deadlines. Enable uploads only
after viewer consent, with bounded budgets and an immediate origin-only switch.
Native Safari playback does not use an hls.js loader. Acceptance needs two real
browser peers, verified peer media, working fallback on departure, no increased
stalls, and measured origin-byte reduction. Public NAT, mobile and a real show
are separate checks. The [receiver TV page](tv-player.md) currently uses
origin-only playback and does not enable peers.

See RelaySwarm's [host integration contract](https://github.com/forgesworn/relayswarm/blob/main/INTEGRATION.md)
and `src/hls-publisher.mjs` for the current publisher boundary.

## Local validation, 26 October 2026

Twenty automated tests pass, including real FFmpeg preparation, matching codec
configuration, names containing quotes, credential redaction, restart backoff,
active-child SIGTERM cleanup and subsecond output freshness. Bash syntax and
ShellCheck pass. CI runs the tests after installing FFmpeg in the smoke job.

Three generated testsrc2/sine clips represented 720p24/44.1 kHz (8 s),
2160p25/48 kHz (6 s) and 480p30/48 kHz (7 s). They were normalised with the
default format and `TV_PRESET=ultrafast`, then played for 52 s per transport,
more than two full loops, into the Sapflow build at `27627c1`.

| Transport | Playlist renders | Media sequence range | Init segments | Discontinuities | Publish attempts |
|-----------|------------------|----------------------|---------------|-----------------|------------------|
| RTMP | 197 | 0 to 35 | 1 | 0 | 1 |
| SRT, FFmpeg plus srt-live-transmit | 188 | 0 to 32 | 1 | 0 | 1 |

Both runs passed the Apple playlist-rule checker on every collected render,
with stable per-sequence discontinuity numbers. A fetched init plus completed
segment after two loops decoded without errors: H.264 1920x1080 at 25 fps,
stereo AAC 48 kHz. Neither playout log contained non-monotonic DTS warnings.
This is generated local protocol/decoding evidence, not a show-length A/V sync,
browser, phone, live-service or public-NAT acceptance result. The sample's
synthetic media and child processes were removed afterwards.

The HAProxy sample was checked with checksum-verified upstream HAProxy 3.2.0:
configuration syntax, verified localhost certificate, HTTP/2 ALPN, an actual
HTTP/1.1 proxy response, exact LL-HLS query/prefix handling, preserved CORS/cache
headers and rejection of a control-API path. That is a functional config check,
not a new proxy performance measurement or a production deployment.
