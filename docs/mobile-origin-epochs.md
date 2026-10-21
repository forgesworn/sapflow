# Origin continuity across mobile orientation epochs

A phone orientation change closes its encoder/publisher and recording, then
starts a new epoch on the same destination path. The origin must retain the
HLS presentation: replacing segment 7 with another URI under the same playlist
causes hls.js's observed `media sequence mismatch 7` and can stop native Safari.
A reconnect grace period alone does not solve changed codec initialization.

This packet narrowly backports the reconnect/health implementation from
`775d97d` and its expiry guard from `98c9dc3` into the mobile feature checkout.
It preserves the checkout's late-AAC fix, blocking-reload readiness fixes,
recent-part retention, optional-track file initialization and zeroising SRT
configuration. It does not merge branches, replace crypto configuration, change
mobile media bounds, or refresh native packages. The existing SRT implementation
selection/default is retained.

## Presentation and initialization identities

The path, chunk prefix, segment and part IDs, media/discontinuity sequences,
output decode timeline, TARGETDURATION and PART-TARGET survive reconnects.
At the new connection's first validated keyframe, changed SPS/PPS/AAC create a
new immutable initialization descriptor. Parts and segments share that descriptor
by Arc. Retained old segments keep their old initialization bytes and URI;
new segments receive DISCONTINUITY and their new MAP before their first part.
A global MAP replacement would incorrectly relabel retained old fragments.

Initialization lookup scans the bounded retained part window plus the current
epoch; no unbounded epoch table exists. An HTTP response that already holds
Bytes or an Arc may finish after eviction. Preload requests retain their URI.
RFC 8216bis explicitly allows a hinted part to acquire a different initialization
or discontinuity; while that initialization is known but no real part exists,
a TYPE=MAP preload hint accompanies the part hint. A binding MAP is emitted only
with real published media, so empty intermediate connections advertise no new
codec epoch. Old blocking playlist requests retain their original MSN meaning.

The single-rendition master omits RESOLUTION, which is optional under
[RFC 8216 section 4.3.4.2](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.4.2).
This omission violates [Apple's authoring specification section 9.2](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html),
which requires it for video. Native Safari interoperability and Apple authoring
compliance are separate gates. No static display-canvas policy for changing
portrait/landscape aspect has been qualified; this packet does not resolve that
gap. Actual encoded dimensions remain in each AVC initialization.

The reconnect hold has an original outage deadline. Failed detached reconnect attempts cannot
extend it; accepted media recovery rearms a later independent outage. An expired
claim is rejected from the old presentation even if its expiry task is delayed.
Grace zero still closes immediately. Generation checks fence old expiry tasks.
B2 remains open: a replacement can attach just before the deadline without
recovering media. Non-media or keepalive traffic can keep resetting RTMP idle,
so there is no absolute retention bound while that connection remains attached.
This packet does not add a forced attached-connection outage deadline and makes
no absolute wall-clock bound claim for that pre-existing case.

## Recording isolation and copies

Detach interrupts and feeds the old recording's final complete parts while the
registry still excludes a new publisher. A configuration change starts another
origin recording. Its initial part cursor is floored at the new epoch, so retained
old parts can never be written under new parameter sets.

The HLS decode timeline continues; standalone files use a shared rational origin
chosen from the earliest first-fragment track decode time. The recorder subtracts
that same time in each track's timescale, preserving an AAC lead or lag rather
than independently zeroing tracks. Subtraction and timescale conversion are
checked. The bounded helper accepts our moof+mdat layout and tfdt versions 0/1,
rejecting unsupported structure/version/track IDs and backward relative times.
It runs on the recorder task, makes one bounded file-output copy and leaves HLS
Bytes unchanged. Existing fMP4 assembly copies are unchanged; this is not a
zero-copy recording claim.

Scope is **a new publisher connection with a new ingest epoch**. Mid-connection
SPS changes retain the existing warning/old-init behaviour and are unsupported
by this packet. The phone's orientation restart already creates a new connection.

## Regression and acceptance gates

`epoch-portrait.ts` and `epoch-landscape.ts` are 2.2-second locally generated
ffmpeg colour/sine fixtures: H.264 720x1280 / 1280x720 at 30 fps, one-second GOP,
no B frames, AAC mono 48 kHz. Repeating portrait/landscape/portrait restarts the
input clock while preserving a common audio format. No external copyrighted
capture or credentials are included.

Focused commands use this checkout's existing target and jobs=2:

```
cargo test --offline -p sapflow-core -p sapflow-hls -p sapflow-server --lib --bins -j 2
cargo test --offline -p sapflow-server --bin sapflow each_resolution_epoch_recording_decodes_independently_with_audio -- --ignored --nocapture
cargo clippy --offline -p sapflow-core -p sapflow-hls -p sapflow-server --lib --bins --tests -j 2 -- -D warnings
```

The fixed-rate regression covers H.264 aspect changes at 30 fps with unchanged
AAC 48 kHz. Other codec families, new frame rates, or audio layout changes need
separate acceptance. Protocol references are [RFC 8216 live playlists](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.2)
and [RFC 8216bis preload hints](https://datatracker.ietf.org/doc/draft-pantos-hls-rfc8216bis/22/).

The ignored-by-default test requires installed ffmpeg/ffprobe and must actually
be run for this packet. It creates three origin files, verifies both tracks and
near-zero file origins, then independently decodes each video and audio stream.
Task-owned test directories are removed. Other tests exercise pending playlist
and part requests, retained old init/media, a media-less intermediate connection,
MAP ordering, URI identity, fixed targets, monotonic video decode time, repeated
epochs/eviction, shared recording timebases and original grace deadlines.

These gates establish source/automated behaviour only. Release binary build,
lab deployment, native Safari recovery on the same URL without manual reload,
repeated phone rotations, upright images and sustained decoded A/V remain
separate acceptance gates. The prior failed viewer timeline is retained as the
counterexample; app capture continuing successfully did not repair that timeline.
