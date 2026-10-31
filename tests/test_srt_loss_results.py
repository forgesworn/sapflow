"""Qualification regressions with fake tools and tiny local UDP ownership checks."""

import copy
import csv
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("srt_loss_results", ROOT / "scripts/srt-loss-results.py")
results = importlib.util.module_from_spec(spec)
spec.loader.exec_module(results)
udp_spec = importlib.util.spec_from_file_location("srt_loss_udp", ROOT / "scripts/srt-loss-udp.py")
udp = importlib.util.module_from_spec(udp_spec)
udp_spec.loader.exec_module(udp)

GOOD = {"metrics": dict(video=100, audio=200, decode_msgs=0, data_orig=80,
                        data_retx=0, naks=0, handshake_flags="0xbf",
                        reordered=0, orig_distinct=80, in_time=80),
        "outcomes": {field: 0 for field in [*results.EXITS, *results.STOPS]}}

# These executable fixtures only write text and wait for owned signals. They
# exercise the real shell orchestration without a codec or network process.
FAKE_TOOL = r'''#!PYTHON
import sys
import time

args = sys.argv[1:]
# POSIX executable fixture names are generated as commands/<role>.
# Keep the exact sleep payload without importing unrelated fake-tool support.
if sys.argv[0].rsplit("/", 1)[-1] == "sleep":
    time.sleep(min(float(args[0]), 0.08))
    sys.exit(0)

import os
from pathlib import Path
import signal
import socket
from urllib.parse import urlsplit

role = Path(sys.argv[0]).name
case = os.environ.get("FIXTURE_CASE", "pass")
if role == "pkill":
    with open(os.environ["FIXTURE_SIGNAL_TRACE"], "a") as trace:
        trace.write("pkill\t" + " ".join(args) + "\n")
    os.execv(os.environ["FIXTURE_REAL_PKILL"], [os.environ["FIXTURE_REAL_PKILL"], *args])
elif role == "cargo":
    raise RuntimeError("offline test must never build Rust")
elif role == "ffprobe":
    if case == "probe_exit":
        sys.exit(21)
    video = args[args.index("-select_streams") + 1] == "v"
    print(0 if case == "zero_audio" and not video else (100 if video else 200))
elif role == "ffmpeg":
    if "-re" in args:
        if case == "encoder_exit":
            sys.exit(17)
        target = urlsplit(args[-1])
        with open(os.environ["FIXTURE_UDP_TRACE"], "a") as trace:
            trace.write("feeder\t" + args[-1] + "\n")
        source = Path(args[args.index("-i") + 1]).read_bytes()
        if target.scheme == "udp":
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as output:
                output.sendto(source, (target.hostname, target.port))
        else:
            sys.stdout.buffer.write(source)
    elif "lavfi" in args:
        if case == "generation_exit":
            sys.exit(19)
        Path(args[-1]).write_bytes(bytes([0x47, 0x01, 0, 0x10]) + b"g" * 184)
    else:
        if case == "decoder_messages":
            print("fixture decoding error", file=sys.stderr)
        if case == "decoder_exit":
            sys.exit(23)
elif role == "srt-live-transmit":
    if args[0].startswith("srt://"):
        sys.stdout.buffer.write(b"generated fixture output")
        sys.stdout.buffer.flush()
        signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            time.sleep(0.01)
    elif any(arg.startswith("udp://") for arg in args):
        source = urlsplit(next(arg for arg in args if arg.startswith("udp://")))
        signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as incoming:
            incoming.bind((source.hostname, source.port))
            with open(os.environ["FIXTURE_UDP_TRACE"], "a") as trace:
                trace.write("bound\t" + str(os.getpid()) + "\t" + str(source.port) + "\t" + " ".join(args) + "\n")
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                time.sleep(0.01)
    else:
        sys.stdin.buffer.read()
elif role == "receiver":
    output = Path(os.environ["SAPFLOW_RECORDINGS"]) / "live/event-loss/output.mp4"
    output.parent.mkdir(parents=True)
    output.write_bytes(b"generated fixture output")
    if case == "trial_crash":
        os.kill(os.getppid(), signal.SIGKILL)
        sys.exit(0)
    if case == "receiver_exit":
        sys.exit(29)
    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        time.sleep(0.01)
elif role == "proxy":
    output = Path(args[args.index("--stats") + 1])
    def finish(*_):
        output.write_text("data_orig=80\ndata_retx=0\nnak_packets=0\nhs_flags=0xbf\nreordered_orig=0\norig_distinct=80\nin_time=80\n")
        sys.exit(0)
    signal.signal(signal.SIGINT, finish)
    signal.signal(signal.SIGTERM, finish)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        time.sleep(0.01)
    finish()
else:
    raise RuntimeError(role)
'''


