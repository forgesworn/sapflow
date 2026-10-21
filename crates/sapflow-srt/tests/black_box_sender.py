#!/usr/bin/env python3
"""Lab-only sender interoperability against the srt-live-transmit executable.

Run after `cargo build -p sapflow-srt --example send` from the workspace root:
  python3 crates/sapflow-srt/tests/black_box_sender.py

No reference implementation source is inspected. Generates MPEG-TS with ffmpeg,
checks the stream ID on the wire, and compares received bytes and SHA-256.
A task-owned UDP proxy adds seeded data loss, reordering and ACK loss. This is
loopback qualification, not encryption, public-network or phone acceptance.
All processes, sockets and scratch are cleaned on success and failure.
"""
import hashlib
import heapq
import json
import pathlib
import random
import socket
import struct
import subprocess
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
SENDER = ROOT / "target/debug/examples/send"
STREAM_ID = b"publish:lab"


def ephemeral():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    return sock


def handshake_stream_id(data):
    if len(data) < 64 or data[:4] != b"\x80\x00\x00\x00":
        return None
    offset = 64
    while offset + 4 <= len(data):
        kind, words = struct.unpack_from("!HH", data, offset)
        offset += 4
        body = data[offset:offset + words * 4]
        offset += words * 4
        if kind == 5:
            return b"".join(body[i:i + 4][::-1] for i in range(0, len(body), 4)).rstrip(b"\0")
    return None


def run_case(tmp, source, seed, impaired):
    rng = random.Random(seed)
    reserve = ephemeral()
    receiver_addr = reserve.getsockname()
    reserve.close()
    proxy = ephemeral()
    proxy.settimeout(0.001)
    stop = threading.Event()
    counts = {"dropped_data": 0, "dropped_acks": 0, "delayed_data": 0, "stream_id_seen": False}
    relay_errors = []

    def relay():
        caller = None
        pending = []
        serial = 0
        try:
            while not stop.is_set():
                now = time.monotonic()
                while pending and pending[0][0] <= now:
                    _, _, packet, target = heapq.heappop(pending)
                    proxy.sendto(packet, target)
                try:
                    packet, address = proxy.recvfrom(2048)
                except socket.timeout:
                    continue
                if address == receiver_addr:
                    if caller is None:
                        continue
                    if impaired and packet[:2] == b"\x80\x02" and rng.random() < .10:
                        counts["dropped_acks"] += 1
                        continue
                    proxy.sendto(packet, caller)
                else:
                    caller = address
                    sid = handshake_stream_id(packet)
                    if sid is not None:
                        if sid != STREAM_ID:
                            raise AssertionError("unexpected stream ID")
                        counts["stream_id_seen"] = True
                    is_data = len(packet) >= 16 and not packet[0] & 0x80
                    if impaired and is_data:
                        if rng.random() < .10:
                            counts["dropped_data"] += 1
                            continue
                        delay = rng.uniform(0, .020)
                        counts["delayed_data"] += 1
                        serial += 1
                        heapq.heappush(pending, (now + delay, serial, packet, receiver_addr))
                    else:
                        proxy.sendto(packet, receiver_addr)
        except Exception as error:
            relay_errors.append(repr(error))
            stop.set()

    thread = threading.Thread(target=relay, name="task-owned-srt-interop-proxy")
    thread.start()
    output_path = tmp / f"received-{seed}-{impaired}.ts"
    log_path = tmp / f"receiver-{seed}-{impaired}.log"
    latency = 2000 if impaired else 120
    run = None
    try:
        with output_path.open("wb") as output, log_path.open("wb") as log:
            receiver = subprocess.Popen([
                "srt-live-transmit", "-a:no", "-t:12",
                f"srt://127.0.0.1:{receiver_addr[1]}?mode=listener&latency={latency}",
                "file://con",
            ], stdout=output, stderr=log)
            try:
                time.sleep(.15)
                run = subprocess.run([
                    str(SENDER), f"127.0.0.1:{proxy.getsockname()[1]}", STREAM_ID.decode(),
                    str(source), "1250000", str(latency),
                ], capture_output=True, text=True, timeout=15)
                try:
                    receiver.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    receiver.terminate()
                    receiver.wait(timeout=2)
            finally:
                if receiver.poll() is None:
                    receiver.kill()
                    receiver.wait(timeout=2)
    finally:
        stop.set()
        thread.join(timeout=2)
        proxy.close()
    original = source.read_bytes()
    received = output_path.read_bytes()
    receipt = {
        "profile": "10pct-data-loss-10pct-ack-loss-0to20ms-reorder" if impaired else "no-loss",
        "seed": seed, "latency_ms": latency, "source_bytes": len(original),
        "received_bytes": len(received), "equal": original == received,
        "sha256": hashlib.sha256(received).hexdigest(), "sender_exit": run.returncode,
        "sender_stats": run.stdout.strip(), **counts,
    }
    print(json.dumps(receipt), flush=True)
    if run.returncode or original != received or not counts["stream_id_seen"] or relay_errors:
        print(run.stderr, log_path.read_text(), relay_errors, flush=True)
        raise AssertionError("black-box sender interoperability failed")


def main():
    subprocess.run(["srt-live-transmit", "-version"], check=True)
    with tempfile.TemporaryDirectory(prefix="srt-sender-interop-", dir=ROOT / "target") as scratch:
        tmp = pathlib.Path(scratch)
        source = tmp / "input.ts"
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "testsrc2=size=160x90:rate=15", "-t", "2", "-c:v", "libx264",
            "-preset", "ultrafast", "-g", "15", "-f", "mpegts", str(source),
        ], check=True)
        run_case(tmp, source, 0, False)
        for seed in range(6):
            run_case(tmp, source, seed, True)


if __name__ == "__main__":
    main()
