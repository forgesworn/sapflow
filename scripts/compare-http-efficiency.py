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
import math
import os
from pathlib import Path
import platform
import re
import resource
import shutil
import signal
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.request

import performance

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("measure", ROOT / "scripts/measure-process.py")
measure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)

# Frozen before the first qualified campaign. Failed guards are evidence, not
# permission to change the workload or replace a trial.
PROTOCOL = dict(
    version="linux-http-qualification-v1", pairs=4, seconds=60, ramp=5,
    http_threads=2, loader_threads=2,
    profiles=[dict(name="single", publishers=1, viewers=128),
              dict(name="sparse", publishers=16, viewers=2),
              dict(name="target", publishers=16, viewers=64)],
    minimum_cpu_ticks=200, maximum_host_busy_percent=85,
    maximum_host_steal_percent=.5, maximum_alignment_seconds=.1,
    maximum_clock_drift_seconds=.05, useful_work_ratio=[.98, 1.02],
    maximum_drain_seconds=1.0,
    minimum_reach_fraction=.99, minimum_latency_samples=100,
    target_cpu=dict(median=.90, three_of_four=.95, worst=1.05),
    control_cpu=dict(median=1.05, worst=1.10, low_cpu_absolute_points=.2),
    rss=dict(median=1.05, worst=1.15),
    latency=dict(median_ratio=1.10, median_allowance_ms=2,
                 worst_ratio=1.25, worst_allowance_ms=5),
)


def window_marker(text):
    matches = re.findall(r"^qualification window_start_unix_ns (\d+) window_end_unix_ns (\d+)$", text, re.M)
    if len(matches) != 1:
        return None
    return tuple(int(value) / 1e9 for value in matches[0])


def exact_work(text):
    found = re.findall(r"^qualification received_bytes (\d+) playlist_samples (\d+) segment_samples (\d+) observed_parts (\d+) mean_part_reach ([\d.]+)$", text, re.M)
    if len(found) != 1:
        raise ValueError("missing or duplicate exact workload scalars")
    values = found[0]
    result = dict(zip(("received_bytes", "playlist_samples", "segment_samples", "observed_parts"), map(int, values[:4])))
    result["mean_part_reach"] = float(values[4])
    if not math.isfinite(result["mean_part_reach"]):
        raise ValueError("non-finite part reach")
    return result


def host_ticks(text):
    fields = text.splitlines()[0].split()
    if fields[0] != "cpu" or len(fields) < 9:
        raise ValueError("invalid aggregate Linux CPU sample")
    values = list(map(int, fields[1:9]))  # Guest ticks already belong to user/nice.
    return sum(values), values[3] + values[4], values[7]


def host_usage(before, after):
    total, idle, steal = (b - a for a, b in zip(before, after))
    if total <= 0 or min(idle, steal) < 0:
        raise ValueError("non-monotonic Linux host CPU sample")
    return dict(host_busy_percent=100 * (total - idle) / total,
                host_steal_percent=100 * steal / total)


def linux_sample(pid):
    base = Path("/proc") / str(pid)
    text = (base / "stat").read_text()
    fields = text[text.rfind(")") + 1:].split()
    ticks = int(fields[11]) + int(fields[12])
    rss = measure.parse_vm_rss((base / "status").read_text())
    return ticks, rss, host_ticks(Path("/proc/stat").read_text())


def linux_measure(pid, planned_start, seconds):
    wall, mono = time.time(), time.monotonic()
    deadline = mono + planned_start - wall
    time.sleep(max(0, deadline - time.monotonic()))
    before = time.monotonic()
    first_cpu, peak, first_host = linux_sample(pid)
    started = (before + time.monotonic()) / 2
    wall_started = time.time()
    end = started + seconds
    while True:
        time.sleep(max(0, min(1, end - time.monotonic())))
        before = time.monotonic()
        last_cpu, rss, last_host = linux_sample(pid)
        finished = (before + time.monotonic()) / 2
        peak = max(peak, rss)
        if finished >= end:
            break
    wall_finished = time.time()
    elapsed = finished - started
    ticks = last_cpu - first_cpu
    if ticks < 0:
        raise ValueError("non-monotonic process CPU")
    hz = os.sysconf("SC_CLK_TCK")
    return dict(cpu_percent=100 * ticks / hz / elapsed, peak_rss_mib=peak,
                sample_seconds=elapsed, process_cpu_ticks=ticks, clock_ticks_per_second=hz,
                process_cpu_seconds=ticks / hz, sample_start_unix=wall_started,
                sample_end_unix=wall_finished,
                start_error_seconds=abs(wall_started - planned_start),
                end_error_seconds=abs(wall_finished - (planned_start + seconds)),
                clock_drift_seconds=abs((wall_finished - wall_started) - elapsed),
                **host_usage(first_host, last_host))


