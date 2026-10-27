"""Private TV routing regressions. Only generated media and local fixtures."""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("private_tv", ROOT / "scripts/tv-private-publish.py")
PRIVATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRIVATE)


def synthetic_onion():
    key = bytes(range(32))  # Synthetic bytes, not an operator's onion identity.
    checksum = hashlib.sha3_256(b".onion checksum" + key + b"\x03").digest()[:2]
    return base64.b32encode(key + checksum + b"\x03").decode().lower() + ".onion"


class AddressPolicy(unittest.TestCase):
    def test_only_valid_v3_onion_identities_are_allowed(self):
        onion = synthetic_onion()
        self.assertEqual(PRIVATE.onion_host(onion), onion)
        for value in ("relay.example", "127.0.0.1", "::1", "short.onion", "a" * 56 + ".onion",
                      onion.upper(), "-oProxyCommand=bad", onion + "/path", onion + "\n"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                PRIVATE.onion_host(value)

    def test_ports_cannot_inject_ssh_options(self):
        for value in ("0", "65536", "-1", "9050;bad", "22\n", "1.0"):
            with self.assertRaises(argparse.ArgumentTypeError):
                PRIVATE.port(value)
        self.assertEqual(PRIVATE.port("9150"), 9150)


class Supervisor(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        home = Path(self.tmp.name)
        key, hosts = home / "dedicated key", home / "pinned hosts"
        for file in (key, hosts):
            file.write_text("generated test placeholder, not a key")
            file.chmod(0o600)
        self.args = argparse.Namespace(onion_host=synthetic_onion(), ssh_user="tv_ingest",
                                       identity_file=key, known_hosts=hosts, socks_port=9050,
                                       ssh_port=22, ingest_port=1935, once=True, shuffle=False,
                                       prepared_dir=home)
        self.calls, self.children = [], []
        self.drop_tunnel = self.reject_forward = self.reject_host = self.stop_playout = False

    def execute(self):
        def popen(command, **kwargs):
            self.calls.append((command, kwargs))
            child = mock.Mock(pid=900000 + len(self.children))
            if len(self.children) == 0:
                Path(command[command.index("-S") + 1]).touch()
                child.poll.side_effect = lambda: 255 if self.reject_host or (
                    self.drop_tunnel and len(self.children) > 1) else None
            else:
                if self.stop_playout:
                    os.kill(os.getpid(), signal.SIGTERM)
                child.poll.return_value = 0
            self.children.append(child)
            return child

        def run(command, **kwargs):
            self.calls.append((command, kwargs))
            code = 1 if self.reject_forward and "forward" in command else 0
            return subprocess.CompletedProcess(command, code)

        with mock.patch.dict(os.environ, TV_PRIVATE_PUBLISH_PATH="live/event-test?user=fixture&pass=fixturepass",
                             TV_PUBLISH_URL="rtmp://forbidden.example/live/key"), \
             mock.patch.object(PRIVATE.shutil, "which", side_effect=lambda name: "/usr/bin/" + name), \
             mock.patch.object(PRIVATE.subprocess, "Popen", side_effect=popen), \
             mock.patch.object(PRIVATE.subprocess, "run", side_effect=run), \
             mock.patch.object(PRIVATE.os, "killpg") as killed:
            try:
                return PRIVATE.run(self.args)
            finally:
                self.killed = killed.call_args_list

    def test_pins_host_key_uses_socks_only_and_local_media_url(self):
        self.assertEqual(self.execute(), 0)
        master = self.calls[1][0]
        self.assertEqual(master[master.index("-F") + 1], "/dev/null")
        options = [master[i + 1] for i, v in enumerate(master) if v == "-o"]
        for value in ("StrictHostKeyChecking=yes", "IdentityAgent=none", "ForwardAgent=no",
                      "ForwardX11=no", "VerifyHostKeyDNS=no", "CanonicalizeHostname=no",
                      "IdentitiesOnly=yes", "PreferredAuthentications=publickey", "UpdateHostKeys=no"):
            self.assertIn(value, options)
        self.assertIn("ProxyCommand=exec /usr/bin/nc -X 5 -x 127.0.0.1:9050 %h %p", options)
        forward = next(c for c, _ in self.calls if "forward" in c)
        value = forward[forward.index("-L") + 1]
        self.assertRegex(value, r"^127\.0\.0\.1:[0-9]+:127\.0\.0\.1:1935$")
        command, kwargs = self.calls[-1]
        self.assertIn("--strip-metadata", command)
        self.assertIn("--once", command)
        self.assertEqual(kwargs["env"]["TV_PUBLISH_URL"],
                         f"rtmp://127.0.0.1:{value.split(':')[1]}/live/event-test?user=fixture&pass=fixturepass")
        for c, kw in self.calls[:-1]:
            self.assertNotIn("TV_PUBLISH_URL", kw["env"])
            self.assertNotIn("TV_PRIVATE_PUBLISH_PATH", kw["env"])
            self.assertNotIn("fixturepass", repr(c))
        self.assertTrue(all(kw["start_new_session"] for c, kw in self.calls if "start_new_session" in kw))
        self.assertGreaterEqual(len(self.killed), 4)

    def test_tunnel_failure_never_starts_playout(self):
        self.reject_host = True
        with self.assertRaisesRegex(ValueError, "onion SSH failed"):
            self.execute()
        self.assertEqual(len(self.children), 1)
        self.assertTrue(self.killed)

    def test_failed_forward_never_starts_playout(self):
        self.reject_forward = True
        with self.assertRaisesRegex(ValueError, "forwarding failed"):
            self.execute()
        self.assertEqual(len(self.children), 1)

    def test_disconnect_stops_publisher_instead_of_switching_routes(self):
        self.drop_tunnel = True
        with self.assertRaisesRegex(ValueError, "disconnected"):
            self.execute()
        self.assertEqual(len(self.children), 2)
        self.assertIn(mock.call(self.children[1].pid, signal.SIGTERM), self.killed)
        self.assertEqual(sum("-M" in c for c, _ in self.calls), 1)

    def test_sigterm_stops_both_owned_process_groups(self):
        self.stop_playout = True
        self.assertEqual(self.execute(), 130)
        self.assertEqual(len(self.children), 2)
        for child in self.children:
            self.assertIn(mock.call(child.pid, signal.SIGTERM), self.killed)

    def test_audio_copy_cannot_bypass_metadata_removal(self):
        result = subprocess.run(["bash", str(ROOT / "scripts/tv-playout.sh"), "--strip-metadata",
                                 "--audio", "copy", str(self.args.prepared_dir)],
                                capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"requires encoded audio", result.stderr)

    def test_private_files_and_explicit_username_required_before_network(self):
        self.args.known_hosts.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "owner-only"):
            self.execute()
        self.assertEqual(self.calls, [])
        self.args.known_hosts.chmod(0o600)
        self.args.ssh_user = "-oProxyCommand=bad"
        with self.assertRaisesRegex(ValueError, "dedicated account"):
            self.execute()
        self.assertEqual(self.calls, [])

    def test_invalid_publish_paths_are_rejected_before_network(self):
        for value in ("", "rtmp://forbidden.example/live/test", "//forbidden/live/test",
                      "live/test\nBAD", "live/test#secret", "live/../test"):
            with self.subTest(value=value), mock.patch.dict(os.environ, TV_PRIVATE_PUBLISH_PATH=value), \
                 mock.patch.object(PRIVATE.subprocess, "run") as invoked:
                with self.assertRaisesRegex(ValueError, "TV_PRIVATE_PUBLISH_PATH"):
                    PRIVATE.run(self.args)
                invoked.assert_not_called()


@unittest.skipUnless(shutil.which("nc"), "OpenBSD-compatible nc is required")
class SocksRouting(unittest.TestCase):
    def test_real_nc_passes_onion_hostname_to_socks_without_local_resolution(self):
        requests, errors = [], []
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            listener.settimeout(5)

            def receive():
                try:
                    with listener.accept()[0] as peer:
                        peer.settimeout(5)

                        def exact(n):
                            result = b""
                            while len(result) < n:
                                more = peer.recv(n - len(result))
                                if not more:
                                    raise AssertionError("truncated SOCKS request")
                                result += more
                            return result

                        version, count = exact(2)
                        methods = exact(count)
                        self.assertEqual(version, 5)
                        self.assertIn(0, methods)
                        peer.sendall(b"\x05\x00")
                        self.assertEqual(exact(4), b"\x05\x01\x00\x03")
                        host = exact(exact(1)[0]).decode("ascii")
                        target_port = int.from_bytes(exact(2), "big")
                        requests.append((host, target_port))
                        peer.sendall(b"\x05\x01\x00\x01\x00\x00\x00\x00\x00\x00")
                except BaseException as error:
                    errors.append(error)

            worker = threading.Thread(target=receive)
            worker.start()
            try:
                result = subprocess.run(["nc", "-X", "5", "-x",
                                         f"127.0.0.1:{listener.getsockname()[1]}", synthetic_onion(), "22"],
                                        stdin=subprocess.DEVNULL, capture_output=True, timeout=8)
            finally:
                worker.join(timeout=6)
            self.assertFalse(worker.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(requests, [(synthetic_onion(), 22)])
            self.assertNotEqual(result.returncode, 0)


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg is required")
class MediaRemoval(unittest.TestCase):
    def test_playout_removes_tags_chapters_sei_and_encoder_markers_and_still_decodes(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            media, tools = home / "media", home / "bin"
            media.mkdir()
            tools.mkdir()
            marker = "SYNTHETIC-PRIVATE-MARKER"
            chapters = home / "chapters.txt"
            chapters.write_text(f";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle={marker}\n")
            source, capture = media / "fixture.mp4", home / "received.flv"
            ffmpeg = shutil.which("ffmpeg")
            generated = subprocess.run(
                [ffmpeg, "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=128x96:r=25:d=1.2",
                 "-f", "lavfi", "-i", "sine=r=48000:d=1.2", "-i", str(chapters),
                 "-map", "0:v:0", "-map", "1:a:0", "-map_metadata", "2", "-map_chapters", "2",
                 "-metadata", f"title={marker}", "-metadata:s:v:0", f"handler_name={marker}",
                 "-metadata:s:a:0", f"handler_name={marker}", "-c:v", "libx264", "-preset", "ultrafast",
                 "-bsf:v", f"h264_metadata=sei_user_data=0123456789abcdef0123456789abcdef+{marker}",
                 "-c:a", "aac", str(source)], capture_output=True, timeout=60)
            self.assertEqual(generated.returncode, 0, generated.stderr.decode())
            self.assertIn(marker.encode(), source.read_bytes())
            self.assertIn(b"x264", source.read_bytes())
            # Run the actual playout pipeline; the shim replaces only its final
            # network URL with a local capture. The source and codec path are real.
            shim = tools / "ffmpeg"
            shim.write_text("#!/usr/bin/env python3\nimport os, sys\n"
                            "args = sys.argv[1:]\n"
                            "if args[-1].startswith('rtmp://'): args[-1] = os.environ['TV_TEST_CAPTURE']\n"
                            "os.execv(os.environ['TV_TEST_FFMPEG'], [os.environ['TV_TEST_FFMPEG']] + args)\n")
            shim.chmod(0o755)
            env = dict(os.environ, PATH=f"{tools}{os.pathsep}{os.environ['PATH']}",
                       TV_TEST_CAPTURE=str(capture), TV_TEST_FFMPEG=ffmpeg)
            sent = subprocess.run(["bash", str(ROOT / "scripts/tv-playout.sh"), "--strip-metadata",
                                   "--once", "--no-verify", str(media),
                                   "rtmp://127.0.0.1:1935/live/event-test"],
                                  env=env, capture_output=True, timeout=30)
            self.assertEqual(sent.returncode, 0, sent.stderr.decode())
            content = capture.read_bytes()
            for forbidden in (marker.encode(), b"x264", b"Lavc", b"Lavf"):
                self.assertNotIn(forbidden, content)
            probe = subprocess.run(["ffprobe", "-v", "error", "-show_format", "-show_streams",
                                    "-show_chapters", "-of", "json", str(capture)],
                                   capture_output=True, check=True, timeout=15)
            facts = json.loads(probe.stdout)
            self.assertEqual(facts["chapters"], [])
            self.assertEqual([s["codec_name"] for s in facts["streams"]], ["h264", "aac"])
            decoded = subprocess.run([ffmpeg, "-v", "error", "-xerror", "-i", str(capture),
                                      "-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"],
                                     capture_output=True, timeout=30)
            self.assertEqual(decoded.returncode, 0, decoded.stderr.decode())


if __name__ == "__main__":
    unittest.main()
