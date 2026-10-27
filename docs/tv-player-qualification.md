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
| Lifecycle/privacy regressions | Eight player tests and seventeen preview tests passed with PycURL installed, including real SOCKS hostname forwarding despite NO_PROXY and a dead proxy that never connected to the origin. A real idle socket also closed within the shutdown deadline. |
| Manifest mutation | 20,000 deterministic mutation executions passed. This is finite, not coverage-guided fuzzing. |

The browser route guard allowed only the operator-owned loopback preview
origin; unexpected external requests and peer-connection attempts failed the
check. The production media transport separately required remote DNS through
SOCKS and HTTPS certificate verification. These observations do not establish
a whole-machine packet capture or exclude identifying programme content,
traffic correlation, compromised software or activity from other applications.

Playwright WebKit stalled while creating a page on this Mac, before playback
could be tested. Actual Safari and iPhone playback remain unqualified. The
public player now supports ManagedMediaSource with a native-only fallback and
has lifecycle regression tests,
but Chrome results are not promoted into Safari acceptance. High-bitrate Tor
publishing and large-audience capacity are also outside this check.

Only generated test patterns and sine-wave audio were broadcast. The test
publisher and preview processes are stopped after qualification. No personal
programme, camera footage or frames are published by this work.

## Complete-fragment MIT player and continuity follow-up

The historical hls.js receipt above predates the replacement player. The new
MediaSource engine uses Sapflow's own strict master/media playlist parser and
native fMP4 decoding. A 60-second public Chrome observation decoded 1,480 frames
with 59.227 seconds of media progression, no dropped frames or buffering events,
and no hls.js module loaded. This short observation does not clear the owner's
reported long-running pauses or physical Safari acceptance.

Prepared programme diagnosis found two AAC tail gaps of 2.461 and 1.272 seconds
while video timestamps remained continuous. Preparation now pads short audio to
the video end, and playout maintains a continuous audio clock across file cuts.
A generated three-track fixture deliberately includes short audio tails, checks
all three decoded colours plus a loop, and injects a three-second fragment delay.
The player, quiet-tone lifecycle, artwork availability state, peer consent and
private-preview restrictions have separate unit regressions. The image prompts
and model/digest receipts establish generated artwork provenance, without camera
references. Two-peer and hosted CI receipts are recorded with the delivered PR;
local loopback peer results do not qualify restrictive NATs or audience capacity.

The generated two-browser gate received 47,522 verified peer bytes, rejected a
corrupted signed commitment, advanced 10.025 seconds after its serving peer
left, and reported zero connected peers after Stop. The separate origin-only
fixture decoded 507 frames over 20.341 seconds with audio, all three track
colours, no external requests and no peer attempts. Loopback peer ICE used an
explicit fake-device permission accommodation; production does not request
camera or microphone access. These are finite checks, not public-NAT or
long-show acceptance.


A subsequent full public loop presented 20,260 frames over 810.37 seconds with
zero waiting events or dropped frames. An independent publishing-Mac test
through the private Tor preview exposed another bottleneck: sequential fragment
fetches could not hide the route's round-trip delay. The player now overlaps at
most four complete fragments and keeps native appends ordered. The generated
browser gate asserts two to four overlapping requests, and a separate lifecycle
regression requires Stop to abort all four loaders. This transport correction's
full Mac-loop and hosted receipts belong to the delivered PR; the earlier public
result alone did not qualify the private Tor preview.


The full publishing-Mac Chrome run through the trusted Tor preview presented
20,263 frames over 810.58 media seconds, with zero dropped frames or waiting
events and zero external page requests or peer attempts. A later public run
caught a real publishing-tunnel outage of about a minute. The receiver's old
15-second trial reconnect grace expired before the publisher returned; its
operational setting is now the normal 90 seconds. This keeps the channel's
identity across a return but cannot manufacture video while the upload is down.

Forced reconnect testing also found an early seek to the newest buffered range
and small AAC holes at a timestamp boundary. The player now preserves the older
range and nudges only a waiting decoder over a bounded empty interval. The
browser regression records every requested seek and rejects skipped buffered
programme beyond the 100 ms recovery tolerance, then verifies continued playback
when the serving peer leaves. A transient playlist 503 is injected separately
and must not reset decoding. These checks distinguish ordinary track continuity,
client recovery and actual upstream network availability.


## iPhone playback-path correction

The previous capability check considered only ordinary MediaSource. An iPhone
with ManagedMediaSource alone therefore fell back to native HLS, missing the
complete-fragment buffering, gap recovery and peer loader. Selection now checks
both APIs against the advertised codecs and disables remote playback for the
managed attachment, restoring it on Stop. Managed streaming hints stop new
media transfers after initial buffering. Native-only public fallback uses the
whole-fragment mirror with a twenty-second start offset.

The generated browser gate exercises managed API selection with real Chrome
fMP4 decoding, transfer suspension/resumption and remote-playback cleanup.
This simulated capability check is distinct from actual iOS WebKit or physical
Safari acceptance. Public peers default on at Play with a visible opt-out;
private preview remains forced off. The public-origin browser gate verifies
the default and opt-out before playback without making external connections.
