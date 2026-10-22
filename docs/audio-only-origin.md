# Explicit AAC-only origin contract

The B4R prerequisite is an audio-only origin path through our SRT/TS and RTMP
implementations, HLS packaging, recording and status callbacks. The native apps
also implement the [foreground microphone-only prerequisite](audio-only-mobile.md).
Physical capture and background operation remain unfinished qualification gates.

## Declare tracks before media

`TrackSet` contains `AudioOnly`, `VideoOnly` and `AudioVideo`. An unknown topology
remains unknown: an early AAC header, an audio hint or missing video metadata
cannot establish audio-only mode. Legacy undeclared RTMP publishing retains its
video-led startup behaviour.

TS ingest requires a complete PMT declaring AAC and no supported or recognised
unsupported video stream before selecting audio-only. Recognised unsupported
video is refused. RTMP uses one `onMetaData` object or ECMA array with both
`hasVideo` and `hasAudio` as unique booleans. At least one must be true. Optional
codec fields must match their enabled tracks: H.264 is `7` or `avc1`, AAC is `10`
or `mp4a`. Partial, duplicate, non-boolean, empty or contradictory declarations
fail closed. A metadata object without either flag retains legacy hint semantics.

Our sans-IO `Publisher::declare_tracks` emits one bounded metadata message after
publish acceptance and before codec headers or media. The existing native
`declareTracks` method delegates to it without changing its public signature.
Identical declarations are idempotent; a changed declaration or forbidden track
is refused. An omitted declaration preserves the previous publisher output.
Each connection freezes its topology and first accepted AAC configuration.
A fresh connection may change them through a new initialization epoch.

## Media and timeline

The audio-only profile is AAC-LC, mono or stereo, an indexed sample rate no
greater than 65,535 Hz, and exactly 1,024 samples per access unit. Explicit RTMP
mode requires canonical two-byte ASC. It refuses 960-sample mode, core-coder
dependency, extension flags and trailing configuration bytes; a header arriving
before a later declaration cannot bypass this restriction. This does not tighten
the existing legacy A/V ASC parser.

AAC uses its sample-rate clock. The 44.1 kHz path derives timestamps from total
samples rather than repeatedly rounding a frame increment. Completed AAC parts
are independent; the known final frame can be sealed without waiting for video.
Existing playlist bounds, fixed timing targets, retained initialization epochs,
part/segment identifiers and program-date-time rules still apply. No dummy video
track, fabricated resolution or synthetic keyframe is added. See the
[Apple HLS authoring rules](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html)
and the existing `playlist::RuleCheck` assertions.

Within-run AAC timestamps tolerate TS/RTMP rounding up to 180 ticks at 90 kHz.
A larger gap or repetition starts a discontinuous run and sets `Update.media_loss`.
Merging updates preserves that flag. Intentional interruption, reconnect and
codec replacement alone do not set it. The server marks the matching recording
incomplete before feeding parts, including both files when one batch combines
loss with epoch rollover. A protocol rejection after accepted RTMP media also
marks the accepted prefix incomplete, even if a subsequent reply write fails.
Each recording pins its initialization identity; a later epoch is refused and
the old prefix is synced incomplete if ingest fails before reporting rollover.
Normal zero-grace End seals the accepted tail before closing the writer.

`TsIngest::finish` now supplies an explicit connection EOF, distinct from a live
feed temporarily running out of bytes. It releases a complete, independently
validated bounded AAC PES even when it occupies only one TS start packet. Empty
pushes remain starvation. Repeated finish is harmless; further pushes return
`IngestError::InputFinished`. A reconnect uses a fresh parser and the retained
presentation. Raw PES header stuffing is included in advertised-length checks.

Terminal processing rejects truncated bounded PES/TS and reports definite media
loss, including AAC without a usable timestamp. An unbounded video tail is not
guessed or emitted; its uncertain duration alone does not invalidate the accepted
prefix. This remains an accepted-prefix recording policy, not a complete-show
guarantee. Finish may seal that prefix before returning a semantic error; callers
must preserve the incomplete-recording result before feeding its final parts.

