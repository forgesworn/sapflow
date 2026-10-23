#!/usr/bin/env python3
"""Repeated, alternating M4 screenings using the existing socket benchmark."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import resource
import shutil
import signal
import statistics
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
VARIANTS = ("hyper", "own", "own-1", "own-4", "mediamtx")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def trial_order(variants, round_number):
    """Rotate three or more treatments; reverse each paired round."""
    start = round_number % len(variants)
    return variants[start:] + variants[:start]


def parse_report(output, expected_viewers):
    rows = []
    for block in output.split("--- ")[1:]:
        match = re.search(r"^(\w+), (\d+) viewers: server CPU ([\d.]+)%  peak RSS ([\d.]+) MB", block)
        counts = re.search(r"^requests (\d+)  errors (\d+)  received ([\d.]+) MB$", block, re.M)
        playlist = re.search(r"^playlist\s+n=(\d+)\s+p50 ([\d.]+) ms  p95 ([\d.]+) ms  p99 ([\d.]+) ms", block, re.M)
        segment = re.search(r"^segment\s+n=(\d+)\s+p50 ([\d.]+) ms  p95 ([\d.]+) ms  p99 ([\d.]+) ms", block, re.M)
        fanout = re.search(r"^part fan-out .*parts=(\d+) reach ([\d.]+)/\d+  p50 ([\d.]+) ms  p95 ([\d.]+) ms  max ([\d.]+) ms", block, re.M)
        if not all((match, counts, playlist, segment, fanout)):
            raise ValueError("incomplete benchmark output")
        if int(counts[1]) == 0 or any(int(m[1]) == 0 for m in (playlist, segment, fanout)):
            raise ValueError("no requests or media samples")
        rows.append(dict(
            viewers=int(match[2]), cpu_percent=float(match[3]), peak_rss_mb=float(match[4]),
            requests=int(counts[1]), errors=int(counts[2]), received_mb=float(counts[3]),
            playlist_p99_ms=float(playlist[4]), segment_p99_ms=float(segment[4]),
            part_reach=float(fanout[2]), part_p50_ms=float(fanout[3]),
            part_p95_ms=float(fanout[4]), part_max_ms=float(fanout[5]),
        ))
    if [row["viewers"] for row in rows] != expected_viewers:
        raise ValueError("missing, duplicate or unexpected viewer measurements")
    return rows


def summary(runs):
    groups = {}
    for run in runs:
        for row in run["measurements"]:
            groups.setdefault((run["variant"], row["viewers"]), []).append(row)
    result = []
    for (variant, viewers), rows in sorted(groups.items()):
        record = dict(variant=variant, viewers=viewers, runs=len(rows))
        for key in ("cpu_percent", "peak_rss_mb", "playlist_p99_ms", "segment_p99_ms", "part_p50_ms", "part_p95_ms", "part_max_ms"):
            values = [row[key] for row in rows]
            record[key] = dict(median=statistics.median(values), minimum=min(values), maximum=max(values))
        record["requests"] = sum(row["requests"] for row in rows)
        record["errors"] = sum(row["errors"] for row in rows)
        result.append(record)
    return result


def save(output, data):
    (output / "results.json").write_text(json.dumps(data, indent=2) + "\n")


def ensure_ports_unused():
    # Check both address families and protocols. Never kill an existing owner.
    check = subprocess.run(
        ["lsof", "-nP", "-iTCP:8888", "-iTCP:1935", "-iUDP:8890"],
        capture_output=True, text=True, check=False,
    )
    if check.returncode != 1 or check.stdout.strip():
        raise RuntimeError("benchmark ports are occupied, or lsof could not verify them")


def run_trial(env, server, output):
    with output.open("w") as log:
        process = subprocess.Popen(
            ["bash", "scripts/bench.sh", server], cwd=ROOT, env=env,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        try:
            return process.wait()
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new ignored directory for scalar results")
    parser.add_argument("--variants", nargs="+", choices=VARIANTS, default=["hyper", "own", "own-1", "mediamtx"])
    parser.add_argument("--viewers", nargs="+", type=int, default=[250, 750])
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--mediamtx", type=Path)
    args = parser.parse_args()
    if args.rounds < 1 or args.seconds < 10 or any(n < 1 for n in args.viewers):
        parser.error("positive rounds/viewers and at least ten measured seconds are required")
    if len(set(args.variants)) != len(args.variants) or len(set(args.viewers)) != len(args.viewers):
        parser.error("duplicate variants or viewer counts")
    if "mediamtx" in args.variants and not args.mediamtx:
        parser.error("--mediamtx is required for the mediamtx treatment")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("run with native ARM64 Python on the M4")
    for tool in ("cargo", "rustc", "ffmpeg", "lsof", "file"):
        if not shutil.which(tool):
            parser.error("missing tool: " + tool)
    chip = command("sysctl", "-n", "machdep.cpu.brand_string")
    if "M4" not in chip:
        parser.error("this screening requires the M4 test platform")
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    wanted = max(4096, soft)
    if hard != resource.RLIM_INFINITY:
        wanted = min(wanted, hard)
    resource.setrlimit(resource.RLIMIT_NOFILE, (wanted, hard))
    if wanted < 4096:
        parser.error("at least 4096 file descriptors are required")
    ensure_ports_unused()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = {key: value for key, value in os.environ.items() if not key.startswith("SAPFLOW_")}
    env.update(VIEWERS=" ".join(map(str, args.viewers)), SECS=str(args.seconds))
    target = env.get("CARGO_BUILD_TARGET", "")
    release = ROOT / "target" / target / "release"
    data = dict(
        status="preparing", source=command("git", "rev-parse", "HEAD"),
        tracked_changes=command("git", "status", "--porcelain", "--untracked-files=no"),
        tracked_diff_sha256=hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=ROOT)).hexdigest(),
        runner_sha256=sha256(__file__), bench_script_sha256=sha256(ROOT / "scripts/bench.sh"),
        lock_sha256=sha256(ROOT / "Cargo.lock"), cpu=chip, logical_cpus=os.cpu_count(),
        macos=command("sw_vers", "-productVersion"), rustc=command("rustc", "-Vv"),
        ffmpeg=command("ffmpeg", "-version").splitlines()[0], file_descriptor_limit=wanted,
        load_before=os.getloadavg(), measured_seconds=args.seconds, ramp_seconds=10,
        viewers=args.viewers, variants=args.variants, rounds=args.rounds,
        notes="Loopback HTTP/1.1; full media viewers; generated 720p30 H.264/AAC. Server and encoder/load generator share the host. RSS excludes kernel memory. Existing workloads remain running. Separate server and viewer builds avoid feature unification. No recording, TLS, WAN, phone, energy or thermal qualification.",
        runs=[],
    )
    save(output, data)
    try:
        # The existing runner repeats these cheap freshness checks outside timing.
        for package in ("sapflow", "sapflow-bench"):
            subprocess.run(["cargo", "build", "--locked", "--release", "-q", "-p", package], cwd=ROOT, env=env, check=True)
        data["binaries"] = {}
        binaries = {name: release / name for name in ("sapflow", "sapflow-bench")}
        if args.mediamtx:
            binaries["mediamtx"] = args.mediamtx.resolve()
            env["MEDIAMTX"] = str(binaries["mediamtx"])
            data["mediamtx_version"] = command(str(binaries["mediamtx"]), "--version")
        for name, binary in binaries.items():
            architecture = command("file", "-b", str(binary))
            if "arm64" not in architecture or "x86_64" in architecture:
                raise ValueError(name + " must be a thin native ARM64 binary")
            data["binaries"][name] = dict(sha256=sha256(binary), bytes=binary.stat().st_size, architecture=architecture)
        for round_number in range(args.rounds):
            for variant in trial_order(args.variants, round_number):
                ensure_ports_unused()
                trial_env = env.copy()
                server = "mediamtx" if variant == "mediamtx" else "sapflow"
                if server == "sapflow":
                    trial_env["SAPFLOW_HTTP_IMPL"] = "hyper" if variant == "hyper" else "own"
                    if variant.startswith("own-"):
                        trial_env["SAPFLOW_HTTP_THREADS"] = variant.split("-")[1]
                raw = output / f"round-{round_number + 1}-{variant}.txt"
                print(f"round {round_number + 1}/{args.rounds}: {variant}", flush=True)
                started = time.monotonic()
                before = os.getloadavg()
                status = run_trial(trial_env, server, raw)
                if status:
                    raise RuntimeError(f"{raw.name}: runner exit {status}")
                rows = parse_report(raw.read_text(), args.viewers)
                data["runs"].append(dict(round=round_number + 1, variant=variant, raw=raw.name,
                    wall_seconds=time.monotonic() - started, load_before=before,
                    load_after=os.getloadavg(), measurements=rows))
                data["summary"] = summary(data["runs"])
                data["status"] = "running"
                save(output, data)
                if any(row["errors"] for row in rows):
                    raise RuntimeError(raw.name + ": viewer request failures")
                print(json.dumps(rows), flush=True)
        data["status"] = "complete"
    except (Exception, KeyboardInterrupt) as error:
        data.update(status="failed", failure=str(error) or "interrupted")
        raise
    finally:
        data["load_after"] = os.getloadavg()
        save(output, data)


if __name__ == "__main__":
    main()
