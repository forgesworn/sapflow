# Native playback latency: finite observations and follow-up

On 2026-10-21 the owner measured approximately 10.5 seconds glass-to-glass.
The parent diagnostic observed native Safari returning to approximately six
seconds of buffered media after a manual seek. These are finite observations,
not sustained latency or general-release acceptance.

A read-only origin response contained HTTP/1.1, TARGETDURATION=2, 24 completed
one-second segments, PART-TARGET=0.234 and PART-HOLD-BACK=0.585. Only the last two
completed segments carried part tags and programme-date-time tags. The whole
playlist therefore exceeded six seconds; the two-parent limit applied to part
history, not the whole live window. A later request returned 404, so further
sampling stopped. No active origin or viewer was changed by this investigation.

## Later physical-run snapshot: source and deployed origin differ

A later read-only review of the physical Android RTMP run on 2026-10-21
observed playback through the HTTP/2 viewer proxy. The staging receipt for
that run identifies the origin binary as SHA256
`7ee772c3315fea4fe6e93cdf173f1717dd7c895a5e675215fe97b89088c905f2`.
The running origin still returned 24 completed segments with PART tags on
exactly the last two completed parents, five parts each, plus any published
open parts. TARGETDURATION remained 2, PART-TARGET 0.234 and PART-HOLD-BACK
0.585. The duration-based PART history described below was present in source
but was not deployed in this sampled run. Source tests and a newer local
binary do not qualify the older running origin.

The sampled origin log contained 20 `break_stream` warnings from
05:46:48 through 05:53:50 UTC. Each reported a 1.1–2.0-second video timestamp
gap with `keyframe=true`. This is consistent with the separately reported
Android publishing fanout overflow and wait-for-IDR recovery. The origin log
alone does not identify which queue limit caused those resets. These are
missing-media intervals, not merely a player choosing a larger buffer.

The muxer's one-second break threshold closes the available media, drops the
pending frame, rebases the next keyframe to the prior output endpoint, clears
pending audio and marks the new segment as a discontinuity. This preserves a
decodable recovery boundary but cannot restore discarded capture frames.
The readiness and PART-history changes below do not alter that behaviour.

A bounded request-log snapshot, filtered to its final 600 seconds, contained
5,610 HTTP/2 requests: 5,605 returned 200, four were client cancellations
(499), and one viewer-route request returned 404. All 2,791 playlist requests
returned 200, with response-time p95 311 ms and maximum 1.147 seconds.
Of 2,791 part requests, 2,788 returned 200 and three were client-cancelled;
part response-time p95 was 293 ms and maximum 1.150 seconds. Requests used
`_HLS_msn` and `_HLS_part`, demonstrating active low-latency part fetching.
The retained log has no client identity, so these aggregate counts cannot
independently separate Safari from another simultaneous viewer. Successful
requests do not prove uninterrupted decoding or absence of upstream loss.

The local evidence sources are the origin staging receipt and the viewer
request log under `target/mobile-origin-av/`, together with the staged
origin's server log. They remain local run evidence; private addresses,
ports and credentials are intentionally omitted here. No fixture was
restarted and no capture or server state was changed by this review.

## Interpretation and source evidence

