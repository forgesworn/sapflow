# Private TV: bounded qualification, 27 October 2026

This records generated-media checks on an Apple M1 and a separate Linux
receiver. No real camera footage was used. The receiver identity, host keys,
credentials and detailed host evidence remain in the operator's private
handover. See [setup and privacy boundaries](private-tv-publishing.md).

## Observed

| Check | Result |
| --- | --- |
| Apple VideoToolbox preparation | Generated 720p and 1080p H.264/AAC inputs prepared and verified on the M1 with hardware encoding required. |
| Actual publishing route | Mac loopback RTMP, SSH through local Tor SOCKS, onion-only receiver SSH, loopback Sapflow ingest. |
| Socket observation | Publisher/SSH/netcat TCP sockets used loopback; receiver SSH and ingest connections arrived over loopback. This is process/socket observation, not a full packet-capture audit. |
| Lower-rate run | HLS advertised about 1.04 Mbit/s average. A sample advanced 178 media sequences over 178.995 wall seconds, with no discontinuity in its playlist window. |
| Higher-rate run | The 1080p profile targeted 4.5 Mbit/s video; the actual HLS average was about 3.60 Mbit/s. A sample advanced 249 media sequences over 254.564 wall seconds, with no discontinuity in its playlist window. |
| Longer higher-rate observation | Decoded media tail timestamps advanced only 353.960 seconds over 397.254 wall seconds: about 43.3 seconds of additional delay. This route did not sustain that profile in real time. |
| Media readback | Received init/parts decoded both H.264 video and AAC audio on the M1. |
| Injected metadata | Synthetic source tags, stream handler names and H.264 user-data markers were absent from received parts; both tracks still decoded. |
| Connection interruption | Terminating the task-owned SOCKS helper stopped the supervisor, encoder and SSH groups. The existing Tor service was left running. |
| Unavailable proxy | Exit status 1 before a publisher started; no alternate route was selected. |
| Restricted receiver identity | Shell access, remote forwarding and a disallowed TCP destination were refused over the real onion endpoint. |
| HTTPS route | Public master playlist fetched through Tor; the private-control path returned 404. Only the dedicated TV stream was routed to the media receiver. |
| Other receiver services | Existing Caddy, Tor and Nostr gate stayed active. Nostr information readback retained its previous relay version and policy. Existing Tor configuration and onion fragment hashes stayed unchanged. |

Start with the lower-rate preparation example and measure again on the actual
route. The higher-rate profile failed the real-time delivery gate in this run.
No automatic bitrate adaptation or direct-route fallback is provided. These
are bounded synthetic runs, not a real show, glass-to-glass latency measurement
or a sustained 4.66 Mbit/s wire-rate qualification.

## Regressions

The existing 20 TV script regressions pass. Fourteen private-publisher checks
cover configuration rejection, credential isolation, missing/failed tunnels,
SIGTERM during verification/playout, metadata removal and rejecting network
references disguised as local media. A real OpenSSH/RTMP integration checks
strict host pinning, SOCKS routing, decodable media and the absence of FFmpeg
version markers from the original RTMP bytes. These pass on macOS and in an
isolated Linux container.

The Linux run caught an extra `exec` in `ProxyCommand`: OpenSSH supplies that
prefix itself, and dash rejects a duplicated one. The proxy command now starts
with netcat directly. Linux CI runs only its generated sshd integration fixture
with the privileges needed for PAM account checks; it does not unlock a runner
password account or change the publisher's runtime privilege requirements.

## Still open

Native Safari/browser playback, measured end-to-end latency, full packet-capture
verification, long-duration jitter/stall and reconnect behaviour, and a real
programme remain separate gates. The broadcaster does not join a peer swarm.
Public NAT/mobile and RelaySwarm savings have not been qualified by these runs.

Tor hides the connection's source address from the receiver; it does not erase
accounts, prior identifying contact, content, software negotiation or traffic
correlation. No zero-metadata or complete-anonymity claim is made.
