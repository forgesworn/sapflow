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
timestamps are random on each run. Delete the rest of the scratch directory
afterwards.

The fixture covers the CM profile only: FFmpeg has no AES-GCM SRTP, so the
GCM profile (including its 96-bit salt handling) is first checked against a
real peer in W2. Fixtures and fuzz seeds from the W0 browser captures, and
the A1.3 fuzz coverage report, are still pending (see `fuzz/README.md`).
