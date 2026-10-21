# Live viewer evidence fixture

Serve `live-viewer.html` at `/`, the selected pinned hls.js build at `/hls.js`,
and proxy `/live/` to the origin on the same test route. Use the actual intended
HTTP/2 or HTTP/3 and compressed-playlist route when qualifying LL-HLS. The HTML
does not provide a TLS server, install certificates or change origin settings.
Select a synthetic event with `?event=event-test`; only bounded lowercase event
identifiers are accepted. Record the served hls.js version/hash and both the
installed broadcaster and origin hashes separately.

`window.metrics()` reports the complete buffered ranges and recent errors.
`window.evidence.packet(metrics())`, also available through the Save button,
exports bounded init/fragment bytes as base64, response metadata, playlist text,
fragment/part timing and the first parsing/gap/append incident with surrounding
parts. The loader copies bytes before hls.js transfers them to a demux worker;
an observer of `FRAG_LOADED` alone may receive a detached buffer. The production
loader and worker remain selected. Retention has an 8 MiB/32-item rolling budget,
an additional 8 MiB incident budget and bounded init/playlist/error history.
Oversized media is omitted. Capture failures are explicit in `captureErrors`
and cannot interrupt the playback callback.

This page handles live media. Save evidence only for an authorised test, retain
it privately and remove media after the needed result is recorded. A page reload
clears history. An export is a finite observation: startup bytes may already
have left the rolling window, and bounded history cannot retain every incident.

The test configuration selects a one-second live-sync target and allows catch-up
playback. Its distance from hls.js's estimated origin edge is not glass-to-glass
latency. Measure a visible capture clock separately. Manual Go live seeks and
viewer reloads must be recorded when interpreting a sustained run. This fixture
does not qualify Safari's native HLS path or a shipping mobile viewer.

Run deterministic diagnostic ownership/retention checks with:

```sh
node tests/live-viewer-diagnostics.test.mjs
```
