# Test fixtures

`ffmpeg-srtp-h264.rec` is SRTP and SRTCP sent by FFmpeg 8.1.2, run as a
black box by `tests/capture_ffmpeg.py`:

- Source: FFmpeg's generated `testsrc2` pattern at 160x120 and 15 frames per
  second (no camera, microphone or recorded media), encoded with libx264
  (baseline, ultrafast, zerolatency, a keyframe every 15 frames).
- Output: `-f rtp -srtp_out_suite AES_CM_128_HMAC_SHA1_80` to `srtp://` on
  127.0.0.1, with `-srtp_out_params` set to base64 of the public test master
  key `10 11 ... 1f` and salt `40 41 ... 4d`. The key protects nothing else.
- Sequence numbers start at 65520 (`-seq 65520`), so the 30 packets cross
  the wrap and the later ones are under ROC 1.
- Contents: the first 30 SRTP packets and the two SRTCP sender reports, as
  records of `[channel (0 RTP, 1 RTCP), u16 big-endian length, datagram]`.

`ffmpeg_srtp_interop` in `src/srtp/tests.rs` unprotects every packet with
our code, reads it as RTP or RTCP, and checks that our protect gives back
FFmpeg's exact bytes.

To recapture, run `python3 crates/sapflow-rtp/tests/capture_ffmpeg.py
<scratch dir>` and copy `h264-srtp.rec` here. The SSRC, sequence numbers and
timestamps are random on each run. Move the rest of the scratch directory to
the Trash afterwards.

The fixture covers the CM profile only: FFmpeg has no AES-GCM SRTP, so the
GCM profile (including its 96-bit salt handling) is first checked against a
real peer in W2. Fixtures and fuzz seeds from the W0 browser captures, and
the A1.3 fuzz coverage report, are still pending (see `fuzz/README.md`).

## Media (A1.2)

Made by `tests/capture_media.py` with FFmpeg 8.1.2 as a black box, from
generated sources only, sent as plain RTP to 127.0.0.1 on ports 47000 to
47199. Records as above, RTP only.

- `ffmpeg-h264-fu.rec`: 200 packets, 2 s of `testsrc2` at 240x180 and 15
  frames per second, libx264 baseline (ultrafast, zerolatency, a keyframe
  every 15 frames, 200 kbit/s, `slices=2`, which libx264's sliced threads
  raised to three slices per frame) in packets of at most
  400 bytes, so most frames are FU-A. FFmpeg sends each IDR's SPS and PPS
  as a STAP-A. Sequence numbers start at 65450 and cross the wrap.
- `ffmpeg-h264-fu.h264`: the same encode written as Annex B through FFmpeg's
  `tee` muxer, the independent reference for the lossless run.
- `ffmpeg-opus-stereo.rec`: 101 packets, 2 s of a 440 Hz and 660 Hz stereo
  sine in libopus at 64 kbit/s, 20 ms packets, from sequence number 65500.
- `silence-check.txt`: the Opus silence check's output. For every silence
  shape the depacketiser fills with (2.5, 5, 10 and 20 ms frames, and 20 ms
  frames in twos, threes and sixes; mono and stereo), the script sent 50
  silence packets, 25 tone packets, 50 silence packets and 25 tone packets
  as RTP to FFmpeg and decoded them with FFmpeg's own Opus decoder and with
  libopus. Silence from a fresh decoder is exact zeros; after the tone, the
  first 20 ms carry the tone's tail (a peak of 4157 in 16-bit samples) and
  everything after is exact zeros; the tone decodes again afterwards.

The silence frame itself (`FF FE` after the TOC) is what libopus produced
for digital silence (`anullsrc`) in every CELT frame size, observed the same
way. The A1.2 tests in `src/receive/tests.rs` check that the fixtures still
contain FU-A, STAP-A and the wrap. To recapture, run `python3
crates/sapflow-rtp/tests/capture_media.py <scratch dir>`, copy the four
files here, regenerate the media fuzz seeds (`cargo test -p sapflow-rtp
--lib -- --ignored write_media_seeds`) and move the scratch directory to
the Trash. The SSRCs and timestamps are random on each run.
