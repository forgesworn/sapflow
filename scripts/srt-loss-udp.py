#!/usr/bin/env python3
"""Bounded local port/source checks for the generated SRT loss fixture."""

import argparse
import hashlib
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time


def port_number(value):
    port = int(value)
    if not 1 <= port <= 65535:
        raise ValueError("port must be in 1..65535")
    return port


def check_free(udp_ports, tcp_ports):
    """Refuse occupied ports without signalling their owner or enabling reuse."""
    reserved = []
    try:
        for kind, ports in [(socket.SOCK_DGRAM, udp_ports), (socket.SOCK_STREAM, tcp_ports)]:
            for port in ports:
                probe = socket.socket(socket.AF_INET, kind)
                reserved.append(probe)
                probe.bind(("127.0.0.1", port_number(port)))
    finally:
        for probe in reserved:
            probe.close()


def proc_udp_owner(pid, port, proc_root=Path("/proc")):
    """Match the child's socket inode, not just a shared network-table row."""
    directory = proc_root / str(pid)
    inodes = set()
    for descriptor in (directory / "fd").iterdir():
        try:
            target = descriptor.readlink().as_posix()
        except OSError:
            continue
        if target.startswith("socket:[") and target.endswith("]"):
            inodes.add(target[8:-1])
    for line in (directory / "net/udp").read_text().splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 10:
            address, local_port = fields[1].split(":")
            if (address in ("0100007F", "00000000") and int(local_port, 16) == port
                    and fields[9] in inodes):
                return True
    return False


def lsof_udp_owner(pid, port, executable):
    result = subprocess.run(
        [executable, "-nP", "-a", "-p", str(pid), "-iUDP:" + str(port), "-Fpn"],
        capture_output=True, text=True, timeout=2,
    )
    rows = result.stdout.splitlines()
    return (result.returncode == 0 and rows.count("p" + str(pid)) == 1
            and any(row in (f"n127.0.0.1:{port}", f"n*:{port}") for row in rows))


def owns_udp(pid, port):
    if Path("/proc").is_dir():
        try:
            return proc_udp_owner(pid, port)
        except (FileNotFoundError, ProcessLookupError):
            return False
    executable = shutil.which("lsof")
    if executable is None:
        raise ValueError("UDP child ownership needs /proc on Linux or lsof on macOS")
    return lsof_udp_owner(pid, port, executable)


def wait_udp(pid, port, timeout):
    import os

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError as error:
            raise ValueError("sender exited before binding its UDP input") from error
        if owns_udp(pid, port):
            return
        time.sleep(0.025)
    raise ValueError("sender PID did not own the expected UDP input before deadline")


def check_ts(path):
    """Reject malformed generated inputs instead of masking their sync errors."""
    digest = hashlib.sha256()
    packets = 0
    with Path(path).open("rb") as source:
        while block := source.read(188 * 4096):
            if len(block) % 188 or any(block[offset] != 0x47 for offset in range(0, len(block), 188)):
                raise ValueError("generated source is not whole sync-aligned188-byte TS packets")
            digest.update(block)
            packets += len(block) // 188
    if not packets:
        raise ValueError("generated TS source is empty")
    return packets, digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    ports = commands.add_parser("free")
    ports.add_argument("--udp", type=port_number, nargs="*", default=[])
    ports.add_argument("--tcp", type=port_number, nargs="*", default=[])
    ready = commands.add_parser("wait")
    ready.add_argument("--pid", type=int, required=True)
    ready.add_argument("--port", type=port_number, required=True)
    ready.add_argument("--timeout", type=float, default=5)
    source = commands.add_parser("source")
    source.add_argument("path", type=Path)
    extent = commands.add_parser("range")
    extent.add_argument("--base", type=port_number, required=True)
    extent.add_argument("--trials", type=int, required=True)
    args = parser.parse_args()
    if args.command == "free":
        check_free(args.udp, args.tcp)
    elif args.command == "wait":
        if args.pid <= 0 or not 0 < args.timeout <= 10:
            raise ValueError("positive child PID and readiness timeout in (0,10] required")
        wait_udp(args.pid, args.port, args.timeout)
        print(f"sender_udp_ready=1\nsender_pid={args.pid}\nsender_udp_port={args.port}")
    elif args.command == "source":
        packets, digest = check_ts(args.path)
        print(f"source_ts_packets={packets}\nsource_ts_sha256={digest}")
    else:
        if args.trials <= 0 or args.base + 5 * args.trials - 1 > 65535:
            raise ValueError("planned five-port trial blocks exceed the UDP/TCP port range")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print("SRT loss fixture: " + str(error), file=sys.stderr)
        sys.exit(1)
