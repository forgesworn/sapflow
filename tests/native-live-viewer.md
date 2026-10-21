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
actual encoded resolution, and media time and presented callbacks must resume.
Save the bounded diagnostic evidence and the origin's request/timeline evidence.
The event history retains at most 128 entries. The fixture observes the native
player; it does not fetch or retain media bytes.

The observer owns one `requestVideoFrameCallback` when that API and its cancel
method are available. It retains only a bounded numeric callback count and the
last finite `mediaTime`; it retains no frames or per-frame event history.
`presentedCallbacks`, `lastPresentedMediaTime` and `presentationSupported` are
included in the snapshot. One callback is re-armed after each invocation.
Disposal and pagehide cancel the outstanding request; a late callback is fenced
and cannot update counters or re-arm itself.

Playback status requires advancing media time, advancing presentation callbacks
and sufficient ready state. A frozen `getVideoPlaybackQuality` counter therefore
cannot override observed presentation. `decodedFrames` remains a separate
diagnostic. Where the callback API or its cancel method is absent, decoded-counter
progress is the fallback. A present but unresponsive or failing callback API
remains the selected signal; decoder activity cannot silently replace it.
`presentationRequestFailed` records a failed request without retrying it.

The `playing` gate requires paired evidence within four seconds. After the
initial four-second observation window, advancing clock without that evidence
reports `unverified`, with an explicit `stateReason`: callback unresponsive or
unavailable, decoder counter unresponsive, no frame signal, or paired progress
unconfirmed. It does not establish either successful presentation or frozen
pictures. Four seconds without observed clock progress reports `stalled` with
`media-clock-nonprogress`, even if callbacks continue. This describes the sampled
media clock, not proven physical screen behaviour. Pause, end and media error
retain priority over both classifications.

`progressSignal` identifies the selected evidence. `clockProgressAgeMs` and
`selectedFrameProgressAgeMs` measure time since independently observed advances,
or since observation began if no advance has been seen. The corresponding
`lastClockProgressElapsedMs` and `lastSelectedFrameProgressElapsedMs` are null
until progress is observed. These sampled scalar diagnostics do not relax the
paired gate. `lastProgressElapsedMs` still identifies confirmed paired progress.
Clock and frame signals are compared with the last confirmed pair so staggered
native updates cannot cause a false stall. A callback arriving after the clock
has been frozen for four seconds cannot turn that stale pair into playing.
Counter/time resets establish a baseline without fabricating progress.
The callback is a browser presentation signal, not physical screen/camera proof.
Buffered-ahead time and seekable ranges are diagnostic
values, not glass-to-glass latency. Native HLS may buffer differently from the
separate hls.js fixture; qualify both independently.

Run `node tests/native-live-viewer.test.mjs` for event/path validation,
single-source identity through simulated dimension changes and errors,
paired-progress/unverified/clock-stall/pause/end handling, a frozen quality counter
with continuing presentation, advancing clock with no selected frame signal,
frozen clock with callbacks, bounded immutable snapshots, one owned callback,
dispose/pagehide cancellation and stale-callback fencing. These deterministic
tests do not establish physical Safari recovery. This observer correction does
not fix playback or retrospectively certify periods without presentation evidence.