def trial_issues(row):
    issues = []
    for field, limit in (("host_busy_percent", PROTOCOL["maximum_host_busy_percent"]),
                         ("host_steal_percent", PROTOCOL["maximum_host_steal_percent"]),
                         ("start_error_seconds", PROTOCOL["maximum_alignment_seconds"]),
                         ("end_error_seconds", PROTOCOL["maximum_alignment_seconds"]),
                         ("runtime_start_spread_seconds", PROTOCOL["maximum_alignment_seconds"]),
                         ("clock_drift_seconds", PROTOCOL["maximum_clock_drift_seconds"])):
        if not math.isfinite(row[field]) or row[field] > limit:
            issues.append(field)
    for stream in row["streams"]:
        if stream["max_drain_seconds"] > PROTOCOL["maximum_drain_seconds"]:
            issues.append("drain_exceeded")
        if min(stream["playlist_samples"], stream["segment_samples"]) < PROTOCOL["minimum_latency_samples"]:
            issues.append("insufficient_latency_samples")
        if not stream["observed_parts"] or stream["mean_part_reach"] < PROTOCOL["minimum_reach_fraction"] * stream["viewers"]:
            issues.append("part_reach")
    return sorted(set(issues))


def qualification(trials):
    issues, summaries = [], {}
    if len(trials) != len(PROTOCOL["profiles"]) * PROTOCOL["pairs"] * 2 or any(not row.get("qualified") for row in trials):
        return dict(accepted=False, issues=["incomplete_or_invalid_trials"], profiles={})
    for profile in PROTOCOL["profiles"]:
        name = profile["name"]
        ratios = {key: [] for key in ("cpu_per_byte", "rss", "received_bytes", "requests")}
        latency = {key: [] for key in ("playlist_p99_ms", "segment_p99_ms")}
        absolute_cpu, precise_cpu = [], []
        for pair in range(PROTOCOL["pairs"]):
            rows = [row for row in trials if row["profile"] == name and row["pair"] == pair]
            if len(rows) != 2 or {row["variant"] for row in rows} != {"baseline", "candidate"}:
                issues.append(name + ":missing_or_duplicate_pair")
                continue
            b, c = (next(row for row in rows if row["variant"] == variant)
                    for variant in ("baseline", "candidate"))
            useful = {}
            for key in ("received_bytes", "requests"):
                bv, cv = (sum(s[key] for s in row["streams"]) for row in (b, c))
                useful[key] = cv / bv
                ratios[key].append(cv / bv)
                if not PROTOCOL["useful_work_ratio"][0] <= cv / bv <= PROTOCOL["useful_work_ratio"][1]:
                    issues.append(name + ":useful_work_" + key)
            ratios["cpu_per_byte"].append((c["process_cpu_seconds"] / b["process_cpu_seconds"]) / useful["received_bytes"]
                                          if b["process_cpu_seconds"] else None)
            ratios["rss"].append(c["peak_rss_mib"] / b["peak_rss_mib"])
            absolute_cpu.append(c["cpu_percent"] - b["cpu_percent"])
            precise_cpu.append(min(b["process_cpu_ticks"], c["process_cpu_ticks"]) >= PROTOCOL["minimum_cpu_ticks"])
            for key in latency:
                for aggregation in (statistics.median, max):
                    bv, cv = (aggregation(s[key] for s in row["streams"]) for row in (b, c))
                    latency[key].append(dict(pair=pair, aggregation=aggregation.__name__, baseline=bv, candidate=cv))
        if len(ratios["rss"]) != PROTOCOL["pairs"]:
            continue
        cpu = ratios["cpu_per_byte"]
        if name == "target":
            guard = PROTOCOL["target_cpu"]
            if not all(precise_cpu) or statistics.median(cpu) > guard["median"] or sum(r <= guard["three_of_four"] for r in cpu) < 3 or max(cpu) > guard["worst"]:
                issues.append(name + ":cpu_benefit")
        else:
            guard = PROTOCOL["control_cpu"]
            controls = [ratio for ratio, precise in zip(cpu, precise_cpu) if precise]
            if (controls and (statistics.median(controls) > guard["median"] or max(controls) > guard["worst"])) or any(
                    delta > guard["low_cpu_absolute_points"] for delta, precise in zip(absolute_cpu, precise_cpu) if not precise):
                issues.append(name + ":cpu_non_regression")
        if statistics.median(ratios["rss"]) > PROTOCOL["rss"]["median"] or max(ratios["rss"]) > PROTOCOL["rss"]["worst"]:
            issues.append(name + ":rss")
        for key, samples in latency.items():
            guard = PROTOCOL["latency"]
            for aggregation in ("median", "max"):
                subset = [s for s in samples if s["aggregation"] == aggregation]
                # Median and every-pair guards use a predeclared absolute floor
                # so sub-millisecond baselines do not magnify timer noise.
                excess = [s["candidate"] - max(s["baseline"] * guard["median_ratio"], s["baseline"] + guard["median_allowance_ms"]) for s in subset]
                if statistics.median(excess) > 0 or any(s["candidate"] > max(s["baseline"] * guard["worst_ratio"], s["baseline"] + guard["worst_allowance_ms"]) for s in subset):
                    issues.append(name + ":" + key + ":" + aggregation)
        summaries[name] = dict(paired_ratios=ratios, median_ratios={k: statistics.median([x for x in v if x is not None])
                                                                if any(x is not None for x in v) else None for k, v in ratios.items()},
                               cpu_ticks_sufficient=precise_cpu, absolute_cpu_point_changes=absolute_cpu, latency=latency)
    return dict(accepted=not issues, issues=sorted(set(issues)), profiles=summaries)


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


