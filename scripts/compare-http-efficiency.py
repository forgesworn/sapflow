#!/usr/bin/env python3
"""Bounded parent/candidate HTTP screening with generated, shared media.

Build the server variants separately from sapflow-bench. This tool never builds,
changes a service, or retains raw logs/media. Each result keeps binary hashes,
the declared source identities and every failed trial. It is loopback screening,
not a capacity, physical playback or production qualification.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

import performance

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("measure", ROOT / "scripts/measure-process.py")
measure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def port(kind):
    with socket.socket(socket.AF_INET, kind) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(children):
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
    deadline = time.monotonic() + 5
    for child in reversed(children):
        try:
            child.wait(timeout=max(.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


def trial(binary, bench, fixture, streams, viewers, seconds, threads):
    children, logs = [], []
    with tempfile.TemporaryDirectory(prefix="sapflow-http-trial-") as scratch:
        work = Path(scratch)

        def launch(command, name, env=None):
            log = (work / name).open("w+")
            logs.append(log)
            child = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=env)
            children.append(child)
            return child, log

        try:
            http, rtmp, srt = port(socket.SOCK_STREAM), port(socket.SOCK_STREAM), port(socket.SOCK_DGRAM)
            # Refuse inherited deployment settings; the fixture is loopback only.
            env = {k: v for k, v in os.environ.items() if not k.startswith("SAPFLOW_")}
            env.update(SAPFLOW_OPEN_PUBLISH="1", SAPFLOW_HTTP_IMPL="own",
                       SAPFLOW_HTTP_THREADS=str(threads), SAPFLOW_HTTP_ADDR=f"127.0.0.1:{http}",
                       SAPFLOW_RTMP_ADDR=f"127.0.0.1:{rtmp}", SAPFLOW_SRT_ADDR=f"127.0.0.1:{srt}",
                       RUST_LOG="error")
            server, _ = launch([str(binary)], "server.log", env)
            deadline = time.monotonic() + 10
            while True:
                if server.poll() is not None:
                    raise RuntimeError("server exited during readiness")
                try:
                    with socket.create_connection(("127.0.0.1", rtmp), timeout=.2):
                        break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("server readiness timeout") from None
                    time.sleep(.05)
            urls = []
            publishers = []
            publish_started = time.monotonic()
            for index in range(streams):
                # Spread independent stream changes across the part cadence;
                # apply the identical launch schedule to both server variants.
                time.sleep(max(0, publish_started + index * .037 - time.monotonic()))
                name = f"event-perf-{index}"
                publisher, _ = launch([
                    "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-re",
                    "-stream_loop", "-1", "-i", str(fixture), "-c", "copy", "-f", "flv",
                    f"rtmp://127.0.0.1:{rtmp}/live/{name}",
                ], f"publisher-{index}.log")
                publishers.append(publisher)
                urls.append(f"http://127.0.0.1:{http}/live/{name}/index.m3u8")
            deadline = time.monotonic() + 15
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            for url in urls:
                while True:
                    if server.poll() is not None or any(p.poll() is not None for p in publishers):
                        raise RuntimeError("server or publisher exited during media readiness")
                    try:
                        with opener.open(url, timeout=.5) as response:
                            if not response.read().startswith(b"#EXTM3U"):
                                raise RuntimeError("non-playlist readiness response")
                        break
                    except (OSError, TimeoutError):
                        if time.monotonic() >= deadline:
                            raise RuntimeError("media readiness timeout") from None
                        time.sleep(.1)
            ramp = 3
            loaders = []
            started = time.monotonic()
            for index, url in enumerate(urls):
                loaders.append(launch([
                    str(bench), "--url", url, "--viewers", str(viewers),
                    "--secs", str(seconds), "--ramp", str(ramp),
                ], f"viewer-{index}.log"))
            skew = time.monotonic() - started
            if skew > .25:
                raise RuntimeError("viewer launch spread exceeded 250 ms")
            time.sleep(max(0, started + ramp - time.monotonic()))
            cpu, rss, elapsed = measure.measure(server.pid, seconds, sample=measure.platform_sampler())
            rows = []
            drain_deadline = time.monotonic() + 15
            for child, log in loaders:
                child.wait(timeout=max(.01, drain_deadline - time.monotonic()))
                log.seek(0)
                report = log.read(200_000)
                if child.returncode != 0:
                    raise RuntimeError(f"viewer exited with code {child.returncode}")
                # Reuse the existing strict workload/collection parser. CPU/RSS
                # is one shared server sample, not one measurement per stream.
                wrapped = (f"--- sapflow, {viewers} viewers: server CPU {cpu:.6f}%  peak RSS {rss:.6f} MB\n"
                           f"server sampling {elapsed:.6f} s target {seconds} s\n" + report)
                rows.extend(performance.parse_report(wrapped, [viewers], expected_server="sapflow",
                            expected_seconds=seconds, expected_ramp=ramp))
            if server.poll() is not None or any(p.poll() is not None for p in publishers):
                raise RuntimeError("server or publisher exited during measurement")
            return dict(cpu_percent=cpu, peak_rss_mib=rss, sample_seconds=elapsed,
                        launch_spread_seconds=skew, streams=rows,
                        qualified=all(r["errors"] == 0 and r["excluded_errors"] == 0 for r in rows))
        finally:
            stop(children)
            for log in logs:
                log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "bench"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--baseline-source", required=True)
    parser.add_argument("--candidate-source", required=True)
    parser.add_argument("--seconds", type=int, default=12)
    parser.add_argument("--pairs", type=int, default=4)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if (not 5 <= args.seconds <= 60 or args.pairs not in (2, 4, 6)
            or not 1 <= args.threads <= 8):
        parser.error("bounded screening requires seconds 5..60, pairs 2/4/6 and threads 1..8")
    if args.output.exists():
        parser.error("refusing to overwrite an existing result")
    if any(not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value)
           for value in (args.baseline_source, args.candidate_source)):
        parser.error("source identities must be Git commit or source-manifest SHA-256 hashes")
    binaries = {name: getattr(args, name).resolve(strict=True) for name in ("baseline", "candidate", "bench")}
    ffmpeg = Path(shutil.which("ffmpeg") or "missing-ffmpeg").resolve(strict=True)
    harness = [Path(__file__).resolve(), ROOT / "scripts/performance.py", ROOT / "scripts/measure-process.py"]
    data = dict(schema="sapflow-http-efficiency-v1", host=dict(system=platform.system(),
                release=platform.release(), machine=platform.machine(), cpus=os.cpu_count()),
                sources=dict(baseline=args.baseline_source, candidate=args.candidate_source),
                binaries={k: digest(v) for k, v in binaries.items()}, seconds=args.seconds,
                harness_sha256={p.name: digest(p) for p in harness},
                ffmpeg_sha256=digest(ffmpeg),
                ffmpeg_version=subprocess.check_output([str(ffmpeg), "-version"], text=True).splitlines()[0],
                pairs=args.pairs, http_threads=args.threads,
                publisher_launch_spacing_seconds=.037,
                notes="Loopback HTTP/1.1, full-media simulated viewers, generated 360p30 H.264/AAC. "
                      "Server, remuxing publishers and viewers share the host with existing workloads. "
                      "One server CPU/RSS interval per trial, nominally aligned to viewer windows within "
                      "250 ms of launch spread. Viewer process/runtime startup adds unmeasured skew, "
                      "so CPU sampling is not an exact viewer-window measurement. "
                      "RSS sampled each second, excludes kernel memory. "
                      "No recording, proxy, TLS, WAN, device, decoder or maximum-capacity claim.", trials=[])
    args.output.parent.mkdir(parents=True, exist_ok=True)

    def save():
        args.output.write_text(json.dumps(data, indent=2) + "\n")

    def interrupted(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    with tempfile.TemporaryDirectory(prefix="sapflow-http-fixture-") as scratch:
        fixture = Path(scratch) / "generated.mp4"
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi",
            "-i", "testsrc2=size=640x360:rate=30", "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
            "-t", "10", "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
            "-pix_fmt", "yuv420p", "-g", "30", "-keyint_min", "30", "-sc_threshold", "0",
            "-b:v", "750k", "-c:a", "aac", "-b:a", "64k", str(fixture),
        ], check=True, timeout=60, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        data["fixture_sha256"] = digest(fixture)
        save()
        for streams, viewers in ((1, 32), (8, 16)):
            for pair in range(args.pairs):
                for variant in (("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")):
                    row = dict(variant=variant, pair=pair, publishers=streams,
                               viewers_per_stream=viewers, qualified=False)
                    data["trials"].append(row)
                    try:
                        row.update(trial(binaries[variant], binaries["bench"], fixture,
                                         streams, viewers, args.seconds, args.threads))
                    except BaseException as error:
                        # No arbitrary logs/credentials in the portable scalar receipt.
                        row.update(qualified=False, error_type=type(error).__name__,
                                   error=str(error) if isinstance(error, RuntimeError) else "trial failed")
                        if not isinstance(error, Exception):
                            raise
                    finally:
                        save()
                    print(json.dumps({k: v for k, v in row.items() if k != "streams"}), flush=True)
    data["inputs_unchanged"] = (
        all(digest(path) == data["binaries"][name] for name, path in binaries.items())
        and all(digest(path) == data["harness_sha256"][path.name] for path in harness)
        and digest(ffmpeg) == data["ffmpeg_sha256"]
    )
    save()
    return 0 if data["inputs_unchanged"] and all(t["qualified"] for t in data["trials"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
