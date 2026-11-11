#!/usr/bin/env python3
"""Capture the W1b media fixtures from FFmpeg, and check the Opus silence
frames by decoding them with FFmpeg. FFmpeg is only run, as a black box.

FFmpeg publishes generated sources only (lavfi testsrc2 and sine; no camera
or microphone) as plain RTP to UDP sockets on 127.0.0.1 that this script
owns, on ports from 47000 to 47199 (see capture_ffmpeg.py). Writes to
OUT_DIR, each .rec a sequence of [channel (0 RTP), u16 big-endian length,
datagram] records:

  ffmpeg-h264-fu.rec    H.264 (libx264, slices=2, which its sliced threads
                        raise to three slices per frame) in 400-byte
                        packets, so most frames need FU-A; sequence numbers
                        start at 65450 and cross the wrap
  ffmpeg-h264-fu.h264   the same encode, written as Annex B through FFmpeg's
                        tee muxer, as an independent reference
  ffmpeg-opus-stereo.rec  Opus stereo, 20 ms packets, from 65500
  silence-check.txt     the decode check below

The silence check sends RTP to FFmpeg (an SDP file, then RTCP BYE to end
the stream) and decodes it to 16-bit PCM with both of FFmpeg's Opus
decoders (its own and libopus). For each silence packet shape sapflow
fills with, mono and stereo, a stream of 50 silence packets, 25 captured
tone packets, 50 silence packets and 25 tone packets must give:

  - exact zeros for the first 50 (silence from a fresh decoder);
  - exact zeros in the second silence run after its first 20 ms (the
    decoder carries state across frames, so the tone's tail may ring
    into the first silent frame; the peak there is reported);
  - non-zero samples in both tone runs (the decoder recovers).

Usage: python3 capture_media.py OUT_DIR [--ffmpeg PATH] [--check-only]
Raw captures are scratch: keep only the small fixtures.
"""
import argparse, pathlib, select, socket, struct, subprocess, sys, time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from capture_ffmpeg import bind_pair  # noqa: E402

SILENT_FRAME = bytes([0xFF, 0xFE])
CELT_CONFIG = {120: 28, 240: 29, 480: 30, 960: 31}
# Every shape the depacketiser may fill with: (frame samples, frames).
SHAPES = [(120, 1), (240, 1), (480, 1), (960, 1), (960, 2), (960, 3), (960, 6),
          (480, 3)]


def silence(frame_samples, frames, stereo):
    """The same bytes as sapflow_rtp::opus::silence."""
    toc = CELT_CONFIG[frame_samples] << 3 | (4 if stereo else 0)
    if frames == 1:
        head = bytes([toc])
    elif frames == 2:
        head = bytes([toc | 1])
    else:
        head = bytes([toc | 3, frames])
    return head + SILENT_FRAME * frames


def run_capture(ffmpeg, args, seconds, out_path):
    port, socks = bind_pair()
    url = f'rtp://127.0.0.1:{port}'
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-re',
           *[a.replace('{url}', url) for a in args]]
    child = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    records = []
    try:
        deadline = time.monotonic() + seconds + 10
        while time.monotonic() < deadline:
            ready, _, _ = select.select(socks, [], [], 0.2)
            for s in ready:
                data, _ = s.recvfrom(65535)
                if s is socks[0]:
                    records.append(data)
            if not ready and child.poll() is not None:
                break
        child.wait(timeout=10)
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=5)
        for s in socks:
            s.close()
    err = child.stderr.read().decode(errors='replace') if child.stderr else ''
    if child.returncode != 0:
        raise SystemExit(f'{out_path.name}: ffmpeg exited {child.returncode}: {err.strip()}')
    with out_path.open('wb') as f:
        for data in records:
            f.write(b'\x00' + len(data).to_bytes(2, 'big') + data)
    print(f'{out_path.name}: {len(records)} RTP packets, {out_path.stat().st_size} bytes')
    return records


def read_rec(path):
    data, out = path.read_bytes(), []
    while data:
        n = int.from_bytes(data[1:3], 'big')
        out.append(data[3:3 + n])
        data = data[3 + n:]
    return out


def rtp(seq, ts, payload, pt=111, ssrc=0x5A5A0001):
    return struct.pack('!BBHII', 0x80, pt, seq & 0xFFFF, ts & 0xFFFFFFFF, ssrc) + payload


