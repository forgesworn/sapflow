"""Finite cadence and owned-process checks, without codecs or a real SRT peer."""

import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("smoke_media", ROOT / "scripts/smoke-media.py")
media = importlib.util.module_from_spec(spec)
spec.loader.exec_module(media)


def framehash(pts):
    return "#tb 0: 1/30\n" + "".join(f"0, {p}, {p}, 1, 10, {'0' * 64}\n" for p in pts)


FAKE = r'''#!PYTHON
from pathlib import Path
import json, os, signal, socket, sys, time
from urllib.parse import urlsplit
role = Path(sys.argv[0]).name
mode = os.environ["FIXTURE_MODE"]
trace = Path(os.environ["FIXTURE_TRACE"])
if role == "srt-live-transmit" and mode == "refuse":
    sys.exit(23)
def record(event):
    with trace.open("a") as output:
        output.write(json.dumps(dict(event=event, pid=os.getpid(), arguments=sys.argv[1:])) + "\n")
if role == "srt-live-transmit":
    uri = urlsplit(next(value for value in sys.argv if value.startswith("udp://")))
    incoming = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    incoming.bind((uri.hostname, uri.port))
    record("bound")
    if mode == "early":
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and '"feeder"' not in trace.read_text():
            time.sleep(0.01)
        time.sleep(0.025)
        sys.exit(17)
else:
    record("feeder")
    if mode == "natural":
        time.sleep(0.1)
        sys.exit(0)
time.sleep(8)
'''


class CadenceTests(unittest.TestCase):
    def test_decoded_30fps_passes(self):
        result = media.cadence(framehash(range(60)))
        self.assertEqual(result["decoded_video_frames"], 60)
        self.assertEqual(result["mean_fps"], 30)

    def test_positive_sparse_or_slow_video_fails(self):
        for pts in [[n * 30 for n in range(30)], [n * 2 for n in range(60)]]:
            with self.subTest(pts=pts[:3]), self.assertRaises(ValueError):
                media.cadence(framehash(pts))

    def test_duplicate_backward_missing_and_large_steps_fail(self):
        for pts in [range(29), [*range(40), 39, *range(40, 60)],
                    [*range(40), 38, *range(40, 60)], [*range(30), *range(33, 70)]]:
            with self.subTest(pts=list(pts)[:3]), self.assertRaises(ValueError):
                media.cadence(framehash(pts))

    def test_pts_are_compared_as_rational_times(self):
        data = framehash([3000 * n for n in range(60)]).replace("1/30", "1/90000")
        self.assertEqual(media.cadence(data)["mean_fps"], 30)
        with self.assertRaises(ValueError):
            media.cadence(data.replace("#tb 0:", "#tb 1:"))


class OwnershipTests(unittest.TestCase):
    def test_tcp_preflight_preserves_live_listener_and_allows_restart(self):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
            listener.listen(1)
            with self.assertRaises(OSError):
                media.check_tcp_ports([port])
            with socket.create_connection(listener.getsockname(), timeout=1) as client:
                accepted, _ = listener.accept()
                accepted.close()
                self.assertEqual(client.recv(1), b"")
        media.check_tcp_ports([port])

    def test_smoke_busy_port_exits_before_build_or_server_and_cleans_empty_list(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            marker = work / "unexpected-server"
            forbidden = work / "server"
            forbidden.write_text("#!/bin/sh\ntouch '" + str(marker) + "'\nexit 1\n")
            forbidden.chmod(0o755)
            for name in ["ffmpeg", "ffprobe", "srt-live-transmit", "curl"]:
                (work / name).symlink_to("/usr/bin/true")
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as owner:
                owner.bind(("127.0.0.1", 18890))
                env = {key: value for key, value in os.environ.items() if key != "SMOKE_KEEP"}
                env.update(PATH=str(work) + os.pathsep + env["PATH"], SAPFLOW_BIN=str(forbidden))
                run = subprocess.run(["bash", str(ROOT / "scripts/smoke.sh")], env=env,
                                     capture_output=True, text=True, timeout=5)
                self.assertNotEqual(run.returncode, 0)
                self.assertFalse(marker.exists())
                self.assertNotIn("unbound variable", run.stderr)
                directory = Path(run.stdout.split("smoke work: ", 1)[1].splitlines()[0])
                self.assertFalse(directory.exists())
                owner.sendto(b"preserved", owner.getsockname())
                owner.settimeout(1)
                self.assertEqual(owner.recvfrom(16)[0], b"preserved")

    def run_fixture(self, mode, occupied=False):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            for name in ["ffmpeg", "srt-live-transmit"]:
                path = work / name
                path.write_text(FAKE.replace("#!PYTHON", "#!" + sys.executable, 1))
                path.chmod(0o755)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
                if not occupied:
                    reservation.close()
                trace = work / "trace.jsonl"
                env = dict(os.environ, PATH=str(work) + os.pathsep + os.environ["PATH"],
                           FIXTURE_MODE=mode, FIXTURE_TRACE=str(trace))
                command = [sys.executable, "-B", str(ROOT / "scripts/smoke-media.py"), "publish",
                           "--uri", "srt://127.0.0.1:9", "--seconds", "1", "--port", str(port)]
                process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, text=True, start_new_session=True)
                try:
                    if mode == "cancel":
                        deadline = time.monotonic() + 4
                        while time.monotonic() < deadline and (not trace.exists() or '"feeder"' not in trace.read_text()):
                            time.sleep(0.025)
                        self.assertTrue(trace.exists() and '"feeder"' in trace.read_text())
                        process.send_signal(signal.SIGTERM)
                    stdout, stderr = process.communicate(timeout=10)
                finally:
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
                events = [json.loads(line) for line in trace.read_text().splitlines()] if trace.exists() else []
                for row in events:
                    with self.assertRaises(ProcessLookupError):
                        os.kill(row["pid"], 0)
                if occupied:
                    reservation.sendto(b"owner preserved", reservation.getsockname())
                    reservation.settimeout(1)
                    self.assertEqual(reservation.recvfrom(64)[0], b"owner preserved")
                else:
                    media.udp.check_free([port], [])
                return process.returncode, events, stdout + stderr

    def test_natural_finish_stops_and_reaps_sender(self):
        code, events, output = self.run_fixture("natural")
        self.assertEqual(code, 0, output)
        self.assertEqual([row["event"] for row in events], ["bound", "feeder"])
        self.assertIn("-c:1316", events[0]["arguments"])
        self.assertIn("testsrc=size=1280x720:rate=30", events[1]["arguments"])
        self.assertTrue(events[1]["arguments"][-1].endswith("?pkt_size=1316"))

    def test_refusal_never_starts_encoder(self):
        code, events, _ = self.run_fixture("refuse")
        self.assertNotEqual(code, 0)
        self.assertEqual(events, [])

    def test_early_sender_exit_reaps_encoder(self):
        code, events, output = self.run_fixture("early")
        self.assertEqual(code, 17, output)
        self.assertEqual([row["event"] for row in events], ["bound", "feeder"])

    def test_cancel_reaps_both_children(self):
        code, events, output = self.run_fixture("cancel")
        self.assertEqual(code, 130, output)
        self.assertEqual(len(events), 2)

    def test_occupied_port_preserves_unrelated_owner(self):
        code, events, _ = self.run_fixture("natural", occupied=True)
        self.assertNotEqual(code, 0)
        self.assertEqual(events, [])


if __name__ == "__main__":
    unittest.main()
