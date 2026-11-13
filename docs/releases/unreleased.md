# Unreleased

Notes for the release after 0.2.0-rc.1.

## sapflow-client

- The MPEG-TS muxer that the SRT publishers use writes each PES
  continuation whose bytes lie in one piece straight into the output chunk,
  instead of building a packet for the `mpeg2ts` writer; first packets,
  tables, PCR packets and continuations across two pieces still go through
  the writer. The bytes are unchanged (the same hash over 5.6 GB of four
  benchmark workloads, and a test against the writer for every payload
  length and continuity counter, four PIDs and every flag). On the M4, time per sample
  fell to 0.76 of before for 320-byte AAC, 0.26 for 8.5 kB H.264 and 0.20
  for a 512 KiB keyframe.
- `MuxStats::packet_payload_copy_bytes` and `writer_payload_copy_bytes`,
  exported by sapflow-ffi among the SRT diagnostics, now count only the
  bytes that take the writer path, since the direct path does not copy
  them through the dependency.

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

- A keyframe within 10 ms of `MuxerConfig::segment_min` now ends a video
  segment. OBS over RTMP floors its millisecond timestamps, so at 30 fps
  its keyframe a second in is stamped 999 ms; the first segment ran on to
  the next keyframe, 1.999 s, and fixed a target duration of 3 for the
  whole stream where SRT from the same OBS got 2. Players that hold back
  three target durations were 3 s further behind on RTMP.
- `MuxerConfig::window` (new, default none) bounds the window by time as
  well as by `segment_count`. Segment length follows the encoder's keyframe
  interval, so 24 segments of a publisher sending a keyframe every 8 s held
  192 s of media, and memory grew with bitrate times keyframe interval. With
  a bound, the oldest segment goes while the rest still last at least the
  bound, but the window never lists fewer than `LEADING_GAPS` (7) entries
  or less than three target durations: an 8 s interval under a 48 s bound
  keeps seven segments, 56 s. A window that `segment_count` already keeps
  shorter is unchanged. `MuxerConfig` has public fields, so code that builds
  one with a struct literal needs `..MuxerConfig::default()`.
- `Muxer::held_window_bytes` (new) says what the live window and the open
  segment hold, and `Muxer::projected_window_bytes` (new) estimates what
  the window will hold once full, from the completed segments, for a
  server's admission decision (`None` until a segment completes).

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

- A stop on SIGTERM or ctrl-c, as at every `docker compose up` redeploy or
  Docker restart, told the app nothing: a file open at the stop never had
  its `recordSegmentComplete`, and a stage still live never had its
  `unpublish`, so v4v kept the stage live and refused its encoder's next
  connection. sapflow now ends every stream at once with no reconnect
  grace and closes its encoder listeners (HLS keeps serving until the
  exit); a publish request already under way is refused (RTMP
  `NetStream.Publish.Denied` "server stopping", WHIP 503), and an SRT
  caller still in its handshake gets no answer and times out. It then
  waits up to 7.5 s for its recordings to finish and its callbacks to be
  sent or fail, logs which happened, with counts, and exits; a second
  SIGTERM or ctrl-c exits at once. A live stage gets `unpublish` and a file marked `incomplete` with
  the new reason `server-stopped`; a stage held for a reconnect ends as at
  the end of its grace. The runtime then has half a second for blocking
  file work, so the exit fits Docker's default 10 s. A SIGKILL, or a file
  or callback not finished by the deadline, is still unreported; finding
  such files at startup is not built.

- A recording refused its first part, for want of a writer or byte budget,
  stopped before it had a file, so its codec epoch was served and then lost
  with no file, no `recordSegmentComplete` and no mark on either side. The
  next file is now marked `epoch-unrecorded`, as the v4v contract says. It
  happened when an encoder changed its codec configuration faster than a
  file was synced to disk: the file just ended is held open for its late
  audio, and with two writers a path the one before it, still syncing, left
  none for the new epoch. A path now has three writers, one more for the
  file still syncing, which makes this rarer; a disk too slow to finish a
  file before the next epoch ends still loses epochs, each now marked (with
  writes capped at 6 a second and codec changes 120 ms apart, 8 to 12 of 20
  were still lost). Real encoders change configuration far less often, and
  the roll budget limits how often a file rolls. A show's last epoch
  refused this way still has no later file to carry the mark.