class OfflineTrial:
    """Reusable by the private before/after proof against the frozen old shell."""

    def __init__(self, directory, script=None):
        self.root = Path(directory)
        self.commands = self.root / "commands"
        self.commands.mkdir()
        for name in ["ffmpeg", "ffprobe", "srt-live-transmit", "receiver", "proxy", "sleep", "cargo"]:
            path = self.commands / name
            path.write_text(FAKE_TOOL.replace("#!PYTHON", "#!" + sys.executable, 1))
            path.chmod(0o755)
        self.script = Path(script) if script else ROOT / "scripts/srt-loss.sh"
        self.output = self.root / "output"
        self.udp_trace = self.root / "udp-order.tsv"
        # Reserve a free ten-port block for the two planned fixture trials.
        for candidate in range(24000, 64000, 10):
            reservations = []
            try:
                for port in range(candidate, candidate + 10):
                    reservation = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                    reservations.append(reservation)
                    reservation.bind(("127.0.0.1", port))
                self.port_base = candidate
                break
            except OSError:
                pass
            finally:
                for reservation in reservations:
                    reservation.close()
        else:
            raise RuntimeError("no free fixture port block")

    def run(self, case="pass", encrypted=False, libsrt=False, trace_signals=False):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("SAPFLOW_") and key not in [
                   "PASSPHRASE", "PBKEYLEN", "KMREFRESH", "KMPREANNOUNCE",
                   "SENDER_PASSPHRASE", "KEEP", "OUT", "WORK"]}
        env.update(PATH=str(self.commands) + os.pathsep + os.environ["PATH"],
                   FIXTURE_CASE=case, SAPFLOW_BIN=str(self.commands / "receiver"),
                   PROXY_BIN=str(self.commands / "proxy"), OUT=str(self.output),
                   TMPDIR=str(self.root), RECEIVERS="own-fixture=" + str(self.commands / "receiver"),
                   SEEDS="1", LOSSES="random:3", LATENCIES="200", CLIP_SECS="1", JOBS="1",
                   PORT_BASE=str(self.port_base), FIXTURE_UDP_TRACE=str(self.udp_trace))
        if libsrt:
            env["RECEIVERS"] = "libsrt"
        if trace_signals:
            self.signal_trace = self.root / "child-signals.tsv"
            environment = self.root / "bash-env"
            environment.write_text('''
kill() {
  if [ "${1:-}" != -0 ]; then printf 'kill\\t%s\\n' "$*" >> "$FIXTURE_SIGNAL_TRACE"; fi
  builtin kill "$@"
}
wait() {
  local fixture_wait_code=0
  builtin wait "$@" || fixture_wait_code=$?
  printf 'wait\\t%s\\t%s\\n' "${1:-all}" "$fixture_wait_code" >> "$FIXTURE_SIGNAL_TRACE"
  return "$fixture_wait_code"
}
''')
            env.update(BASH_ENV=str(environment), FIXTURE_SIGNAL_TRACE=str(self.signal_trace),
                       FIXTURE_REAL_PKILL=shutil.which("pkill"))
            wrapped_pkill = self.commands / "pkill"
            wrapped_pkill.write_text(FAKE_TOOL.replace("#!PYTHON", "#!" + sys.executable, 1))
            wrapped_pkill.chmod(0o755)
        if encrypted:
            env.update(PASSPHRASE="fixture-test-key", PBKEYLEN="32", KMREFRESH="2000")
        process = subprocess.Popen(["bash", str(self.script)], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=15)
        finally:
            # Include orphaned fixtures if the crash case killed its trial shell.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        self.stdout, self.stderr = stdout, stderr
        return process.returncode

    def rows(self):
        with (self.output / "results.tsv").open(newline="") as stream:
            return list(csv.DictReader(stream, delimiter="\t"))


def write_plan(directory, count):
    (directory / "trials").mkdir()
    with (directory / "plan.tsv").open("w", newline="") as stream:
        writer = csv.writer(stream, delimiter="\t", lineterminator="\n")
        writer.writerow(["trial_index", "receiver", "receiver_spec", "loss", "latency_ms", "seed", "encryption"])
        for index in range(1, count + 1):
            writer.writerow([index, "own", "own", "random:10", 200, index, ""])


