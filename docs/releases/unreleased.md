# Unreleased

Notes for the release after 0.2.0-rc.1.

## sapflow-hls

- `RuleCheck::default()` now enforces the live window (RFC 8216bis 6.2.2's
  segment rule), so it rejects some renders that rc.1 accepted. A muxer
  whose 64 MiB byte cap cut its window short says so through
  `Muxer::window_byte_capped()`; pass that to
  `RuleCheck::check_with_byte_cap`.
- The muxer keeps more than `MuxerConfig::segment_count` segments while
  removing one would leave the playlist lasting less than three target
  durations, up to `muxer::MAX_WINDOW_ENTRIES` (256) entries and 64 MiB
  beyond the count. AAC output changes only where the count alone fell
  short of that.