- `SAPFLOW_WINDOW_SECONDS` bounds each stream's playlist by time as well as
  by `SAPFLOW_SEGMENT_COUNT` (`MuxerConfig::window`, above). It defaults to
  2 s per segment, 48 s at the default count, so an encoder sending a
  keyframe every 2 s or more often keeps every segment it kept before, and
  v4v's 1 s keyframes keep 24. A longer interval keeps fewer, longer
  segments: 8 s keyframes keep seven segments, 56 s, where they kept 192 s,
  and a stream's memory fell with it. 0 turns the bound off.
- A new stream is refused when the live fragment budget would be too full
  for it, with SRT `402`, RTMP `NetStream.Publish.Denied` "media memory
  limit" and WHIP 503. Once the budget was full, the next part any stream
  sealed was refused, which in testing dropped 30 of 32 publishers within
  10 s, and encoders that reconnected into it kept being dropped. A stream
  fills its window over tens of seconds, so admission counts what the
  streams hold, plus the growth expected of each stream until its window is
  full (`Muxer::projected_window_bytes`, from its completed segments; a
  stream held for a reconnect too, since its publisher may come back), plus
  an assumed window for each stream still starting and for the newcomer:
  the median projection, but at least a ninth of the mark, so that eight
  unmeasured streams and a newcomer fill it on their own. The mark is 80%
  (`stream::NEW_STREAM_FRAGMENT_PERCENT`). While eight live streams have no
  completed segment (`stream::MAX_STARTING_STREAMS`, each for at most 20 s,
  `stream::STARTING_FOR`), a newcomer waits, so a crowd arriving at once is
  let in as its windows can be measured. A publisher reconnecting to a
  stream still held for it is not refused for memory. A publisher whose bitrate rises after admission
  can still fill the budget. The refusal does not count against the
  failed-attempt throttle, but a retrying encoder asks
  `SAPFLOW_PUBLISH_AUTH_URL` again each time. sapflow-core adds
  `FragmentQuota::maximum`.

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
  packets that arrived as originals, not retransmissions, for a caller that
  flags its retransmissions as libsrt does: the path reordered them. Those
  whose NAK had gone out were resent for nothing.
- The SRT receiver now waits before its first NAK of a gap once it has
  seen the caller's path reorder packets, so a packet that is only late is
  no longer resent (see sapflow-srt below). `SAPFLOW_SRT_REORDER_WAIT_MAX_MS`
  (`--srt-reorder-wait-max-ms`) caps the wait: default 50, 0 to 1000, and 0
  sends every NAK at the first gap as before. The "SRT connection closed"
  log line also gives `nak_held`, the reordered packets whose NAK was still
  held when they came (so none was sent), and `max_reorder_wait_us`, the
  longest wait reached. Until a caller's path reorders, and for a caller
  that does not flag its retransmissions, nothing changes.
- WHIP logs at debug how each session's start was aligned, each keyframe
  request and arrival, and its receive counters once a second.
- A `test-feedback` feature, off in every other build, lets
  `SAPFLOW_TEST_WHIP_FEEDBACK` make WHIP sessions claim loss in their
  receiver reports or send REMB, for A3.10. Not for production use.

- A second RTMP listener for a TLS front that sends PROXY protocol v2,
  such as Caddy layer4 or HAProxy terminating RTMPS:
  `SAPFLOW_RTMP_PROXY_ADDR` (off by default) with
  `SAPFLOW_RTMP_TRUSTED_PROXY`, the front's address. Each connection on it
  must come from the front and start with the header, whose source is the
  publisher's address for the failed-attempt throttle and the app's publish
  check (`ip`). Anything else is closed unanswered, LOCAL (the front's own
  connection) once its header is in, and a refused peer's address is
  logged. Without it every RTMPS publisher had the front's address, so one
  encoder retrying a wrong key could throttle them all. The plain listener
  is unchanged and never reads a header, and now gives the throttle an
  IPv4-mapped peer as IPv4, as the proxied one does. Both share one cap on
  open connections (four per allowed publisher), so a flood on either can
  hold off the other until the 10 s setup deadline. Headers over 536 bytes
  are refused: send plain PROXY v2, without SSL, authority or unique-ID
  TLVs.

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

