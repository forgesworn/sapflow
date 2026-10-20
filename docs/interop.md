# Encoder interoperability

Which real encoders have published to sapflow, and how. Add a row when one
is tried.

| Date | Encoder | Protocol | Settings | Result |
|------|---------|----------|----------|--------|
| 2026-10-20 | ffmpeg 8.0.1 | RTMP, SRT | `scripts/smoke.sh`, v4v's ingest gate | Pass |
| 2026-10-20 | `srt-live-transmit` (libsrt 1.5.4) | SRT | `scripts/smoke.sh` | Pass |
| 2026-10-20 | OBS 32.1.2 | RTMP | x264 2500 kbit/s CBR, 1 s keyframes, no B-frames; credentials in the stream key | Servable in 1.1 s, ran 3 min 20 s, clean unpublish |
| 2026-10-20 | OBS 32.1.2 (libsrt 1.5.2) | SRT | as above, `latency=2000000` as v4v's Studio sets | Servable in 3.2 s |
| 2026-10-20 | OBS 32.1.2 | RTMP | OBS's default x264 (250-frame keyframes, B-frames) | Servable in 9.3 s; one 8.33 s GOP per segment, target duration 8; decodes cleanly |
| 2026-10-20 | Larix Broadcaster (iPhone) | RTMP | Larix defaults (H.264 Baseline 720p30, AAC mono, 2 s keyframes); credentials in the URL | Servable in 1.0 s; 2 s segments decode cleanly, A/V start within 7 ms |
| 2026-10-20 | Larix Broadcaster (iPhone) | SRT | Larix defaults, credentials in the `streamid`, 2 s latency | Servable in 4.5 s; 24 segments decode cleanly; 4 late packets dropped once over Wi-Fi (one segment 59 frames of 60) |

Output checked for the 1 s keyframe runs: H.264 High 1280x720 30 fps and
AAC-LC 48 kHz stereo, 1 s segments, 200 ms parts, a keyframe at the start of
each segment.

With OBS's defaults the stream plays, but segments follow the keyframes, so
viewers join up to about 8 s behind. v4v's advice to set 1 s keyframes
matters.

## Not yet tried

- Larix on Android.
- A network drop (Wi-Fi off), the phone locked or backgrounded, SRT with a
  passphrase, bursty loss.

## Stopping and restarting

Tried with Larix over RTMP: stopping the broadcast ends the stream at once
(the playlist is 404); restarting gives a new stream about 1 to 2 s after
the encoder reconnects (media sequence 0, new chunk names, seven gap
segments). sapflow holds nothing for a publisher that leaves; a silent one
is dropped after 10 s. Holding the stage across a drop is v4v's side (its
ingest gate covers a dropped encoder held and resumed). How hls.js and
Safari recover from the 404 and the new playlist is not yet checked.
- OBS dropping and reconnecting; SRT under packet loss; OBS's Apple
  hardware encoder; vMix.

## SRT under packet loss (2026-10-20)

libsrt's `srt-live-transmit` sent a 60 s clip in real time through a UDP
proxy that dropped packets at random in both directions (handshakes
exempt) and added 60 ms round trip; the same sender fed sapflow and
libsrt's own receiver, one run per condition, on loopback.

| Loss | Sender traffic, ideal | libsrt receiver | sapflow receiver |
|------|-----------------------|-----------------|------------------|
| 3% | 1.03x | 1.03x | 1.13x |
| 10% | 1.11x | 1.10x | 1.73x |
| 20% | 1.25x | 1.23x | 2.34x |

- With 2 s latency sapflow delivered the same media at every loss rate,
  with no decode errors: recovery is correct.
- Before the fix it asked for far more retransmission than libsrt, and at
  200 ms latency and 10% loss lost 87 video packets that libsrt's receiver
  recovered.
- Cause: `srt-protocol`'s handshake did not advertise `TLPKTDROP` and
  `NAKREPORT`, which its receiver implements, so libsrt senders also
  retransmitted on their own timer. Advertising them alone made delivery
  worse (the receiver backed off its re-requests: 2, 3, 4... RTTs); with
  re-requests every RTT as well, both are patched in `vendor/srt-protocol`:

| Receiver | 10% loss, 2 s | 20% loss, 2 s | 10% loss, 200 ms |
|----------|---------------|---------------|------------------|
| sapflow before | 1.69x, no decode errors | 2.34x, none | 1.59x, 139 decode messages |
| sapflow patched | 1.09x, none | 1.21x, none | 1.08x, 83 |
| libsrt | 1.10x | 1.23x | 1.11x, 18 |

- One run per condition, one seed: the 200 ms column is a direction, not a
  measurement. At tight latency libsrt still recovers more; re-requesting
  more often as a packet's deadline nears is the next thing to try.
