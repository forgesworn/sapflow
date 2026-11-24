"""Regression checks for the library benchmark gates; no builds or timed work."""
import copy
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "qualify_library_bench", Path(__file__).resolve().parents[1] / "scripts/qualify-library-bench.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def campaign(name="ts-ingest", candidate=4.0, baseline=5.0):
    spec = RUNNER.BENCHES[name]
    rows = []
    for pair in range(1, RUNNER.PAIRS + 1):
        for arm in (("baseline", "candidate") if pair % 2 else ("candidate", "baseline")):
            seconds = candidate if arm == "candidate" else baseline
            rows.append({"pair": pair, "arm": arm, "exit": 0, "host": {"valid": True},
                         "result": {"profile": spec.get("profile", name), "elapsed_ns": round(seconds * 1e9),
                                    "operations": spec["rounds"] * spec["operations_per_round"]}})
    return rows


class GateTests(unittest.TestCase):
    def test_a_balanced_improving_campaign_passes_for_each_bench(self):
        for name in RUNNER.BENCHES:
            self.assertTrue(RUNNER.qualify(campaign(name), name)["passed"])

    def test_no_benefit_or_one_regressing_pair_fails(self):
        self.assertFalse(RUNNER.qualify(campaign(candidate=4.9), "ts-ingest")["passed"])
        rows = campaign()
        rows[1]["result"]["elapsed_ns"] = 5_010_000_000
        result = RUNNER.qualify(rows, "ts-ingest")
        self.assertTrue(result["median_pass"])
        self.assertFalse(result["passed"])

    def test_short_unbalanced_incomplete_and_invalid_rows_fail(self):
        original = campaign()
        cases = [campaign(candidate=2.9, baseline=3.5), original[:-1]]
        rows = copy.deepcopy(original)
        rows[0], rows[1] = rows[1], rows[0]
        cases.append(rows)
        for change in ({"exit": 1}, {"host": {"valid": False}}, {"host": {}}, {"result": None}):
            rows = copy.deepcopy(original)
            rows[2].update(change)
            cases.append(rows)
        for key, bad in (("operations", 1), ("profile", "other"), ("elapsed_ns", 0)):
            rows = copy.deepcopy(original)
            rows[3]["result"][key] = bad
            cases.append(rows)
        for rows in cases:
            self.assertFalse(RUNNER.qualify(rows, "ts-ingest")["passed"])


if __name__ == "__main__":
    unittest.main()
