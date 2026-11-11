#!/usr/bin/env python3
"""Capture RTP, RTCP, SRTP and SRTCP from FFmpeg, run as a black box.

FFmpeg publishes generated sources only (lavfi testsrc2 and sine; no camera
or microphone) to UDP sockets on 127.0.0.1 that this script owns. The RTP
port is even and RTCP arrives on the next port up, as FFmpeg's RTP muxer
sends it. Ports come from 47000 to 47199.

Writes one file per capture to OUT_DIR, each a sequence of records
[channel (0 RTP, 1 RTCP), length (u16, big-endian), datagram]:

  h264.rec        H.264, plain RTP and RTCP
  opus.rec        Opus, plain RTP and RTCP
  h264-srtp.rec   H.264 under SRTP_AES128_CM_HMAC_SHA1_80, starting at
                  sequence number 65520 so it crosses the wrap, with the
                  public test key below (FFmpeg's -srtp_out_params is
                  base64 of the 16-byte master key and 14-byte salt)

Usage: python3 capture_ffmpeg.py OUT_DIR [--ffmpeg PATH]

Leaves no child processes or sockets behind. Raw captures are scratch: keep
only the small fixtures and seeds made from them.
"""
import argparse, base64, pathlib, select, socket, subprocess, time

# Public test key and salt, used only for the committed interop fixture.
SRTP_KEY = bytes(range(0x10, 0x20))
SRTP_SALT = bytes(range(0x40, 0x4e))
PORTS = range(47000, 47200, 2)


def bind_pair():
    """An even RTP port and RTCP on the next one, both free."""
    for port in PORTS:
        socks = []
        try:
            for p in (port, port + 1):
                s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                s.bind(('127.0.0.1', p))
                socks.append(s)
            return port, socks
        except OSError:
            for s in socks:
                s.close()
    raise SystemExit('no free port pair in 47000-47199')


def capture(ffmpeg, name, inputs, codec, scheme, extra, out_dir, seconds, max_rtp):
    port, socks = bind_pair()
    url = f'{scheme}://127.0.0.1:{port}?pkt_size=1200'
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-re',
           *inputs, '-t', str(seconds), *codec, '-f', 'rtp', *extra, url]
    child = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    records, rtp = [], 0
    try:
        deadline = time.monotonic() + seconds + 10
        while time.monotonic() < deadline:
            ready, _, _ = select.select(socks, [], [], 0.2)
            for s in ready:
                data, _ = s.recvfrom(65535)
                channel = 0 if s is socks[0] else 1
                if channel == 0:
                    rtp += 1
                    if rtp > max_rtp:
                        continue
                records.append((channel, data))
            if not ready and child.poll() is not None:
                break
        child.wait(timeout=10)
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=2)
        for s in socks:
            s.close()
    err = child.stderr.read().decode(errors='replace') if child.stderr else ''
    if child.returncode != 0:
        raise SystemExit(f'{name}: ffmpeg exited {child.returncode}: {err.strip()}')
    path = out_dir / f'{name}.rec'
    with path.open('wb') as f:
        for channel, data in records:
            f.write(bytes([channel]) + len(data).to_bytes(2, 'big') + data)
    kinds = [c for c, _ in records]
    print(f'{name}: {kinds.count(0)} RTP, {kinds.count(1)} RTCP -> {path.name}')


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('out_dir', type=pathlib.Path)
    parser.add_argument('--ffmpeg', default='ffmpeg')
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    video = ['-f', 'lavfi', '-i', 'testsrc2=size=160x120:rate=15']
    h264 = ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
            '-profile:v', 'baseline', '-g', '15', '-pix_fmt', 'yuv420p', '-b:v', '150k']
    audio = ['-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000']
    opus = ['-c:a', 'libopus', '-b:a', '32k']
    params = base64.b64encode(SRTP_KEY + SRTP_SALT).decode()
    srtp = ['-srtp_out_suite', 'AES_CM_128_HMAC_SHA1_80', '-srtp_out_params', params]
    capture(args.ffmpeg, 'h264', video, h264, 'rtp', [], args.out_dir, 6, 400)
    capture(args.ffmpeg, 'opus', audio, opus, 'rtp', [], args.out_dir, 6, 400)
    # Start near the top of the sequence space so the fixture crosses the
    # wrap and ROC 1 is checked against FFmpeg too.
    capture(args.ffmpeg, 'h264-srtp', video, h264, 'srtp', srtp + ['-seq', '65520'],
            args.out_dir, 6, 30)


if __name__ == '__main__':
    main()
