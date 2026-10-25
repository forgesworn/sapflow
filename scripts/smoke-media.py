#!/usr/bin/env python3
"""Generated UDP publisher ownership and decoded smoke-fixture cadence."""

import argparse
from fractions import Fraction
import importlib.util
import json
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("srt_loss_udp", Path(__file__).with_name("srt-loss-udp.py"))
udp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(udp)


def stop(child):
    if child is None:
        return
    if child.poll() is None:
        child.terminate()
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=2)


def publish(uri, seconds, port, ffmpeg="ffmpeg", srt="srt-live-transmit", drain=2.1):
    if not 0 < seconds <= 60 or not 0 <= drain <= 3:
        raise ValueError("bounded generated publisher duration required")
    udp.check_free([port], [])
    cancelled = False

    def cancel(_signum, _frame):
        nonlocal cancelled
        cancelled = True

    handlers = {signum: signal.signal(signum, cancel) for signum in (signal.SIGINT, signal.SIGTERM)}
    sender = encoder = None
    try:
        sender = subprocess.Popen([srt, "-q", "-a:no", "-c:1316", f"udp://127.0.0.1:{port}", uri],
                                  stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 5
        while not cancelled and sender.poll() is None and time.monotonic() < deadline:
            if udp.owns_udp(sender.pid, port):
                break
            time.sleep(0.025)
        else:
            if cancelled:
                return 130
            raise ValueError("sender exited or did not own its UDP input before deadline")
        encoder = subprocess.Popen([
            ffmpeg, "-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi", "-i",
            "testsrc=size=1280x720:rate=30", "-f", "lavfi", "-i",
            "sine=frequency=440:sample_rate=48000", "-t", str(seconds), "-pix_fmt", "yuv420p",
            "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-g", "30",
            "-b:v", "2500k", "-c:a", "aac", "-b:a", "128k", "-f", "mpegts",
            f"udp://127.0.0.1:{port}?pkt_size=1316",
        ], stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + seconds + 10
        while not cancelled and time.monotonic() < deadline:
            if sender.poll() is not None:
                return sender.returncode or 1
            if encoder.poll() is not None:
                if encoder.returncode:
                    return encoder.returncode
                # Allow the negotiated two-second TSBPD delay before shutdown.
                until = time.monotonic() + drain
                while not cancelled and sender.poll() is None and time.monotonic() < until:
                    time.sleep(0.025)
                if cancelled:
                    return 130
                return (sender.returncode or 1) if sender.poll() is not None else 0
            time.sleep(0.025)
        return 130 if cancelled else 1
    finally:
        stop(encoder)
        stop(sender)
        for signum, handler in handlers.items():
            signal.signal(signum, handler)


def cadence(framehash):
    timebase = None
    pts = []
    for line in framehash.splitlines():
        if line.startswith("#tb"):
            match = re.fullmatch(r"#tb\s+0:\s+(\d+)/(\d+)", line.strip())
            if match is None:
                raise ValueError("one decoded video stream required")
            timebase = Fraction(int(match[1]), int(match[2]))
        elif line.strip() and not line.startswith("#"):
            fields = [field.strip() for field in line.split(",")]
            if len(fields) != 6 or fields[0] != "0":
                raise ValueError("invalid decoded framehash row")
            pts.append(int(fields[2]))
    if timebase is None or timebase <= 0 or len(pts) < 30:
        raise ValueError("at least30 decoded video frames with a positive timebase required")
    steps = [(right - left) * timebase for left, right in zip(pts, pts[1:])]
    if any(step <= 0 or step > Fraction(2, 30) + Fraction(1, 90000) for step in steps):
        raise ValueError("decoded video PTS must advance by at most two30fps frames")
    span = (pts[-1] - pts[0]) * timebase
    fps = Fraction(len(pts) - 1, 1) / span
    if fps < 25:
        raise ValueError("decoded mean video cadence below25fps")
    return dict(decoded_video_frames=len(pts), first_pts=pts[0], last_pts=pts[-1],
                timebase=str(timebase), maximum_step_seconds=float(max(steps)), mean_fps=float(fps))


def check_media(media):
    result = subprocess.run([
        "ffmpeg", "-hide_banner", "-v", "error", "-xerror", "-i", str(media),
        "-map", "0:v:0", "-an", "-fps_mode", "passthrough", "-f", "framehash", "-",
    ], capture_output=True, text=True, timeout=30)
    if result.returncode or result.stderr.strip():
        raise ValueError("video decoding failed or emitted error messages")
    return cadence(result.stdout)


def check_tcp_ports(ports):
    # Match a restarting server: permit TIME_WAIT, refuse a live listener.
    owned = []
    try:
        for port in ports:
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            owned.append(probe)
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            probe.bind(("127.0.0.1", port))
            probe.listen(1)
    finally:
        for probe in owned:
            probe.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    publisher = commands.add_parser("publish")
    publisher.add_argument("--uri", required=True)
    publisher.add_argument("--seconds", type=float, required=True)
    publisher.add_argument("--port", type=udp.port_number, required=True)
    media = commands.add_parser("cadence")
    media.add_argument("media", type=Path)
    ports = commands.add_parser("ports")
    ports.add_argument("--tcp", type=udp.port_number, nargs="+", required=True)
    args = parser.parse_args()
    if args.command == "publish":
        return publish(args.uri, args.seconds, args.port)
    if args.command == "ports":
        check_tcp_ports(args.tcp)
        return 0
    print(json.dumps(check_media(args.media), sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print("Smoke media: " + str(error), file=sys.stderr)
        sys.exit(1)
