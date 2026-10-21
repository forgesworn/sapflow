"""Deterministic observer checks. Never invokes ADB or accesses a device."""
import importlib.util
from pathlib import Path
import sys
import time
import unittest

SPEC = importlib.util.spec_from_file_location("observer", Path(__file__).with_name("mobile-device-observer.py"))
observer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observer)
PACKAGE = "com.example.publisher"
STAT = "42 (worker (main)) S 1 2 3 4 5 6 7 8 9 10 120 30 0 0 20 0 8 0 999 4096 20\n"
MEMORY = """Applications Memory Usage (in Kilobytes):
** MEMINFO in pid 42 [com.example.publisher] **
 App Summary
                       Pss(KB)                        Rss(KB)
           TOTAL PSS:    12345            TOTAL RSS:    23456       TOTAL SWAP PSS: 0
"""
BATTERY = """Current Battery Service state:
  AC powered: false
  USB powered: true
  Wireless powered: false
  status: 2
  health: 2
  level: 75
  scale: 100
  voltage: 4100
  temperature: 329
"""


class ParsingTests(unittest.TestCase):
    def test_pid_missing_multiple_and_malformed(self):
        self.assertEqual(observer.parse_pid("")["unavailable_reason"], "process_absent")
        for value in ("42 43", "permission denied", "0", "-1", "42; other"):
            self.assertIsNone(observer.parse_pid(value)["value"])
        self.assertEqual(observer.parse_pid("42\n")["value"], 42)

    def test_stat_fields_and_parentheses(self):
        self.assertEqual(observer.parse_stat(STAT, 42), {"pid": 42, "start_ticks": 999, "cpu_ticks": 150})
        for value in ("Permission denied", "42 (worker) S 1", STAT.replace("120", "oops"), STAT.replace("120", "-1")):
            self.assertIsNone(observer.parse_stat(value, 42))
        self.assertIsNone(observer.parse_stat(STAT, 99))

    def test_cpu_delta_one_core_and_measured_zero(self):
        previous = {"pid": 42, "start_ticks": 999, "cpu_ticks": 100, "at": 10}
        current = dict(previous, cpu_ticks=500, at=12)
        self.assertEqual(observer.cpu_percent(previous, current, 100)["value"], 200)
        self.assertEqual(observer.cpu_percent(previous, dict(previous, at=12), 100)["value"], 0)

    def test_cpu_no_false_zero_on_baseline_restart_missing_or_regression(self):
        previous = {"pid": 42, "start_ticks": 999, "cpu_ticks": 100, "at": 10}
        for old, new, ticks in (
            (None, previous, 100), (previous, None, 100), (previous, previous, None),
            (previous, dict(previous, at=12, pid=43), 100),
            (previous, dict(previous, at=12, start_ticks=1000), 100),
            (previous, dict(previous, at=12, cpu_ticks=99), 100),
            (previous, dict(previous, at=9), 100),
        ):
            result = observer.cpu_percent(old, new, ticks)
            self.assertIsNone(result["value"])
            self.assertIsNotNone(result["unavailable_reason"])

    def test_memory_totals_and_missing_rss(self):
        result = observer.parse_meminfo(MEMORY, 42, PACKAGE)
        self.assertEqual(result["pss_kib"]["value"], 12345)
        self.assertEqual(result["rss_kib"]["value"], 23456)
        legacy = MEMORY.replace("TOTAL RSS:    23456", "")
        self.assertIsNone(observer.parse_meminfo(legacy, 42, PACKAGE)["rss_kib"]["value"])

    def test_memory_missing_wrong_pid_and_package_not_zero(self):
        for text in ("", "No process found for: 42", MEMORY.replace("pid 42", "pid 43"), MEMORY.replace(PACKAGE, "com.example.other")):
            self.assertTrue(all(item["value"] is None for item in observer.parse_meminfo(text, 42, PACKAGE).values()))

    def test_battery_units_and_plugged(self):
        result = observer.parse_battery(BATTERY)
        self.assertEqual(result["temperature_c"]["value"], 32.9)
        self.assertEqual(result["voltage_mv"]["value"], 4100)
        self.assertEqual(result["level_percent"]["value"], 75)
        self.assertIs(result["plugged"]["value"], True)
        self.assertIs(observer.parse_battery(BATTERY.replace("USB powered: true", "USB powered: false"))["plugged"]["value"], False)

    def test_battery_missing_invalid_and_zero(self):
        self.assertTrue(all(item["value"] is None for item in observer.parse_battery("").values()))
        self.assertIsNone(observer.parse_battery("USB powered: false")["plugged"]["value"])
        self.assertIsNone(observer.parse_battery("level: 90\nscale: 0")["level_percent"]["value"])
        self.assertEqual(observer.parse_battery("level: 0\nscale: 100")["level_percent"]["value"], 0)

    def test_thermal_zero_valid_missing_not_zero(self):
        parsed = observer.parse_thermal("Thermal Status: 0\nIsStatusOverride: false\n")
        self.assertEqual(parsed["status"]["value"], 0)
        self.assertIs(parsed["status_overridden"]["value"], False)
        for text in ("", "Thermal Status: 9", "Thermal Status: unknown"):
            self.assertIsNone(observer.parse_thermal(text)["status"]["value"])

    def test_package_scoped_version(self):
        text = """Resolver data versionName=unrelated
Packages:
  Package [com.example.publisher] (abcd):
    versionCode=27 minSdk=26 targetSdk=35
    versionName=0.4-preview
  Package [com.example.other] (ffff):
    versionCode=99
    versionName=other
"""
        result = observer.parse_package(text, PACKAGE)
        self.assertEqual(result["version_code"]["value"], 27)
        self.assertEqual(result["version_name"]["value"], "0.4-preview")
        self.assertIsNone(observer.parse_package(text, "com.example.missing")["version_code"]["value"])
        # Never borrow an adjacent package's omitted version name.
        self.assertIsNone(observer.parse_package(text.replace("    versionName=0.4-preview\n", ""), PACKAGE)["version_name"]["value"])

    def test_numeric_limits_reject_nan_inf(self):
        import argparse
        for value in ("nan", "inf", "0", "86401"):
            with self.assertRaises(argparse.ArgumentTypeError):
                observer.bounded_number(1, 86400)(value)


