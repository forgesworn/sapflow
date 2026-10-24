"""Refuse misleading benchmark receipts without building or running a server."""

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
        treatments = ["hyper", "own-1", "own-4", "own", "mediamtx"]
        orders = [performance.trial_order(treatments, n) for n in range(len(treatments))]
        for column in zip(*orders):
            self.assertEqual(set(column), set(treatments))

    def test_pair_order_reverses_and_three_treatments_rotate(self):
        self.assertEqual(performance.trial_order(["hyper", "own"], 1), ["own", "hyper"])
        treatments = ["hyper", "own", "mediamtx"]
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
            (7, "viewer failed\n", "runner exit 7", "hyper", None),
            (0, REPORT.replace("measured 60 s", "measured 1 s"), "viewer workload", "hyper", None),
            (7, "viewer failed\n", "runner exit 7", "own", "8"),
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
                    self.assertEqual(env["SAPFLOW_HTTP_IMPL"], "hyper" if variant == "hyper" else "own")
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
                settings = {"SAPFLOW_HTTP_IMPL": "hyper" if variant == "hyper" else "own"}
                if threads is not None:
                    settings["SAPFLOW_HTTP_THREADS"] = threads
                self.assertEqual(result["http_settings"], {variant: settings})
                trial = result["runs"][0]
                self.assertEqual(trial["runner_exit"], runner_exit)
                self.assertIn(reason, trial["failure"])
                self.assertEqual(trial["measurements"], [])
                self.assertEqual((output / trial["raw"]).read_text(), report)
                self.assertTrue((output / trial["diagnostics"]).is_dir())

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
                    (None, None, "own", "8"), ("hyper", "99", "hyper", None),
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

    def test_ci_smoke_covers_explicit_hyper_and_own_worker_settings(self):
        workflow = (performance.ROOT / ".github/workflows/ci.yml").read_text()
        steps = [step for step in workflow.split("\n      - name:") if "\n        run: scripts/smoke.sh" in step]
        self.assertEqual(len(steps), 2)
        self.assertEqual(sum("SAPFLOW_HTTP_IMPL: hyper" in step for step in steps), 1)
        self.assertEqual(sum("SAPFLOW_HTTP_IMPL: own" in step and "SAPFLOW_HTTP_THREADS: 1" in step for step in steps), 1)

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