def trial(binary, bench, fixture, streams, viewers, seconds, threads, qualify_linux=False):
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
            ramp = 5 if qualify_linux else 3
            loaders = []
            started = time.monotonic()
            for index, url in enumerate(urls):
                loaders.append(launch([
                    str(bench), "--url", url, "--viewers", str(viewers),
                    "--secs", str(seconds), "--ramp", str(ramp),
                ], f"viewer-{index}.log", {**os.environ, "TOKIO_WORKER_THREADS": "2"}))
            skew = time.monotonic() - started
            if skew > .25:
                raise RuntimeError("viewer launch spread exceeded 250 ms")
            sampling = {}
            if qualify_linux:
                markers = []
                ready_deadline = started + ramp - .5
                while len(markers) != len(loaders):
                    markers = [window_marker(Path(log.name).read_text()) for _, log in loaders]
                    if all(marker is not None for marker in markers):
                        break
                    if time.monotonic() >= ready_deadline or any(child.poll() is not None for child, _ in loaders):
                        raise RuntimeError("viewer runtime markers missing before measurement")
                    time.sleep(.01)
                    markers = []
                if any(abs(end - begin - seconds) > .001 for begin, end in markers):
                    raise RuntimeError("viewer runtime window differs from protocol")
                earliest = min(begin for begin, _ in markers)
                sampling = linux_measure(server.pid, earliest, seconds)
                sampling["runtime_start_spread_seconds"] = max(begin for begin, _ in markers) - earliest
                sampling["runtime_windows_unix"] = markers
                cpu, rss, elapsed = (sampling[k] for k in ("cpu_percent", "peak_rss_mib", "sample_seconds"))
            else:
                time.sleep(max(0, started + ramp - time.monotonic()))
                cpu, rss, elapsed = measure.measure(server.pid, seconds, sample=measure.platform_sampler())
            rows = []
            drain_deadline = time.monotonic() + 35
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
                parsed = performance.parse_report(wrapped, [viewers], expected_server="sapflow",
                                                  expected_seconds=seconds, expected_ramp=ramp)
                if qualify_linux:
                    parsed[0].update(exact_work(report))
                rows.extend(parsed)
            if server.poll() is not None or any(p.poll() is not None for p in publishers):
                raise RuntimeError("server or publisher exited during measurement")
            result = dict(cpu_percent=cpu, peak_rss_mib=rss, sample_seconds=elapsed,
                          launch_spread_seconds=skew, streams=rows,
                          qualified=all(r["errors"] == 0 and r["excluded_errors"] == 0 for r in rows))
            result.update(sampling)
            if qualify_linux:
                result["qualification_issues"] = trial_issues(result)
                result["qualified"] &= not result["qualification_issues"]
            return result
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
    parser.add_argument("--qualify-linux", action="store_true", help="use the fixed Linux acceptance protocol")
    args = parser.parse_args()
    if (not 5 <= args.seconds <= 60 or args.pairs not in (2, 4, 6)
            or not 1 <= args.threads <= 8):
        parser.error("bounded screening requires seconds 5..60, pairs 2/4/6 and threads 1..8")
    if args.output.exists():
        parser.error("refusing to overwrite an existing result")
    if args.qualify_linux:
        if platform.system() != "Linux":
            parser.error("qualification requires a fresh Linux runner")
        if (args.pairs, args.seconds, args.threads) != (4, 60, 2):
            parser.error("qualification requires --pairs 4 --seconds 60 --threads 2")
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        if hard != resource.RLIM_INFINITY and hard < 8192:
            parser.error("qualification requires descriptor hard limit >=8192")
        resource.setrlimit(resource.RLIMIT_NOFILE, (max(soft, 8192), hard))
    if any(not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value)
           for value in (args.baseline_source, args.candidate_source)):
        parser.error("source identities must be Git commit or source-manifest SHA-256 hashes")
    binaries = {name: getattr(args, name).resolve(strict=True) for name in ("baseline", "candidate", "bench")}
    ffmpeg = Path(shutil.which("ffmpeg") or "missing-ffmpeg").resolve(strict=True)
    harness = [Path(__file__).resolve(), ROOT / "scripts/performance.py", ROOT / "scripts/measure-process.py"]
    if args.qualify_linux:
        harness.extend((ROOT / "scripts/build-http-comparison.sh",
                        ROOT / ".github/workflows/performance-qualification.yml"))
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
        args.output.write_text(json.dumps(data, indent=2, allow_nan=False) + "\n")

    if args.qualify_linux:
        if subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip() != args.candidate_source:
            parser.error("qualification checkout does not match the declared candidate commit")
        subprocess.run(["git", "diff", "--exit-code"], check=True, stdout=subprocess.DEVNULL)
        data["protocol"] = PROTOCOL
        data["protocol_sha256"] = hashlib.sha256(json.dumps(PROTOCOL, sort_keys=True).encode()).hexdigest()
        data["build"] = dict(
            rustc=subprocess.check_output(["rustc", "-vV"], text=True).strip(),
            environment={key: os.environ.get(key) for key in (
                "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN")},
            inputs_sha256={name: digest(ROOT / name) for name in (
                "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
                "crates/sapflow-server/Cargo.toml", "crates/sapflow-bench/Cargo.toml")},
        )
        data["host"].update(clock_ticks_per_second=os.sysconf("SC_CLK_TCK"),
                            descriptor_limit=resource.getrlimit(resource.RLIMIT_NOFILE)[0],
                            load_before=os.getloadavg(),
                            cpu_model=next((line.partition(":")[2].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                                            if line.startswith("model name")), "unknown"))
        data["notes"] += " Fixed Linux qualification uses runtime window markers, exact workload bytes, raw process CPU ticks and host busy/steal guards. Request-start-window bytes include completion drain (guarded <=1s); CPU ends at the 60s window, so CPU per byte is bounded screening, not exact attribution or energy. Per-stream tail summaries are not pooled percentiles."
        save()

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
        profiles = PROTOCOL["profiles"] if args.qualify_linux else [dict(name="single", publishers=1, viewers=32), dict(name="multi", publishers=8, viewers=16)]
        for profile in profiles:
            streams, viewers = profile["publishers"], profile["viewers"]
            for pair in range(args.pairs):
                for variant in (("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")):
                    row = dict(variant=variant, pair=pair, profile=profile["name"], publishers=streams,
                               viewers_per_stream=viewers, qualified=False)
                    data["trials"].append(row)
                    try:
                        row.update(trial(binaries[variant], binaries["bench"], fixture,
                                         streams, viewers, args.seconds, args.threads, args.qualify_linux))
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
    if args.qualify_linux:
        data["host"]["load_after"] = os.getloadavg()
        data["qualification"] = qualification(data["trials"])
        if not data["inputs_unchanged"]:
            data["qualification"]["accepted"] = False
            data["qualification"]["issues"].append("inputs_changed")
    save()
    return 0 if data["inputs_unchanged"] and all(t["qualified"] for t in data["trials"]) and (
        not args.qualify_linux or data["qualification"]["accepted"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
