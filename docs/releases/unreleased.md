# Unreleased

Notes for the release after 0.2.0-rc.1.

## sapflow-hls

- `RuleCheck::default()` now enforces the live window (RFC 8216bis 6.2.2's
  segment rule), so it rejects some renders that rc.1 accepted. It also
  accepts short `INDEPENDENT=YES` parts in a segment whose every part is
  independent, which rc.1 rejected. A muxer
  whose 64 MiB byte cap cut its window short says so through
  `Muxer::window_byte_capped()`; pass that to
  `RuleCheck::check_with_byte_cap`.
- The muxer keeps more than `MuxerConfig::segment_count` segments while
  removing one would leave the playlist lasting less than three target
  durations, up to `muxer::MAX_WINDOW_ENTRIES` (256) entries and 64 MiB
  beyond the count. AAC output changes only where the count alone fell
  short of that.
- With a retention pool (below), a part stays fetchable through
  `Muxer::part` for three target durations
  of published media after the playlist stops listing it (RFC 8216bis
  6.2.2's part rule), up to 64 MiB and `muxer::MAX_RETAINED_PARTS` parts
  beyond the window. Past either cap, or when the retention pool is full,
  the oldest go early; `Muxer::parts_released_early()` counts them.
  Playlists and segments are unchanged; where segments leave a short
  window, more parts and their codec epoch's init are served than in rc.1.
  `playlist::RetentionCheck` checks the rule.
- With a retention pool, a segment that leaves the window stays fetchable
  through `Muxer::segment`, with its init, until its Availability Duration (its
  duration plus the longest playlist that listed it) has passed since it
  first appeared (RFC 8216bis 6.2.2); rc.1 dropped it at once, one segment
  duration early. It shares the 64 MiB, which counts a segment and its
  parts once, and is capped at `muxer::MAX_RETAINED_SEGMENTS` (1,024)
  segments; `Muxer::segments_released_early()` counts any that go early.
  `playlist::RetentionCheck` checks this rule too.
- Retention for these rules needs a pool, and is off without one: media
  then leaves with the window, exactly as in rc.1. Turn it on with
  `Muxer::with_retention_pool`, or `EsIngest`, `TsIngest` and
  `FlvIngest::with_retention_pool` or `with_fragment_budget` (all new and
  additive). What a muxer keeps is charged to the pool, not to the
  fragment budget: the budget's use is rc.1's, so retention never refuses
  live media. The server always gives a pool.
- The server creates the pool beside its fragment budget:
  `SAPFLOW_MAX_RETAINED_BYTES` (`--max-retained-bytes`), default 128 MiB.
  Fragment memory is `SAPFLOW_MAX_FRAGMENT_BYTES` plus this pool.
- After a stream ends, kept media stays fetchable only while the server
  keeps the presentation: set `SAPFLOW_RECONNECT_GRACE` (default 90 s) to
  at least a segment's duration plus the longest playlist for the rules to
  hold past the end.
- Opus audio beside AAC (#66), with timeline slew carried in audio-only
  Opus and the refusal order tested (#68). See `docs/whip-ingest.md`.
- An interrupt no longer drops what the muxer holds. At a codec epoch
  change, a reconnect or the end of a stream, the held video frame and the
  held audio frame are written into the run, and audio at or past that
  frame's time is kept: the video frame lasts as long as the frame before
  it, or until the end of the audio received, and audio past a full part
  goes in one short final part. What cannot fit is dropped and reported as
  `media_loss`, where rc.1 dropped it silently. New and additive:
  `Muxer::reconfigure_with_audio_at` ends a run at the next keyframe on a
  clock that carries on, without moving output time; `EsIngest` uses it for
  a mid-stream parameter-set change. A run after a cut opens its first part
  at its keyframe, so its program date is that keyframe's arrival.
  Video-led output changes at every interrupt, including the end of each
  stream; audio-only output and output with no interrupt are unchanged.
  The fragment budget's use moves with it at an interrupt, where more
  media is now kept. A cut on a continuing clock ends the old segment with
  its media, so `EXTINF` is the sum of its parts and a video pause before
  the new keyframe falls at the discontinuity. `RuleCheck` and
  `scripts/check-playlist.py` now check that sum, and allow a longer
  segment only when one of its parts is exactly `PART-TARGET` (a clamped
  stall, whose hole the next part's time carries). Audio stamped before
  the new keyframe that has to be dropped belongs to the old epoch: it is
  not in the new run's `media_loss` but counted by the new
  `Muxer::take_late_audio_loss`, and `Muxer::late_audio_epoch` says while
  it may still come. A library caller that records should drain
  `take_late_audio_loss()` after every push and interrupt and mark the
  recording of each epoch it names, and should hold the outgoing
  recording open, not finish it, while `late_audio_epoch()` names that
  recording's epoch; sapflow's server does both, and marks the old file,
  not the next one, `media-loss`. `Muxer::late_audio_dropped` counts the
  frames, and WHIP's session-end loss counts include them
  (`late_audio=`). A break or interrupt right after a frame clamped to
  `PART-TARGET` (a stall) now ends the segment with its media and resumes
  the next run there, for SRT and RTMP as for WHIP, where `EXTINF` used to
  include the stall's hole before the discontinuity. A recording's length
  (`segmentDuration` and the file's header) now comes from its parts' own
  timestamps (`fmp4::rebase_part_for_file_timed`), not their spans, which
  over-reported a file by a clamped stall's hole at such a break.
