"""Regression checks for qualification gates; no Rust builds or timed workloads."""
import copy
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "qualify_srt_gap", Path(__file__).resolve().parents[1] / "scripts/qualify-srt-gap.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def sample(profile, work, seconds=5.0):
    operations = work * RUNNER.PROFILES[profile]["multiplier"]
    return {"profile": profile, "operations": operations, "elapsed_ns": round(seconds * 1e9),
            "verified_deliveries": 1 if profile == "pending-and-timer" else operations,
            "gap": 4096 if profile == "bursty-stream" else 0, "payload_bytes": 1200}


def campaign():
    workloads = {name: {"work": spec["pilot"]} for name, spec in RUNNER.PROFILES.items()}
    rows = []
    for pair in range(1, RUNNER.PAIRS + 1):
        for profile in RUNNER.PROFILES:
            for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline")):
                seconds = 4.5 if profile == "bursty-stream" and arm == "candidate" else 5.0
                rows.append({"profile": profile, "pair": pair, "arm": arm, "exit": 0,
                             "host": {"valid": True},
                             "result": sample(profile, workloads[profile]["work"], seconds)})
    return rows, workloads


class SampleTests(unittest.TestCase):
    def test_valid_profiles(self):
        for profile in RUNNER.PROFILES:
            RUNNER.validate_sample(sample(profile, 100), profile, 100)

    def test_invalid_payload_or_work_cannot_qualify(self):
        for key, bad in (("operations", 99), ("operations", True), ("verified_deliveries", 99),
                         ("elapsed_ns", 0), ("payload_bytes", 1199), ("gap", 1),
                         ("profile", "timer")):
            with self.subTest(key=key, bad=bad):
                value = sample("clean-stream", 100)
                value[key] = bad
                with self.assertRaises(ValueError):
                    RUNNER.validate_sample(value, "clean-stream", 100)

    def test_non_object_rejected(self):
        for value in ([], None, "sample"):
            with self.assertRaises(ValueError):
                RUNNER.validate_sample(value, "clean-stream", 100)

    def test_pending_requires_one_verified_delivery(self):
        value = sample("pending-and-timer", 100)
        value["verified_deliveries"] = 100
        with self.assertRaises(ValueError):
            RUNNER.validate_sample(value, "pending-and-timer", 100)


class CalibrationTests(unittest.TestCase):
    def pilots(self, first, second, profile="clean-stream"):
        work = RUNNER.PROFILES[profile]["pilot"]
        return {"baseline": sample(profile, work, first), "candidate": sample(profile, work, second)}

    def test_faster_arm_determines_common_work(self):
        result = RUNNER.choose_work("clean-stream", self.pilots(0.5, 1.0))
        self.assertEqual(result["work"], 200_000_000)
        self.assertEqual(result["predicted_slower_seconds"], 10.0)

    def test_count_and_slow_time_caps_are_hard_failures(self):
        for durations in ((0.001, 0.002), (0.5, 20.0)):
            with self.assertRaises(ValueError):
                RUNNER.choose_work("clean-stream", self.pilots(*durations))

    def test_missing_or_mismatched_pilot_rejected(self):
        pilots = self.pilots(0.5, 1.0)
        del pilots["candidate"]
        with self.assertRaises(ValueError):
            RUNNER.choose_work("clean-stream", pilots)
        pilots = self.pilots(0.5, 1.0)
        pilots["candidate"]["operations"] += 1
        with self.assertRaises(ValueError):
            RUNNER.choose_work("clean-stream", pilots)


