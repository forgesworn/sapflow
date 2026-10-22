# Explicit AAC-only origin contract

The B4R prerequisite is an audio-only origin path through our SRT/TS and RTMP
implementations, HLS packaging, recording and status callbacks. Native audio-mode
selection and physical background operation are separate, unfinished work.

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

## API and observation

The pre-release Rust API now returns `Option<&H264Config>` from `Muxer::video()`.
The existing A/V constructor and reconfigure method remain available;
`audio_only`, `reconfigure_tracks` and updating `audio_at` methods cover explicit
topologies. Audio-only callers must forward updates and errors. The older void
audio methods remain video-led compatibility paths.

`Health.keyframe_interval_ms` is now `Option<u64>`: A/V JSON retains its numeric
field, while audio-only JSON carries `keyframeIntervalMs: null`. Bitrate remains
measured. A scoped fresh-media receipt reports the actual tracks in a current
independent part, not just configuration, retained prior media or a socket state.
It is encoded availability, separate from fetching and decoding.

## Remaining release gates

Finite generated loopback tests decode four SRT/RTMP audio profiles through HLS
and recording, plus an A/V SRT regression. RTMP recordings retain all 283/260
source AAC frames at 48/44.1 kHz. The corresponding SRT recordings contain
272/253 frames. The source remux preserves every input frame; the loss stage
between black-box SRT sending and origin close is not yet isolated. These SRT
results establish decodable prefixes, not complete tail retention.

The native apps still need mic-only capture/permissions, audio-only recording
and remote PCM observation, with no camera, video encoder or keyframe wait.
The existing strict A/V observer and V4V profile admission need an explicit audio
profile. Qualify background/lock behaviour, interruptions, routes and USB audio
on each supported OS, plus scoped creator integration and sustained performance.
Generated loopback fixtures and host consumers do not establish these gates or
performance superiority. See the [delivery status](mobile-delivery-status.md),
[mobile plan](mobile-streaming-plan.md) and [library-quality plan](library-quality-plan.md).
