# Publishing from a private Mac

The prepared TV publisher can send one H.264/AAC programme from a Mac to a
Sapflow receiver without making the Mac's address part of the viewer or receiver
connection. This mode requires an onion-only SSH endpoint and a trusted local
Tor SOCKS proxy. It has no direct network fallback.

```text
Prepared files on Mac -> metadata stripping -> loopback RTMP
  -> SSH through local Tor SOCKS -> receiver's onion SSH
  -> loopback Sapflow ingest -> receiver's HTTPS player/viewers
```

Only the publishing hop uses Tor. Viewers fetch from the receiver, so adding
viewers does not multiply the Mac's upload. The broadcaster must not join a
WebRTC/RelaySwarm swarm, serve its media URL directly or open a public listener.
RelaySwarm can later operate among viewers independently of this publisher.

## Privacy boundary

An onion service is designed to hide the client's network location from the
service and vice versa. Onion traffic stays inside Tor; there is no exit node
on this publishing route. See the [Tor onion-service overview](https://community.torproject.org/onion-services/overview/index.html).

This is address hiding, not a promise of zero metadata or complete anonymity:

- The receiver sees the programme, stream path, credentials, dedicated SSH
  account/public key, SSH software negotiation and traffic timing. Codec
  configuration, dimensions, frame rate and media timestamps remain necessary.
- A reused personal account, SSH key, payment method or setup connection can
  identify an operator. Provision access through a privacy-preserving route too.
  A dedicated key does not undo earlier identifying contact with that host.
- Tor guards and the Mac's ISP can observe the source connection. Tor does not
  eliminate traffic correlation, especially for long continuous broadcasts.
- Faces, voices, signs, reflections, locations and content watermarks remain in
  the picture/sound. Stripping tags does not change that content.
- A compromised Mac, local SOCKS proxy or receiver falls outside this guarantee.
  This script constrains its own route; it is not a system-wide firewall or a
  way to anonymise other applications running on the Mac.
- Local process inspection can still expose the publish URL. Keep the Mac,
  private files and operator logs under the broadcaster's control.
- Opening the public viewer URL in an ordinary browser on the broadcasting Mac
  reveals that browser's connection address to the HTTPS service. Use a
  separate privacy-preserving viewing route for an operator preview; this
  publisher does not proxy the browser or make its fingerprint anonymous.

The receiver observes the local Tor-to-SSH hop and loopback RTMP originator,
rather than the Mac's public address. Its own public HTTPS endpoint remains
visible to viewers. Do not forward the Mac's address in application headers,
callbacks, playlists, signed commitments or discovery events.

## Receiver setup

Use a dedicated restricted SSH account such as `tv_ingest`. Keep the existing
SSH administration service and other workloads intact. A separate sshd can
listen on `127.0.0.1:2222` with its own host key and configuration:

```text
ListenAddress 127.0.0.1
Port 2222
HostKey /etc/ssh/sapflow_tv_host_ed25519_key
AllowUsers tv_ingest
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowTcpForwarding local
PermitOpen 127.0.0.1:1935
AllowAgentForwarding no
X11Forwarding no
PermitTTY no
MaxSessions 0
```

Create a separate publishing key with no personal comment. Keep its private
half on the Mac. The receiver's dedicated account gets only the public half in
its normal `~/.ssh/authorized_keys`, with restrictive permissions and options:

```text
restrict,port-forwarding,permitopen="127.0.0.1:1935" ssh-ed25519 PUBLIC_KEY
```

Do not put an administrator key in this file. The dedicated daemon must reject
shell/session requests, remote forwarding, agent forwarding and destinations
other than the Sapflow ingest port. Validate its effective configuration with
`sshd -t -f /path/to/config` and `sshd -T -f /path/to/config` before starting it.

Add a new onion service to the receiver's Tor configuration, preserving its
existing services and identities:

```text
HiddenServiceDir /var/lib/tor/sapflow-publish/
HiddenServiceVersion 3
HiddenServicePort 22 127.0.0.1:2222
```

Validate the Tor configuration, reload it and read the generated `hostname`
file privately. Never copy or publish `hs_ed25519_secret_key`. This mapping
does not require a new public firewall port. Keep Sapflow RTMP and viewer HTTP
bound to loopback, SRT bound to loopback if enabled, and recording disabled or
in private storage according to the operator's needs. Publish only the desired
HLS route through the receiver's HTTPS front. The SSH restriction does not
replace Sapflow's existing publish authentication.

Before a host change, record the previous service/configuration hashes, retain
rollback copies and check the host's existing services. Do not replace an
existing public Nostr relay with a media server or expose its private ports.

## Mac setup

Requires Python 3, Bash, FFmpeg/libx264/AAC, FFprobe, OpenSSH and OpenBSD-compatible
`nc` (included on macOS). Apple VideoToolbox is an optional preparation encoder.
There is no camera capture or OBS control in this initial prepared-TV path.

Use an existing trusted Tor SOCKS listener, or configure a separate client:

```text
ClientOnly 1
DataDirectory /path/to/private/tor-data
SocksPort 127.0.0.1:9050
SafeSocks 1
TestSocks 1
```

Keep its data directory private and wait for Tor to bootstrap. Do not overwrite
an existing Tor configuration or run a second process on an occupied SOCKS
port. [Tor's SOCKS guidance](https://support.torproject.org/little-t-tor/troubleshooting/check-for-leaks/)
explains how `TestSocks` reports and `SafeSocks` refuses unsafe DNS requests.
This publisher passes the onion hostname to SOCKS5 without resolving it on the
Mac, using [netcat's proxy mode](https://man.openbsd.org/nc).

Pin the dedicated receiver's SSH host public key through a trusted route. A
mode-0600 known-hosts file contains the onion hostname and that host public key:

```text
ONION_HOSTNAME ssh-ed25519 HOST_PUBLIC_KEY
```

Do not accept an unknown key on first connection. Do not fetch the pin by making
a direct connection from the Mac. For a non-default onion SSH port, use the
usual `[ONION_HOSTNAME]:PORT` known-hosts entry.

Prepare operator-owned files locally. Start below the default TV bitrate until
the actual Tor route has demonstrated adequate sustained bandwidth:

```sh
TV_ENCODER=videotoolbox TV_WIDTH=1280 TV_HEIGHT=720 \
  TV_VIDEO_BITRATE=1200k TV_AUDIO_BITRATE=96k \
  scripts/tv-prepare.sh /private/source /private/prepared
```

This bitrate is a test setting, not a Tor capacity guarantee. The default TV
profile is about 4.66 Mbit/s before overhead. Measure the actual circuit rather
than assuming either profile can run continuously with low latency.

Put `live/event-tv?user=USER&pass=PASSWORD` in a private mode-0600 publish-path
file, with URL-encoded credentials. Keep the SSH private key and known-hosts
file owner-only too. An unlocked agent is deliberately not used; an encrypted
identity needs a separately suitable unattended-key arrangement.

```sh
read -r TV_PRIVATE_PUBLISH_PATH < /private/config/publish-path
export TV_PRIVATE_PUBLISH_PATH
python3 -B scripts/tv-private-publish.py \
  --onion-host ONION_HOSTNAME --ssh-user tv_ingest \
  --identity-file /private/config/publisher-key \
  --known-hosts /private/config/known_hosts \
  /private/prepared
```

Replace the placeholders; only checksum-valid v3 onion hostnames are accepted.
`--once` runs the playlist once. `--shuffle` randomises the initial order.
`--socks-port`, `--ssh-port` and `--ingest-port` select non-default ports; all
forwarding endpoints remain loopback. The script ignores `TV_PUBLISH_URL`,
verifies prepared media before networking, ignores user/system SSH config,
uses only the explicit SSH identity and pin, and establishes a fresh private
SSH master before asking it to bind the local RTMP port.

If Tor or SSH fails, it stops. It never switches to public SSH, SRT, direct
RTMP or a different relay. SSH keepalives detect a stalled connection; until
then FFmpeg can block on the existing local tunnel. After a failure, restore
Tor/access and restart explicitly. The media supervisor can retry encoder
failures while the same SSH master remains alive.

`--strip-metadata` is mandatory in this wrapper. It disables source global and
stream tags, chapters, FFmpeg version markers and all H.264 SEI. The RTMP connect
command uses a generic client identifier instead of FFmpeg's version. Audio is
re-encoded. It retains the technical FLV track metadata Sapflow needs to start
audio/video correctly. Removing all SEI also removes embedded captions and
supplementary presentation data, as documented by [FFmpeg](https://ffmpeg.org/ffmpeg-bitstream-filters.html#filter_005funits).
Use a separate caption workflow if captions are required.

## Evidence required before a real programme

The [27 October bounded qualification](private-tv-qualification.md) records the
M1-to-receiver Tor runs and their remaining acceptance gates.

Local automated checks cover rejected public/malformed addresses, credential
isolation, strict host pinning, loopback-only forwarding, tunnel failure and
cleanup. Real FFmpeg checks inject synthetic tags/chapters/H.264 user data,
check their absence from captured playout and decode both tracks. A real
OpenSSH/RTMP integration uses a local SOCKS stand-in and synthetic keys. It
checks a wrong pin fails and a valid pin delivers decodable media. That stand-in
proves routing and authentication plumbing, not Tor anonymity or capacity.

The live gate is a generated-media run over the configured onion receiver:

1. Check Tor's SOCKS diagnostics and capture Mac egress separately to establish
   that the publishing process uses the local proxy and makes no direct
   receiver/DNS connection. Check receiver SSH/ingest connection addresses.
2. Interrupt Tor, the tunnel and ingest independently. Establish that media
   stops, children are reaped and no alternate public connection appears.
3. Fetch the receiver's playlists, init segments, parts and any recording.
   Decode audio/video and inspect for injected source metadata and addresses.
4. Measure sustained bitrate, stalls and viewer latency on the real route.
   Native Safari and an actual show-length run remain separate acceptance gates.
5. Confirm unrelated receiver services stayed healthy and exercise rollback.

No real footage, origin addresses, operator keys or private relay endpoints
belong in public qualification evidence.
