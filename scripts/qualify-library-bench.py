#!/usr/bin/env python3
"""Qualify one library change with its benchmark example on an idle Linux runner.

Each benchmark names the one runtime file its campaign may change. Both arms
build the same example in this checkout's target/, with that file taken from
the baseline or the candidate, then run in four balanced pairs at fixed work.
Every row is retained; an invalid or failed campaign is not retried here.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import statistics
import subprocess
import time

_SPEC = importlib.util.spec_from_file_location(
    "qualify_srt_gap", Path(__file__).resolve().parent / "qualify-srt-gap.py")
HOST = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(HOST)

BENCHES = {
    "ts-ingest": {"package": "sapflow-hls", "example": "ts-ingest-bench",
                  "runtime": "crates/sapflow-hls/src/ingest.rs", "rounds": 40_000,
                  "operations_per_round": 428},
    "chunk-reassembly": {"package": "sapflow-rtmp", "example": "chunk-reassembly-bench",
                         "runtime": "crates/sapflow-rtmp/src/chunk.rs", "rounds": 30_000_000,
                         "operations_per_round": 1},
}
PAIRS = 4
MIN_SECONDS = 3.0
MAX_SECONDS = 180.0
MEDIAN_LIMIT = 0.95
PAIR_LIMIT = 1.0


def validate_sample(sample, name):
    spec = BENCHES[name]
    if not isinstance(sample, dict):
        raise ValueError("benchmark result must be a JSON object")
    for key in ("operations", "elapsed_ns"):
        if type(sample.get(key)) is not int or sample[key] <= 0:
            raise ValueError(f"invalid {key}")
    if sample.get("profile") != name:
        raise ValueError("profile mismatch")
    if sample["operations"] != spec["rounds"] * spec["operations_per_round"]:
        raise ValueError("fixed-work mismatch")


def qualify(rows, name):
    expected = [(pair, arm) for pair in range(1, PAIRS + 1)
                for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline"))]
    if [(row.get("pair"), row.get("arm")) for row in rows] != expected:
        return {"passed": False, "reason": "incomplete or unbalanced campaign"}
    for row in rows:
        try:
            validate_sample(row["result"], name)
        except (KeyError, ValueError, TypeError):
            return {"passed": False, "reason": "invalid result"}
        if row.get("exit") != 0 or row.get("host", {}).get("valid") is not True:
            return {"passed": False, "reason": "invalid exit or host observation"}
    lookup = {(row["pair"], row["arm"]): row["result"]["elapsed_ns"] for row in rows}
    ratios = [lookup[pair, "candidate"] / lookup[pair, "baseline"] for pair in range(1, PAIRS + 1)]
    shortest = min(lookup.values()) / 1e9
    summary = {"ratios": ratios, "median_ratio": statistics.median(ratios), "maximum_ratio": max(ratios),
               "minimum_seconds": shortest,
               "median_pass": statistics.median(ratios) <= MEDIAN_LIMIT,
               "every_pair_pass": max(ratios) <= PAIR_LIMIT,
               "duration_pass": shortest >= MIN_SECONDS}
    summary["passed"] = all(summary[key] for key in ("median_pass", "every_pair_pass", "duration_pass"))
    return summary


def build_arm(name, source, executable, report, label):
    spec = BENCHES[name]
    Path(spec["runtime"]).write_bytes(source)
    command = ["cargo", "build", "--locked", "--release", "-p", spec["package"],
               "--example", spec["example"], "-j", "2"]
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, text=True, timeout=900)
    report.setdefault("builds", []).append({"arm": label, "command": command, "exit": result.returncode,
                                            "seconds": time.monotonic() - started})
    if result.returncode:
        report["builds"][-1]["failure_tail"] = result.stderr[-3000:].replace(str(Path.cwd()), "<checkout>")
        raise ValueError(f"{label} build failed")
    shutil.copy2(f"target/release/examples/{spec['example']}", executable)
    report["identities"][label] = {"runtime_sha256": HOST.digest(source),
                                   "binary_sha256": HOST.digest(executable.read_bytes())}


def trial(executable, name, arm, pair):
    command = [str(executable), "--rounds", str(BENCHES[name]["rounds"])]
    row = {"arm": arm, "pair": pair, "load_before": os.getloadavg()}
    before = HOST.cpu_snapshot()
    child_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.monotonic()
    try:
        run = subprocess.run(command, capture_output=True, text=True, timeout=MAX_SECONDS)
        row["exit"] = run.returncode
        if run.returncode:
            row["failure_tail"] = run.stderr[-3000:]
        else:
            row["result"] = json.loads(run.stdout)
            validate_sample(row["result"], name)
    except subprocess.TimeoutExpired:
        row["exit"] = None
        row["failure_tail"] = "benchmark exceeded bounded process deadline"
    except (ValueError, TypeError) as error:
        row["validation_error"] = str(error)
    wall = time.monotonic() - start
    child_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = child_after.ru_utime + child_after.ru_stime - child_before.ru_utime - child_before.ru_stime
    row["host"] = HOST.host_observation(before, HOST.cpu_snapshot(), wall, cpu, os.sysconf("SC_CLK_TCK"))
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bench", required=True, choices=sorted(BENCHES))
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
    spec = BENCHES[args.bench]
    runtime = Path(spec["runtime"])
    candidate = runtime.read_bytes()
    example = Path(f"crates/{spec['package']}/examples/{spec['example']}.rs")
    report = {"schema": "sapflow-library-bench-qualification-v1", "status": "running", "bench": args.bench,
              "baseline_revision": args.baseline, "candidate_revision": args.candidate,
              "protocol": {"pairs": PAIRS, "order": ["AB", "BA", "AB", "BA"], "fixed_work": spec,
                           "median_limit": MEDIAN_LIMIT, "pair_limit": PAIR_LIMIT,
                           "minimum_seconds": MIN_SECONDS,
                           "maximum_steal_fraction": HOST.MAX_STEAL,
                           "maximum_external_busy_cores": HOST.MAX_EXTERNAL_CORES},
              "environment": {"platform": platform.platform(), "machine": platform.machine(),
                              "logical_cpus": os.cpu_count(), "cpu_model": HOST.cpu_model()},
              "identities": {}, "rows": [],
              "limitations": ["Library elapsed time, not network or whole-server performance.",
                              "Fixed work chosen before the campaign; no retry or replacement."]}
    scratch = Path("target/library-bench-qualification-binaries")
    binaries = {arm: scratch / arm for arm in ("baseline", "candidate")}
    created = False
    try:
        if platform.system() != "Linux" or HOST.cpu_snapshot() is None:
            raise ValueError("qualification requires measurable Linux CPU accounting")
        if HOST.git("rev-parse", "HEAD") != args.candidate:
            raise ValueError("checked-out HEAD does not match the pinned candidate")
        if os.environ.get("CARGO_TARGET_DIR"):
            raise ValueError("qualification requires this checkout's own target directory")
        if HOST.git("status", "--porcelain", "--untracked-files=no"):
            raise ValueError("tracked checkout must be clean before qualification")
        package = f"crates/{spec['package']}"
        changed = set(HOST.git("diff", "--name-only", args.baseline, "--", "Cargo.toml", "Cargo.lock",
                               f"{package}/Cargo.toml", f"{package}/src").splitlines())
        if changed != {spec["runtime"]}:
            raise ValueError("baseline differs beyond the runtime file under test")
        report["identities"]["example_sha256"] = HOST.digest(example.read_bytes())
        report["identities"]["runner_sha256"] = HOST.digest(Path(__file__).read_bytes())
        report["environment"]["rustc"] = subprocess.check_output(["rustc", "-vV"], text=True).strip()
        scratch.mkdir(parents=True, exist_ok=False)
        created = True
        baseline = subprocess.check_output(["git", "show", f"{args.baseline}:{spec['runtime']}"])
        try:
            build_arm(args.bench, baseline, binaries["baseline"], report, "baseline")
            build_arm(args.bench, candidate, binaries["candidate"], report, "candidate")
        finally:
            runtime.write_bytes(candidate)
        for pair in range(1, PAIRS + 1):
            for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline")):
                row = trial(binaries[arm], args.bench, arm, pair)
                report["rows"].append(row)
                if row.get("exit") != 0 or "validation_error" in row or not row["host"]["valid"]:
                    raise ValueError("trial failed validation or host accounting; not retrying")
        report["qualification"] = qualify(report["rows"], args.bench)
        report["status"] = "passed" if report["qualification"]["passed"] else "failed"
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        report["status"] = "invalid"
        report["error"] = str(error).replace(str(Path.cwd()), "<checkout>")
    finally:
        runtime.write_bytes(candidate)
        if created:
            shutil.rmtree(scratch)
        output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"bench": args.bench, "status": report["status"],
                      "qualification": report.get("qualification"), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
