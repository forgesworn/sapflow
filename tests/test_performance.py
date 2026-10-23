"""Refuse misleading benchmark receipts without building or running a server."""

import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("performance", Path(__file__).resolve().parents[1] / "scripts/performance.py")
performance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(performance)

REPORT = """--- sapflow, 750 viewers: server CPU 34.6%  peak RSS 27.2 MB
server sampling 60.010 s target 60 s
viewers 750  measured 60 s after a 10 s ramp
collection completed requests wholly within the fixed post-ramp window
excluded requests 500  errors 0
requests 588290  errors 0  received 2000.0 MB
playlist  n=45000  p50 8.5 ms  p95 20.0 ms  p99 26.9 ms  max 60.0 ms
segment   n=9000   p50 5.0 ms  p95 15.0 ms  p99 22.0 ms  max 62.0 ms
part fan-out (first to last viewer) parts=300 reach 744/750  p50 27.7 ms  p95 45.5 ms  max 65.5 ms
"""


class PerformanceTests(unittest.TestCase):
    def test_incomplete_failed_and_empty_client_output_are_refused(self):
        for report in ("", REPORT.split("requests")[0], REPORT.replace("n=9000", "n=0"), REPORT.replace("parts=300", "parts=0")):
            with self.subTest(report=report), self.assertRaises(ValueError):
                performance.parse_report(report, [750])

    def test_unexpected_missing_and_duplicate_loads_are_refused(self):
        for report, viewers in ((REPORT, [250, 750]), (REPORT, [250]), (REPORT * 2, [750])):
            with self.subTest(viewers=viewers), self.assertRaises(ValueError):
                performance.parse_report(report, viewers)

    def test_errors_are_retained_for_failure_decision(self):
        row = performance.parse_report(REPORT.replace("errors 0", "errors 3"), [750])[0]
        self.assertEqual(row["errors"], 3)
        self.assertEqual(row["requests"], 588290)
        self.assertEqual(row["part_reach"], 744)

    def test_old_ramp_inclusive_reports_are_refused_and_excluded_errors_survive(self):
        with self.assertRaises(ValueError):
            performance.parse_report(REPORT.replace("collection completed requests wholly within the fixed post-ramp window", ""), [750])
        row = performance.parse_report(REPORT.replace("excluded requests 500  errors 0", "excluded requests 500  errors 2"), [750])[0]
        self.assertEqual((row["errors"], row["excluded_errors"]), (0, 2))

    def test_full_cycle_balances_every_treatment_position(self):
        treatments = ["hyper", "own-1", "own-4", "own", "mediamtx"]
        orders = [performance.trial_order(treatments, n) for n in range(len(treatments))]
        for column in zip(*orders):
            self.assertEqual(set(column), set(treatments))

    def test_pair_order_reverses_and_three_treatments_rotate(self):
        self.assertEqual(performance.trial_order(["hyper", "own"], 1), ["own", "hyper"])
        treatments = ["hyper", "own", "mediamtx"]
        self.assertEqual([performance.trial_order(treatments, n)[0] for n in range(3)], treatments)

    def test_summary_retains_spread_and_sums_failures(self):
        rows = performance.parse_report(REPORT, [750])
        runs = [dict(variant="own", measurements=rows), dict(variant="own", measurements=[dict(rows[0], cpu_percent=40.0, errors=1)])]
        result = performance.summary(runs)[0]
        self.assertEqual(result["cpu_percent"], dict(median=37.3, minimum=34.6, maximum=40.0))
        self.assertEqual(result["errors"], 1)
        self.assertEqual(result["requests"], 1176580)


if __name__ == "__main__":
    unittest.main()
