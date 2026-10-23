#!/usr/bin/env python3
"""Sample process CPU and RSS against a monotonic deadline, without poll drift."""

import argparse
import os
import subprocess
import time


def cpu_seconds(value):
    days, separator, clock = value.partition("-")
    result = 0
    for part in (clock if separator else value).split(":"):
        result = result * 60 + float(part)
    return result + (int(days) * 86400 if separator else 0)


def sample_process(pid):
    output = subprocess.check_output(
        ["ps", "-o", "time=,rss=", "-p", str(pid)],
        text=True, env={**os.environ, "LC_ALL": "C"},
    ).split()
    if len(output) != 2:
        raise RuntimeError("process disappeared or ps returned an unexpected sample")
    return cpu_seconds(output[0]), int(output[1]) / 1024


def measure(pid, seconds, *, clock=time.monotonic, sleep=time.sleep, sample=sample_process):
    before = clock()
    first_cpu, peak = sample(pid)
    started = (before + clock()) / 2
    deadline = started + seconds
    while True:
        remaining = deadline - clock()
        if remaining > 0:
            sleep(min(1, remaining))
        before = clock()
        cpu, rss = sample(pid)
        finished = (before + clock()) / 2
        peak = max(peak, rss)
        if finished >= deadline:
            elapsed = finished - started
            return 100 * (cpu - first_cpu) / elapsed, peak, elapsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int)
    parser.add_argument("seconds", type=float)
    args = parser.parse_args()
    if args.pid < 1 or args.seconds <= 0:
        parser.error("positive process ID and sample duration required")
    cpu, peak, elapsed = measure(args.pid, args.seconds)
    print(f"{cpu:.1f} {peak:.1f} {elapsed:.3f}")


if __name__ == "__main__":
    main()
