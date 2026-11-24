#!/usr/bin/env python3
"""Paired server CPU for SRT ingest between two sapflow builds, no viewers.

Each trial starts one server, publishes the same generated MPEG-TS clip to it
over SRT from several publishers (FFmpeg remuxing in real time into
srt-live-transmit, as scripts/smoke.sh publishes), waits until every stream
is servable, and reads the server's CPU time over a fixed window. Trials
alternate AB, BA. Every trial is retained; a failed one fails the campaign.

Loopback only, generated media, one host shared by the server and the
publishers: a screening of ingest CPU, not a capacity or network result.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def free_port(kind):
    with socket.socket(socket.AF_INET, kind) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def cpu_seconds(pid):
    """User plus system CPU the process has used."""
    stat = Path(f"/proc/{pid}/stat")
    if stat.exists():
        fields = stat.read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    value = subprocess.check_output(["ps", "-o", "time=", "-p", str(pid)], text=True).strip()
    total = 0.0
    for part in value.replace("-", ":").split(":"):
        total = total * 60 + float(part)
    return total


def peak_rss_mib(pid):
    status = Path(f"/proc/{pid}/status")
    if not status.exists():
        return None
    for line in status.read_text().splitlines():
        if line.startswith("VmHWM:"):
            return int(line.split()[1]) / 1024
    return None


def media_sequence(opener, url):
    """How many segments the stream's media playlist has reached, found
    through its multivariant playlist; None until both are served."""
    with opener.open(url, timeout=2) as response:
        lines = response.read().decode().splitlines()
    variant = next((line for line in lines if line and not line.startswith("#")), None)
    if variant is None:
        return None
    with opener.open(urllib.parse.urljoin(url, variant), timeout=2) as response:
        lines = response.read().decode().splitlines()
    first = next((int(line.split(":", 1)[1]) for line in lines
                  if line.startswith("#EXT-X-MEDIA-SEQUENCE:")), None)
    # The sequence number after the last whole segment listed.
    return None if first is None else first + sum(line.startswith("#EXTINF:") for line in lines)


def trial(binary, clip, publishers, seconds, work):
    children, logs = [], []

    def launch(command, name, env=None):
        log = (work / name).open("w+")
        logs.append(log)
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log,
                                 stderr=subprocess.STDOUT, env=env)
        children.append(child)
        return child

    try:
        http, srt = free_port(socket.SOCK_STREAM), free_port(socket.SOCK_DGRAM)
        # Refuse inherited deployment settings; raise only the limits that
        # would turn this many loopback publishers away.
        env = {k: v for k, v in os.environ.items() if not k.startswith("SAPFLOW_")}
        env.update(SAPFLOW_OPEN_PUBLISH="1", SAPFLOW_HTTP_ADDR=f"127.0.0.1:{http}",
                   SAPFLOW_SRT_ADDR=f"127.0.0.1:{srt}", SAPFLOW_RTMP_ADDR="off",
                   SAPFLOW_MAX_PUBLISHERS=str(max(16, publishers)),
                   SAPFLOW_MAX_FRAGMENT_BYTES=str(4 << 30), RUST_LOG="error")
        server = launch([str(binary)], "server.log", env)
        deadline = time.monotonic() + 10
        while True:
            if server.poll() is not None:
                raise RuntimeError("server exited during readiness")
            try:
                with socket.create_connection(("127.0.0.1", http), timeout=.2):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("server readiness timeout") from None
                time.sleep(.05)
        urls = []
        for index in range(publishers):
            port = free_port(socket.SOCK_DGRAM)
            name = f"event-ingest-{index}"
            launch(["srt-live-transmit", "-q", "-a:no", "-c:1316", f"udp://127.0.0.1:{port}",
                    f"srt://127.0.0.1:{srt}?streamid=publish:live/{name}&latency=200"],
                   f"sender-{index}.log")
            time.sleep(.15)
            launch(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-re",
                    "-stream_loop", "-1", "-i", str(clip), "-c", "copy", "-f", "mpegts",
                    f"udp://127.0.0.1:{port}?pkt_size=1316"], f"publisher-{index}.log")
            urls.append(f"http://127.0.0.1:{http}/live/{name}/index.m3u8")

        def alive():
            if any(child.poll() is not None for child in children):
                raise RuntimeError("server or publisher exited")

        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 30
        for url in urls:
            while True:
                alive()
                try:
                    if media_sequence(opener, url) is not None:
                        break
                except (OSError, urllib.error.URLError):
                    pass
                if time.monotonic() >= deadline:
                    raise RuntimeError("stream readiness timeout")
                time.sleep(.1)
        time.sleep(3)
        alive()
        before = [media_sequence(opener, url) for url in urls]
        cpu_before, started = cpu_seconds(server.pid), time.monotonic()
        time.sleep(seconds)
        cpu, wall = cpu_seconds(server.pid) - cpu_before, time.monotonic() - started
        alive()
        after = [media_sequence(opener, url) for url in urls]
        advanced = [b - a for a, b in zip(before, after)]
        # One-second segments: every stream must have kept pace over the window.
        if min(advanced) < seconds - 3:
            raise RuntimeError(f"a stream advanced only {min(advanced)} segments")
        return dict(qualified=True, cpu_seconds=cpu, wall_seconds=wall,
                    cpu_percent=100 * cpu / wall, peak_rss_mib=peak_rss_mib(server.pid),
                    segments_advanced_min=min(advanced), segments_advanced_max=max(advanced))
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
        for child in children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for log in logs:
            log.close()


def summarise(trials, pairs):
    if len(trials) != 2 * pairs or not all(row.get("qualified") for row in trials):
        return dict(complete=False)
    lookup = {(row["pair"], row["variant"]): row["cpu_seconds"] / row["wall_seconds"] for row in trials}
    ratios = [lookup[pair, "candidate"] / lookup[pair, "baseline"] for pair in range(pairs)]
    return dict(complete=True, cpu_ratios=ratios, median_cpu_ratio=statistics.median(ratios),
                maximum_cpu_ratio=max(ratios), minimum_cpu_ratio=min(ratios),
                baseline_cpu_percent_median=statistics.median(
                    row["cpu_percent"] for row in trials if row["variant"] == "baseline"),
                candidate_cpu_percent_median=statistics.median(
                    row["cpu_percent"] for row in trials if row["variant"] == "candidate"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--publishers", type=int, default=12)
    parser.add_argument("--bitrate-kbit", type=int, default=6000)
    parser.add_argument("--seconds", type=int, default=30)
    parser.add_argument("--pairs", type=int, default=4)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not (1 <= args.publishers <= 64 and 5 <= args.seconds <= 300 and 1 <= args.pairs <= 12
            and 500 <= args.bitrate_kbit <= 20_000):
        parser.error("bounded publishers, seconds, pairs and bitrate required")
    if args.output.exists():
        parser.error("refusing to overwrite an existing receipt")
    for tool in ("ffmpeg", "srt-live-transmit"):
        if not shutil.which(tool):
            parser.error(f"missing {tool}")
    binaries = dict(baseline=args.baseline.resolve(strict=True), candidate=args.candidate.resolve(strict=True))
    data = dict(schema="sapflow-srt-ingest-comparison-v1",
                host=dict(system=platform.system(), release=platform.release(),
                          machine=platform.machine(), cpus=os.cpu_count()),
                binaries={name: digest(path) for name, path in binaries.items()},
                harness_sha256=digest(__file__), publishers=args.publishers,
                video_kbit=args.bitrate_kbit, seconds=args.seconds, pairs=args.pairs,
                notes="Loopback SRT, generated 720p30 H.264/AAC remuxed in real time, no viewers. "
                      "Server and publishers share the host. One server CPU interval per trial.",
                trials=[])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    failed = False
    with tempfile.TemporaryDirectory(prefix="sapflow-srt-ingest-") as scratch:
        clip = Path(scratch) / "generated.ts"
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i",
                        "testsrc2=size=1280x720:rate=30", "-f", "lavfi", "-i",
                        "sine=frequency=440:sample_rate=48000", "-t", "20", "-pix_fmt", "yuv420p",
                        "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-g", "30",
                        "-b:v", f"{args.bitrate_kbit}k", "-maxrate", f"{args.bitrate_kbit}k",
                        "-bufsize", f"{args.bitrate_kbit}k", "-c:a", "aac", "-b:a", "128k",
                        "-f", "mpegts", str(clip)], check=True)
        data["clip_bytes"] = clip.stat().st_size
        for pair in range(args.pairs):
            for variant in (("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")):
                row = dict(pair=pair, variant=variant, load_before=os.getloadavg()[0])
                work = Path(scratch) / f"trial-{pair}-{variant}"
                work.mkdir()
                try:
                    row.update(trial(binaries[variant], clip, args.publishers, args.seconds, work))
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    row.update(qualified=False, error=str(error))
                    for name in ("server.log", "sender-0.log", "publisher-0.log"):
                        log = work / name
                        if log.exists():
                            row[f"{name}_tail"] = log.read_text(errors="replace")[-600:]
                    failed = True
                data["trials"].append(row)
                print(json.dumps(row), flush=True)
                args.output.write_text(json.dumps(data, indent=2) + "\n")
                shutil.rmtree(work, ignore_errors=True)
    data["summary"] = summarise(data["trials"], args.pairs)
    args.output.write_text(json.dumps(data, indent=2) + "\n")
    print(json.dumps(data["summary"], indent=2))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
