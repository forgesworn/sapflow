# TV player: bounded qualification, 27 October 2026

This records generated-media checks for the receiver page and private Mac
preview. Operator endpoints, keys and detailed receipts remain in private
storage. See [setup and privacy boundaries](tv-player.md).

| Check | Observed result |
|---|---|
| Real Chrome through Tor, 720p | Over a 60-second observation window, 1,466 video frames counted, 31 dropped, 58.708 seconds of media progression; decoded AAC audio had a non-zero Web Audio signal. This was a short qualification run, not uninterrupted long-show acceptance. |
| Connection reuse | Three consecutive HTTPS playlist fetches used one Tor connection. The pooled preview later completed 234 media requests using six connections. The earlier per-request transport had repeated buffering and was replaced. |
| Installed private launcher | A separate 30-second run decoded 766 frames at 1280 x 720 with audio, recovered from two injected off-air responses, used no external page requests or peer connections and ended requests on Stop. |
| Public deployment | Six served page/module/style/licence files matched local hashes over Tor-routed HTTPS. Private control, other streams, build metadata and source map paths returned 404. |
| Existing relay | Nostr information readback retained version 1.1.2 and its existing CSP. TV's readback included the required blob media policy. Existing services remained active. |
| Configuration rollback | Restored the exact pre-player Caddy fragment and reloaded it, then restored the validated player configuration. The enclosing CSP was subsequently made a default so it respected the specific TV policy. |
| Separate loopback Sapflow fixture | Generated 320 x 180 H.264/AAC decoded 399 frames over 15 seconds with audio and off-air recovery. This local run reused an existing sibling-checkout binary; the CI gate builds Sapflow separately before running the fixture. |
| Lifecycle/privacy regressions | Eight player tests and sixteen preview tests passed with PycURL installed, including real SOCKS hostname forwarding despite NO_PROXY and a dead proxy that never connected to the origin. |
| Manifest mutation | 20,000 deterministic mutation executions passed. This is finite, not coverage-guided fuzzing. |

The browser route guard allowed only the operator-owned loopback preview
origin; unexpected external requests and peer-connection attempts failed the
check. The production media transport separately required remote DNS through
SOCKS and HTTPS certificate verification. These observations do not establish
a whole-machine packet capture or exclude identifying programme content,
traffic correlation, compromised software or activity from other applications.

Playwright WebKit stalled while creating a page on this Mac, before playback
could be tested. Actual Safari and iPhone playback remain unqualified. The
public player retains the native HLS path and has lifecycle regression tests,
but Chrome results are not promoted into Safari acceptance. High-bitrate Tor
publishing and large-audience capacity are also outside this check.

Only generated test patterns and sine-wave audio were broadcast. The test
publisher and preview processes are stopped after qualification. No personal
programme, camera footage or frames are published by this work.
