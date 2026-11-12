#!/usr/bin/env python3
"""The publishers for scripts/ingest-capacity.py, run in their own container
on the server's Docker network, so that their processes are outside the
server's cgroup. Encodes each profile's clip once (testsrc2 and a tone, x264
CBR with filler, AAC), then starts SRT (through srt-live-transmit, 2000 ms
latency and AES-256, as v4v asks for) and RTMP publishers `--step` seconds
apart, optionally restarting each 2 s after it ends as an encoder's
auto-reconnect does, records when each stream lists its first segment and
when any publisher exits, and prints one JSON summary line."""
import argparse
import json
import os
import signal
import re
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

PROFILES = {
    # v4v's encoder settings: 720p30, 2.5 Mbit/s CBR, 1 s keyframes.
    "v4v720": dict(size="1280x720", rate=30, vb="2500k", gop=30, ab="128k"),
    "hd1080": dict(size="1920x1080", rate=30, vb="6000k", gop=60, ab="160k"),
    # A misconfigured encoder: high bitrate and OBS-like long keyframe interval.
    "heavy": dict(size="1920x1080", rate=30, vb="20000k", gop=240, ab="160k"),
}
CLIP = 600


def clip(profile, d: Path):
    p = PROFILES[profile]
    ts, flv = d / f"{profile}.ts", d / f"{profile}.flv"
    if not ts.exists():
        tmp = d / f"{profile}.tmp.ts"
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
             "-f", "lavfi", "-i", f"testsrc2=size={p['size']}:rate={p['rate']}",
             "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
             "-t", str(CLIP), "-map", "0:v", "-map", "1:a",
             "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
             "-profile:v", "high", "-pix_fmt", "yuv420p",
             "-g", str(p["gop"]), "-keyint_min", str(p["gop"]), "-sc_threshold", "0",
             "-b:v", p["vb"], "-maxrate", p["vb"], "-bufsize", p["vb"],
             "-x264-params", "nal-hrd=cbr:force-cfr=1",
             "-c:a", "aac", "-b:a", p["ab"], "-ac", "2", "-f", "mpegts", str(tmp)],
            check=True)
        tmp.rename(ts)
    if not flv.exists():
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                        "-i", str(ts), "-c", "copy", "-f", "flv", str(flv)], check=True)
    return ts, flv


def start(protocol, name, ts, flv, a, log):
    cmd = command(protocol, name, ts, flv, a)
    if a.reconnect:
        # Like an encoder's auto-reconnect: start again 2 s after any end.
        cmd = f"while :; do {cmd}; echo reconnect >&2; sleep 2; done"
    return subprocess.Popen(cmd, shell=True, stdout=log, stderr=subprocess.STDOUT,
                            start_new_session=True)


def command(protocol, name, ts, flv, a):
    feed = "ffmpeg -hide_banner -loglevel error -nostdin -re -stream_loop -1"
    if protocol == "srt":
        q = f"latency={a.srt_latency}&streamid=publish:live/{name}"
        if a.passphrase:
            q += f"&passphrase={a.passphrase}&pbkeylen={a.pbkeylen}"
        return (f"{feed} -i {ts} -c copy -f mpegts - | dd ibs=188 obs=1316 2>/dev/null | "
                f"srt-live-transmit -a:no file://con 'srt://{a.host}:{a.srt_port}?{q}'")
    return f"{feed} -i {flv} -c copy -f flv rtmp://{a.host}:{a.rtmp_port}/live/{name}"


def fetch(a, name):
    url = f"http://{a.host}:{a.http_port}/live/{name}/stream.m3u8"
    try:
        with urllib.request.urlopen(url, timeout=2) as r:
            return r.read().decode()
    except Exception:
        return ""


