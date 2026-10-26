"""Refuse misleading benchmark receipts without building or running a server."""

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("performance", Path(__file__).resolve().parents[1] / "scripts/performance.py")
performance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(performance)

REPORT = """--- sapflow, 750 viewers: server CPU 34.6%  peak RSS 27.2 MB
server sampling 60.010 s target 60 s
viewers 750  measured 60 s after a 10 s ramp
collection requests started within the fixed post-ramp window, including drain
excluded requests 500  errors 0
drained requests 2 max 0.030000 s
requests 588290  errors 0  received 2000.0 MB
playlist  n=45000  p50 8.5 ms  p95 20.0 ms  p99 26.9 ms  max 60.0 ms
segment   n=9000   p50 5.0 ms  p95 15.0 ms  p99 22.0 ms  max 62.0 ms
part fan-out (first to last viewer) parts=300 reach 744/750  p50 27.7 ms  p95 45.5 ms  max 65.5 ms
"""

SLOW = """slow viewers 5 of 750  rate 50000 B/s each; the figures above cover the other 745
slow excluded requests 10  errors 0
slow requests 400  errors 30  received 21.5 MB
  slow error part: HTTP 404: 20
  slow error segment: body timeout: 10
slow playlist n=120    p50 12.0 ms  p99 80.5 ms
slow segment  n=0      p50 NaN ms  p99 NaN ms
slow parts fetched 150
"""
# With five slow viewers the figures above are the other 745 viewers'.
SLOW_REPORT = REPORT.replace("reach 744/750", "reach 739/745") + SLOW


def parse_report(report, viewers, **options):
    return performance.parse_report(report, viewers, **{
        "expected_server": "sapflow", "expected_seconds": 60,
        "expected_ramp": 10, **options,
    })