## sapflow-srt

- `receiver::Stats` gains `reordered`: packets reported missing that then
  arrived as originals, not retransmissions (counted correctly only when
  the caller sets the retransmission flag, as libsrt does). `Stats` is not
  `#[non_exhaustive]`, so code that builds one with a struct literal needs
  `..Default::default()`.
- Reorder tolerance in the receiver. It still sends a NAK at the first
  sequence gap, as libsrt does by default, until a missing packet arrives
  as an original rather than a retransmission. It then holds the first NAK
  of each new gap for 5/4 of the latest such packet's lateness plus 1 ms
  (the latest since the path last went a second without reordering), at most
  `Config::reorder_wait_max` (new, default 50 ms) and an eighth of the
  latency, and never into the last three round trips before the gap's
  delivery time; the wait halves after every second with no reordering,
  back to none. A held gap is reported from the timer when the wait ends,
  or at once if the loss list cannot keep it. The wait adapts only for a
  caller that set `REXMITFLG` in its handshake, since otherwise a
  retransmission that fills a gap looks like a late original. In the Linux
  lab with 0 to 20 ms of reordering jitter at 2000 ms latency,
  retransmissions fell from 66.5% of originals to 3.1% at 2% random loss
  and from 65.1% to 6.4% at 5% bursty loss, delivering everything as
  before. Without reordering nothing changed, and at 200 ms latency, where
  on that 60 ms path a gap almost never had room to be held, overhead and
  delivery were as before within the run-to-run spread
  (`docs/benchmarks.md`).
- `receiver::Settings` gains `retransmit_flag` and `reorder_wait_max`, and
  `listener::Config` gains `reorder_wait_max`; neither struct is
  `#[non_exhaustive]`, so a struct literal needs the new fields (or, for
  `Config`, `..Config::default()`).
- `receiver::Stats` gains `nak_held` (reordered packets whose first NAK was
  still held when they came) and `max_reorder_wait` (microseconds).
  `reordered` still counts every gap filled by an original, whether or not
  its NAK had gone out. As above, a struct literal needs
  `..Default::default()`.
- `loss::LossList` gains `hold`, which lists a loss with its first report
  held, and `take`, which says when a loss was found and whether it had
  been asked for, and bounds the hold of the part of a held loss before
  the packet that arrived by that packet's delivery time. `loss::Loss` gains
  `found`, and its `requests` is 0 while the first report is held. A held
  loss the list has to give up to stay within `MAX_RANGES` is reported at
  the next `due`, ahead of the rest; until then `next_due` returns `now`,
  and `is_empty` is false even when no range is listed.
- Keys the application holds, one passphrase per stream. With
  `Config::app_keys` (new, default off), a caller's key material waits for
  the application even when the listener has no `crypto` secret; off, such a
  caller is still refused in the handshake with `UNSECURE`, and nothing else
  changes. `Listener::check_key` (new) unwraps a waiting caller's key
  material with the application's passphrase without answering it, so it
  can be called again and the caller can still be refused for another
  reason. It takes AES-128 and AES-256, whichever the caller chose, and
  returns the key length or a `KeyCheckError` (new: `NotPending`,
  `NoKeyMaterial`, `BadSecret`, `UnsupportedKey`, `InvalidConfig`), whose
  `reject_code()` says how to refuse. `Listener::accept_with_key` (new)
  accepts only a matching key and never falls back to plaintext: a caller
  with no key material is refused with `UNSECURE` even with
  `allow_plaintext`, a wrong key with `BADSECRET`. `accept` and
  `accept_with_crypto` are unchanged. `listener::Config` is not
  `#[non_exhaustive]`, so a struct literal needs `..Config::default()`.
  `examples/listen-app-key.rs` is a complete listener, and the new fuzz
  target `own_srt_app_keys` drives the check and both accepts.
