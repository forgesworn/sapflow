"""The frozen HTTP qualification rejects invalid work and adverse controls."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("http_efficiency", SCRIPTS / "compare-http-efficiency.py")
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)


def campaign():
    rows = []
    for profile in h.PROTOCOL["profiles"]:
        for pair in range(4):
            for variant in ("baseline", "candidate"):
                cpu = 8 if profile["name"] == "target" and variant == "candidate" else 10
                stream = dict(received_bytes=1000000, requests=1000, playlist_p99_ms=10,
                              segment_p99_ms=10, viewers=profile["viewers"],
                              mean_part_reach=profile["viewers"], observed_parts=300,
                              max_drain_seconds=.1,
                              playlist_samples=120, segment_samples=120)
                rows.append(dict(profile=profile["name"], pair=pair, variant=variant,
                                 qualified=True, process_cpu_seconds=cpu, process_cpu_ticks=cpu * 100,
                                 cpu_percent=cpu / 60 * 100, peak_rss_mib=20,
                                 streams=[copy.copy(stream) for _ in range(profile["publishers"])],
                                 host_busy_percent=50, host_steal_percent=0,
                                 start_error_seconds=.001, end_error_seconds=.002,
                                 runtime_start_spread_seconds=.01, clock_drift_seconds=.0001))
    return rows


class QualificationTests(unittest.TestCase):
    def test_known_good_campaign_passes(self):
        result = h.qualification(campaign())
        self.assertTrue(result["accepted"], result)
        self.assertEqual(result["profiles"]["target"]["median_ratios"]["cpu_per_byte"], .8)

    def test_missing_failed_and_duplicate_trials_fail(self):
        rows = campaign()
        self.assertFalse(h.qualification(rows[:-1])["accepted"])
        rows[0]["qualified"] = False
        self.assertFalse(h.qualification(rows)["accepted"])
        rows = campaign()
        rows[0] = copy.deepcopy(rows[1])
        self.assertFalse(h.qualification(rows)["accepted"])

    def test_less_delivered_work_cannot_claim_cpu_improvement(self):
        rows = campaign()
        for row in rows:
            if row["profile"] == "target" and row["variant"] == "candidate":
                for stream in row["streams"]:
                    stream["received_bytes"] //= 2
        result = h.qualification(rows)
        self.assertIn("target:useful_work_received_bytes", result["issues"])
        self.assertIn("target:cpu_benefit", result["issues"])

    def test_adverse_single_control_rejects_otherwise_good_target(self):
        rows = campaign()
        for row in rows:
            if row["profile"] == "single" and row["variant"] == "candidate":
                row["process_cpu_seconds"] = 12
        self.assertIn("single:cpu_non_regression", h.qualification(rows)["issues"])

    def test_low_cpu_control_uses_absolute_overhead_and_serialises_zero(self):
        rows = campaign()
        for row in rows:
            if row["profile"] == "sparse":
                row.update(process_cpu_seconds=0, process_cpu_ticks=0, cpu_percent=0)
        result = h.qualification(rows)
        self.assertTrue(result["accepted"])
        json.dumps(result, allow_nan=False)
        for row in rows:
            if row["profile"] == "sparse" and row["variant"] == "candidate":
                row["cpu_percent"] = .21
        self.assertIn("sparse:cpu_non_regression", h.qualification(rows)["issues"])

    def test_target_requires_sufficient_ticks_and_three_good_pairs(self):
        rows = campaign()
        for row in rows:
            if row["profile"] == "target":
                row["process_cpu_ticks"] = 199
        self.assertIn("target:cpu_benefit", h.qualification(rows)["issues"])
        rows = campaign()
        for row in rows:
            if row["profile"] == "target" and row["variant"] == "candidate" and row["pair"] < 2:
                row["process_cpu_seconds"] = 9.6
        self.assertIn("target:cpu_benefit", h.qualification(rows)["issues"])

    def test_rss_worst_pair_cannot_hide_behind_median(self):
        rows = campaign()
        rows[1]["peak_rss_mib"] = 24
        self.assertIn("single:rss", h.qualification(rows)["issues"])

    def test_tail_guard_checks_worst_stream_and_absolute_floor(self):
        rows = campaign()
        candidate = next(r for r in rows if r["profile"] == "target" and r["variant"] == "candidate")
        candidate["streams"][0]["segment_p99_ms"] = 16
        result = h.qualification(rows)
        self.assertIn("target:segment_p99_ms:max", result["issues"])
        self.assertNotIn("target:segment_p99_ms:median", result["issues"])
        candidate["streams"][0]["segment_p99_ms"] = 11
        self.assertTrue(h.qualification(rows)["accepted"])

    def test_host_alignment_samples_and_reach_are_gated(self):
        row = campaign()[0]
        self.assertEqual(h.trial_issues(row), [])
        row.update(host_steal_percent=.51, clock_drift_seconds=.06)
        row["streams"][0].update(mean_part_reach=120, segment_samples=99)
        self.assertEqual(h.trial_issues(row), ["clock_drift_seconds", "host_steal_percent", "insufficient_latency_samples", "part_reach"])

    def test_runtime_and_exact_scalars_are_strict(self):
        marker = "qualification window_start_unix_ns 1000000000 window_end_unix_ns 61000000000\n"
        self.assertEqual(h.window_marker(marker), (1, 61))
        self.assertIsNone(h.window_marker(marker * 2))
        text = "qualification received_bytes 123456789 playlist_samples 101 segment_samples 102 observed_parts 300 mean_part_reach 15.999999\n"
        self.assertEqual(h.exact_work(text)["received_bytes"], 123456789)
        self.assertEqual(h.exact_work(text)["mean_part_reach"], 15.999999)
        with self.assertRaises(ValueError):
            h.exact_work(text * 2)

    def test_post_window_drain_cannot_hide_substantial_work(self):
        row = campaign()[0]
        row["streams"][0]["max_drain_seconds"] = 1.001
        self.assertIn("drain_exceeded", h.trial_issues(row))

    def test_one_shared_part_does_not_hide_nine_single_viewer_parts(self):
        row = next(r for r in campaign() if r["profile"] == "sparse")
        row["streams"][0].update(observed_parts=10, mean_part_reach=1.1)
        self.assertIn("part_reach", h.trial_issues(row))

    def test_host_ticks_do_not_double_count_guest(self):
        self.assertEqual(h.host_ticks("cpu 10 20 30 40 50 60 70 80 900 1000\n"), (360, 90, 80))
        self.assertEqual(h.host_usage((100, 20, 0), (200, 60, 1)),
                         dict(host_busy_percent=60, host_steal_percent=1))


if __name__ == "__main__":
    unittest.main()