def seq(text):
    m = re.search(r"#EXT-X-MEDIA-SEQUENCE:(\d+)", text)
    return int(m.group(1)) if m else -1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default="v4v720", choices=sorted(PROFILES))
    ap.add_argument("--srt", type=int, default=0)
    ap.add_argument("--rtmp", type=int, default=0)
    ap.add_argument("--step", type=float, default=5.0)
    ap.add_argument("--hold", type=float, default=180.0)
    ap.add_argument("--host", default="cap-srv")
    ap.add_argument("--srt-port", type=int, default=8890)
    ap.add_argument("--rtmp-port", type=int, default=1935)
    ap.add_argument("--http-port", type=int, default=8888)
    ap.add_argument("--srt-latency", type=int, default=2000)
    ap.add_argument("--passphrase", default="")
    ap.add_argument("--pbkeylen", type=int, default=32)
    ap.add_argument("--reconnect", action="store_true")
    ap.add_argument("--heavy-rtmp", type=int, default=0,
                    help="first start this many RTMP publishers of the heavy profile")
    ap.add_argument("--clips", default="/clips")
    ap.add_argument("--out", default="/out")
    a = ap.parse_args()
    out = Path(a.out)
    ts, flv = clip(a.profile, Path(a.clips))
    heavy = clip("heavy", Path(a.clips)) if a.heavy_rtmp else None
    probe = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=bit_rate,duration",
                            "-of", "json", str(ts)], capture_output=True, text=True).stdout
    for _ in range(100):
        if _server_up(a):
            break
        time.sleep(0.2)
    order = ["heavy"] * a.heavy_rtmp
    for i in range(max(a.srt, a.rtmp)):
        if i < a.srt:
            order.append("srt")
        if i < a.rtmp:
            order.append("rtmp")
    t0 = time.monotonic()
    events = open(out / "events.jsonl", "w")
    log = open(out / "publishers.log", "wb")

    def ev(**k):
        k["t"] = round(time.monotonic() - t0, 2)
        events.write(json.dumps(k) + "\n")
        events.flush()

    pubs = []
    next_start = 0.0
    end_at = a.step * len(order) + a.hold
    while True:
        now = time.monotonic() - t0
        while len(pubs) < len(order) and now >= next_start:
            proto = order[len(pubs)]
            name = f"event-cap-{proto}-{len(pubs)}"
            pts, pflv = heavy if proto == "heavy" else (ts, flv)
            wire = "rtmp" if proto == "heavy" else proto
            pubs.append(dict(proto=proto, name=name, started=now, ready=None, exited=None,
                             proc=start(wire, name, pts, pflv, a, log)))
            ev(e="start", name=name, proto=proto)
            next_start += a.step
        for p in pubs:
            if p["ready"] is None and p["exited"] is None and "#EXTINF:" in fetch(a, p["name"]):
                p["ready"] = round(now - p["started"], 2)
                ev(e="ready", name=p["name"], after=p["ready"])
            if p["exited"] is None and p["proc"].poll() is not None:
                p["exited"] = round(now, 2)
                ev(e="exit", name=p["name"], code=p["proc"].returncode)
        if now >= end_at:
            break
        time.sleep(0.5)
    mid = {p["name"]: seq(fetch(a, p["name"])) for p in pubs}
    time.sleep(10)
    lists = {p["name"]: fetch(a, p["name"]) for p in pubs}
    end = {n: seq(t) for n, t in lists.items()}
    extinf = {n: t.count("#EXTINF:") for n, t in lists.items() if t}
    for p in pubs:
        if p["proc"].poll() is None:
            try:
                os.killpg(p["proc"].pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    summary = dict(
        profile=a.profile, srt=a.srt, rtmp=a.rtmp, step=a.step, hold=a.hold, clip=json.loads(probe or "{}"),
        started=len(pubs),
        ready=sum(1 for p in pubs if p["ready"] is not None),
        exited_early=[(p["name"], p["exited"]) for p in pubs if p["exited"] is not None],
        advancing=sum(1 for n in end if end[n] > mid[n] >= 0),
        ready_s=sorted(p["ready"] for p in pubs if p["ready"] is not None),
        extinf_counts=sorted(set(extinf.values())),
    )
    print(json.dumps(summary), flush=True)


def _server_up(a):
    try:
        urllib.request.urlopen(f"http://{a.host}:{a.http_port}/", timeout=1)
        return True
    except urllib.error.HTTPError:
        return True
    except Exception:
        return False


if __name__ == "__main__":
    main()
