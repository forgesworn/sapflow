#!/usr/bin/env python3
"""Read-only, bounded Android counters for a separately arranged broadcast trial.

Usage: python3 tests/mobile-device-observer.py --serial DEVICE --package
com.example.publisher --duration 120 --interval 5 --output trial.local.jsonl

No app launch, install, settings changes, media capture, system-wide process
listing or raw command output persistence. Only the main package process is
observed; remote services/codec processes are excluded. Requires Python 3.9+ on
POSIX. Output is exclusive-created JSONL; use a private *.local.jsonl path.

References: https://developer.android.com/tools/dumpsys
https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html
"""
import argparse
from datetime import datetime, timezone
import json
import math
import os
import re
import selectors
import subprocess
import time

MAX_OUTPUT = 512 * 1024
CAVEATS = [
    "Read-only counters, not proof of a Sapflow/Larix performance advantage.",
    "Plugged-in battery level/voltage/temperature are not application energy consumption.",
    "ADB and especially dumpsys meminfo add profiler overhead; match interval and tooling across trials.",
    "Match device, codec, resolution, fps, bitrate, protocol, network, recording, duration and thermal starting state separately.",
    "Only the exact main package PID is measured, excluding secondary processes and system codec work.",
    "CPU percent is one-core equivalent (may exceed 100), using host monotonic time between ADB reads; transport latency adds uncertainty.",
    "Metrics are sequential, not an atomic snapshot; null with reason means unavailable, never an inferred zero.",
    "Package version/build fingerprint are identifiers, not an APK content hash or proof of identical configuration.",
]


def metric(value=None, reason="missing_or_unrecognised"):
    return {"value": value, "unavailable_reason": None if value is not None else reason}


def match_metric(pattern, text, convert=int):
    match = re.search(pattern, text, re.MULTILINE)
    return metric(convert(match[1])) if match else metric()


def parse_pid(text):
    fields = text.split()
    if not fields:
        return metric(reason="process_absent")
    if len(fields) != 1 or not re.fullmatch(r"[1-9][0-9]{0,9}", fields[0]):
        return metric(reason="ambiguous_or_invalid_pid")
    return metric(int(fields[0]))


def parse_stat(text, pid):
    # comm can contain spaces and parentheses; field 3 follows the last ')'.
    match = re.fullmatch(r"([0-9]+) \(.*\) (.+)\s*", text.strip())
    if not match or int(match[1]) != pid:
        return None
    fields = match[2].split()
    try:
        user, system, start = (int(fields[i]) for i in (11, 12, 19))
        if min(user, system, start) < 0 or fields[0] not in "RSDZTtWXxKPI":
            return None
        return {"pid": pid, "start_ticks": start, "cpu_ticks": user + system}
    except (IndexError, ValueError):
        return None


def cpu_percent(previous, current, ticks_per_second):
    if current is None:
        return metric(reason="process_stat_unavailable")
    if ticks_per_second is None or ticks_per_second <= 0:
        return metric(reason="clock_ticks_unavailable")
    if previous is None:
        return metric(reason="baseline_required")
    if (previous["pid"], previous["start_ticks"]) != (current["pid"], current["start_ticks"]):
        return metric(reason="process_restarted")
    elapsed = current["at"] - previous["at"]
    delta = current["cpu_ticks"] - previous["cpu_ticks"]
    if elapsed <= 0 or delta < 0:
        return metric(reason="counter_or_clock_regressed")
    return metric(100 * delta / ticks_per_second / elapsed)


def parse_meminfo(text, pid, package):
    header = re.search(r"\*\* MEMINFO in pid (\d+) \[([^\]]+)\] \*\*", text)
    if not header or (int(header[1]), header[2]) != (pid, package):
        return {name: metric(reason="missing_or_mismatched_process") for name in ("pss_kib", "rss_kib")}
    # App Summary totals have stable labels; do not guess legacy table columns.
    return {
        "pss_kib": match_metric(r"\bTOTAL PSS:\s*(\d+)\b", text),
        "rss_kib": match_metric(r"\bTOTAL RSS:\s*(\d+)\b", text),
    }