class GateTests(unittest.TestCase):
    def test_complete_balanced_campaign_passes(self):
        self.assertTrue(RUNNER.qualify(*campaign())["passed"])

    def test_single_bad_nominal_pair_fails_despite_good_median(self):
        rows, workloads = campaign()
        rows[1]["result"]["elapsed_ns"] = 5_550_000_000
        result = RUNNER.qualify(rows, workloads)
        self.assertFalse(result["passed"])
        self.assertTrue(result["profiles"]["clean-stream"]["median_pass"])

    def test_burst_requires_every_pair_not_to_regress(self):
        rows, workloads = campaign()
        rows[5]["result"]["elapsed_ns"] = 5_010_000_000
        self.assertFalse(RUNNER.qualify(rows, workloads)["passed"])

    def test_median_guard_can_fail_without_worst_pair_failure(self):
        rows, workloads = campaign()
        for row in rows:
            if row["profile"] == "clean-stream" and row["arm"] == "candidate":
                row["result"]["elapsed_ns"] = 5_300_000_000
        result = RUNNER.qualify(rows, workloads)
        self.assertFalse(result["passed"])
        self.assertTrue(result["profiles"]["clean-stream"]["every_pair_pass"])

    def test_short_missing_duplicate_unbalanced_and_invalid_host_fail(self):
        original, workloads = campaign()
        cases = []
        rows = copy.deepcopy(original)
        rows[0]["result"]["elapsed_ns"] = 2_999_999_999
        cases.append(rows)
        cases.append(original[:-1])
        rows = copy.deepcopy(original)
        rows[1] = copy.deepcopy(rows[0])
        cases.append(rows)
        rows = copy.deepcopy(original)
        rows[0], rows[1] = rows[1], rows[0]
        cases.append(rows)
        for host in ({}, {"valid": False}, {"valid": "unknown"}):
            rows = copy.deepcopy(original)
            rows[0]["host"] = host
            cases.append(rows)
        for rows in cases:
            self.assertFalse(RUNNER.qualify(rows, workloads)["passed"])

    def test_invalid_results_fail_closed(self):
        for value in (None, [], {"operations": True}):
            rows, workloads = campaign()
            rows[0]["result"] = value
            self.assertFalse(RUNNER.qualify(rows, workloads)["passed"])


class HostTests(unittest.TestCase):
    def observation(self, delta, cpu=5.0):
        return RUNNER.host_observation([0] * 8, delta, 5.0, cpu, 100)

    def test_idle_remaining_cpus_are_valid(self):
        result = self.observation([500, 0, 0, 1500, 0, 0, 0, 0])
        self.assertTrue(result["valid"])
        self.assertEqual(result["external_busy_cores"], 0)

    def test_external_work_and_steal_fail_independently(self):
        self.assertFalse(self.observation([551, 0, 0, 1449, 0, 0, 0, 0])["valid"])
        self.assertFalse(self.observation([500, 0, 0, 1489, 0, 0, 0, 11])["valid"])
        self.assertTrue(self.observation([550, 0, 0, 1440, 0, 0, 0, 10])["valid"])

    def test_missing_backwards_or_inconsistent_accounting_fails(self):
        for delta in (None, [0] * 8, [-1] + [0] * 7, [1, 2], [400, 0, 0, 1600, 0, 0, 0, 0]):
            self.assertFalse(self.observation(delta)["valid"])


class RecoveryTests(unittest.TestCase):
    def test_failed_build_restores_candidate_and_keeps_failure_receipt(self):
        previous = Path.cwd()
        with tempfile.TemporaryDirectory() as directory:
            try:
                os.chdir(directory)
                for name in (RUNNER.RECEIVER, RUNNER.EXAMPLE, "Cargo.toml", "Cargo.lock",
                             "crates/sapflow-srt/Cargo.toml"):
                    path = Path(name)
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"candidate")
                revision = "a" * 40

                def fake_git(*args):
                    return {"rev-parse": revision, "status": "", "diff": RUNNER.RECEIVER}[args[0]]

                def failed_build(source, executable, receiver, output, report, label):
                    receiver.write_bytes(b"baseline installed before compile failure")
                    raise ValueError("simulated compiler failure")

                with mock.patch.object(RUNNER, "git", side_effect=fake_git), \
                     mock.patch.object(RUNNER.platform, "system", return_value="Linux"), \
                     mock.patch.object(RUNNER.platform, "platform", return_value="test Linux"), \
                     mock.patch.object(RUNNER.platform, "machine", return_value="test machine"), \
                     mock.patch.object(RUNNER, "cpu_snapshot", return_value=[1] * 8), \
                     mock.patch.object(RUNNER.subprocess, "check_output", side_effect=["rustc test", b"baseline"]), \
                     mock.patch.object(RUNNER, "build_arm", side_effect=failed_build), \
                     mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": ""}), \
                     mock.patch("sys.argv", ["qualify", "--baseline", "b" * 40, "--candidate", revision,
                                             "--output", "target/result.json"]), \
                     mock.patch("builtins.print"):
                    self.assertEqual(RUNNER.main(), 1)
                self.assertEqual(Path(RUNNER.RECEIVER).read_bytes(), b"candidate")
                receipt = RUNNER.json.loads(Path("target/result.json").read_text())
                self.assertEqual(receipt["status"], "invalid")
                self.assertIn("simulated compiler failure", receipt["error"])
                self.assertFalse(Path("target/srt-gap-qualification-binaries").exists())
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
