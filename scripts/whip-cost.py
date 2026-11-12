#!/usr/bin/env python3
"""A4.2: sapflow's CPU and memory per publisher, WHIP beside SRT and RTMP.

The method of docs/benchmarks.md section B ("Ingest cost per stream"): N
concurrent publishers into one fresh server with no viewers, the v4v encoder
settings (1280x720, 30 fps, H.264 2.5 Mbit/s, 1 s keyframes) encoded once
into a 150 s clip and played with `ffmpeg -re -stream_loop -1 -c copy`, so the
publishers do not encode. SRT carries AAC (ffmpeg | dd | srt-live-transmit at
200 ms latency), RTMP the same clip remuxed to FLV, and WHIP the same video
with Opus at 128 kbit/s through FFmpeg's WHIP muxer, which WHIP requires.
Only the sapflow process is measured; the publishers run outside it.

A run waits for every media playlist to have a segment and a part, then 25 s,
then measures the server's CPU time over 45 s and its peak RSS from one
sample a second, then checks every playlist is still advancing. Each run
starts on a fresh server, after the one-minute load average is below 8; a
run whose wait for that (--gate-wait, 300 s) runs out goes ahead but is
recorded as not ok, and so stays out of the summary.

    scripts/whip-cost.py --bin target/release/sapflow --out DIR \
        [--streams 1,10,50] [--rounds 3] [--protocols srt,rtmp,whip] [--base 47300]
        [--gate-wait 300]

Writes DIR/runs.tsv (one row a run) and DIR/summary.tsv (low to high of the
rounds, per cell). Ports are base+0 (HTTP), +1 (SRT), +2 (RTMP), +3 (WHIP
signalling) and +4 (WHIP media). Every child is the driver's own and is
stopped by its PID.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

WINDOW = 45.0
SETTLE = 25.0
LOAD_GATE = 8.0
CLIP_SECONDS = 150


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def tools():
    for name in ("ffmpeg", "srt-live-transmit", "dd", "ps"):
        if not shutil.which(name):
            sys.exit(f"missing {name}")
    muxers = subprocess.run(["ffmpeg", "-hide_banner", "-muxers"], capture_output=True, text=True).stdout
    if not re.search(r"^\s*E\s+whip\b", muxers, re.M):
        sys.exit("this ffmpeg has no WHIP muxer (FFmpeg 8 or newer)")


def clips(out: Path):
    """The clip once: H.264 (constrained baseline, no B-frames, as WHIP
    needs), then AAC for SRT and RTMP and Opus for WHIP over the same
    video stream."""
    video = out / "video.mkv"
    if not video.exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
             "testsrc2=size=1280x720:rate=30", "-t", str(CLIP_SECONDS), "-pix_fmt", "yuv420p",
             "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-profile:v", "baseline",
             "-g", "30", "-keyint_min", "30", "-sc_threshold", "0", "-b:v", "2500k", "-maxrate", "2500k",
             "-bufsize", "2500k", str(video)])
    tone = ["-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={CLIP_SECONDS}"]
    if not (out / "src.ts").exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(video), *tone, "-map", "0:v",
             "-map", "1:a", "-c:v", "copy", "-c:a", "aac", "-b:a", "128k", "-ac", "2", "-f", "mpegts",
             str(out / "src.ts")])
    if not (out / "src.flv").exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(out / "src.ts"), "-c", "copy",
             "-f", "flv", str(out / "src.flv")])
    if not (out / "src-opus.mkv").exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(video), *tone, "-map", "0:v",
             "-map", "1:a", "-c:v", "copy", "-c:a", "libopus", "-b:a", "128k", "-ac", "2", "-ar", "48000",
             str(out / "src-opus.mkv")])


def ports(base):
    return {"http": base, "srt": base + 1, "rtmp": base + 2, "whip": base + 3, "media": base + 4}


def server(binary, p, log):
    env = {
        **os.environ,
        "SAPFLOW_OPEN_PUBLISH": "1",
        "SAPFLOW_HTTP_ADDR": f"127.0.0.1:{p['http']}",
        "SAPFLOW_SRT_ADDR": f"127.0.0.1:{p['srt']}",
        "SAPFLOW_RTMP_ADDR": f"127.0.0.1:{p['rtmp']}",
        "SAPFLOW_WHIP_ADDR": f"127.0.0.1:{p['whip']}",
        "SAPFLOW_WHIP_UDP_ADDR": f"127.0.0.1:{p['media']}",
        "SAPFLOW_WHIP_CANDIDATES": f"udp 127.0.0.1:{p['media']}",
        "SAPFLOW_WHIP_PUBLIC_URL": f"http://127.0.0.1:{p['whip']}",
        "SAPFLOW_MAX_PUBLISHERS": "100",
        "RUST_LOG": "warn",
    }
    return subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT)


def publisher(protocol, i, p, out, log):
    event = f"event-cost-{i}"
    feed = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1"]
    if protocol == "srt":
        cmd = (f"{' '.join(feed)} -i {out / 'src.ts'} -c copy -f mpegts - | dd ibs=188 obs=1456 2>/dev/null | "
               f"srt-live-transmit -a:no file://con 'srt://127.0.0.1:{p['srt']}?latency=200"
               f"&streamid=publish:live/{event}'")
        return subprocess.Popen(cmd, shell=True, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    if protocol == "rtmp":
        cmd = [*feed, "-i", str(out / "src.flv"), "-c", "copy", "-f", "flv",
               f"rtmp://127.0.0.1:{p['rtmp']}/live/{event}"]
    else:
        cmd = [*feed, "-i", str(out / "src-opus.mkv"), "-c", "copy", "-f", "whip", "-whip_flags",
               "dtls_active", "-authorization", "cost:bench", f"http://127.0.0.1:{p['whip']}/whip/live/{event}"]
    return subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)


def playlist(p, i):
    url = f"http://127.0.0.1:{p['http']}/live/event-cost-{i}/stream.m3u8"
    try:
        with urllib.request.urlopen(url, timeout=2) as r:
            return r.read().decode()
    except Exception:
        return ""


def media_sequence(text):
    m = re.search(r"#EXT-X-MEDIA-SEQUENCE:(\d+)", text)
    return int(m.group(1)) if m else -1


def playable(text):
    return "#EXTINF:" in text and "#EXT-X-PART:" in text


def sample(pid):
    out = subprocess.check_output(["ps", "-o", "time=,rss=", "-p", str(pid)], text=True,
                                  env={**os.environ, "LC_ALL": "C"}).split()
    clock, rss = out[0], int(out[1]) / 1024
    days, _, clock = clock.rpartition("-")
    secs = 0.0
    for part in clock.split(":"):
        secs = secs * 60 + float(part)
    return secs + (int(days) * 86400 if days else 0), rss


def cpu_of(pids):
    total = 0.0
    for pid in pids:
        try:
            total += sample(pid)[0]
        except Exception:
            pass
    return total


def descendants(pid):
    out = subprocess.run(["ps", "-A", "-o", "pid=,ppid="], capture_output=True, text=True).stdout
    kids, frontier = set(), {pid}
    pairs = [tuple(map(int, l.split())) for l in out.splitlines() if l.strip()]
    while frontier:
        nxt = {c for c, pp in pairs if pp in frontier and c not in kids}
        kids |= nxt
        frontier = nxt
    return kids


def stop(proc, group=False):
    if proc.poll() is not None:
        return
    try:
        if group:
            os.killpg(proc.pid, 15)
        else:
            proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        try:
            os.killpg(proc.pid, 9) if group else proc.kill()
        except Exception:
            pass
        proc.wait()


def wait_for_load(limit):
    waited = 0
    while os.getloadavg()[0] >= LOAD_GATE and waited < limit:
        time.sleep(5)
        waited += 5
    return waited


def one_run(args, protocol, n, round_no, out):
    p = ports(args.base)
    waited = wait_for_load(args.gate_wait)
    load = os.getloadavg()[0]
    tag = f"{protocol}-{n}-r{round_no}"
    logs = out / "logs"
    logs.mkdir(exist_ok=True)
    slog = open(logs / f"{tag}-server.log", "w")
    plog = open(logs / f"{tag}-publishers.log", "w")
    srv = server(args.bin, p, slog)
    pubs = []
    row = {"protocol": protocol, "streams": n, "round": round_no, "load": round(load, 2),
           "gate_wait_s": waited, "ok": False}
    try:
        time.sleep(1)
        if srv.poll() is not None:
            raise RuntimeError("server exited")
        spacing = 0.3 if protocol == "whip" else 0.1
        started = time.monotonic()
        for i in range(n):
            pubs.append(publisher(protocol, i, p, out, plog))
            time.sleep(spacing)
        # Every playlist with a segment and a part.
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if all(playable(playlist(p, i)) for i in range(n)):
                break
            dead = [i for i, q in enumerate(pubs) if q.poll() is not None]
            if dead:
                raise RuntimeError(f"publishers exited early: {dead}")
            time.sleep(0.5)
        else:
            raise RuntimeError("not every stream became playable")
        row["first_playable_s"] = round(time.monotonic() - started, 1)
        time.sleep(args.settle)
        seq0 = [media_sequence(playlist(p, i)) for i in range(n)]
        pub_pids = set()
        for q in pubs:
            pub_pids |= {q.pid} | descendants(q.pid)
        cpu0, _ = sample(srv.pid)
        pcpu0 = cpu_of(pub_pids)
        t0 = time.monotonic()
        peak = 0.0
        while True:
            now = time.monotonic()
            if now - t0 >= args.window:
                break
            peak = max(peak, sample(srv.pid)[1])
            time.sleep(max(0.0, 1.0 - (time.monotonic() - now)))
        cpu1, rss = sample(srv.pid)
        elapsed = time.monotonic() - t0
        peak = max(peak, rss)
        pcpu1 = cpu_of(pub_pids)
        seq1 = [media_sequence(playlist(p, i)) for i in range(n)]
        advanced = min(b - a for a, b in zip(seq0, seq1))
        alive = all(q.poll() is None for q in pubs)
        row.update({
            "window_s": round(elapsed, 1),
            "server_cpu_pct": round(100 * (cpu1 - cpu0) / elapsed, 2),
            "cpu_per_stream_pct": round(100 * (cpu1 - cpu0) / elapsed / n, 3),
            "peak_rss_mb": round(peak, 1),
            "rss_per_stream_mb": round(peak / n, 2),
            "publishers_cpu_pct": round(100 * (pcpu1 - pcpu0) / elapsed, 1),
            "min_sequence_advance": advanced,
            "publishers_alive": alive,
            "load_end": round(os.getloadavg()[0], 2),
            # A run that started above the load gate (its wait ran out) is
            # recorded but is not a result.
            "ok": advanced >= 30 and alive and load < LOAD_GATE,
        })
    except Exception as e:
        row["error"] = str(e)
    finally:
        for q in reversed(pubs):
            stop(q, group=True)
        stop(srv)
        slog.close()
        plog.close()
        time.sleep(1)
    return row


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--streams", default="1,10,50")
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--protocols", default="srt,rtmp,whip")
    ap.add_argument("--base", type=int, default=47300)
    ap.add_argument("--settle", type=float, default=SETTLE, help="seconds after every stream is playable")
    ap.add_argument("--window", type=float, default=WINDOW, help="seconds measured")
    ap.add_argument("--gate-wait", type=float, default=300,
                    help="most seconds to wait for the load gate before a run; a run that starts above it is not ok")
    args = ap.parse_args()
    tools()
    args.out.mkdir(parents=True, exist_ok=True)
    clips(args.out)
    protocols = args.protocols.split(",")
    streams = [int(s) for s in args.streams.split(",")]
    rows = []
    with open(args.out / "runs.tsv", "w") as f:
        keys = ["protocol", "streams", "round", "load", "gate_wait_s", "first_playable_s", "window_s",
                "server_cpu_pct", "cpu_per_stream_pct", "peak_rss_mb", "rss_per_stream_mb",
                "publishers_cpu_pct", "min_sequence_advance", "publishers_alive", "load_end", "ok", "error"]
        f.write("\t".join(keys) + "\n")
        for r in range(1, args.rounds + 1):
            for n in streams:
                # The order rotates each round.
                order = protocols[(r - 1) % len(protocols):] + protocols[:(r - 1) % len(protocols)]
                for protocol in order:
                    row = one_run(args, protocol, n, r, args.out)
                    rows.append(row)
                    f.write("\t".join(str(row.get(k, "")) for k in keys) + "\n")
                    f.flush()
                    print(json.dumps(row), flush=True)
    with open(args.out / "summary.tsv", "w") as f:
        f.write("protocol\tstreams\truns_ok\tserver_cpu_pct\tcpu_per_stream_pct\tpeak_rss_mb\trss_per_stream_mb\n")
        for n in streams:
            for protocol in protocols:
                cell = [x for x in rows if x["protocol"] == protocol and x["streams"] == n and x.get("ok")]
                span = lambda k: (f"{min(x[k] for x in cell)} to {max(x[k] for x in cell)}" if cell else "")
                f.write(f"{protocol}\t{n}\t{len(cell)}\t{span('server_cpu_pct')}\t{span('cpu_per_stream_pct')}"
                        f"\t{span('peak_rss_mb')}\t{span('rss_per_stream_mb')}\n")
    print((args.out / "summary.tsv").read_text())
    return 0 if all(x.get("ok") for x in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