class ObservationTests(unittest.TestCase):
    def make_observer(self, responses, **options):
        calls = []
        def runner(argv, timeout):
            calls.append(argv[4:])
            self.assertGreater(timeout, 0)
            return responses.get(tuple(argv[4:]), ("", "command_failed"))
        item = observer.Observer("never-executed-adb", "synthetic-device", PACKAGE, 1, time.monotonic() + 10, runner, **options)
        item.ticks = 100
        return item, calls

    def test_missing_process_still_records_device_metrics(self):
        item, calls = self.make_observer({("pidof", PACKAGE): ("", None), ("dumpsys", "battery"): (BATTERY, None)})
        sample = item.sample()
        self.assertIsNone(sample["cpu_percent_one_core"]["value"])
        self.assertIsNone(sample["pss_kib"]["value"])
        self.assertEqual(sample["battery"]["level_percent"]["value"], 75)
        self.assertFalse(any(call[0] == "cat" or "meminfo" in call for call in calls))
        self.assertEqual(sample["command_errors"]["thermal"], "command_failed")

    def test_target_only_commands_and_successful_memory(self):
        item, calls = self.make_observer({
            ("pidof", PACKAGE): ("42", None),
            ("cat", "/proc/42/stat"): (STAT, None),
            ("dumpsys", "meminfo", "-s", "42"): (MEMORY, None),
        })
        sample = item.sample()
        self.assertEqual(sample["pss_kib"]["value"], 12345)
        self.assertIsNone(sample["cpu_percent_one_core"]["value"])
        self.assertEqual(calls[:4], [["pidof", PACKAGE], ["cat", "/proc/42/stat"],
                                   ["dumpsys", "meminfo", "-s", "42"], ["cat", "/proc/42/stat"]])

    def test_proc_permission_failure_keeps_independent_scoped_memory(self):
        item, _ = self.make_observer({("pidof", PACKAGE): ("42", None),
                                     ("dumpsys", "meminfo", "-s", "42"): (MEMORY, None)})
        sample = item.sample()
        self.assertEqual(sample["pss_kib"]["value"], 12345)
        self.assertIsNone(sample["cpu_percent_one_core"]["value"])
        self.assertIsNone(sample["cpu_ticks"]["value"])

    def test_pid_failure_is_unavailable_not_confirmed_absence(self):
        item, _ = self.make_observer({})
        self.assertEqual(item.sample()["pid"]["unavailable_reason"], "command_failed")

    def test_tick_only_mode_omits_all_per_sample_dumpsys(self):
        item, calls = self.make_observer({("pidof", PACKAGE): ("42", None),
                                         ("cat", "/proc/42/stat"): (STAT, None)},
                                        skip_memory=True, skip_device_state=True)
        first = item.sample()
        self.assertEqual(calls, [["pidof", PACKAGE], ["cat", "/proc/42/stat"]])
        self.assertEqual(first["cpu_ticks"]["value"], 150)
        self.assertEqual(first["cpu_percent_one_core"]["unavailable_reason"], "baseline_required")
        self.assertEqual(item.sample()["cpu_percent_one_core"]["value"], 0)
        for value in [first["pss_kib"], first["rss_kib"], *first["battery"].values(), *first["thermal"].values()]:
            self.assertEqual(value, observer.metric(reason="skipped_by_option"))
        self.assertEqual(first["command_errors"], {})

    def test_skip_flags_are_independent(self):
        responses = {("pidof", PACKAGE): ("42", None), ("cat", "/proc/42/stat"): (STAT, None)}
        item, calls = self.make_observer(responses, skip_memory=True)
        item.sample()
        self.assertNotIn(["dumpsys", "meminfo", "-s", "42"], calls)
        self.assertIn(["dumpsys", "battery"], calls)
        item, calls = self.make_observer(responses, skip_device_state=True)
        item.sample()
        self.assertIn(["dumpsys", "meminfo", "-s", "42"], calls)
        self.assertNotIn(["dumpsys", "battery"], calls)
        self.assertNotIn(["dumpsys", "thermalservice"], calls)

    def test_memory_timeout_reason_propagates_without_breaking_cpu(self):
        item, _ = self.make_observer({("pidof", PACKAGE): ("42", None),
                                     ("cat", "/proc/42/stat"): (STAT, None),
                                     ("dumpsys", "meminfo", "-s", "42"): ("", "command_timeout")})
        result = item.sample()
        for name in ("pss_kib", "rss_kib"):
            self.assertEqual(result[name], observer.metric(reason="command_timeout"))
        self.assertEqual(result["cpu_ticks"]["value"], 150)

    def test_other_direct_command_failure_reasons_propagate(self):
        item, _ = self.make_observer({("pidof", PACKAGE): ("42", None),
                                     ("cat", "/proc/42/stat"): ("", "command_timeout"),
                                     ("dumpsys", "battery"): ("", "command_timeout"),
                                     ("dumpsys", "thermalservice"): ("", "command_output_limit")})
        result = item.sample()
        self.assertEqual(result["cpu_ticks"]["unavailable_reason"], "command_timeout")
        self.assertEqual(result["cpu_percent_one_core"]["unavailable_reason"], "command_timeout")
        self.assertTrue(all(value["unavailable_reason"] == "command_timeout" for value in result["battery"].values()))
        self.assertTrue(all(value["unavailable_reason"] == "command_output_limit" for value in result["thermal"].values()))

    def test_light_mode_still_collects_bounded_identity_with_failure_reasons(self):
        item, calls = self.make_observer({("getconf", "CLK_TCK"): ("100", None),
                                         ("dumpsys", "package", PACKAGE): ("", "command_timeout")},
                                        skip_memory=True, skip_device_state=True)
        identity = item.identity()
        self.assertEqual(identity["version_code"]["unavailable_reason"], "command_timeout")
        self.assertEqual(identity["version_name"]["unavailable_reason"], "command_timeout")
        self.assertEqual(identity["clock_ticks_per_second"]["value"], 100)
        self.assertEqual(identity["android_build_fingerprint"]["unavailable_reason"], "command_failed")
        self.assertEqual(calls, [["dumpsys", "package", PACKAGE], ["getconf", "CLK_TCK"],
                                 ["getprop", "ro.build.fingerprint"]])

    def test_skipped_metrics_stay_skipped_when_process_is_absent(self):
        item, _ = self.make_observer({("pidof", PACKAGE): ("", None)},
                                     skip_memory=True, skip_device_state=True)
        result = item.sample()
        self.assertEqual(result["pss_kib"]["unavailable_reason"], "skipped_by_option")
        self.assertEqual(result["cpu_ticks"]["unavailable_reason"], "process_absent")

    def test_restart_during_memory_read_discards_cross_lifetime_values(self):
        stats = iter((STAT, STAT.replace("999", "1000")))
        def runner(argv, timeout):
            command = argv[4:]
            if command == ["pidof", PACKAGE]:
                return "42", None
            if command == ["cat", "/proc/42/stat"]:
                return next(stats), None
            if command == ["dumpsys", "meminfo", "-s", "42"]:
                return MEMORY, None
            return "", "command_failed"
        item = observer.Observer("never-executed-adb", "synthetic-device", PACKAGE, 1, time.monotonic() + 10, runner)
        result = item.sample()
        self.assertIsNone(result["cpu_ticks"]["value"])
        self.assertEqual(result["pss_kib"]["unavailable_reason"], "process_restarted_during_collection")


class SubprocessTests(unittest.TestCase):
    # Synthetic host subprocesses exercise bounds, never ADB.
    def test_success_and_nonzero(self):
        self.assertEqual(observer.run_bounded([sys.executable, "-c", "print('fixture')"], 2), ("fixture\n", None))
        self.assertEqual(observer.run_bounded([sys.executable, "-c", "raise SystemExit(1)"], 2), ("", "command_failed"))

    def test_timeout_and_output_cap(self):
        self.assertEqual(observer.run_bounded([sys.executable, "-c", "import time; time.sleep(5)"], .05)[1], "command_timeout")
        self.assertEqual(observer.run_bounded([sys.executable, "-c", "print('x'*5000)"], 2, limit=100)[1], "command_output_limit")

    def test_missing_executable_and_deadline(self):
        self.assertEqual(observer.run_bounded(["/definitely-absent-observer-fixture"], 1)[1], "executable_unavailable")
        self.assertEqual(observer.run_bounded([sys.executable], 0)[1], "observation_deadline")


if __name__ == "__main__":
    unittest.main()
