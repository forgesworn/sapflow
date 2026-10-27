# TV receiver peer adapter

This optional Node sidecar runs on the receiver, alongside Sapflow. It fetches
only the fixed loopback TV channel and mirrors complete fragments into a bounded
64 MiB cache. It does not fetch a Mac address, publish recordings, share publisher
credentials or expose the publisher's route. Its private, persistent Nostr key
signs commitments derived by the released RelaySwarm publisher from the mirrored
playlists. Viewer-supplied digests are never signed.

The dedicated same-host `/tv/swarm/relay` WebSocket accepts only signed ephemeral
presence and encrypted signalling events for this one channel. It preserves the
existing Nostr relay's authentication policy. Presence stays in memory for at
most 60 seconds, with 128 entries and 128 connections, four subscriptions per
connection, 32 KiB messages, heartbeat and send-queue limits. HTTP and signalling
have bounded global/concurrent and connection rate budgets. It writes no viewer
IP logs. Neither the cache nor the signing identity contains source filenames.

Public viewers start with peer assistance enabled and a visible opt-out before
Play. No peers connect while idle; private Mac preview always disables peers. It verifies media before
append, limits waiting and upload, and stops peer connections on Stop or page
exit. A failed peer media service falls back to the ordinary receiver path on
reconnect. The private Mac preview never loads these assets or joins the swarm.
Cloudflare's documented [STUN endpoint](https://developers.cloudflare.com/realtime/turn/)
is used only by opted-in viewers. No TURN allocation or public general-purpose
relay is enabled. Restrictive NATs can fall back to HTTP; this is not universal
NAT qualification. RelaySwarm 0.4.4 remains experimental.

## Build and run

Use Node 22.13 or later (Node 24 for repository tooling) and Python 3.12 or later:

```sh
npm ci --prefix tools/tv-relayswarm --ignore-scripts --no-audit --no-fund
python3 tools/tv-relayswarm/fetch.py
python3 tools/tv-relayswarm/wasm-notices.py --check
node tools/tv-relayswarm/build.mjs
node --test tests/tv-swarm-server.test.mjs tests/tv-peers.test.mjs
node fuzz/tv_swarm_fuzz.mjs 20000
install -d -m 0700 /private/sapflow-tv-swarm
TV_SWARM_STATE=/private/sapflow-tv-swarm \
TV_PUBLIC_ORIGIN=https://receiver.example \
node target/tv-relayswarm/server.mjs
```

The release downloader verifies fixed SHA-256 digests. The bundle includes only
the required publisher modules and pinned cryptographic/WebSocket dependencies;
it does not install the upstream project's hls.js, DHT or Node WebRTC stacks.
The combined notices include the pinned Rust WASM dependency graph, selecting MIT
where dual licences allow it, plus BSD, Boost and Zlib dependencies. The notice
collector reads Cargo metadata without rebuilding Rust. Distribute the generated
notices alongside the executable and browser assets. Install
`docs/examples/tv-swarm.service` with the generated bundle and configure its exact
public origin in an owner-readable environment file. The upstream defaults to
`http://127.0.0.1:28888/live/event-tv/`, listener to loopback port 28777.

Use the specific routes in `docs/examples/tv.Caddyfile`. Preserve existing site
routes, disable access logging, strip forwarded viewer IP headers, validate the
full candidate before reloading and keep a rollback copy. The edge connection
Worker has its own exact `/tv/` and `/tv/connection` routes and is separate from this sidecar.
The service's signing key stays in its private state directory, outside the
static site; never include it in deployment archives or source control.

For generated-media browser evidence, build Sapflow, provide the Playwright
module, and run `SAPFLOW_TV_TEST_PEERS=1 python3 -B tests/tv_browser_fixture.py`.
The peer check explicitly constructs two loopback SDK clients outside the
private-preview UI. It requires verified peer bytes, real decoded frames,
continued origin playback after peer departure, and no connected peers after
Stop. The ordinary private-preview check separately forbids every peer attempt.