def decode(ffmpeg, decoder, payloads):
    """Sends Opus payloads as RTP to FFmpeg and returns the decoded PCM as
    a list of 16-bit samples, interleaved stereo."""
    port, socks = bind_pair()
    for s in socks:
        s.close()
    scratch = pathlib.Path(OUT_DIR)
    sdp = scratch / 'check.sdp'
    raw = scratch / 'check.raw'
    sdp.write_text('v=0\no=- 0 0 IN IP4 127.0.0.1\ns=check\nc=IN IP4 127.0.0.1\nt=0 0\n'
                   f'm=audio {port} RTP/AVP 111\na=rtpmap:111 opus/48000/2\n'
                   'a=fmtp:111 sprop-stereo=1\n')
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
           '-protocol_whitelist', 'file,udp,rtp', '-c:a', decoder, '-i', str(sdp),
           '-f', 's16le', '-ac', '2', '-ar', '48000', str(raw)]
    child = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        time.sleep(1.0)
        out = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        ts = 0
        for n, p in enumerate(payloads):
            out.sendto(rtp(n, ts, p), ('127.0.0.1', port))
            ts += duration(p)
            time.sleep(0.002)
        time.sleep(0.3)
        bye = struct.pack('!BBHI', 0x81, 203, 1, 0x5A5A0001)
        out.sendto(bye, ('127.0.0.1', port + 1))
        out.close()
        child.wait(timeout=15)
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=5)
    err = child.stderr.read().decode(errors='replace') if child.stderr else ''
    if child.returncode != 0:
        raise SystemExit(f'decode with {decoder}: exit {child.returncode}: {err.strip()}')
    # check.sdp and check.raw stay in OUT_DIR, which is scratch.
    pcm = raw.read_bytes()
    return list(struct.unpack(f'<{len(pcm) // 2}h', pcm))


def duration(packet):
    """Total samples of an Opus packet, from its TOC (RFC 6716 3.1)."""
    toc = packet[0]
    config, code = toc >> 3, toc & 3
    if config < 12:
        fs = [480, 960, 1920, 2880][config % 4]
    elif config < 16:
        fs = [480, 960][config % 2]
    else:
        fs = [120, 240, 480, 960][config % 4]
    frames = 1 if code == 0 else 2 if code < 3 else packet[1] & 0x3F
    return fs * frames


def check_silence(ffmpeg, tone, out_dir):
    lines, ok = [], True
    for decoder in ('opus', 'libopus'):
        for fs, frames in SHAPES:
            for stereo in (False, True):
                s = silence(fs, frames, stereo)
                runs = [[s] * 50, tone[:25], [s] * 50, tone[25:50]]
                payloads = [p for run in runs for p in run]
                pcm = decode(ffmpeg, decoder, payloads)
                bounds, at = [], 0
                for run in runs:
                    n = sum(duration(p) for p in run) * 2
                    bounds.append((at, at + n))
                    at += n
                if len(pcm) != at:
                    ok = False
                    lines.append(f'{decoder} {fs}x{frames} stereo={stereo}: '
                                 f'{len(pcm)} samples, expected {at}')
                    continue
                first = pcm[bounds[0][0]:bounds[0][1]]
                second = pcm[bounds[2][0]:bounds[2][1]]
                settle = 960 * 2
                ring = max((abs(x) for x in second[:settle]), default=0)
                tail = max((abs(x) for x in second[settle:]), default=0)
                tone1 = max(abs(x) for x in pcm[bounds[1][0]:bounds[1][1]])
                tone2 = max(abs(x) for x in pcm[bounds[3][0]:bounds[3][1]])
                good = max(map(abs, first)) == 0 and tail == 0 and tone1 > 0 and tone2 > 0
                ok &= good
                lines.append(f'{decoder:8} {fs:4} samples x {frames} '
                             f'{"stereo" if stereo else "mono  "} '
                             f'{s.hex():28} fresh peak {max(map(abs, first))}, '
                             f'after tone: first 20 ms peak {ring}, then {tail}; '
                             f'tone peaks {tone1} {tone2}: {"ok" if good else "FAIL"}')
    version = subprocess.run([ffmpeg, '-version'], capture_output=True, text=True).stdout.splitlines()[0]
    report = '\n'.join([version, *lines, 'PASS' if ok else 'FAIL']) + '\n'
    (out_dir / 'silence-check.txt').write_text(report)
    print(report)
    return ok


OUT_DIR = '.'


def main():
    global OUT_DIR
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('out_dir', type=pathlib.Path)
    parser.add_argument('--ffmpeg', default='ffmpeg')
    parser.add_argument('--check-only', action='store_true')
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    OUT_DIR = str(args.out_dir)
    opus_path = args.out_dir / 'ffmpeg-opus-stereo.rec'
    if not args.check_only:
        annexb = args.out_dir / 'ffmpeg-h264-fu.h264'
        run_capture(args.ffmpeg, [
            '-f', 'lavfi', '-i', 'testsrc2=size=240x180:rate=15', '-t', '2',
            '-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
            '-profile:v', 'baseline', '-g', '15', '-pix_fmt', 'yuv420p', '-b:v', '200k',
            '-x264-params', 'slices=2', '-map', '0:v', '-f', 'tee',
            f'[f=rtp:seq=65450]{{url}}?pkt_size=400|[f=h264]{annexb}',
        ], 2, args.out_dir / 'ffmpeg-h264-fu.rec')
        print(f'{annexb.name}: {annexb.stat().st_size} bytes')
        run_capture(args.ffmpeg, [
            '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
            '-f', 'lavfi', '-i', 'sine=frequency=660:sample_rate=48000',
            '-filter_complex', '[0:a][1:a]join=inputs=2:channel_layout=stereo[a]',
            '-map', '[a]', '-t', '2', '-c:a', 'libopus', '-b:a', '64k',
            '-f', 'rtp', '-seq', '65500', '{url}',
        ], 2, opus_path)
    tone = [p[12:] for p in read_rec(opus_path)]
    if not check_silence(args.ffmpeg, tone, args.out_dir):
        raise SystemExit('silence check failed')


if __name__ == '__main__':
    main()
