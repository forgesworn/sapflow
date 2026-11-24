"""Regression checks for the SRT ingest comparison summary; no servers or media."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "compare_srt_ingest", Path(__file__).resolve().parents[1] / "scripts/compare-srt-ingest.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def trials(pairs, candidate=0.8, baseline=1.0):
    rows = []
    for pair in range(pairs):
        for variant in (("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")):
            cpu = candidate if variant == "candidate" else baseline
            rows.append(dict(pair=pair, variant=variant, qualified=True, cpu_seconds=cpu,
                             wall_seconds=10.0, cpu_percent=10 * cpu))
    return rows


class SummaryTests(unittest.TestCase):
    def test_ratios_pair_each_candidate_with_its_baseline(self):
        result = RUNNER.summarise(trials(4), 4)
        self.assertTrue(result["complete"])
        self.assertEqual(len(result["cpu_ratios"]), 4)
        for ratio in result["cpu_ratios"]:
            self.assertAlmostEqual(ratio, 0.8)
        self.assertAlmostEqual(result["median_cpu_ratio"], 0.8)
        self.assertAlmostEqual(result["baseline_cpu_percent_median"], 10.0)

    def test_a_failed_or_missing_trial_leaves_no_summary(self):
        rows = trials(4)
        rows[3] = dict(pair=1, variant="baseline", qualified=False, error="server or publisher exited")
        self.assertEqual(RUNNER.summarise(rows, 4), dict(complete=False))
        self.assertEqual(RUNNER.summarise(trials(4)[:-1], 4), dict(complete=False))


if __name__ == "__main__":
    unittest.main()
