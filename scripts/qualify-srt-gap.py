#!/usr/bin/env python3
"""Qualify the SRT receive-slot cache on one otherwise idle Linux runner.

Build both arms sequentially in this checkout's target/. One fixed pilot per
arm/profile chooses common work counts before four balanced measured pairs.
Every result is retained; invalid or failed campaigns are never retried here.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import statistics
import subprocess
import time


RECEIVER = "crates/sapflow-srt/src/receiver.rs"
EXAMPLE = "crates/sapflow-srt/examples/receiver-gap-bench.rs"
PROFILES = {
    # Fixed before the campaign: lengthen the fast-arm pilots so scheduler tick
    # quantisation is small relative to the unchanged background-CPU guard.
    "clean-stream": {"pilot": 20_000_000, "cap": 500_000_000, "multiplier": 1},
    "pending-and-timer": {"pilot": 1_000_000_000, "cap": 8_000_000_000, "multiplier": 1},
    "bursty-stream": {"pilot": 32_768, "cap": 1_000_000, "multiplier": 128},
}
PAIRS = 4
TARGET_SECONDS = 5.0
MIN_SECONDS = 3.0
MAX_SECONDS = 120.0
MAX_STEAL = 0.005
MAX_EXTERNAL_CORES = 0.10


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def cpu_snapshot():
    try:
        fields = Path("/proc/stat").read_text().splitlines()[0].split()
        if fields[0] != "cpu" or len(fields) < 9:
            return None
        return [int(value) for value in fields[1:9]]
    except (OSError, ValueError):
        return None


def host_observation(before, after, wall, child_cpu, ticks):
    if before is None or after is None or wall <= 0 or ticks <= 0:
        return {"valid": False, "reason": "Linux CPU accounting unavailable"}
    delta = [end - start for start, end in zip(before, after)]
    if len(delta) != 8 or any(value < 0 for value in delta) or sum(delta) == 0:
        return {"valid": False, "reason": "invalid Linux CPU accounting delta"}
    busy = sum(delta[index] for index in (0, 1, 2, 5, 6)) / ticks
    # /proc/stat is quantised in clock ticks, while wait4 child CPU is finer.
    if busy + 2 / ticks < child_cpu:
        return {"valid": False, "reason": "system CPU delta is below child CPU"}
    steal = delta[7] / sum(delta)
    external = max(0.0, busy - child_cpu) / wall
    return {
        "valid": steal <= MAX_STEAL and external <= MAX_EXTERNAL_CORES,
        "steal_fraction": steal,
        "external_busy_cores": external,
        "wall_seconds": wall,
        "child_cpu_seconds": child_cpu,
        "system_busy_seconds": busy,
        "cpu_tick_delta": delta,
    }


def validate_sample(sample, profile, work):
    if not isinstance(sample, dict):
        raise ValueError("benchmark result must be a JSON object")
    expected_operations = work * PROFILES[profile]["multiplier"]
    expected_deliveries = 1 if profile == "pending-and-timer" else expected_operations
    expected_gap = 4096 if profile == "bursty-stream" else 0
    for key in ("operations", "elapsed_ns", "verified_deliveries"):
        if type(sample.get(key)) is not int or sample[key] <= 0:
            raise ValueError(f"invalid {key}")
    if sample.get("profile") != profile or sample["operations"] != expected_operations:
        raise ValueError("profile or fixed-work mismatch")
    if sample["verified_deliveries"] != expected_deliveries or sample.get("gap", 0) != expected_gap:
        raise ValueError("verified delivery or gap mismatch")
    if profile != "pending-and-timer" and sample.get("payload_bytes") != 1200:
        raise ValueError("payload-size mismatch")


def choose_work(profile, pilots):
    if set(pilots) != {"baseline", "candidate"}:
        raise ValueError("both pilot arms are required")
    spec = PROFILES[profile]
    for sample in pilots.values():
        validate_sample(sample, profile, spec["pilot"])
    durations = [sample["elapsed_ns"] / 1e9 for sample in pilots.values()]
    count = max(spec["pilot"], math.ceil(spec["pilot"] * TARGET_SECONDS / min(durations)))
    predicted = max(durations) * count / spec["pilot"]
    if count > spec["cap"] or predicted > MAX_SECONDS:
        raise ValueError("pilot predicts work beyond the safety count or time cap")
    return {"work": count, "predicted_slower_seconds": predicted, "pilot_seconds": durations}


def qualify(rows, workloads):
    summaries = {}
    if len(rows) != len(PROFILES) * PAIRS * 2 or set(workloads) != set(PROFILES):
        return {"passed": False, "reason": "incomplete campaign", "profiles": summaries}
    for profile in PROFILES:
        samples = [row for row in rows if row.get("profile") == profile]
        expected = {(pair, arm) for pair in range(1, PAIRS + 1) for arm in ("baseline", "candidate")}
        keys = [(row.get("pair"), row.get("arm")) for row in samples]
        if len(keys) != len(expected) or set(keys) != expected:
            return {"passed": False, "reason": "missing or duplicate pair arm", "profiles": summaries}
        order = [(pair, arm) for pair in range(1, PAIRS + 1)
                 for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline"))]
        if keys != order:
            return {"passed": False, "reason": "unbalanced arm order", "profiles": summaries}
        valid = True
        for row in samples:
            try:
                validate_sample(row["result"], profile, workloads[profile]["work"])
            except (KeyError, ValueError, TypeError):
                valid = False
            valid &= row.get("exit") == 0 and row.get("host", {}).get("valid") is True
        if not valid:
            return {"passed": False, "reason": "invalid result or host observation", "profiles": summaries}
        lookup = {(row["pair"], row["arm"]): row["result"] for row in samples}
        ratios = [lookup[pair, "candidate"]["elapsed_ns"] / lookup[pair, "baseline"]["elapsed_ns"]
                  for pair in range(1, PAIRS + 1)]
        durations = [row["result"]["elapsed_ns"] / 1e9 for row in samples]
        burst = profile == "bursty-stream"
        summaries[profile] = {
            "ratios": ratios,
            "median_ratio": statistics.median(ratios),
            "maximum_ratio": max(ratios),
            "minimum_seconds": min(durations),
            "median_pass": statistics.median(ratios) <= (0.95 if burst else 1.05),
            "every_pair_pass": max(ratios) <= (1.0 if burst else 1.10),
            "duration_pass": min(durations) >= MIN_SECONDS,
        }
    return {"passed": all(all(row[key] for key in ("median_pass", "every_pair_pass", "duration_pass"))
                          for row in summaries.values()), "profiles": summaries}


def save(path, report):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(path)


def build_arm(source, executable, receiver, output, report, label):
    receiver.write_bytes(source)
    command = ["cargo", "build", "--locked", "--release", "-p", "sapflow-srt",
               "--example", "receiver-gap-bench", "-j", "2"]
    log = output.parent / "srt-build.log"
    started = time.monotonic()
    try:
        with log.open("w") as stream:
            result = subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT, timeout=600)
        report.setdefault("builds", []).append({"arm": label, "command": command, "exit": result.returncode,
                                                "seconds": time.monotonic() - started})
        if result.returncode:
            report["builds"][-1]["failure_tail"] = "\n".join(log.read_text().splitlines()[-25:]).replace(str(Path.cwd()), "<checkout>")
            raise ValueError(f"{label} build failed")
        shutil.copy2("target/release/examples/receiver-gap-bench", executable)
        report["identities"][label] = {"receiver_sha256": digest(source),
                                       "binary_sha256": digest(executable.read_bytes())}
    finally:
        log.unlink(missing_ok=True)


def trial(executable, profile, work, arm, phase, pair):
    command = [str(executable), "--profile", profile, "--work", str(work)]
    row = {"profile": profile, "arm": arm, "phase": phase, "pair": pair, "work": work}
    row["load_before"] = os.getloadavg()
    before = cpu_snapshot()
    child_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.monotonic()
    try:
        run = subprocess.run(command, capture_output=True, text=True, timeout=MAX_SECONDS + 15)
        row["exit"] = run.returncode
        if run.returncode:
            row["failure_tail"] = run.stderr[-3000:]
        else:
            try:
                row["result"] = json.loads(run.stdout)
                validate_sample(row["result"], profile, work)
            except (ValueError, TypeError) as error:
                row["validation_error"] = str(error)
    except subprocess.TimeoutExpired:
        row["exit"] = None
        row["failure_tail"] = "benchmark exceeded bounded process deadline"
    wall = time.monotonic() - start
    child_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    after = cpu_snapshot()
    cpu = child_after.ru_utime + child_after.ru_stime - child_before.ru_utime - child_before.ru_stime
    row["host"] = host_observation(before, after, wall, cpu, os.sysconf("SC_CLK_TCK"))
    row["load_after"] = os.getloadavg()
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="full immutable baseline commit")
    parser.add_argument("--candidate", required=True, help="full immutable checked-out candidate commit")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        parser.error("refusing to overwrite an existing qualification receipt")
    for revision in (args.baseline, args.candidate):
        if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
            parser.error("revisions must be full lowercase commit hashes")
    output.parent.mkdir(parents=True, exist_ok=True)
    receiver = Path(RECEIVER)
    candidate = receiver.read_bytes()
    report = {"schema": "sapflow-srt-linux-qualification-v1", "status": "running",
              "baseline_revision": args.baseline, "candidate_revision": args.candidate,
              "protocol": {"pairs": PAIRS, "order": ["AB", "BA", "AB", "BA"],
                           "pilot": "one fixed pilot per arm/profile; common work frozen before timing",
                           "target_seconds": TARGET_SECONDS, "minimum_seconds": MIN_SECONDS,
                           "maximum_predicted_seconds": MAX_SECONDS, "profiles": PROFILES,
                           "nominal_median_limit": 1.05, "nominal_pair_limit": 1.10,
                           "burst_median_limit": 0.95, "burst_pair_limit": 1.0,
                           "maximum_steal_fraction": MAX_STEAL, "maximum_external_busy_cores": MAX_EXTERNAL_CORES},
              "environment": {"platform": platform.platform(), "machine": platform.machine(),
                              "logical_cpus": os.cpu_count()},
              "identities": {"example_sha256": digest(Path(EXAMPLE).read_bytes())},
              "pilots": [], "workloads": {}, "rows": [],
              "limitations": ["Library elapsed time, not network or whole-server performance.",
                              "All calibration and measured aggregate rows retained; no retry or replacement.",
                              "No CPU affinity changes; Linux system/child CPU accounting gates shared-host noise."]}
    scratch = Path("target/srt-gap-qualification-binaries")
    binaries = {arm: scratch / arm for arm in ("baseline", "candidate")}
    created = False
    try:
        if platform.system() != "Linux" or cpu_snapshot() is None:
            raise ValueError("strong qualification requires measurable Linux CPU accounting")
        if git("rev-parse", "HEAD") != args.candidate:
            raise ValueError("checked-out HEAD does not match the pinned candidate")
        if os.environ.get("CARGO_TARGET_DIR"):
            raise ValueError("qualification requires this checkout's own target directory")
        if git("status", "--porcelain", "--untracked-files=no"):
            raise ValueError("tracked checkout must be clean before qualification")
        changed = set(git("diff", "--name-only", args.baseline, "--", "Cargo.toml", "Cargo.lock",
                          "crates/sapflow-srt/Cargo.toml", "crates/sapflow-srt/src").splitlines())
        if changed != {RECEIVER}:
            raise ValueError("baseline differs beyond the receiver runtime under test")
        for path in ("Cargo.toml", "Cargo.lock", "crates/sapflow-srt/Cargo.toml"):
            report["identities"][path] = digest(Path(path).read_bytes())
        report["identities"]["common_source_sha256"] = {
            str(path): digest(path.read_bytes()) for path in sorted(Path("crates/sapflow-srt/src").rglob("*.rs"))
            if str(path) != RECEIVER}
        report["identities"]["runner_sha256"] = digest(Path(__file__).read_bytes())
        report["environment"]["rustc"] = subprocess.check_output(["rustc", "-vV"], text=True).strip()
        report["environment"]["build_env"] = {key: os.environ.get(key) for key in
                                                 ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_INCREMENTAL")}
        scratch.mkdir(parents=True, exist_ok=False)
        created = True
        baseline = subprocess.check_output(["git", "show", f"{args.baseline}:{RECEIVER}"])
        try:
            build_arm(baseline, binaries["baseline"], receiver, output, report, "baseline")
            build_arm(candidate, binaries["candidate"], receiver, output, report, "candidate")
        finally:
            receiver.write_bytes(candidate)
        save(output, report)
        for profile, spec in PROFILES.items():
            pilots = {}
            for arm in ("baseline", "candidate"):
                row = trial(binaries[arm], profile, spec["pilot"], arm, "pilot", 0)
                report["pilots"].append(row)
                save(output, report)
                if row["exit"] != 0 or "validation_error" in row or not row["host"]["valid"]:
                    raise ValueError("pilot failed validation or host accounting; not retrying")
                pilots[arm] = row["result"]
            report["workloads"][profile] = choose_work(profile, pilots)
        save(output, report)
        for pair in range(1, PAIRS + 1):
            for profile in PROFILES:
                for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline")):
                    row = trial(binaries[arm], profile, report["workloads"][profile]["work"], arm, "measured", pair)
                    report["rows"].append(row)
                    save(output, report)
                    if row["exit"] != 0 or "validation_error" in row:
                        raise ValueError("measured trial failed; not retrying")
                    if not row["host"]["valid"] or row["result"]["elapsed_ns"] < MIN_SECONDS * 1e9:
                        raise ValueError("measured host or duration gate invalid; not retrying")
        if digest(Path(EXAMPLE).read_bytes()) != report["identities"]["example_sha256"] or receiver.read_bytes() != candidate:
            raise ValueError("benchmark or receiver source changed during qualification")
        for arm, executable in binaries.items():
            if digest(executable.read_bytes()) != report["identities"][arm]["binary_sha256"]:
                raise ValueError("benchmark executable changed during qualification")
        report["qualification"] = qualify(report["rows"], report["workloads"])
        report["status"] = "passed" if report["qualification"]["passed"] else "failed"
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report["status"] = "invalid"
        report["error"] = str(error).replace(str(Path.cwd()), "<checkout>")
    finally:
        receiver.write_bytes(candidate)
        if created:
            shutil.rmtree(scratch)
        save(output, report)
    print(json.dumps({"status": report["status"], "qualification": report.get("qualification"),
                      "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