def parse_battery(text):
    result = {}
    for name in ("AC", "USB", "Wireless", "Dock"):
        result[name.lower() + "_powered"] = match_metric(
            rf"^\s*{name} powered:\s*(true|false)\s*$", text, lambda x: x == "true")
    powers = [v["value"] for v in result.values()]
    result["plugged"] = metric(True) if True in powers else (
        metric(False) if all(v is False for v in powers[:3]) else metric())
    for key in ("level", "scale", "voltage", "temperature", "status", "health"):
        result[key] = match_metric(rf"^\s*{key}:\s*(-?\d+)\s*$", text)
    result["voltage_mv"] = result.pop("voltage")
    temperature = result.pop("temperature")
    result["temperature_c"] = metric(temperature["value"] / 10) if temperature["value"] is not None else temperature
    level, scale = result["level"]["value"], result["scale"]["value"]
    result["level_percent"] = metric(100 * level / scale) if (
        level is not None and scale is not None and 0 <= level <= scale and scale > 0
    ) else metric(reason="missing_or_invalid_level_scale")
    return result


def parse_thermal(text):
    status = match_metric(r"^\s*Thermal Status:\s*([0-6])\s*$", text)
    override = match_metric(r"^\s*IsStatusOverride:\s*(true|false)\s*$", text, lambda x: x == "true")
    return {"status": status, "status_overridden": override}


def parse_package(text, package):
    # Ignore resolver tables, old versions and other package sections.
    block = re.search(r"^  Package \[" + re.escape(package) + r"\][^\n]*\n((?:[ \t]+[^\n]*\n?)*)", text, re.MULTILINE)
    body = block[1] if block else ""
    body = re.split(r"\n  Package \[", body)[0]
    return {
        "version_code": match_metric(r"^\s+versionCode=(\d+)\b", body),
        "version_name": match_metric(r"^\s+versionName=([^\r\n]{1,256})$", body, str),
    }


def run_bounded(argv, timeout, limit=MAX_OUTPUT):
    """No shell, bounded pipe storage, timeout, kill and reap on every exit path."""
    if timeout <= 0:
        return "", "observation_deadline"
    try:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    except OSError:
        return "", "executable_unavailable"
    data = bytearray()
    deadline = time.monotonic() + timeout
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return "", "command_timeout"
                if not selector.select(remaining):
                    return "", "command_timeout"
                chunk = os.read(process.stdout.fileno(), min(8192, limit + 1 - len(data)))
                if not chunk:
                    try:
                        code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
                    except subprocess.TimeoutExpired:
                        return "", "command_timeout"
                    return (data.decode("utf-8", errors="replace"), None) if code == 0 else ("", "command_failed")
                data.extend(chunk)
                if len(data) > limit:
                    return "", "command_output_limit"
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        process.stdout.close()


