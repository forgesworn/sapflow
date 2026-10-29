#!/usr/bin/env python3
"""Repeated, alternating screenings using the existing socket benchmark.

`--platform m4` (the default) is the macOS screening on the Apple M4 Max.
`--platform linux` runs the same workload inside native aarch64 Linux, for
example a linux/arm64 container on that Mac; its receipts are a separate cohort.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import resource
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
VARIANTS = ("own", "own-1", "own-4", "mediamtx")
PLATFORMS = ("m4", "linux")
OS_RELEASE = "/etc/os-release"
CGROUP_CPU_MAX = "/sys/fs/cgroup/cpu.max"
CGROUP_MEMORY_MAX = "/sys/fs/cgroup/memory.max"


NOTES = dict(
    m4="Loopback HTTP/1.1; full media viewers; generated 720p30 H.264/AAC. Server and encoder/load generator share the host. RSS excludes kernel memory. Existing workloads remain running. Separate server and viewer builds avoid feature unification. No recording, TLS, WAN, phone, energy or thermal qualification.",
    linux="Loopback HTTP/1.1 inside one native linux/arm64 environment; full media viewers; generated 720p30 H.264/AAC. Server, FFmpeg publisher and load generator share its CPU and memory allocation. A separate cohort: Linux/epoll results are not comparable with macOS/kqueue receipts. CPU is utime plus stime from /proc at clock-tick resolution, as a percentage of one core; RSS is VmRSS and excludes kernel memory. Separate server and viewer builds avoid feature unification. No recording, TLS, WAN, phone, energy or thermal qualification.",
)
SLOW_NOTES = " Slow viewers are counted in the viewer total and read each response body at no more than the recorded rate; they are reported apart and excluded from the ordinary viewers' figures. Their request errors are recorded and do not fail a trial."


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def trial_order(variants, round_number):
    """Rotate treatments so a full cycle balances every order position."""
    start = round_number % len(variants)
    return variants[start:] + variants[:start]


def read_optional(path):
    """A file's text, stripped, or None when it is absent (cgroup v1, no os-release)."""
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def os_pretty_name(text):
    """PRETTY_NAME from the text of /etc/os-release, or None."""
    for line in (text or "").splitlines():
        key, separator, value = line.partition("=")
        if separator and key.strip() == "PRETTY_NAME":
            try:
                words = shlex.split(value)
            except ValueError:
                return value.strip() or None
            return " ".join(words) or None
    return None


def parse_cpu_max(text):
    """(quota, period) in microseconds from cgroup v2 cpu.max; the quota is None for "max"."""
    fields = text.split()
    if len(fields) != 2:
        raise ValueError("unexpected cgroup cpu.max: " + repr(text))
    quota = None if fields[0] == "max" else int(fields[0])
    period = int(fields[1])
    if period < 1 or (quota is not None and quota < 1):
        raise ValueError("unexpected cgroup cpu.max: " + repr(text))
    return quota, period


def parse_memory_max(text):
    """Bytes from cgroup v2 memory.max, or None for "max" (no limit)."""
    value = text.strip()
    if value == "max":
        return None
    limit = int(value)
    if limit < 1:
        raise ValueError("unexpected cgroup memory.max: " + repr(text))
    return limit