class QualificationTests(unittest.TestCase):
    def test_completed_av_and_controlled_sender_shutdown_pass(self):
        self.assertEqual(results.qualify(GOOD, 0), [])
        controlled = copy.deepcopy(GOOD)
        for name in ["sender", "receiver", "proxy"]:
            controlled["outcomes"][name + "_stop"] = 1
            controlled["outcomes"][name + "_exit"] = 143
        self.assertEqual(results.qualify(controlled, 0), [])

    def test_error_messages_zero_packets_and_actual_exits_fail(self):
        changes = [("metrics", "decode_msgs", 1), ("metrics", "audio", 0),
                   ("metrics", "video", "NA"), ("metrics", "in_time", 81),
                   ("metrics", "data_orig", 0), ("outcomes", "encoder_exit", 17),
                   ("outcomes", "sender_exit", 143), ("outcomes", "decoder_exit", 23),
                   ("outcomes", "video_probe_exit", 21), ("outcomes", "receiver_exit", 29),
                   ("outcomes", "proxy_exit", None), ("outcomes", "encoder_timeout", 1)]
        for section, key, value in changes:
            with self.subTest(key=key, value=value):
                raw = copy.deepcopy(GOOD)
                raw[section][key] = value
                self.assertTrue(results.qualify(raw, 0))
        self.assertTrue(results.qualify(GOOD, 137))
        self.assertTrue(results.qualify(GOOD, None))
        raw = copy.deepcopy(GOOD)
        raw["outcomes"]["encoder_exit"] = False
        self.assertTrue(results.qualify(raw, 0))

    def test_decoder_messages_fail_clean_runs_but_are_a_result_under_loss(self):
        raw = copy.deepcopy(GOOD)
        raw["metrics"]["decode_msgs"] = 7
        self.assertTrue(results.qualify(raw, 0))
        self.assertTrue(results.qualify(raw, 0, "none"))
        self.assertEqual(results.qualify(raw, 0, "random:10"), [])
        self.assertEqual(results.qualify(raw, 0, "burst:8,8"), [])
        for value in [-1, None, "7"]:
            with self.subTest(value=value):
                raw["metrics"]["decode_msgs"] = value
                self.assertTrue(results.qualify(raw, 0, "random:10"))

    def test_collation_retains_order_crashes_and_not_attempted_trials(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            write_plan(output, 4)
            for index in [4, 1, 2]:
                (output / f"trials/{index}.started").touch()
            for index in [1, 4]:
                (output / f"trials/{index}.json").write_text(json.dumps(GOOD))
                (output / f"trials/{index}.exit").write_text("0\n")
            (output / "trials/2.exit").write_text("137\n")
            self.assertFalse(results.collate(output))
            receipt = json.loads((output / "qualification.json").read_text())
            self.assertEqual([row["trial_index"] for row in receipt["trials"]], ["1", "2", "3", "4"])
            self.assertEqual([row["status"] for row in receipt["trials"]], ["ok", "FAIL", "not_attempted", "ok"])
            self.assertEqual((receipt["planned"], receipt["attempted"], receipt["passed"], receipt["failed"], receipt["not_attempted"]), (4, 3, 2, 1, 1))
            self.assertIn("missing or invalid trial receipt", receipt["trials"][1]["failure"])
            self.assertEqual(receipt["trials"][1]["trial_exit"], 137)

    def test_malformed_receipt_and_duplicate_plan_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            write_plan(output, 1)
            (output / "trials/1.started").touch()
            (output / "trials/1.json").write_text("{partial")
            self.assertFalse(results.collate(output))
            with (output / "plan.tsv").open("a") as stream:
                stream.write("1\town\town\trandom:10\t200\t1\t\n")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                results.collate(output)

    def test_real_shell_retains_tool_failures_and_returns_nonzero(self):
        for case, field, value in [
            ("decoder_exit", "decoder_exit", "23"),
            ("encoder_exit", "encoder_exit", "17"), ("probe_exit", "video_probe_exit", "21"),
            ("zero_audio", "audio", "0"), ("receiver_exit", "receiver_exit", "29"),
        ]:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as temporary:
                trial = OfflineTrial(temporary)
                self.assertNotEqual(trial.run(case), 0, trial.stdout + trial.stderr)
                rows = trial.rows()
                self.assertEqual(len(rows), 2)
                for row in rows:
                    self.assertEqual((row["status"], row[field]), ("FAIL", value), row)
                    self.assertTrue(row["failure"])
                    self.assertTrue((trial.output / f"logs/{row['trial_index']}-own-fixture/ff.log").exists())

    def test_real_shell_decoder_messages_fail_the_clean_run_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            # The clean baseline fails, so the collection still exits nonzero.
            self.assertNotEqual(trial.run("decoder_messages"), 0, trial.stdout + trial.stderr)
            rows = {row["loss"]: row for row in trial.rows()}
            self.assertEqual((rows["none"]["status"], rows["none"]["decode_msgs"]), ("FAIL", "1"))
            self.assertIn("decode_msgs=1", rows["none"]["failure"])
            self.assertEqual((rows["random:3"]["status"], rows["random:3"]["decode_msgs"],
                              rows["random:3"]["trial_exit"], rows["random:3"]["failure"]),
                             ("ok", "1", "0", ""))
            self.assertIn("| random:3 | 200 ms | own-fixture | 1 |", trial.stdout)

    def test_real_shell_pass_and_encryption_metadata(self):
        for encrypted, libsrt in [(False, False), (True, False), (False, True)]:
            with self.subTest(encrypted=encrypted, libsrt=libsrt), tempfile.TemporaryDirectory() as temporary:
                trial = OfflineTrial(temporary)
                self.assertEqual(trial.run(encrypted=encrypted, libsrt=libsrt), 0, trial.stdout + trial.stderr)
                self.assertEqual([row["status"] for row in trial.rows()], ["ok", "ok"])
                receipt = json.loads((trial.output / "qualification.json").read_text())
                self.assertEqual((receipt["status"], receipt["planned"], receipt["passed"]), ("passed", 2, 2))
                self.assertIn("aes256" if encrypted else "none", trial.rows()[0]["encryption"])
                self.assertNotIn("Encryption: 0", trial.stdout)

    def test_trial_crash_stays_failed_instead_of_disappearing(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            self.assertNotEqual(trial.run("trial_crash"), 0, trial.stdout + trial.stderr)
            rows = trial.rows()
            self.assertEqual(len(rows), 2)
            self.assertTrue(all(row["status"] == "FAIL" and row["attempted"] == "1" for row in rows))
            self.assertTrue(all("missing or invalid trial receipt" in row["failure"] for row in rows))
            self.assertIn("own-fixture random:3 200 ms", trial.stdout)
            self.assertTrue((trial.output / "logs/1-own-fixture/rcv.log").is_file())

    def test_generation_failure_keeps_full_plan_without_claiming_attempts(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            self.assertNotEqual(trial.run("generation_exit"), 0)
            rows = trial.rows()
            self.assertEqual(len(rows), 2)
            self.assertTrue(all(row["status"] == "not_attempted" and row["attempted"] == "0" for row in rows))
            receipt = json.loads((trial.output / "qualification.json").read_text())
            self.assertEqual(receipt["runner_body_exit"], 19)
            self.assertNotEqual((trial.output / "run.exit").read_text().strip(), "0")

    def test_existing_output_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            trial.output.mkdir()
            prior = trial.output / "plan.tsv"
            prior.write_text("retained prior receipt\n")
            self.assertNotEqual(trial.run(), 0)
            self.assertEqual(prior.read_text(), "retained prior receipt\n")

    def test_reaped_child_ids_are_not_signalled_by_late_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            self.assertEqual(trial.run(trace_signals=True), 0, trial.stdout + trial.stderr)
            reaped = set()
            for line in trial.signal_trace.read_text().splitlines():
                action, arguments, *_ = line.split("\t")
                if action == "wait" and arguments.isdecimal():
                    reaped.add(arguments)
                elif action in ("kill", "pkill"):
                    self.assertNotIn(arguments.split()[-1], reaped, line)
            # Producer shell, sender, receiver and proxy from both trials.
            self.assertEqual(len(reaped), 8)

    def test_udp_sender_owns_input_before_feeder_without_a_byte_pipe(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            self.assertEqual(trial.run(), 0, trial.stdout + trial.stderr)
            events = [line.split("\t") for line in trial.udp_trace.read_text().splitlines()]
            self.assertEqual([row[0] for row in events], ["bound", "feeder", "bound", "feeder"])
            for bound, feeder in zip(events[::2], events[1::2]):
                self.assertIn("-c:1316", bound[3])
                self.assertNotIn("file://con", bound[3])
                self.assertEqual(feeder[1], f"udp://127.0.0.1:{bound[2]}?pkt_size=1316")
            self.assertNotEqual(events[0][2], events[2][2])

    def test_occupied_fixture_port_fails_without_stopping_its_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            trial = OfflineTrial(temporary)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as unrelated:
                unrelated.bind(("127.0.0.1", trial.port_base + 4))
                self.assertNotEqual(trial.run(), 0, trial.stdout + trial.stderr)
                self.assertEqual([row["status"] for row in trial.rows()], ["FAIL", "ok"])
                unrelated.sendto(b"owner remains live", unrelated.getsockname())
                unrelated.settimeout(1)
                self.assertEqual(unrelated.recvfrom(64)[0], b"owner remains live")


class UdpFixtureTests(unittest.TestCase):
    def test_generated_source_rejects_zero_tail_slots_and_truncated_messages(self):
        packet = bytes([0x47, 0x01, 0, 0x10]) + b"g" * 184
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "generated.ts"
            for size in (1, 3, 7):
                source.write_bytes(packet * size)
                self.assertEqual(udp.check_ts(source)[0], size)
            for invalid in (b"", packet * 6 + bytes(188), packet * 7 + b"x"):
                source.write_bytes(invalid)
                with self.assertRaises(ValueError):
                    udp.check_ts(source)

    def test_actual_udp_owner_does_not_match_another_pid(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as owned:
            owned.bind(("127.0.0.1", 0))
            port = owned.getsockname()[1]
            self.assertTrue(udp.owns_udp(os.getpid(), port))
            self.assertFalse(udp.owns_udp(2**30, port))
            with self.assertRaises(OSError):
                udp.check_free([port], [])

    def test_linux_shared_udp_table_requires_child_socket_inode(self):
        with tempfile.TemporaryDirectory() as temporary:
            proc = Path(temporary)
            child = proc / "123"
            (child / "fd").mkdir(parents=True)
            (child / "net").mkdir()
            (child / "fd/4").symlink_to("socket:[99]")
            table = child / "net/udp"
            table.write_text("header\n0: 0100007F:4A3C 00000000:0000 07 0 0 0 0 0 100\n")
            self.assertFalse(udp.proc_udp_owner(123, 19004, proc), "other process table row")
            (child / "fd/4").unlink()
            (child / "fd/4").symlink_to("socket:[100]")
            self.assertTrue(udp.proc_udp_owner(123, 19004, proc))

    def test_all_planned_five_port_blocks_must_fit_without_wrapping(self):
        for base, count, expected in [(19000, 84, 0), (65531, 1, 0), (65532, 1, 1), (19000, 0, 1)]:
            run = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/srt-loss-udp.py"),
                                  "range", "--base", str(base), "--trials", str(count)],
                                 capture_output=True, text=True, timeout=3)
            self.assertEqual(run.returncode, expected, run.stderr)

    def test_historical_error_rows_do_not_pass_summary(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "historical.tsv"
            # The former writer labelled this row ok despite an error message.
            path.write_text("\t".join(results.HEADER[:15]) + "\n" +
                            "own\tnone\t200\t1\t100\t200\t1\t80\t0\t0\t0xbf\tok\t0\t80\t80\n")
            run = subprocess.run(["bash", str(ROOT / "scripts/srt-loss.sh"), "--summarise", str(path)],
                                 text=True, capture_output=True, timeout=5)
            self.assertNotEqual(run.returncode, 0)
            self.assertIn("Unqualified trial: own none", run.stdout)

    def test_summary_reports_decoder_messages_under_loss(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "damaged.tsv"
            path.write_text("\t".join(results.HEADER[:15]) + "\n" +
                            "libsrt\tnone\t200\t1\t100\t200\t0\t80\t0\t0\t0xbf\tok\t0\t80\t80\n" +
                            "libsrt\trandom:10\t200\t1\t100\t198\t12\t80\t9\t9\t0xbf\tok\t0\t80\t79\n")
            run = subprocess.run(["bash", str(ROOT / "scripts/srt-loss.sh"), "--summarise", str(path)],
                                 text=True, capture_output=True, timeout=5)
            self.assertEqual(run.returncode, 0, run.stdout)
            self.assertNotIn("Unqualified", run.stdout)
            self.assertIn("| random:10 | 200 ms | libsrt | 1 |", run.stdout)
            self.assertIn("| 12.0 ± 0.0 |", run.stdout)

    def test_empty_summary_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "empty.tsv"
            path.write_text("\t".join(results.HEADER) + "\n")
            run = subprocess.run(["bash", str(ROOT / "scripts/srt-loss.sh"), "--summarise", str(path)],
                                 text=True, capture_output=True, timeout=5)
            self.assertNotEqual(run.returncode, 0)
            self.assertIn("no trial rows", run.stdout)


if __name__ == "__main__":
    unittest.main()
