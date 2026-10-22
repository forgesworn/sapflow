#!/usr/bin/env python3
"""Independent ffmpeg listener/decoder qualification for our publishing client.

Run after: cargo build --example rtmp-publish -p sapflow-rtmp
No downloaded media or protocol implementation source is used. All owned
fixtures/processes are removed, including failure paths. TLS is not tested.
"""
import json
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs)


class HardWindowProxy:
    """Independent fixed wire fixture: ffmpeg defaults to initial Dynamic,
    which RTMP 5.4.5 says to ignore. Inject one Hard before its controls so this
    run actually exercises the governor. No media/protocol source is reused."""
    def __init__(self, receiver_port):
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.listener.settimeout(5)
        self.port = self.listener.getsockname()[1]
        self.receiver_port = receiver_port
        self.sockets = []
        self.threads = []
        self.errors = []
        self.stopped = threading.Event()
        self.acceptor = threading.Thread(target=self.accept, name="rtmp-fixture-proxy")
        self.acceptor.start()

    def accept(self):
        try:
            client, _ = self.listener.accept()
            self.sockets.append(client)
            origin = socket.create_connection(("127.0.0.1", self.receiver_port), timeout=3)
            self.sockets.append(origin)
            for transport in self.sockets:
                transport.settimeout(2)
            self.threads = [threading.Thread(target=self.relay, args=(client, origin, False)),
                            threading.Thread(target=self.relay, args=(origin, client, True))]
            for worker in self.threads:
                worker.start()
            for worker in self.threads:
                worker.join(timeout=40)
        except Exception as error:
            if not self.stopped.is_set():
                self.errors.append(str(error))

    def relay(self, source, destination, inject):
        handshake_left = 3073 if inject else 0
        try:
            while not self.stopped.is_set():
                try:
                    data = source.recv(min(64 * 1024, handshake_left) if handshake_left else 64 * 1024)
                except socket.timeout:
                    continue
                if not data:
                    destination.shutdown(socket.SHUT_WR)
                    return
                destination.sendall(data)
                if handshake_left:
                    handshake_left -= len(data)
                    if handshake_left == 0:
                        # fmt0, csid2, timestamp0, length5, type6, stream0;
                        # independently specified big-endian window + Hard0.
                        destination.sendall(bytes([2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0])
                                            + (2_500_000).to_bytes(4, "big") + b"\x00")
        except Exception as error:
            if not self.stopped.is_set():
                self.errors.append(str(error))

    def close(self):
        self.stopped.set()
        self.listener.close()
        for transport in self.sockets:
            try:
                transport.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            transport.close()
        self.acceptor.join(timeout=6)
        for worker in self.threads:
            worker.join(timeout=3)
        assert not self.acceptor.is_alive() and all(not worker.is_alive() for worker in self.threads)


def main():
    for executable in ["ffmpeg", "ffprobe"]:
        if not shutil.which(executable):
            raise RuntimeError(f"{executable} is required")
    publisher = ROOT / "target/debug/examples/rtmp-publish"
    if not publisher.is_file():
        raise RuntimeError("Build the rtmp-publish example first")
    with tempfile.TemporaryDirectory(prefix="rtmp-interop-", dir=ROOT / "target") as temporary:
        directory = Path(temporary)
        fixture, received = directory / "input.flv", directory / "received.flv"
        # More than two default 2.5 MB windows: a short fixture would never
        # qualify cumulative acknowledgements or the sender's byte governor.
        run("ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000", "-t", "6", "-c:v", "libx264",
            "-preset", "ultrafast", "-b:v", "8M", "-minrate", "8M", "-maxrate", "8M", "-bufsize", "8M",
            "-x264-params", "nal-hrd=cbr:force-cfr=1", "-pix_fmt", "yuv420p", "-profile:v", "baseline",
            "-bf", "0", "-g", "30", "-c:a", "aac", "-ac", "1", "-f", "flv", str(fixture))
        assert fixture.stat().st_size > 5_000_000, "fixture must cross two acknowledgement windows"
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        # Do not probe-connect: a listener configured for one RTMP connection
        # must see the real client first. The driver's connect retries are bounded.
        log = open(directory / "receiver.log", "w+")
        receiver = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-listen", "1", "-i",
            f"rtmp://127.0.0.1:{port}/live/event", "-c", "copy", "-f", "flv", str(received)], stdout=log, stderr=log)
        proxy = HardWindowProxy(port)
        try:
            time.sleep(0.3)
            result = run(str(publisher), f"127.0.0.1:{proxy.port}", "live", "event", str(fixture), timeout=35)
            receiver.wait(timeout=10)
            if receiver.returncode != 0:
                log.seek(0)
                raise RuntimeError(f"ffmpeg receiver failed: {log.read()}")
            metadata = json.loads(run("ffprobe", "-v", "error", "-count_frames", "-show_streams", "-of", "json", str(received)).stdout)
            streams = {stream["codec_type"]: stream for stream in metadata["streams"]}
            assert streams["video"]["codec_name"] == "h264"
            assert int(streams["video"]["nb_read_frames"]) == 180
            assert streams["audio"]["codec_name"] == "aac"
            assert int(streams["audio"]["sample_rate"]) == 48000
            assert int(streams["audio"]["nb_read_frames"]) >= 280
            counters = dict(field.split("=", 1) for field in result.stdout.split())
            assert int(counters["peer_window_bytes"]) == 2_500_000, "fixed Hard control must install the governor"
            assert int(counters["peer_ack_sequence"]) > 2_500_000, "receiver must actually acknowledge a full window"
            run("ffmpeg", "-v", "error", "-i", str(received), "-f", "null", "-")
            print(json.dumps({"passed": True, "independent_receiver": "ffmpeg", "video_frames": 180,
                "audio_frames": int(streams["audio"]["nb_read_frames"]), "decode_passed": True,
                "fixture_bytes": fixture.stat().st_size, "peer_ack_sequence": int(counters["peer_ack_sequence"]),
                "peer_window_bytes": int(counters["peer_window_bytes"]),
                "injected_control": "SetPeerBandwidth Hard2500000 after S0S1S2",
                "tls_tested": False, "publisher": result.stdout.strip(), "owned_fixture_cleanup": True}))
        finally:
            try:
                proxy.close()
            finally:
                if receiver.poll() is None:
                    receiver.terminate()
                    try:
                        receiver.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        receiver.kill()
                        receiver.wait(timeout=3)
                log.close()


if __name__ == "__main__":
    main()
