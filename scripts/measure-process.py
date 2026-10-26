#!/usr/bin/env python3
"""Sample process CPU and RSS against a monotonic deadline, without poll drift."""

import argparse
import os
from pathlib import Path
import subprocess
import sys
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


def parse_proc_stat(text, clock_ticks):
    """CPU seconds (utime + stime) from the text of /proc/<pid>/stat.

    The command name is in parentheses and may hold spaces or parentheses, so
    the fields are counted from the last closing one: the state is field 3,
    utime and stime fields 14 and 15, in clock ticks.
    """
    end = text.rfind(")")
    rest = text[end + 1:].split()
    try:
        if end < 0:
            raise ValueError("no command name")
        return (int(rest[11]) + int(rest[12])) / clock_ticks
    except (IndexError, ValueError):
        raise RuntimeError("unexpected /proc stat sample") from None


def parse_vm_rss(text):
    """Resident set size in MiB from the text of /proc/<pid>/status (VmRSS, in KiB)."""
    for line in text.splitlines():
        if line.startswith("VmRSS:"):
            fields = line.split()
            if len(fields) != 3 or fields[2] != "kB":
                break
            try:
                return int(fields[1]) / 1024
            except ValueError:
                break
    raise RuntimeError("process has no resident set size in /proc status")


def sample_proc(pid, *, root="/proc", clock_ticks=None):
    """The Linux sample: CPU from /proc/<pid>/stat, RSS from /proc/<pid>/status.

    Returns what sample_process does: CPU seconds and RSS in MiB.
    """
    if clock_ticks is None:
        clock_ticks = os.sysconf("SC_CLK_TCK")
    base = Path(root) / str(pid)
    try:
        stat = (base / "stat").read_text()
        status = (base / "status").read_text()
    except (FileNotFoundError, ProcessLookupError):
        raise RuntimeError("process disappeared or /proc returned an unexpected sample") from None
    return parse_proc_stat(stat, clock_ticks), parse_vm_rss(status)


def platform_sampler(platform=sys.platform):
    return sample_proc if platform.startswith("linux") else sample_process


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
    cpu, peak, elapsed = measure(args.pid, args.seconds, sample=platform_sampler())
    print(f"{cpu:.1f} {peak:.1f} {elapsed:.3f}")


if __name__ == "__main__":
    main()
