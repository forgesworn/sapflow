"""Real SSH/RTMP over a loopback SOCKS stand-in. This is not Tor qualification.

Run with OpenSSH server, OpenBSD nc, FFmpeg/libx264/AAC and FFprobe installed.
Every key and media byte is generated for the fixture and deleted afterwards.
"""

import getpass
import importlib.util
import os
from pathlib import Path
import selectors
import shutil
import socket
import socketserver
import subprocess
import tempfile
import threading
import time
import unittest

SPEC = importlib.util.spec_from_file_location("private_tv_tests", Path(__file__).with_name("test_private_tv_publish.py"))
HELPERS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPERS)
ROOT = Path(__file__).resolve().parents[1]
SSHD = shutil.which("sshd") or ("/usr/sbin/sshd" if Path("/usr/sbin/sshd").exists() else None)


def free_port():
    with socket.socket() as candidate:
        candidate.bind(("127.0.0.1", 0))
        return candidate.getsockname()[1]


def await_listener(process, port):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError("local fixture exited before listening; inspect its fixture log")
        with socket.socket() as probe:
            try:
                probe.bind(("127.0.0.1", port))
            except OSError:
                return
        time.sleep(0.05)
    raise AssertionError("local fixture did not listen")


class SocksFixture(socketserver.ThreadingTCPServer):
    daemon_threads = True


def pump(downstream, upstream, server, capture=False):
    with selectors.DefaultSelector() as selector:
        selector.register(downstream, selectors.EVENT_READ, upstream)
        selector.register(upstream, selectors.EVENT_READ, downstream)
        while not server.stopping.is_set():
            for key, _ in selector.select(timeout=0.1):
                data = key.fileobj.recv(65536)
                if not data:
                    return
                if capture and key.fileobj is downstream:
                    server.received.extend(data)
                    if len(server.received) > 2 * 1024 * 1024:
                        raise AssertionError("synthetic wire capture exceeded its fixture budget")
                key.data.sendall(data)