def effective_cpus(affinity, cpu_max):
    """Cores usable as Rust's available_parallelism counts them: the affinity
    mask, lowered to the cgroup CPU quota rounded up (at least one)."""
    if cpu_max and cpu_max[0] is not None:
        return min(affinity, max(1, -(-cpu_max[0] // cpu_max[1])))
    return affinity


def linux_host(*, read=None, cpu_count=None, affinity=None, kernel=None):
    """What the receipt records of a Linux host or container in place of the macOS fields.

    `read` returns a file's text, or None when absent; the other values default
    to this process's own. The cgroup limits are those of its own cgroup (v2).
    """
    read = read_optional if read is None else read
    cpu_count = os.cpu_count() if cpu_count is None else cpu_count
    affinity = len(os.sched_getaffinity(0)) if affinity is None else affinity
    kernel = platform.release() if kernel is None else kernel
    cpu_text = read(CGROUP_CPU_MAX)
    memory_text = read(CGROUP_MEMORY_MAX)
    cpu_max = parse_cpu_max(cpu_text) if cpu_text is not None else None
    return dict(
        os=os_pretty_name(read(OS_RELEASE)), kernel=kernel,
        logical_cpus=cpu_count, affinity_cpus=affinity,
        cgroup_cpu_max=cpu_text,
        cgroup_cpu_quota_cpus=cpu_max[0] / cpu_max[1] if cpu_max and cpu_max[0] is not None else None,
        cgroup_memory_max=memory_text,
        cgroup_memory_max_bytes=parse_memory_max(memory_text) if memory_text is not None else None,
        effective_cpus=effective_cpus(affinity, cpu_max),
    )


def is_native_arm64(description, platform_name="m4"):
    """Whether `file -b` describes a thin native ARM64 binary for the platform."""
    if "x86_64" in description or "x86-64" in description:
        return False
    if platform_name == "linux":
        return "ELF" in description and "aarch64" in description
    return "arm64" in description


def parse_ms(text):
    """A millisecond figure from the loader, or None where it printed NaN (no samples)."""
    value = float(text)
    return value if math.isfinite(value) else None


def parse_slow(block, viewers, expected_slow, expected_slow_rate):
    """The slow-consumer section, which must be present exactly when slow viewers were planned."""
    header = re.search(r"^slow viewers (\d+) of (\d+)  rate (\d+) B/s each; the figures above cover the other (\d+)$", block, re.M)
    if not expected_slow:
        if header or re.search(r"^slow ", block, re.M):
            raise ValueError("unexpected slow viewer section")
        return None
    excluded = re.search(r"^slow excluded requests (\d+)  errors (\d+)$", block, re.M)
    counts = re.search(r"^slow requests (\d+)  errors (\d+)  received ([\d.]+) MB$", block, re.M)
    playlist = re.search(r"^slow playlist\s+n=(\d+)\s+p50 (\S+) ms  p99 (\S+) ms$", block, re.M)
    segment = re.search(r"^slow segment\s+n=(\d+)\s+p50 (\S+) ms  p99 (\S+) ms$", block, re.M)
    parts = re.search(r"^slow parts fetched (\d+)$", block, re.M)
    if not all((header, excluded, counts, playlist, segment, parts)):
        raise ValueError("incomplete benchmark output")
    if tuple(map(int, header.groups())) != (expected_slow, viewers, expected_slow_rate, viewers - expected_slow):
        raise ValueError("slow viewer workload differs from the planned measurement")
    if int(counts[1]) == 0:
        raise ValueError("no requests from the slow viewers")
    kinds = {}
    for kind, count in re.findall(r"^  slow error (.+): (\d+)$", block, re.M):
        kinds[kind] = kinds.get(kind, 0) + int(count)
    return dict(
        viewers=expected_slow, rate_bytes_per_second=expected_slow_rate,
        requests=int(counts[1]), errors=int(counts[2]), received_mb=float(counts[3]),
        excluded_requests=int(excluded[1]), excluded_errors=int(excluded[2]),
        error_kinds=kinds,
        playlist=dict(n=int(playlist[1]), p50_ms=parse_ms(playlist[2]), p99_ms=parse_ms(playlist[3])),
        segment=dict(n=int(segment[1]), p50_ms=parse_ms(segment[2]), p99_ms=parse_ms(segment[3])),
        parts_fetched=int(parts[1]),
    )


def parse_report(output, expected_viewers, *, expected_server, expected_seconds, expected_ramp=10,
                 expected_slow=0, expected_slow_rate=None):
    """Qualify the declared workload and sampling interval, not only sample counts.

    Allow the sampler at most one second or 2% of the planned interval, whichever
    is larger. This is a predeclared screening tolerance, not a timing guarantee.

    With `expected_slow` viewers the figures are those of the other viewers, whose
    part reach is out of `viewers - expected_slow`, and the slow viewers' own
    section is required and returned under `slow`. Their request errors are
    recorded, not refused: falling behind live is what they are there to show.
    """
    rows = []
    for block in output.split("--- ")[1:]:
        match = re.search(r"^(\w+), (\d+) viewers: server CPU ([\d.]+)%  peak RSS ([\d.]+) MB", block)
        workload = re.search(r"^viewers (\d+)  measured (\d+) s after a (\d+) s ramp$", block, re.M)
        counts = re.search(r"^requests (\d+)  errors (\d+)  received ([\d.]+) MB$", block, re.M)
        collection = "collection requests started within the fixed post-ramp window, including drain" in block
        excluded = re.search(r"^excluded requests (\d+)  errors (\d+)$", block, re.M)
        sampling = re.search(r"^server sampling ([\d.]+) s target (\d+) s$", block, re.M)
        drain = re.search(r"^drained requests (\d+) max ([\d.]+) s$", block, re.M)
        playlist = re.search(r"^playlist\s+n=(\d+)\s+p50 ([\d.]+) ms  p95 ([\d.]+) ms  p99 ([\d.]+) ms", block, re.M)
        segment = re.search(r"^segment\s+n=(\d+)\s+p50 ([\d.]+) ms  p95 ([\d.]+) ms  p99 ([\d.]+) ms", block, re.M)
        fanout = re.search(r"^part fan-out .*parts=(\d+) reach ([\d.]+)/(\d+)  p50 ([\d.]+) ms  p95 ([\d.]+) ms  max ([\d.]+) ms", block, re.M)
        if not all((match, workload, counts, playlist, segment, fanout, collection, excluded, sampling, drain)):
            raise ValueError("incomplete benchmark output")
        if match[1] != expected_server:
            raise ValueError("unexpected measured server")
        viewers = int(match[2])
        if tuple(map(int, workload.groups())) != (viewers, expected_seconds, expected_ramp):
            raise ValueError("viewer workload differs from the planned measurement")
        slow = parse_slow(block, viewers, expected_slow, expected_slow_rate)
        ordinary = viewers - expected_slow
        if int(fanout[3]) != ordinary or not 0 < float(fanout[2]) <= ordinary:
            raise ValueError("part fan-out reach or viewer denominator is inconsistent")
        tolerance = max(1.0, expected_seconds * .02)
        if int(sampling[2]) != expected_seconds or abs(float(sampling[1]) - expected_seconds) > tolerance:
            raise ValueError("server sampling differs from the planned interval")
        if int(counts[1]) == 0 or any(int(m[1]) == 0 for m in (playlist, segment, fanout)):
            raise ValueError("no requests or media samples")
        if float(counts[3]) <= 0 or float(match[4]) <= 0:
            raise ValueError("received bytes and sampled server RSS must be positive")
        row = dict(
            viewers=viewers, server=match[1], cpu_percent=float(match[3]), peak_rss_mb=float(match[4]),
            requests=int(counts[1]), errors=int(counts[2]), received_mb=float(counts[3]),
            excluded_requests=int(excluded[1]), excluded_errors=int(excluded[2]),
            server_sample_seconds=float(sampling[1]),
            server_sample_target_seconds=int(sampling[2]),
            drained_requests=int(drain[1]), max_drain_seconds=float(drain[2]),
            playlist_p99_ms=float(playlist[4]), segment_p99_ms=float(segment[4]),
            part_reach=float(fanout[2]), part_p50_ms=float(fanout[4]),
            part_p95_ms=float(fanout[5]), part_max_ms=float(fanout[6]),
        )
        if any(isinstance(value, float) and not math.isfinite(value) for value in row.values()):
            raise ValueError("non-finite benchmark measurement")
        if slow is not None:
            row["slow"] = slow
        rows.append(row)
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
        for key in ("cpu_percent", "peak_rss_mb", "playlist_p99_ms", "segment_p99_ms", "part_p50_ms", "part_p95_ms", "part_max_ms", "max_drain_seconds"):
            values = [row[key] for row in rows]
            record[key] = dict(median=statistics.median(values), minimum=min(values), maximum=max(values))
        record["requests"] = sum(row["requests"] for row in rows)
        record["errors"] = sum(row["errors"] for row in rows)
        record["drained_requests"] = sum(row["drained_requests"] for row in rows)
        if any("slow" in row for row in rows):
            slow = [row["slow"] for row in rows]
            kinds = {}
            for item in slow:
                for kind, count in item["error_kinds"].items():
                    kinds[kind] = kinds.get(kind, 0) + count
            record["slow"] = dict(
                viewers=slow[0]["viewers"], rate_bytes_per_second=slow[0]["rate_bytes_per_second"],
                requests=sum(item["requests"] for item in slow), errors=sum(item["errors"] for item in slow),
                parts_fetched=sum(item["parts_fetched"] for item in slow), error_kinds=kinds,
            )
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
    env = {**env, "BENCH_ARTIFACTS": str(output.with_suffix(".logs"))}
    with output.open("x") as log:
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
    parser.add_argument("--variants", nargs="+", choices=VARIANTS, default=list(VARIANTS))
    parser.add_argument("--viewers", nargs="+", type=int, default=[250, 750])
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--rounds", type=int, default=4, help="a multiple of the treatment count balances order positions")
    parser.add_argument("--mediamtx", type=Path, help="MediaMTX binary: arm64 Mach-O on --platform m4, ELF aarch64 on --platform linux")
    parser.add_argument("--platform", choices=PLATFORMS, default="m4",
                        help="m4: macOS on the Apple M4 Max (default); linux: native aarch64 Linux, e.g. a linux/arm64 container")
    parser.add_argument("--slow", type=int, default=0, help="viewers (of each viewer count) that read slowly, reported apart from the others")
    parser.add_argument("--slow-rate", type=int, help="bytes per second each slow viewer may read; required with --slow")
    args = parser.parse_args()
    if args.rounds < 1 or args.seconds < 10 or any(n < 1 for n in args.viewers):
        parser.error("positive rounds/viewers and at least ten measured seconds are required")
    if len(set(args.variants)) != len(args.variants) or len(set(args.viewers)) != len(args.viewers):
        parser.error("duplicate variants or viewer counts")
    if args.rounds % len(args.variants):
        parser.error("rounds must be a multiple of the treatment count for balanced ordering")
    if "mediamtx" in args.variants and not args.mediamtx:
        parser.error("--mediamtx is required for the mediamtx treatment")
    if args.slow < 0:
        parser.error("--slow cannot be negative")
    if args.slow and (args.slow_rate or 0) < 1:
        parser.error("--slow needs a positive --slow-rate in bytes per second")
    if args.slow_rate is not None and (args.slow_rate < 1 or not args.slow):
        parser.error("--slow-rate must be positive and only applies with --slow N")
    if args.slow and args.slow >= min(args.viewers):
        parser.error("--slow must leave at least one ordinary viewer in the smallest viewer count")
    if args.platform == "m4":
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            parser.error("run with native ARM64 Python on the M4")
    elif platform.system() != "Linux" or platform.machine() != "aarch64":
        parser.error("--platform linux needs native aarch64 Linux, such as a linux/arm64 container")
    for tool in ("cargo", "rustc", "ffmpeg", "lsof", "file"):
        if not shutil.which(tool):
            parser.error("missing tool: " + tool)
    if args.platform == "m4":
        chip = command("sysctl", "-n", "machdep.cpu.brand_string")
        if "M4" not in chip:
            parser.error("this screening requires the M4 test platform")
        host = dict(cpu=chip, logical_cpus=os.cpu_count(), macos=command("sw_vers", "-productVersion"))
        usable_cpus = os.cpu_count() or 4
    else:
        try:
            host = linux_host()
        except ValueError as error:
            parser.error(str(error))
        usable_cpus = host["effective_cpus"]
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
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("SAPFLOW_") and key not in ("SLOW", "SLOW_RATE")}
    env.update(VIEWERS=" ".join(map(str, args.viewers)), SECS=str(args.seconds))
    if args.slow:
        env.update(SLOW=str(args.slow), SLOW_RATE=str(args.slow_rate))
    target = env.get("CARGO_BUILD_TARGET", "")
    release = ROOT / "target" / target / "release"
    own_threads = min(256, usable_cpus)
    http_settings = {}
    for variant in args.variants:
        settings = {}
        if variant != "mediamtx":
            settings["SAPFLOW_HTTP_IMPL"] = "own"
            settings["SAPFLOW_HTTP_THREADS"] = str(own_threads if variant == "own" else int(variant.split("-")[1]))
        http_settings[variant] = settings
    data = dict(
        status="preparing", source=command("git", "rev-parse", "HEAD"),
        tracked_changes=command("git", "status", "--porcelain", "--untracked-files=no"),
        tracked_diff_sha256=hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=ROOT)).hexdigest(),
        runner_sha256=sha256(__file__), bench_script_sha256=sha256(ROOT / "scripts/bench.sh"),
        lock_sha256=sha256(ROOT / "Cargo.lock"), platform=args.platform, **host,
        rustc=command("rustc", "-Vv"),
        ffmpeg=command("ffmpeg", "-version").splitlines()[0], file_descriptor_limit=wanted,
        load_before=os.getloadavg(), measured_seconds=args.seconds, ramp_seconds=10,
        viewers=args.viewers, variants=args.variants, rounds=args.rounds,
        slow_viewers=args.slow, slow_rate_bytes_per_second=args.slow_rate,
        http_settings=http_settings,
        collection="requests started inside a fixed post-ramp interval, with bounded completion drain; warm-up failures retained",
        sampler_tolerance_seconds=max(1.0, args.seconds * .02),
        notes=NOTES[args.platform] + (SLOW_NOTES if args.slow else ""),
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
            if not is_native_arm64(architecture, args.platform):
                raise ValueError(name + " must be a thin native ARM64 binary" + (" (ELF aarch64)" if args.platform == "linux" else ""))
            data["binaries"][name] = dict(sha256=sha256(binary), bytes=binary.stat().st_size, architecture=architecture)
        for round_number in range(args.rounds):
            for variant in trial_order(args.variants, round_number):
                ensure_ports_unused()
                trial_env = env.copy()
                server = "mediamtx" if variant == "mediamtx" else "sapflow"
                trial_env.update(http_settings[variant])
                raw = output / f"round-{round_number + 1}-{variant}.txt"
                print(f"round {round_number + 1}/{args.rounds}: {variant}", flush=True)
                started = time.monotonic()
                before = os.getloadavg()
                status = run_trial(trial_env, server, raw)
                trial = dict(round=round_number + 1, variant=variant, raw=raw.name,
                    diagnostics=raw.with_suffix(".logs").name, runner_exit=status,
                    wall_seconds=time.monotonic() - started, load_before=before,
                    load_after=os.getloadavg(), measurements=[])
                data["runs"].append(trial)
                if status:
                    trial["failure"] = f"runner exit {status}"
                    raise RuntimeError(f"{raw.name}: runner exit {status}")
                try:
                    rows = parse_report(raw.read_text(), args.viewers, expected_server=server,
                        expected_seconds=args.seconds, expected_ramp=10,
                        expected_slow=args.slow, expected_slow_rate=args.slow_rate)
                except ValueError as error:
                    trial["failure"] = str(error)
                    raise
                trial["measurements"] = rows
                data["summary"] = summary(data["runs"])
                data["status"] = "running"
                save(output, data)
                if any(row["errors"] or row["excluded_errors"] for row in rows):
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
