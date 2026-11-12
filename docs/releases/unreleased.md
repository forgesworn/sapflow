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
  over-reported a file by a clamped stall's hole at such a break. Parts
  are held one part target past their three target durations after their
  tag leaves, since one call can seal more media after the muxer stamps
  the removal (an interrupt's short final part of audio). The first
  segment whose part tags the playlist lists never goes back, so a part
  tag once removed is never listed again (RFC 8216bis 6.2.1), even where
  the edge steps back at such a run end; `RetentionCheck` now fails on a
  tag listed again.

## sapflow-rtp

- The timeline's start hold (`TimelineConfig::start_hold`) now defaults to
  4 s, up from 1.5 s. Headless Chrome 157's sender reports for both streams
  came 3.4 to 3.5 s after it connected, so with 1.5 s every Chrome session
  was aligned by arrival time, and 3 of 20 had audio more than 40 ms early
  (A3.8). A publisher whose reports come with its first packets, as
  FFmpeg's do, is still aligned at once. One that sends none waits 4 s for
  its first media. One above about 8 Mbit/s whose reports come late fills
  `max_hold_bytes` (4 MiB) before 4 s and is aligned by arrival time, as
  before.
- Test builds only (`testing`): `FeedbackHooks` and
  `Receiver::set_feedback_hooks` make receiver reports claim loss and add
  REMB (`rtcp::build::remb`), for A3.10.

## sapflow

- A stream's recording no longer rolls to a new file at every codec epoch
  without bound. Each stream has a budget of rolls
  (`SAPFLOW_RECORDING_ROLL_BURST`, default 8; 0 turns it off), regaining
  one per `SAPFLOW_RECORDING_ROLL_REFILL` seconds (default 60) passed in
  both media time and wall time, so timestamps spaced faster than real
  time cannot refill it; neither clock banks more than one period of lead,
  so in any window a stream starts at most the burst, one file per period
  and one more.
  Past it, epochs are still served but not recorded; recording resumes in
  the first epoch that lasts `SAPFLOW_RECORDING_ROLL_SETTLE` seconds
  (default 10) with a roll back, from that epoch's earliest retained part
  that begins a segment.
  The file that ran out and the resumed one are marked with the new reason
  `epoch-rate`. An encoder that changes its parameter sets at every
  keyframe used to make a file and a `recordSegmentComplete` callback a
  second, half of v4v's callback limit for every stage. sapflow-hls adds
  `Muxer::published_media`, the published media time the budget counts
  beside the wall clock.
- The RTMP listener and the scoped control API no longer stop on a failed
  accept (out of file descriptors, or a connection reset before it was
  taken): they log it and try again after 100 ms, as WHIP's listeners do.
- `--help` says the playlist can keep more than `SAPFLOW_SEGMENT_COUNT`
  segments, as above.
- The "SRT connection closed" log line also gives `reordered`,
  `duplicates`, `belated` and `naks_sent`. `reordered` counts missing
  packets that arrived as originals after the receiver had queued their
  NAK: retransmissions the path did not need, for a caller that flags its
  retransmissions as libsrt does. That is field data for whether the
  receiver should wait a little before sending a NAK, as it now sends one at
  the first gap.
- WHIP logs at debug how each session's start was aligned, each keyframe
  request and arrival, and its receive counters once a second.
- A `test-feedback` feature, off in every other build, lets
  `SAPFLOW_TEST_WHIP_FEEDBACK` make WHIP sessions claim loss in their
  receiver reports or send REMB, for A3.10. Not for production use.

## sapflow-srt

- `receiver::Stats` gains `reordered`: packets reported missing that then
  arrived as originals, not retransmissions (counted correctly only when
  the caller sets the retransmission flag, as libsrt does). `Stats` is not
  `#[non_exhaustive]`, so code that builds one with a struct literal needs
  `..Default::default()`.

## sapflow-dtls

- A connected server now refuses every renegotiation with one warning-level
  `no_renegotiation`. It used to answer only a ClientHello whose
  `message_seq` was above the last Finished's, but DTLS 1.2 starts each
  handshake's messages at 0 (RFC 6347 section 4.2.2), so a conforming
  peer's request was silently dropped.
- Unauthenticated (epoch 0) input no longer ends a handshake, or holds it
  until the deadline, in the ways rounds 2 and 3 of the A4.4 review found.
  Round 4 found four more that are still open, two of them single packets
  (see the crate README and
  `docs/whip-evidence/a4.4-security-review-2026-11-12.md`):
  - Every alert at epoch 0 is dropped.
  - Fragments are admitted by `message_seq`, type, epoch and a length the
    parser could accept before anything is kept, so a Finished is kept
    only at epoch 1.
  - A fragment over `max_message_len`, `max_buffered` or `max_fragments`,
    and an empty fragment of a non-empty message, allocate nothing.
  - A fragment that disagrees with bytes already received starts its own
    candidate instead of overwriting them.
  - Room in reassembly is reclaimed from stalled or younger unauthenticated
    candidates, paced by `initial_retransmit`. No limit changes.
  - A complete epoch 0 message that fails a check made before any ECDH or
    verification is refused without moving the handshake on.

  Each is counted in `Stats::records_dropped`.
- So these errors no longer come from `handle_datagram` for epoch 0 input,
  and no alert is sent for them: a hello outside the profile (`Version`,
  `NoCipherSuite` and the rest, or `Decode`), `Limit::HelloVerify`,
  `NoCertificate`, `FingerprintMismatch`, `BadKeyShare` before ECDH,
  `VerifyScheme`, `UnexpectedMessage`, `PeerAlert`, and the reassembly
  `Limit`s. A client that really sends one is ended by the caller's
  handshake deadline. The new `Server::refused` (additive) gives the reason
  for the last refused message. Errors under the read keys, from a
  certificate that matches the offer, and from a failed verification still
  end the handshake with their alert.
- In WHIP, a publisher whose certificate does not match its offer now ends
  at the 10 s handshake deadline, not at once. It still ends as
  `EndReason::Dtls(FingerprintMismatch)` and is counted as `dtls-failed`:
  at the deadline a session reports the reason `sapflow-dtls` last refused
  a message, and is `HandshakeTimeout` only when nothing was refused. The
  server's end log line now carries the DTLS error for a `dtls-failed`
  end.
- The public API changes only by the addition of `Server::refused`.