The server drains accepted input under registry, session and media ownership
before closing or holding the connection. Revocation permits no new input, while
already accepted data can still finish. Stale generations cannot drain a later
parser or relabel its recording. A successful terminal codec change starts a
distinct recording using the newly accepted connection's captured callback scope.
Each file still pins its immutable initialization identity. EOF-created content
can enter reconnect grace; old held parts cannot qualify a new connection's
fresh-media receipt.

Terminal recording rollover uses the runtime captured when the recording was
created, including when connection Drop runs outside an ambient Tokio runtime.
That originating runtime must remain alive until the writers complete. A runtime
handle does not establish process-shutdown durability.

## API and observation

The pre-release Rust API now returns `Option<&H264Config>` from `Muxer::video()`.
The existing A/V constructor and reconfigure method remain available;
`audio_only`, `reconfigure_tracks` and updating `audio_at` methods cover explicit
topologies. Audio-only callers must forward updates and errors. The older void
audio methods remain video-led compatibility paths.

Rust consumers matching RTMP events must handle the new `Event::Tracks` variant.
Update literals need `media_loss` or a default initializer; consumers must
preserve that flag when merging updates and reporting recording completion.
Exhaustive `IngestError` matches must also handle `InputFinished`.

`Health.keyframe_interval_ms` is now `Option<u64>`: A/V JSON retains its numeric
field, while audio-only JSON carries `keyframeIntervalMs: null`. Bitrate remains
measured. A scoped fresh-media receipt reports the actual tracks in a current
independent part, not just configuration, retained prior media or a socket state.
It is encoded availability, separate from fetching and decoding.

## Remaining release gates

Finite generated loopback tests decode four SRT/RTMP audio profiles through HLS
and recording, plus an A/V SRT regression. RTMP recordings retain the source
counts of 283/260 AAC frames at 48/44.1 kHz. The initial SRT recordings contain
272/253 frames. A controlled follow-up uses an explicit 1,316-byte read chunk,
a nine-second sender timer and a loopback UDP proxy. Unpadded input loses its
final 1,128/188 bytes before the origin; the captured wire bytes equal the exact
source prefix. Adding valid null TS packets to complete the last block makes
the transmitted and captured bytes/hash equal, and recordings contain 283/260
frames. This isolates final short-input truncation by the black-box test sender
in that controlled configuration. The original default-chunk/EOF run remains
separate evidence. Neither run qualifies native shutdown or a complete show.

A separate 25-second default-chunk/EOF reproduction now isolates the same test
sender problem: its captured wire input is the exact source prefix, missing
1,448 bytes and ending with 56 partial TS bytes. The origin correctly marks
that accepted recording incomplete and suppresses its complete callback.
The smoke sender now validates whole generated TS packets, appends at most six
valid null packets and explicitly selects a 1,316-byte chunk. A controlled
corrected run transmits all 3,000,480 padded bytes with an identical wire hash,
clean parser EOF, independent decoding and one recording-complete callback.
The full SRT/RTMP/reconnect/encryption smoke passes with its assertions unchanged.
This corrects synthetic test input; it does not relax production loss handling
or qualify native encoder shutdown and complete-show retention.

Executed terminal tests now cover the single-start-packet case, exact fixture
frame counts, truncation, stuffing, missing timestamps, idempotence, reconnect
freshness, scoped revocation and actual writer finalisation. The per-PID terminal
metadata is finite and contains no elementary payload copy. The maintained
reader's pre-existing retained unbounded-PES payload needs separate memory-limit
hardening; the unread-input `MAX_BUFFER` does not establish that bound.

The native apps now select explicit foreground audio-only capture/permissions,
recording and PCM-only observation without a camera, video encoder or keyframe
wait. Qualify background/lock behaviour, interruptions, routes and USB audio
on each supported OS, plus actual scoped creator integration, recording priming
and presentation, complete-show retention and sustained performance.
Generated loopback fixtures and host consumers do not establish these gates or
performance superiority. See the [delivery status](mobile-delivery-status.md),
[mobile plan](mobile-streaming-plan.md) and [library-quality plan](library-quality-plan.md).
