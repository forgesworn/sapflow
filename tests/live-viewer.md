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

The viewer now has an explicitly bounded safety net on the same Hls instance
and source URL. A fatal network failure permits three `startLoad()` attempts;
a fatal media failure permits two `recoverMediaError()` attempts. One timer
covers either retry delay (500/1000/2000ms) or its four-second progress watchdog.
An attempt that hangs without another error still exhausts its budget. Budgets
reset only after advancing media time and decoded frames when that counter is
available, not after a `playing` event. No automatic source reload, Hls
reconstruction or live-edge seek hides a new origin epoch.
Decoder-counter resets establish a fresh observation without clearing attempts;
subsequent advancing time and frames are still required.

The explicit Play/Pause control preserves intent during media detachment;
ordinary native-control pause is also observed. Pausing cancels the timer
without resetting consumed attempts, and resuming continues the remaining
budget. HTTP 401/403, unsupported fatal errors and a media-sequence mismatch
are terminal. ENDLIST cancels retries while allowing listed media to finish;
the buffered ending remains manually playable. A stopped/fatal/waiting player
cannot regain a Playing/edge status from the periodic status timer alone.
`metrics().recovery` exports a bounded 64-action history, counters and terminal
reason; the pre-existing diagnostic evidence survives recovery.

This safety net does not repair origin discontinuities. A logical reconnect
must preserve media/discontinuity sequence identity, retain advertised old
resources and introduce the correct immutable init map for changed codec
configuration. The observed `levelParsingError` / media-sequence mismatch is
reported as an origin failure rather than retried or concealed. Qualify
server continuity first with existing players, then measure these recovery
actions separately. The page still always uses hls.js, including in Safari;
native HLS and V4V require their own unchanged-source player gates.

Run deterministic diagnostic ownership/retention checks with:

```sh
node tests/live-viewer-diagnostics.test.mjs
```

The deterministic harness covers byte retention plus single-flight retries,
hanging/thrown actions, budgets and real-progress reset, pause/resume during
decoder reattachment, End/authentication, invalid-origin sequences, status
rendering and evidence retention. Its browser/Hls adapters are synthetic;
actual pinned hls.js, Safari controls and decoder recovery remain browser
qualification checks. Source changes do not update an already deployed fixture.
