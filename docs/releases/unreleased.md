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
- A part stays fetchable through `Muxer::part` for three target durations
  of published media after the playlist stops listing it (RFC 8216bis
  6.2.2's part rule), up to 64 MiB and `muxer::MAX_RETAINED_PARTS` parts
  beyond the window. Past either cap, or when the fragment budget refuses
  a new part, the oldest go early; `Muxer::parts_released_early()` counts
  them. Playlists and segments are unchanged; where segments leave a short
  window, more parts and their codec epoch's init are served than in rc.1,
  and each stream can hold up to 64 MiB more of the shared fragment budget.
  `playlist::RetentionCheck` checks the rule.
- A segment that leaves the window stays fetchable through
  `Muxer::segment`, with its init, until its Availability Duration (its
  duration plus the longest playlist that listed it) has passed since it
  first appeared (RFC 8216bis 6.2.2); rc.1 dropped it at once, one segment
  duration early. It shares the 64 MiB, which counts a segment and its
  parts once, and is capped at `muxer::MAX_RETAINED_SEGMENTS` (1,024)
  segments; `Muxer::segments_released_early()` counts any that go early.
  `playlist::RetentionCheck` checks this rule too.
- Opus audio beside AAC (#66), with timeline slew carried in audio-only
  Opus and the refusal order tested (#68). See `docs/whip-ingest.md`.