class Observer:
    def __init__(self, adb, serial, package, timeout, deadline, runner=run_bounded,
                 *, skip_memory=False, skip_device_state=False):
        self.adb, self.serial, self.package = adb, serial, package
        self.timeout, self.deadline, self.runner = timeout, deadline, runner
        self.previous = None
        self.ticks = None
        self.errors = {}
        self.skip_memory = skip_memory
        self.skip_device_state = skip_device_state
        self.clock_error = None

    def read(self, name, *args):
        text, error = self.runner([self.adb, "-s", self.serial, "shell", *args],
                                  min(self.timeout, max(0, self.deadline - time.monotonic())))
        if error:
            self.errors[name] = error
        return text

    def read_metrics(self, name, parse, *args):
        result = parse(self.read(name, *args))
        if name in self.errors:
            return {key: metric(reason=self.errors[name]) for key in result}
        return result

    def identity(self):
        package = self.read_metrics("package", lambda text: parse_package(text, self.package),
                                    "dumpsys", "package", self.package)
        ticks = self.read("clock_ticks", "getconf", "CLK_TCK").strip()
        self.ticks = int(ticks) if re.fullmatch(r"[1-9]\d{0,6}", ticks) else None
        self.clock_error = self.errors.get("clock_ticks", "missing_or_unrecognised")
        fingerprint = self.read("build_fingerprint", "getprop", "ro.build.fingerprint").strip()
        return {"package": self.package, **package,
                "android_build_fingerprint": metric(fingerprint) if 0 < len(fingerprint) <= 512 else metric(reason=self.errors.get("build_fingerprint", "missing_or_unrecognised")),
                "clock_ticks_per_second": metric(self.ticks, self.clock_error), "command_errors": dict(self.errors)}

    def sample(self):
        self.errors = {}
        started = time.monotonic()
        pid = parse_pid(self.read("pid", "pidof", self.package))
        if "pid" in self.errors:
            pid = metric(reason=self.errors["pid"])
        current = None
        stat_reason = pid["unavailable_reason"] or "process_stat_unavailable"
        memory = {name: metric(reason="skipped_by_option" if self.skip_memory else stat_reason)
                  for name in ("pss_kib", "rss_kib")}
        if pid["value"] is not None:
            number = pid["value"]
            current = parse_stat(self.read("stat", "cat", f"/proc/{number}/stat"), number)
            stat_reason = self.errors.get("stat", "process_stat_unavailable")
            if current is not None:
                current["at"] = time.monotonic()
            if not self.skip_memory:
                memory = self.read_metrics("memory", lambda text: parse_meminfo(text, number, self.package),
                                           "dumpsys", "meminfo", "-s", str(number))
                after = parse_stat(self.read("stat_after", "cat", f"/proc/{number}/stat"), number)
            else:
                # No intervening memory read to bracket. The one stat result
                # already contains the PID and lifetime for CPU delta checks.
                after = current
            # Memory has its own scoped PID/package header. Keep it when procfs
            # is denied, but discard it if an observed process lifetime changes.
            if current is not None and after is not None and current["start_ticks"] != after["start_ticks"]:
                memory = {name: metric(reason="process_restarted_during_collection") for name in memory}
                stat_reason = "process_restarted_during_collection"
            if current is None or after is None or current["start_ticks"] != after["start_ticks"]:
                current = None
                stat_reason = self.errors.get("stat", self.errors.get("stat_after", stat_reason))
        cpu = cpu_percent(self.previous, current, self.ticks)
        if current is None:
            cpu = metric(reason=stat_reason)
        elif self.ticks is None and self.clock_error:
            cpu = metric(reason=self.clock_error)
        self.previous = current
        if self.skip_device_state:
            battery = {key: metric(reason="skipped_by_option") for key in parse_battery("")}
            thermal = {key: metric(reason="skipped_by_option") for key in parse_thermal("")}
        else:
            battery = self.read_metrics("battery", parse_battery, "dumpsys", "battery")
            thermal = self.read_metrics("thermal", parse_thermal, "dumpsys", "thermalservice")
        return {"type": "sample", "observed_at_utc": utc_now(), "host_monotonic_s": started,
                "collection_duration_s": time.monotonic() - started, "pid": pid,
                "cpu_ticks": metric(current["cpu_ticks"]) if current else metric(reason=stat_reason),
                "process_start_ticks": metric(current["start_ticks"]) if current else metric(reason=stat_reason),
                "cpu_percent_one_core": cpu, **memory, "battery": battery, "thermal": thermal,
                "command_errors": dict(self.errors)}


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def bounded_number(minimum, maximum):
    def parse(value):
        number = float(value)
        if not math.isfinite(number) or not minimum <= number <= maximum:
            raise argparse.ArgumentTypeError(f"must be between {minimum} and {maximum}")
        return number
    return parse


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--adb", default="adb")
    parser.add_argument("--serial", required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--duration", type=bounded_number(1, 86400), default=120)
    parser.add_argument("--interval", type=bounded_number(1, 3600), default=5)
    parser.add_argument("--timeout", type=bounded_number(0.1, 10), default=3)
    parser.add_argument("--output", required=True)
    parser.add_argument("--skip-memory", action="store_true", help="omit per-sample dumpsys meminfo")
    parser.add_argument("--skip-device-state", action="store_true", help="omit per-sample battery and thermal dumpsys")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+", args.package):
        parser.error("package must be an Android package identifier")
    started = time.monotonic()
    observer = Observer(args.adb, args.serial, args.package, args.timeout, started + args.duration,
                        skip_memory=args.skip_memory, skip_device_state=args.skip_device_state)
    # Exclusive create: never overwrite another run or an existing private file.
    with open(args.output, "x", encoding="utf-8") as output:
        def emit(record):
            output.write(json.dumps(record, allow_nan=False) + "\n")
            output.flush()
        emit({"type": "metadata", "schema": 1, "started_at_utc": utc_now(),
              "duration_s": args.duration, "interval_s": args.interval, "command_timeout_s": args.timeout,
              "skip_memory": args.skip_memory, "skip_device_state": args.skip_device_state,
              "identity": observer.identity(), "caveats": CAVEATS})
        count, interrupted = 0, False
        try:
            while time.monotonic() < observer.deadline:
                emit(observer.sample())
                count += 1
                # Skip missed intervals; never issue catch-up bursts.
                next_at = started + (math.floor((time.monotonic() - started) / args.interval) + 1) * args.interval
                time.sleep(max(0, min(next_at, observer.deadline) - time.monotonic()))
        except KeyboardInterrupt:
            interrupted = True
        emit({"type": "end", "ended_at_utc": utc_now(), "elapsed_s": time.monotonic() - started,
              "samples": count, "interrupted": interrupted})


if __name__ == "__main__":
    main()