[HLS draft 22, section 4.4.3.8](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#section-4.4.3.8)
defines the omitted regular HOLD-BACK as three target durations. Six seconds
therefore fits regular-latency fallback. [Appendix B.1](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#appendix-B.1)
requires HTTP/2 or HTTP/3 and gzip playlist delivery for its low-latency profile.
The origin speaks HTTP/1.1 only and has no compression layer
(`crates/sapflow-server/src/own_http.rs`; the sampled build used Hyper's
HTTP/1.1 builder). This supports the fallback hypothesis;
it does not establish Safari's internal decision or explain all 10.5 seconds.
The sampled 0.585-second part hold-back meets the HLS draft's mandatory
two-part-target minimum, but violates [Apple authoring specification
§14.3](https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices/),
which makes at least three Part Target Durations mandatory. Corrected source
now advertises 0.702 seconds for PART-TARGET=0.234. This source correction does
not change the live origin or retrospectively qualify the sampled runs. It is
not evidence of the cause of their latency or playback failures. The separate
Apple §9.2 requirement for video RESOLUTION remains open; this packet does not
change the multivariant playlist policy.

PDT on captured segment 606 was 01:15:56.125Z and on segment 607 was
01:15:57.428Z, a 1.303-second difference despite the first segment's 1.000-second
EXTINF. `LiveStream::apply` supplies server arrival time, and
`Muxer::rotate_segment` assigns it afresh to each segment. Consequently PDT
absorbs arrival jitter and cannot independently measure capture-to-viewer delay.
The initial segment also takes the following frame's arrival time because video
is held until its duration is known. Open parts have no fresh PDT, including
immediately after a discontinuity.

## Narrow changes in this packet

`crates/sapflow-hls/src/muxer.rs` now returns the current playlist for evicted
_HLS_msn requests. An MSN-only request waits for the complete parent; a part
request can still complete when that part appears. Future-request validation and
part-index rollover are preserved. These follow
[section 6.2.5.2](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#section-6.2.5.2).

`crates/sapflow-hls/src/playlist.rs` now keeps whole parent part sets whose end
lies within three target durations of the published edge, including the exact
boundary. Published open-part spans advance that edge. This conservatively
retains up to one extra parent's parts at the boundary and follows the
duration-based guidance in
[section 6.2.2](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#section-6.2.2).
The fixed-count resource buffer and existing PDT policy are unchanged.

Regressions cover evicted requests, incomplete versus complete parents,
part-index rollover, future requests, one- and three-second GOPs, exact history
boundaries, advancing open parts and continued default-window part availability
for three target durations after tag removal. Very short configured resource
windows still need separate duration-based qualification; expanding playlist
part history does not itself extend resource availability.

Validation: `cargo test --offline -p sapflow-hls -j 2` passed all 102 tests;
`cargo clippy --offline -p sapflow-hls --all-targets -j 2 -- -D warnings` passed.
Running servers, installed applications and previous package receipts remain
their original finite snapshots until explicitly rebuilt and retested.

## Proposed PDT change, not implemented here

Store one immutable media-time/wall-time anchor per discontinuity, preserving
that first access unit's arrival time while awaiting its duration. Derive later
PDTs from the media timeline rather than subsequent arrival times. Use the first
presentation timestamp consistently; B-frame decode order needs an explicit
regression. Without authenticated capture-clock metadata, label this as an
arrival-anchored programme clock, not capture-time measurement.

Emit the stored PDT before the open segment's first published part and keep
anchors on retained completed segments, especially discontinuities. Recovery
must start a new anchor without rewriting old tags or silently stretching the
media timeline. Test bursty arrival, wall-clock corrections, first-frame delay,
reconnect/rebase and seek mapping before changing recovery behaviour.

Next qualification should compare native Safari and hls.js on the same
HTTP/2/gzip endpoint, then measure capture, encoding, native queue residence,
socket submission and origin arrival separately. Changing the player is a
comparison, not evidence that native Safari or the origin contract is fixed.

## Post-fix acceptance gates

1. **Identify the deployed artefacts.** Record the installed application and
   running origin hashes for the new run. Verify the served playlist actually
   has the duration-based PART history: with TARGETDURATION=2, retain the
   qualifying six-second history, rather than exactly two parents. Verify
   HTTP/2 or HTTP/3 and gzip on the actual viewer route.
2. **Verify recovery requests against the deployed origin.** An evicted
   `_HLS_msn` must return the current playlist. An MSN-only request must wait
   for the parent segment to complete; a part request may complete when its
   part appears. Preserve future-request validation and part-index rollover.
   Check that removed PART resources remain fetchable for at least three
   target durations after their tags disappear, including after a
   discontinuity. Source tests alone do not establish these runtime results.
3. **Establish nominal upstream continuity.** After startup on the declared
   no-loss profile, require zero unexplained publishing overflows,
   wait-for-IDR resets or muxer breaks. Record overflow reason, queue byte
   and time occupancy, sample residence and capture/codec rejection counts.
   Attribute any missing frames to their stage; increasing buffering or
   suppressing discontinuities is not a continuity fix.
4. **Meet the existing physical playback targets.** Use the declared 720p30
   H.264/AAC profile and a real viewer for two hours per primary device, with
   no unexpected stop or stall. Require absolute A/V offset at most 100 ms
   at startup, after recovery and at the end. Across 20 starts, require
   startup p95 at most 8 seconds; require capture-to-screen p95 at most
   5 seconds on the reference profile. These are the existing targets in
   [the mobile plan](mobile-streaming-plan.md), not results of this review.
5. **Measure latency with a visible clock.** Preserve actual capture and
   viewer evidence, including across discontinuities, without manual
   catch-up seeks during the nominal observation. A low-latency player's
   distance from its estimated live edge is not glass-to-glass proof.
   Arrival-derived PDT, `getStartDate() + currentTime`, buffer depth and
   timeline rebasing cannot independently establish capture-to-screen delay.
6. **Repeat the frozen recovery matrix.** Exercise 3-, 10- and 30-second
   outages and ten Wi-Fi/cellular changes. Require remote decoded media
   within 10 seconds of usable-path restoration, within destination grace,
   with bounded queues, A/V synchronisation and foreground recording
   continuity. Check stop prevents retries or a resurrected event. A clean
   LAN run does not replace these impaired-path gates.
