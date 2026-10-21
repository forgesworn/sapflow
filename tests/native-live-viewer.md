# Native Safari HLS qualification

Serve `native-live-viewer.html` on the same origin as Sapflow's `/live/` route
and open `native-live-viewer.html?event=EVENT_ID` in Safari. A local proxy may
map a shorter route to this file. This fixture uses the native video element
and no JavaScript HLS implementation. Click Play if browser policy blocks
autoplay. Use the video's own controls for pause, volume and full screen.

Keep the page open while rotating the physical broadcaster. The source is
assigned once, with no reload, seek-to-live or player recreation. Check
`sapflowNativeMetrics()` before and after each rotation: the instance identifier
and source-assignment count must stay fixed, dimensions must change to the
actual encoded resolution, and media time and decoded frames must resume.
Save the bounded diagnostic evidence and the origin's request/timeline evidence.
The event history retains at most 128 entries. The fixture observes the native
player; it does not fetch or retain media bytes.

The status requires advancing media time, available decoded-frame progress
and sufficient ready state before reporting playback. Four seconds without
progress reports stalled. An intentional pause, end or media error has its own
status. Time and frame-counter changes are compared with the last confirmed
progress so separate native counter updates cannot cause a false stall. A
decoder counter reset establishes a new baseline without fabricating progress.
The frame counter is sampled once per second, not a count of frames
physically presented. Buffered-ahead time and seekable ranges are diagnostic
values, not glass-to-glass latency. Native HLS may buffer differently from the
separate hls.js fixture; qualify both independently.

Run `node tests/native-live-viewer.test.mjs` for event/path validation,
single-source identity through simulated dimension changes and errors,
progress/stall/pause/end handling, bounded immutable snapshots and listener
cleanup. These deterministic tests do not establish physical Safari recovery.
