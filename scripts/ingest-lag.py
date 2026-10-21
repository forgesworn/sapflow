#!/usr/bin/env python3
"""How long after the encoder sends it does each LL-HLS part become available?

Publishes ffmpeg's test signal in real time (-re) to a running sapflow, polls
the media playlist, and for each new part compares the moment it appeared
with the moment ffmpeg sent its last frame. Includes the SRT latency;
excludes the player. ffmpeg's start-up and its initial read burst shift
every sample by the same small amount, so compare runs, not single numbers.

  python3 -I scripts/ingest-lag.py srt 200          # SRT, 200 ms latency
  python3 -I scripts/ingest-lag.py srt 200 --buffered   # ffmpeg's TS defaults
  python3 -I scripts/ingest-lag.py rtmp

Needs ffmpeg and, for SRT, srt-live-transmit (libsrt). Credentials come from
SAPFLOW_PUBLISH_USER and SAPFLOW_PUBLISH_PASS when the server requires them.
"""

import argparse
import os
import re
import struct
import subprocess
import time
import urllib.request


def boxes(b, want):
    i = 0
    while i + 8 <= len(b):
        size, typ = struct.unpack(">I4s", b[i : i + 8])
        if size < 8:
            return
        if typ == want:
            yield b[i + 8 : i + size]
        if typ in (b"moov", b"trak", b"mdia", b"moof", b"traf"):
            yield from boxes(b[i + 8 : i + size], want)
        i += size


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("proto", choices=["srt", "rtmp"])
    ap.add_argument("latency_ms", type=int, nargs="?", default=200)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--srt-port", type=int, default=8890)
    ap.add_argument("--rtmp-port", type=int, default=1935)
    ap.add_argument("--http-port", type=int, default=8888)
    ap.add_argument("--path", default="event-lag")
    ap.add_argument("--seconds", type=int, default=40)
    ap.add_argument(
        "--buffered",
        action="store_true",
        help="keep ffmpeg's MPEG-TS muxing delay and PES packing",
    )
    a = ap.parse_args()

    user = os.environ.get("SAPFLOW_PUBLISH_USER", "")
    password = os.environ.get("SAPFLOW_PUBLISH_PASS", "")
    base = f"http://{a.host}:{a.http_port}/live/{a.path}/"

    def get(u):
        with urllib.request.urlopen(base + u, timeout=5) as r:
            return r.read()

    enc = [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-re",
        "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30",
        "-f", "lavfi", "-i", "sine",
        "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
        "-g", "60", "-b:v", "2M", "-c:a", "aac", "-t", str(a.seconds),
    ]  # fmt: skip
    if a.proto == "rtmp":
        q = f"?user={user}&pass={password}" if user else ""
        enc += ["-f", "flv", f"rtmp://{a.host}:{a.rtmp_port}/live/{a.path}{q}"]
        t0 = time.time()
        ff = subprocess.Popen(enc)
        sender = None
    else:
        if not a.buffered:
            enc += ["-muxdelay", "0", "-muxpreload", "0",
                    "-pes_payload_size", "0", "-flush_packets", "1"]  # fmt: skip
        enc += ["-f", "mpegts", "-"]
        sid = f"publish:live/{a.path}" + (f":{user}:{password}" if user else "")
        url = f"srt://{a.host}:{a.srt_port}?streamid={sid}&latency={a.latency_ms}"
        t0 = time.time()
        ff = subprocess.Popen(enc, stdout=subprocess.PIPE)
        sender = subprocess.Popen(["srt-live-transmit", "-q", "file://con", url], stdin=ff.stdout)

    seen, timescale, lags = set(), None, []
    while ff.poll() is None:
        try:
            playlist = get("stream.m3u8").decode()
        except Exception:
            time.sleep(0.05)
            continue
        now = time.time()
        if timescale is None:
            m = re.search(r'URI="([^"]+_init\.mp4)"', playlist)
            if m:
                md = next(boxes(get(m.group(1)), b"mdhd"))
                off = 12 if md[0] == 0 else 20
                timescale = struct.unpack(">I", md[off : off + 4])[0]
        for dur, uri in re.findall(r'#EXT-X-PART:DURATION=([\d.]+),URI="([^"]+)"', playlist):
            if uri in seen or timescale is None:
                continue
            seen.add(uri)
            tf = next(boxes(get(uri), b"tfdt"))
            if tf[0] == 1:
                start = struct.unpack(">Q", tf[4:12])[0]
            else:
                start = struct.unpack(">I", tf[4:8])[0]
            lags.append(now - (t0 + start / timescale + float(dur)))
        time.sleep(0.02)

    if sender:
        # srt-live-transmit reconnects for ever once its input ends.
        sender.terminate()
        sender.wait()
    lags = sorted(lags[8:])  # skip the start-up
    if not lags:
        raise SystemExit("no parts seen")
    print(
        f"{a.proto} latency={a.latency_ms}ms buffered={a.buffered} parts={len(lags)} "
        f"median={lags[len(lags) // 2]:.2f}s p90={lags[int(len(lags) * 0.9)]:.2f}s"
    )


if __name__ == "__main__":
    main()