class PerformanceTests(unittest.TestCase):
    def test_incomplete_failed_and_empty_client_output_are_refused(self):
        for report in ("", REPORT.split("requests")[0], REPORT.replace("n=9000", "n=0"), REPORT.replace("parts=300", "parts=0")):
            with self.subTest(report=report), self.assertRaises(ValueError):
                parse_report(report, [750])

    def test_unexpected_missing_and_duplicate_loads_are_refused(self):
        for report, viewers in ((REPORT, [250, 750]), (REPORT, [250]), (REPORT * 2, [750])):
            with self.subTest(viewers=viewers), self.assertRaises(ValueError):
                parse_report(report, viewers)

    def test_errors_are_retained_for_failure_decision(self):
        row = parse_report(REPORT.replace("errors 0", "errors 3"), [750])[0]
        self.assertEqual(row["errors"], 3)
        self.assertEqual(row["requests"], 588290)
        self.assertEqual(row["part_reach"], 744)

    def test_old_ramp_inclusive_reports_are_refused_and_excluded_errors_survive(self):
        with self.assertRaises(ValueError):
            parse_report(REPORT.replace("collection requests started within the fixed post-ramp window, including drain", "collection completed requests wholly within the fixed post-ramp window"), [750])
        row = parse_report(REPORT.replace("excluded requests 500  errors 0", "excluded requests 500  errors 2"), [750])[0]
        self.assertEqual((row["errors"], row["excluded_errors"]), (0, 2))
        self.assertEqual((row["drained_requests"], row["max_drain_seconds"]), (2, .03))

    def test_full_cycle_balances_every_treatment_position(self):
        treatments = ["own-1", "own-4", "own", "mediamtx"]
        orders = [performance.trial_order(treatments, n) for n in range(len(treatments))]
        for column in zip(*orders):
            self.assertEqual(set(column), set(treatments))

    def test_pair_order_reverses_and_three_treatments_rotate(self):
        self.assertEqual(performance.trial_order(["own", "own-1"], 1), ["own-1", "own"])
        treatments = ["own", "own-1", "mediamtx"]
        self.assertEqual([performance.trial_order(treatments, n)[0] for n in range(3)], treatments)

    def test_summary_retains_spread_and_sums_failures(self):
        rows = parse_report(REPORT, [750])
        runs = [dict(variant="own", measurements=rows), dict(variant="own", measurements=[dict(rows[0], cpu_percent=40.0, errors=1)])]
        result = performance.summary(runs)[0]
        self.assertEqual(result["cpu_percent"], dict(median=37.3, minimum=34.6, maximum=40.0))
        self.assertEqual(result["errors"], 1)
        self.assertEqual(result["requests"], 1176580)

    def test_server_and_declared_client_workload_must_match_the_plan(self):
        for before, after in [
            ("--- sapflow,", "--- mediamtx,"),
            ("viewers 750  measured", "viewers 1  measured"),
            ("measured 60 s", "measured 1 s"),
            ("after a 10 s ramp", "after a 0 s ramp"),
            ("reach 744/750", "reach 744/1000"),
            ("reach 744/750", "reach 0/750"),
            ("reach 744/750", "reach 751/750"),
        ]:
            with self.subTest(after=after), self.assertRaises(ValueError):
                parse_report(REPORT.replace(before, after), [750])
        row = parse_report(REPORT.replace("--- sapflow,", "--- mediamtx,"), [750],
            expected_server="mediamtx")[0]
        self.assertEqual(row["server"], "mediamtx")

    def test_sampling_target_and_elapsed_interval_must_match_the_plan(self):
        for sampling in ("0.010 s target 1 s", "60.010 s target 1 s",
            "58.799 s target 60 s", "61.201 s target 60 s"):
            with self.subTest(sampling=sampling), self.assertRaises(ValueError):
                parse_report(REPORT.replace("60.010 s target 60 s", sampling), [750])
        for elapsed in (58.801, 61.199):
            row = parse_report(REPORT.replace("60.010 s", f"{elapsed} s"), [750])[0]
            self.assertEqual(row["server_sample_seconds"], elapsed)
            self.assertEqual(row["server_sample_target_seconds"], 60)
        short = REPORT.replace("60.010 s target 60 s", "10.010 s target 10 s").replace("measured 60 s", "measured 10 s")
        parse_report(short, [750], expected_seconds=10)
        with self.assertRaises(ValueError):
            parse_report(short.replace("10.010 s", "11.001 s"), [750], expected_seconds=10)

    def test_zero_bytes_rss_and_non_finite_measurements_are_refused(self):
        for before, after in [
            ("received 2000.0 MB", "received 0.0 MB"),
            ("peak RSS 27.2 MB", "peak RSS 0.0 MB"),
            ("server CPU 34.6%", "server CPU " + "9" * 400 + ".0%"),
        ]:
            with self.subTest(before=before), self.assertRaises(ValueError):
                parse_report(REPORT.replace(before, after), [750])

    def test_failed_and_rejected_trials_remain_in_the_structured_receipt(self):
        for runner_exit, report, reason, variant, threads in [
            (7, "viewer failed\n", "runner exit 7", "own", "8"),
            (0, REPORT.replace("measured 60 s", "measured 1 s"), "viewer workload", "own", "8"),
            (7, "viewer failed\n", "runner exit 7", "own-1", "1"),
            (7, "viewer failed\n", "runner exit 7", "own-4", "4"),
        ]:
            with self.subTest(runner_exit=runner_exit), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                release = root / "target/release"
                release.mkdir(parents=True)
                for name in ("sapflow", "sapflow-bench"):
                    (release / name).write_bytes(b"executable fixture")
                output = root / "scratch/result"

                def command(*arguments, **kwargs):
                    if arguments[0] == "sysctl":
                        return "Apple M4 Max"
                    if arguments[0] == "file":
                        return "Mach-O 64-bit executable arm64"
                    return "fixture"

                def run_trial(env, server, raw):
                    self.assertEqual(env["SAPFLOW_HTTP_IMPL"], "own")
                    self.assertEqual(env.get("SAPFLOW_HTTP_THREADS"), threads)
                    raw.write_text(report)
                    raw.with_suffix(".logs").mkdir()
                    return runner_exit

                # Exercise the full receipt lifecycle with no builds or host changes.
                with patch.object(performance, "ROOT", root), \
                    patch.object(performance, "command", side_effect=command), \
                    patch.object(performance, "sha256", return_value="fixture"), \
                    patch.object(performance, "ensure_ports_unused"), \
                    patch.object(performance, "run_trial", side_effect=run_trial), \
                    patch.object(performance.platform, "system", return_value="Darwin"), \
                    patch.object(performance.platform, "machine", return_value="arm64"), \
                    patch.object(performance.shutil, "which", return_value="fixture"), \
                    patch.object(performance.resource, "getrlimit", return_value=(4096, 4096)), \
                    patch.object(performance.resource, "setrlimit"), \
                    patch.object(performance.os, "cpu_count", return_value=8), \
                    patch.object(performance.subprocess, "run"), \
                    patch.object(performance.subprocess, "check_output", return_value=b""), \
                    patch.dict(os.environ, {"CARGO_BUILD_TARGET": "", "SAPFLOW_HTTP_IMPL": "own", "SAPFLOW_HTTP_THREADS": "99"}), \
                    patch.object(sys, "argv", ["performance.py", "--output", str(output),
                        "--variants", variant, "--viewers", "750", "--rounds", "1"]), \
                    self.assertRaises((RuntimeError, ValueError)):
                    performance.main()
                result = json.loads((output / "results.json").read_text())
                self.assertEqual(result["status"], "failed")
                settings = {"SAPFLOW_HTTP_IMPL": "own", "SAPFLOW_HTTP_THREADS": threads}
                self.assertEqual(result["http_settings"], {variant: settings})
                trial = result["runs"][0]
                self.assertEqual(trial["runner_exit"], runner_exit)
                self.assertIn(reason, trial["failure"])
                self.assertEqual(trial["measurements"], [])
                self.assertEqual((output / trial["raw"]).read_text(), report)
                self.assertTrue((output / trial["diagnostics"]).is_dir())

    def test_slow_viewers_are_parsed_apart_and_their_errors_are_recorded_not_refused(self):
        row = parse_report(SLOW_REPORT, [750], expected_slow=5, expected_slow_rate=50000)[0]
        # The ordinary figures are the other viewers' and keep their schema.
        self.assertEqual((row["errors"], row["part_reach"], row["requests"]), (0, 739, 588290))
        self.assertEqual(row["playlist_p99_ms"], 26.9)
        self.assertEqual(row["slow"], dict(
            viewers=5, rate_bytes_per_second=50000, requests=400, errors=30, received_mb=21.5,
            excluded_requests=10, excluded_errors=0,
            error_kinds={"part: HTTP 404": 20, "segment: body timeout": 10},
            playlist=dict(n=120, p50_ms=12.0, p99_ms=80.5),
            segment=dict(n=0, p50_ms=None, p99_ms=None), parts_fetched=150))
        # Without slow viewers a row has no slow key at all, as before.
        self.assertNotIn("slow", parse_report(REPORT, [750])[0])

    def test_the_slow_section_must_match_the_plan(self):
        planned = dict(expected_slow=5, expected_slow_rate=50000)
        for report, options, reason in [
            (REPORT, planned, "incomplete"),
            (SLOW_REPORT, {}, "unexpected slow"),
            (SLOW_REPORT.replace("slow parts fetched 150\n", ""), planned, "incomplete"),
            (SLOW_REPORT.replace("slow viewers 5 of", "slow viewers 4 of"), planned, "slow viewer workload"),
            (SLOW_REPORT.replace("rate 50000", "rate 5000"), planned, "slow viewer workload"),
            (SLOW_REPORT.replace("of 750", "of 250"), planned, "slow viewer workload"),
            (SLOW_REPORT.replace("other 745", "other 750"), planned, "slow viewer workload"),
            (SLOW_REPORT.replace("slow requests 400", "slow requests 0"), planned, "no requests"),
            # The ordinary viewers' reach is out of the ordinary viewers.
            (SLOW_REPORT.replace("reach 739/745", "reach 744/750"), planned, "denominator"),
            (SLOW_REPORT.replace("reach 739/745", "reach 746/745"), planned, "denominator"),
        ]:
            with self.subTest(reason=reason, report=report[-120:]), self.assertRaisesRegex(ValueError, reason):
                parse_report(report, [750], **options)

    def test_summary_totals_slow_requests_errors_and_kinds(self):
        rows = parse_report(SLOW_REPORT, [750], expected_slow=5, expected_slow_rate=50000)
        second = dict(rows[0], slow=dict(rows[0]["slow"], errors=2, error_kinds={"part: HTTP 410": 2}, parts_fetched=10))
        result = performance.summary([dict(variant="own", measurements=rows), dict(variant="own", measurements=[second])])[0]
        self.assertEqual(result["slow"], dict(
            viewers=5, rate_bytes_per_second=50000, requests=800, errors=32, parts_fetched=160,
            error_kinds={"part: HTTP 404": 20, "segment: body timeout": 10, "part: HTTP 410": 2}))
        self.assertNotIn("slow", performance.summary([dict(variant="own", measurements=parse_report(REPORT, [750]))])[0])

    def test_os_release_pretty_name_is_unquoted(self):
        text = 'PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"\nNAME="Debian GNU/Linux"\nID=debian\n'
        self.assertEqual(performance.os_pretty_name(text), "Debian GNU/Linux 12 (bookworm)")
        self.assertEqual(performance.os_pretty_name("PRETTY_NAME=Alpine\n"), "Alpine")
        self.assertEqual(performance.os_pretty_name("PRETTY_NAME='Ubuntu 24.04 LTS'\n"), "Ubuntu 24.04 LTS")
        for text in (None, "", "NAME=x\n", "PRETTY_NAME=\n"):
            self.assertIsNone(performance.os_pretty_name(text))

    def test_cgroup_limits_are_parsed_and_malformed_ones_refused(self):
        self.assertEqual(performance.parse_cpu_max("800000 100000\n"), (800000, 100000))
        self.assertEqual(performance.parse_cpu_max("max 100000"), (None, 100000))
        self.assertIsNone(performance.parse_memory_max("max\n"))
        self.assertEqual(performance.parse_memory_max("8589934592\n"), 8589934592)
        for text in ("", "max", "800000", "0 100000", "max 0", "a b", "1 2 3"):
            with self.subTest(cpu_max=text), self.assertRaises(ValueError):
                performance.parse_cpu_max(text)
        for text in ("0", "-1", "lots"):
            with self.subTest(memory_max=text), self.assertRaises(ValueError):
                performance.parse_memory_max(text)

    def test_effective_cpus_follow_affinity_and_the_rounded_up_quota(self):
        self.assertEqual(performance.effective_cpus(16, None), 16)
        self.assertEqual(performance.effective_cpus(16, (None, 100000)), 16)
        self.assertEqual(performance.effective_cpus(16, (800000, 100000)), 8)
        self.assertEqual(performance.effective_cpus(16, (250000, 100000)), 3)
        self.assertEqual(performance.effective_cpus(16, (50000, 100000)), 1)
        self.assertEqual(performance.effective_cpus(4, (800000, 100000)), 4)

    def test_linux_host_records_os_kernel_cpus_and_cgroup_limits(self):
        files = {
            performance.OS_RELEASE: 'PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"\n',
            performance.CGROUP_CPU_MAX: "800000 100000",
            performance.CGROUP_MEMORY_MAX: "8589934592",
        }
        host = performance.linux_host(read=files.get, cpu_count=16, affinity=12, kernel="6.10.14-linuxkit")
        self.assertEqual(host, dict(
            os="Debian GNU/Linux 12 (bookworm)", kernel="6.10.14-linuxkit", logical_cpus=16, affinity_cpus=12,
            cgroup_cpu_max="800000 100000", cgroup_cpu_quota_cpus=8.0,
            cgroup_memory_max="8589934592", cgroup_memory_max_bytes=8589934592, effective_cpus=8))
        # No cgroup v2 limits, or none set: recorded as absent or unlimited.
        host = performance.linux_host(read=lambda path: None, cpu_count=16, affinity=16, kernel="6.1")
        self.assertEqual((host["os"], host["cgroup_cpu_max"], host["cgroup_cpu_quota_cpus"], host["cgroup_memory_max"],
                          host["cgroup_memory_max_bytes"], host["effective_cpus"]), (None, None, None, None, None, 16))
        files[performance.CGROUP_CPU_MAX] = "max 100000"
        files[performance.CGROUP_MEMORY_MAX] = "max"
        host = performance.linux_host(read=files.get, cpu_count=16, affinity=16, kernel="6.1")
        self.assertEqual((host["cgroup_cpu_max"], host["cgroup_cpu_quota_cpus"], host["cgroup_memory_max_bytes"]), ("max 100000", None, None))
        files[performance.CGROUP_CPU_MAX] = "garbage"
        with self.assertRaises(ValueError):
            performance.linux_host(read=files.get, cpu_count=16, affinity=16, kernel="6.1")

    def test_read_optional_returns_none_for_a_missing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(performance.read_optional(Path(directory) / "absent"))
            (Path(directory) / "present").write_text("max 100000\n")
            self.assertEqual(performance.read_optional(Path(directory) / "present"), "max 100000")

    def test_binary_architecture_checks_per_platform(self):
        macho = "Mach-O 64-bit executable arm64"
        universal = "Mach-O universal binary with 2 architectures: [x86_64:Mach-O 64-bit executable x86_64] [arm64]"
        elf = "ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV), statically linked, stripped"
        pie = "ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV), dynamically linked"
        amd = "ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked"
        self.assertTrue(performance.is_native_arm64(macho))
        self.assertTrue(performance.is_native_arm64(macho, "m4"))
        self.assertFalse(performance.is_native_arm64(universal, "m4"))
        self.assertFalse(performance.is_native_arm64(elf, "m4"), "an ELF is not the macOS binary")
        self.assertTrue(performance.is_native_arm64(elf, "linux"))
        self.assertTrue(performance.is_native_arm64(pie, "linux"))
        self.assertFalse(performance.is_native_arm64(amd, "linux"))
        self.assertFalse(performance.is_native_arm64(macho, "linux"), "a Mach-O does not run on Linux")
        self.assertFalse(performance.is_native_arm64("", "linux"))

    def arguments(self, *extra, platform_name="Darwin", machine="arm64"):
        """Run main() as the runner would on the named host, and return its exit if it refuses."""
        with tempfile.TemporaryDirectory() as directory, \
            patch.object(performance.platform, "system", return_value=platform_name), \
            patch.object(performance.platform, "machine", return_value=machine), \
            patch.object(performance.shutil, "which", return_value="fixture"), \
            patch.object(performance, "command", return_value="Apple M4 Max"), \
            patch.object(performance, "ensure_ports_unused"), \
            patch.object(sys, "argv", ["performance.py", "--output", str(Path(directory) / "out"),
                "--variants", "own", "--rounds", "1", *extra]), \
            patch("sys.stderr"), \
            self.assertRaises(SystemExit) as raised:
            performance.main()
        return raised.exception.code

    def test_platform_must_match_the_host(self):
        # Defaults to the M4 and refuses anything else, as ever.
        self.assertEqual(self.arguments(platform_name="Linux", machine="aarch64"), 2)
        self.assertEqual(self.arguments(platform_name="Darwin", machine="x86_64"), 2)
        self.assertEqual(self.arguments("--platform", "m4", platform_name="Linux", machine="aarch64"), 2)
        # Linux needs aarch64 Linux: not macOS, not x86_64.
        self.assertEqual(self.arguments("--platform", "linux", platform_name="Darwin", machine="arm64"), 2)
        self.assertEqual(self.arguments("--platform", "linux", platform_name="Linux", machine="x86_64"), 2)
        self.assertEqual(self.arguments("--platform", "linux", platform_name="Linux", machine="arm64"), 2)

    def test_slow_options_are_validated_before_anything_runs(self):
        for extra in (
            ("--slow", "5"), ("--slow", "5", "--slow-rate", "0"), ("--slow-rate", "5000"),
            ("--slow", "0", "--slow-rate", "5000"), ("--slow", "-1"),
            # Five slow viewers cannot be among fewer than six, and one ordinary viewer must remain.
            ("--viewers", "250", "5", "--slow", "5", "--slow-rate", "5000"),
            ("--viewers", "250", "--slow", "250", "--slow-rate", "5000"),
        ):
            with self.subTest(extra=extra):
                self.assertEqual(self.arguments(*extra), 2)

    def test_linux_and_slow_receipts_record_the_platform_and_pass_settings_to_the_shell(self):
        files = {
            performance.OS_RELEASE: 'PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"\n',
            performance.CGROUP_CPU_MAX: "800000 100000",
            performance.CGROUP_MEMORY_MAX: "8589934592",
        }
        for platform_name, arguments in [
            ("m4", ["--slow", "5", "--slow-rate", "50000"]),
            ("linux", []),
            ("linux", ["--slow", "5", "--slow-rate", "50000"]),
        ]:
            with self.subTest(platform=platform_name, arguments=arguments), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                release = root / "target/release"
                release.mkdir(parents=True)
                for name in ("sapflow", "sapflow-bench"):
                    (release / name).write_bytes(b"executable fixture")
                output = root / "scratch/result"
                calls = []
                seen = []

                def command(*call, **kwargs):
                    calls.append(call[0])
                    if call[0] == "sysctl":
                        return "Apple M4 Max"
                    if call[0] == "file":
                        return "ELF 64-bit LSB executable, ARM aarch64" if platform_name == "linux" else "Mach-O 64-bit executable arm64"
                    return "fixture"

                def run_trial(env, server, raw):
                    seen.append(env)
                    raw.write_text("viewer failed\n")
                    raw.with_suffix(".logs").mkdir()
                    return 7

                linux = platform_name == "linux"
                patches = [
                    patch.object(performance, "ROOT", root),
                    patch.object(performance, "command", side_effect=command),
                    patch.object(performance, "sha256", return_value="fixture"),
                    patch.object(performance, "ensure_ports_unused"),
                    patch.object(performance, "run_trial", side_effect=run_trial),
                    patch.object(performance, "read_optional", side_effect=files.get),
                    patch.object(performance.platform, "system", return_value="Linux" if linux else "Darwin"),
                    patch.object(performance.platform, "machine", return_value="aarch64" if linux else "arm64"),
                    patch.object(performance.platform, "release", return_value="6.10.14-linuxkit"),
                    patch.object(performance.os, "sched_getaffinity", create=True, return_value=set(range(16))),
                    patch.object(performance.shutil, "which", return_value="fixture"),
                    patch.object(performance.resource, "getrlimit", return_value=(4096, 4096)),
                    patch.object(performance.resource, "setrlimit"),
                    patch.object(performance.os, "cpu_count", return_value=16),
                    patch.object(performance.subprocess, "run"),
                    patch.object(performance.subprocess, "check_output", return_value=b""),
                    patch.dict(os.environ, {"CARGO_BUILD_TARGET": "", "SLOW": "9", "SLOW_RATE": "9"}),
                    patch.object(sys, "argv", ["performance.py", "--output", str(output), "--platform", platform_name,
                        "--variants", "own", "--viewers", "750", "--rounds", "1", *arguments]),
                ]
                with contextlib.ExitStack() as stack:
                    for item in patches:
                        stack.enter_context(item)
                    with self.assertRaises((RuntimeError, ValueError)):
                        performance.main()
                result = json.loads((output / "results.json").read_text())
                self.assertEqual(result["platform"], platform_name)
                if linux:
                    self.assertNotIn("sysctl", calls)
                    self.assertTrue(all(key not in result for key in ("cpu", "macos")))
                    self.assertEqual(
                        {key: result[key] for key in ("os", "kernel", "logical_cpus", "affinity_cpus", "cgroup_cpu_max",
                                                       "cgroup_memory_max", "effective_cpus")},
                        dict(os="Debian GNU/Linux 12 (bookworm)", kernel="6.10.14-linuxkit", logical_cpus=16,
                             affinity_cpus=16, cgroup_cpu_max="800000 100000", cgroup_memory_max="8589934592",
                             effective_cpus=8))
                    self.assertIn("Linux/epoll", result["notes"])
                    # The own loop's default worker count is the usable cores, not the host's.
                    self.assertEqual(result["http_settings"], {"own": {"SAPFLOW_HTTP_IMPL": "own", "SAPFLOW_HTTP_THREADS": "8"}})
                else:
                    self.assertEqual((result["cpu"], result["macos"], result["logical_cpus"]), ("Apple M4 Max", "fixture", 16))
                    self.assertNotIn("os", result)
                    self.assertEqual(result["http_settings"]["own"]["SAPFLOW_HTTP_THREADS"], "16")
                    self.assertTrue(result["notes"].startswith("Loopback HTTP/1.1; full media viewers"))
                if arguments:
                    self.assertEqual((result["slow_viewers"], result["slow_rate_bytes_per_second"]), (5, 50000))
                    self.assertEqual((seen[0]["SLOW"], seen[0]["SLOW_RATE"]), ("5", "50000"))
                    self.assertIn("Slow viewers", result["notes"])
                else:
                    self.assertEqual((result["slow_viewers"], result["slow_rate_bytes_per_second"]), (0, None))
                    # Settings left in the caller's environment cannot leak in as a plan.
                    self.assertNotIn("SLOW", seen[0])
                    self.assertNotIn("SLOW_RATE", seen[0])
                    self.assertNotIn("Slow viewers", result["notes"])

    def test_a_binary_for_the_wrong_architecture_is_refused_on_linux(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            release = root / "target/release"
            release.mkdir(parents=True)
            for name in ("sapflow", "sapflow-bench"):
                (release / name).write_bytes(b"executable fixture")
            output = root / "scratch/result"

            def command(*call, **kwargs):
                return "Mach-O 64-bit executable arm64" if call[0] == "file" else "fixture"

            with patch.object(performance, "ROOT", root), \
                patch.object(performance, "command", side_effect=command), \
                patch.object(performance, "sha256", return_value="fixture"), \
                patch.object(performance, "ensure_ports_unused"), \
                patch.object(performance, "linux_host", return_value=dict(effective_cpus=8)), \
                patch.object(performance.platform, "system", return_value="Linux"), \
                patch.object(performance.platform, "machine", return_value="aarch64"), \
                patch.object(performance.shutil, "which", return_value="fixture"), \
                patch.object(performance.resource, "getrlimit", return_value=(4096, 4096)), \
                patch.object(performance.resource, "setrlimit"), \
                patch.object(performance.subprocess, "run"), \
                patch.object(performance.subprocess, "check_output", return_value=b""), \
                patch.dict(os.environ, {"CARGO_BUILD_TARGET": ""}), \
                patch.object(sys, "argv", ["performance.py", "--output", str(output), "--platform", "linux",
                    "--variants", "own", "--rounds", "1"]), \
                self.assertRaisesRegex(ValueError, r"ELF aarch64"):
                performance.main()
            self.assertEqual(json.loads((output / "results.json").read_text())["status"], "failed")

    def test_real_shells_pass_explicit_backend_and_threads_to_server(self):
        # The actual bench/smoke shells launch an environment-reporting fixture
        # and stop before media. Every cargo/tool command is a finite stub.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            commands = root / "commands"
            commands.mkdir()
            release = root / "target/release"
            release.mkdir(parents=True)

            def executable(path, body):
                path.write_text(body)
                path.chmod(0o755)

            for name in ("bench.sh", "smoke.sh"):
                shutil.copyfile(performance.ROOT / "scripts" / name, root / "scripts" / name)
            for name in ("cargo", "ffmpeg", "ffprobe", "srt-live-transmit", "curl"):
                executable(commands / name, "#!/bin/sh\nexit 0\n")
            executable(commands / "python3", '#!/bin/sh\nif [ "${1:-}" = -c ]; then echo 8; fi\nexit 0\n')
            executable(commands / "sleep", f"#!{sys.executable}\nimport os, pathlib, time\nend=time.monotonic()+2\nwhile not pathlib.Path(os.environ['FIXTURE_ENV_FILE']).exists() and time.monotonic()<end: time.sleep(.01)\n")
            server = release / "sapflow"
            executable(server, f"#!{sys.executable}\nimport json, os, pathlib\npathlib.Path(os.environ['FIXTURE_ENV_FILE']).write_text(json.dumps({{k: os.environ.get(k) for k in ['SAPFLOW_HTTP_IMPL', 'SAPFLOW_HTTP_THREADS']}}))\nraise SystemExit(23)\n")
            for script in ("bench.sh", "smoke.sh"):
                for index, (backend, count, expected_backend, expected_count) in enumerate([
                    (None, None, "own", "8"),
                    # A stale selection reaches the server, which refuses it.
                    ("hyper", "99", "hyper", "99"),
                    ("own", "1", "own", "1"), ("own", "4", "own", "4"),
                    (None, "3", "own", "3"),
                ]):
                    with self.subTest(script=script, backend=backend, threads=count):
                        capture = root / f"{script}-{index}.json"
                        env = {key: value for key, value in os.environ.items()
                               if not key.startswith("SAPFLOW_") and key not in ("CARGO_BUILD_TARGET", "BENCH_ARTIFACTS", "SMOKE_KEEP")}
                        env.update(PATH=str(commands) + os.pathsep + env["PATH"],
                                   SAPFLOW_BIN=str(server), FIXTURE_ENV_FILE=str(capture))
                        if backend is not None:
                            env["SAPFLOW_HTTP_IMPL"] = backend
                        if count is not None:
                            env["SAPFLOW_HTTP_THREADS"] = count
                        args = ["bash", str(root / "scripts" / script)]
                        if script == "bench.sh":
                            args.append("sapflow")
                        result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True, timeout=6)
                        self.assertNotEqual(result.returncode, 0, "fixture intentionally exits before media")
                        self.assertEqual(json.loads(capture.read_text()), {
                            "SAPFLOW_HTTP_IMPL": expected_backend, "SAPFLOW_HTTP_THREADS": expected_count})

    def test_ci_smoke_covers_the_own_event_loop_and_no_other_backend(self):
        workflow = (performance.ROOT / ".github/workflows/ci.yml").read_text()
        steps = [step for step in workflow.split("\n      - name:") if "\n        run: scripts/smoke.sh" in step]
        self.assertEqual(len(steps), 1)
        self.assertIn("SAPFLOW_HTTP_IMPL: own", steps[0])
        self.assertIn("SAPFLOW_HTTP_THREADS: 1", steps[0])
        self.assertNotIn("hyper", workflow.lower())

    def test_failed_viewer_keeps_all_diagnostics_and_original_exit_status(self):
        # Execute the real shell orchestration with finite executable fixtures.
        # No Rust build, media generation, network socket or real benchmark runs.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(performance.ROOT / "scripts/bench.sh", root / "scripts/bench.sh")
            commands = root / "commands"
            commands.mkdir()
            release = root / "target/release"
            release.mkdir(parents=True)

            def executable(path, text):
                path.write_text(text)
                path.chmod(0o755)

            executable(commands / "cargo", "#!/bin/sh\nexit 0\n")
            executable(commands / "curl", "#!/bin/sh\nexit 0\n")
            executable(commands / "ps", "#!/bin/sh\nprintf '1024\\n'\n")
            executable(commands / "python3", "#!/bin/sh\nprintf '1.0 2.0 60.001\\n'\n")
            executable(commands / "sleep", f"#!{sys.executable}\nimport time\ntime.sleep(.05)\n")
            for path, label in ((release / "sapflow", "server"), (commands / "ffmpeg", "encoder")):
                executable(path, f"#!{sys.executable}\nimport os, time\nprint('{label} diagnostic pid=' + str(os.getpid()), flush=True)\ntime.sleep(300)\n")
            executable(release / "sapflow-bench", f"#!{sys.executable}\nimport sys\nprint('viewer diagnostic', flush=True)\nprint('viewer failure', file=sys.stderr, flush=True)\nraise SystemExit(7)\n")
            output = root / "trial.txt"
            env = {**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"],
                "VIEWERS": "750", "SECS": "60"}
            env.pop("CARGO_BUILD_TARGET", None)
            with patch.object(performance, "ROOT", root):
                self.assertEqual(performance.run_trial(env, "sapflow", output), 7)
            logs = output.with_suffix(".logs")
            self.assertEqual((logs / "exit-status.txt").read_text(), "7\n")
            self.assertIn("viewer failure", (logs / "bench-750.txt").read_text())
            for name in ("server", "ffmpeg"):
                diagnostic = (logs / (name + ".log")).read_text()
                pid = int(diagnostic.split("pid=")[1].strip())
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)
            # Reusing the same destination must not overwrite the failed trial.
            original = {path.name: path.read_bytes() for path in logs.iterdir()}
            with patch.object(performance, "ROOT", root), self.assertRaises(FileExistsError):
                performance.run_trial(env, "sapflow", output)
            self.assertEqual(original, {path.name: path.read_bytes() for path in logs.iterdir()})

    def test_shell_passes_slow_options_to_the_loader_only_when_asked(self):
        # The real bench.sh with finite stubs; the loader fixture records its arguments.
        # The empty-array expansion must also work on macOS's Bash 3.2 under set -u.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(performance.ROOT / "scripts/bench.sh", root / "scripts/bench.sh")
            commands = root / "commands"
            commands.mkdir()
            release = root / "target/release"
            release.mkdir(parents=True)

            def executable(path, text):
                path.write_text(text)
                path.chmod(0o755)

            for name in ("cargo", "curl"):
                executable(commands / name, "#!/bin/sh\nexit 0\n")
            executable(commands / "ps", "#!/bin/sh\nprintf '1024\\n'\n")
            executable(commands / "python3", "#!/bin/sh\nprintf '1.0 2.0 60.001\\n'\n")
            executable(commands / "sleep", f"#!{sys.executable}\nimport time\ntime.sleep(.05)\n")
            for path in (release / "sapflow", commands / "ffmpeg"):
                executable(path, f"#!{sys.executable}\nimport time\ntime.sleep(300)\n")
            executable(release / "sapflow-bench", f"#!{sys.executable}\nimport json, os, sys\n"
                "open(os.environ['FIXTURE_ARGS'], 'w').write(json.dumps(sys.argv[1:]))\nraise SystemExit(7)\n")
            base = {key: value for key, value in os.environ.items() if key not in ("SLOW", "SLOW_RATE", "BENCH_ARTIFACTS", "CARGO_BUILD_TARGET")}
            base.update(PATH=str(commands) + os.pathsep + base["PATH"], VIEWERS="750", SECS="60")
            expected = ["--url", "http://127.0.0.1:8888/live/event-bench/index.m3u8", "--viewers", "750", "--secs", "60", "--ramp", "10"]
            for index, (extra, tail) in enumerate([
                ({}, []), ({"SLOW": "0"}, []),
                ({"SLOW": "5", "SLOW_RATE": "50000"}, ["--slow", "5", "--slow-rate", "50000"]),
            ]):
                with self.subTest(extra=extra):
                    capture = root / f"args-{index}.json"
                    result = subprocess.run(["bash", str(root / "scripts/bench.sh"), "sapflow"], cwd=root, timeout=20,
                        env={**base, **extra, "FIXTURE_ARGS": str(capture)}, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 7, result.stderr)
                    self.assertEqual(json.loads(capture.read_text()), expected + tail)
            for extra, message in [
                ({"SLOW": "5"}, "SLOW_RATE"), ({"SLOW_RATE": "50000"}, "SLOW_RATE needs SLOW"),
                ({"SLOW": "0", "SLOW_RATE": "50000"}, "SLOW_RATE needs SLOW"),
            ]:
                with self.subTest(extra=extra):
                    result = subprocess.run(["bash", str(root / "scripts/bench.sh"), "sapflow"], cwd=root, timeout=20,
                        env={**base, **extra, "FIXTURE_ARGS": str(root / "unused.json")}, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
            self.assertFalse((root / "unused.json").exists(), "the loader never ran for a refused combination")

    def test_build_failure_with_no_children_keeps_status_and_removes_scratch(self):
        # macOS Bash 3.2 treats an empty array as unset under set -u.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(performance.ROOT / "scripts/bench.sh", root / "scripts/bench.sh")
            commands = root / "commands"
            commands.mkdir()
            cargo = commands / "cargo"
            cargo.write_text("#!/bin/sh\necho 'build failure' >&2\nexit 23\n")
            cargo.chmod(0o755)
            scratch = root / "temporary"
            scratch.mkdir()
            env = {**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"],
                "TMPDIR": str(scratch)}
            output = root / "build.txt"
            with patch.object(performance, "ROOT", root):
                self.assertEqual(performance.run_trial(env, "sapflow", output), 23)
            self.assertEqual((output.with_suffix(".logs") / "exit-status.txt").read_text(), "23\n")
            self.assertIn("build failure", output.read_text())
            self.assertEqual(list(scratch.iterdir()), [])
            existing = root / "existing.logs"
            existing.mkdir()
            (existing / "sentinel").write_text("keep earlier diagnostics")
            with patch.object(performance, "ROOT", root):
                self.assertEqual(performance.run_trial(env, "sapflow", root / "existing.txt"), 1)
            self.assertEqual([path.name for path in existing.iterdir()], ["sentinel"])
            self.assertEqual((existing / "sentinel").read_text(), "keep earlier diagnostics")


if __name__ == "__main__":
    unittest.main()