class RecordingHandler(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            with socket.create_connection(("127.0.0.1", self.server.ingest_port), timeout=5) as upstream:
                upstream.settimeout(None)
                pump(self.request, upstream, self.server, capture=True)
        except BaseException as error:
            self.server.errors.append(str(error))


class SocksHandler(socketserver.BaseRequestHandler):
    def handle(self):
        def exact(n):
            result = b""
            while len(result) < n:
                more = self.request.recv(n - len(result))
                if not more:
                    raise ConnectionError("truncated fixture request")
                result += more
            return result

        self.request.settimeout(5)
        try:
            version, count = exact(2)
            methods = exact(count)
            if version != 5 or 0 not in methods:
                raise AssertionError("unexpected fixture authentication")
            self.request.sendall(b"\x05\x00")
            if exact(4) != b"\x05\x01\x00\x03":
                raise AssertionError("destination was not a SOCKS hostname")
            host = exact(exact(1)[0]).decode("ascii")
            port = int.from_bytes(exact(2), "big")
            if host != self.server.onion or port != 22:
                raise AssertionError("unexpected destination; fixture refuses all other forwarding")
            self.server.requests.append((host, port))
            with socket.create_connection(("127.0.0.1", self.server.ssh_port), timeout=5) as upstream:
                self.request.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
                self.request.settimeout(None)
                upstream.settimeout(None)
                pump(self.request, upstream, self.server)
        except BaseException as error:
            self.server.errors.append(str(error))


@unittest.skipUnless(SSHD and all(shutil.which(x) for x in ("ssh", "ssh-keygen", "nc", "ffmpeg", "ffprobe")),
                     "OpenSSH server, OpenBSD nc and FFmpeg are required")
class PrivateRoute(unittest.TestCase):
    def test_actual_ssh_pinning_forwarding_and_generated_media(self):
        with tempfile.TemporaryDirectory(prefix="sapflow-private-e2e-") as tmp:
            home = Path(tmp)
            media = home / "media"
            media.mkdir()
            for name in ("host", "client", "wrong-host"):
                subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "",
                                "-f", str(home / name)], check=True, timeout=15)
            (home / "authorised").write_text((home / "client.pub").read_text())
            onion = HELPERS.synthetic_onion()
            hosts = home / "known_hosts"
            hosts.write_text(onion + " " + (home / "host.pub").read_text())
            hosts.chmod(0o600)
            ssh_port, ingest_port = free_port(), free_port()
            recording = SocksFixture(("127.0.0.1", 0), RecordingHandler)
            recording.ingest_port = ingest_port
            recording.received, recording.errors = bytearray(), []
            recording.stopping = threading.Event()
            recording_worker = threading.Thread(target=recording.serve_forever, daemon=True)
            recording_worker.start()
            self.addCleanup(recording_worker.join, 3)
            self.addCleanup(recording.server_close)
            self.addCleanup(recording.shutdown)
            self.addCleanup(recording.stopping.set)
            forward_port = recording.server_address[1]
            config = home / "sshd_config"
            config.write_text(
                f"Port {ssh_port}\nListenAddress 127.0.0.1\nHostKey {home}/host\n"
                f"PidFile {home}/sshd.pid\nAuthorizedKeysFile {home}/authorised\n"
                "UsePAM no\nUseDNS no\nStrictModes no\nPasswordAuthentication no\n"
                "KbdInteractiveAuthentication no\nAuthenticationMethods publickey\n"
                "AllowAgentForwarding no\nX11Forwarding no\nPermitTTY no\n"
                f"AllowTcpForwarding local\nPermitOpen 127.0.0.1:{forward_port}\nMaxSessions 0\n")
            source = media / "generated.mp4"
            subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=128x96:r=25:d=2.4",
                            "-f", "lavfi", "-i", "sine=r=48000:d=2.4", "-c:v", "libx264",
                            "-preset", "ultrafast", "-g", "25", "-c:a", "aac", str(source)],
                           check=True, timeout=60)
            socks = SocksFixture(("127.0.0.1", 0), SocksHandler)
            socks.onion, socks.ssh_port = onion, ssh_port
            socks.requests, socks.errors = [], []
            socks.stopping = threading.Event()
            worker = threading.Thread(target=socks.serve_forever, daemon=True)
            worker.start()
            sshd = receiver = None
            try:
                with (home / "sshd.log").open("wb") as log:
                    sshd = subprocess.Popen([SSHD, "-D", "-e", "-f", str(config)],
                                            stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                            start_new_session=True)
                    await_listener(sshd, ssh_port)
                    command = ["python3", "-B", str(ROOT / "scripts/tv-private-publish.py"),
                               "--onion-host", onion, "--ssh-user", getpass.getuser(),
                               "--identity-file", str(home / "client"), "--known-hosts", str(hosts),
                               "--socks-port", str(socks.server_address[1]), "--ingest-port", str(forward_port),
                               "--once", str(media)]
                    env = dict(os.environ, TV_PRIVATE_PUBLISH_PATH="live/event-fixture",
                               TV_PUBLISH_URL="rtmp://forbidden.example/live/key")
                    # A mismatched pin must fail before an RTMP listener exists.
                    hosts.write_text(onion + " " + (home / "wrong-host.pub").read_text())
                    refused = subprocess.run(command, env=env, capture_output=True, timeout=20)
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertNotIn(b"publishing through", refused.stderr)
                    hosts.write_text(onion + " " + (home / "host.pub").read_text())
                    capture = home / "received.flv"
                    with (home / "rtmp.log").open("wb") as rtmp_log:
                        receiver = subprocess.Popen(
                            ["ffmpeg", "-v", "error", "-listen", "1", "-i",
                             f"rtmp://127.0.0.1:{ingest_port}/live/event-fixture", "-c", "copy", str(capture)],
                            stdin=subprocess.DEVNULL, stdout=rtmp_log, stderr=rtmp_log,
                            start_new_session=True)
                        await_listener(receiver, ingest_port)
                        sent = subprocess.run(command, env=env, capture_output=True, timeout=30)
                        self.assertEqual(sent.returncode, 0, sent.stderr.decode() + log.name)
                        receiver.wait(timeout=10)
                    self.assertGreater(capture.stat().st_size, 1000)
                    decoded = subprocess.run(["ffmpeg", "-v", "error", "-xerror", "-i", str(capture),
                                              "-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"],
                                             capture_output=True, timeout=15)
                    self.assertEqual(decoded.returncode, 0, decoded.stderr.decode())
                    self.assertEqual(socks.requests, [(onion, 22), (onion, 22)])
                    self.assertEqual(socks.errors, [])
                    self.assertEqual(recording.errors, [])
                    self.assertIn(b"FMLE/3.0", recording.received)
                    for marker in (b"Lavf", b"Lavc", b"x264"):
                        self.assertNotIn(marker, recording.received)
            finally:
                HELPERS.PRIVATE.stop_child(receiver)
                HELPERS.PRIVATE.stop_child(sshd)
                socks.stopping.set()
                socks.shutdown()
                socks.server_close()
                worker.join(timeout=3)


if __name__ == "__main__":
    unittest.main()
