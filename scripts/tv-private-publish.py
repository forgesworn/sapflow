#!/usr/bin/env python3
"""Publish prepared TV through Tor and onion-only SSH, with no direct fallback.

Requires a running, trusted local Tor SOCKS proxy and a configured onion SSH
receiver. Read docs/private-tv-publishing.md before connecting a real relay.
TV_PRIVATE_PUBLISH_PATH contains live/EVENT?user=USER&pass=PASSWORD; URL-encode
credentials. No media, identity keys or known-hosts files belong in Git.
"""

import argparse
import base64
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent


def onion_host(value):
    """Reject public addresses and malformed v3 onion identities before networking."""
    if not re.fullmatch(r"[a-z2-7]{56}\.onion", value):
        raise argparse.ArgumentTypeError("a lower-case v3 onion hostname is required")
    raw = base64.b32decode(value[:-6].upper())
    checksum = hashlib.sha3_256(b".onion checksum" + raw[:32] + b"\x03").digest()[:2]
    if raw[-1:] != b"\x03" or raw[32:34] != checksum:
        raise argparse.ArgumentTypeError("invalid v3 onion checksum or version")
    return value


def port(value):
    if not re.fullmatch(r"[0-9]{1,5}", value) or not 1 <= int(value) <= 65535:
        raise argparse.ArgumentTypeError("port must be between 1 and 65535")
    return int(value)


def private_file(value):
    path = Path(value).expanduser().resolve(strict=True)
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("identity and known-hosts files must be owner-only regular files")
    # OpenSSH expands percent tokens and tilde in some path options.
    if any(c in str(path) for c in '%"\\\n\r'):
        raise ValueError("SSH file paths must not contain percent, quotes, backslashes or newlines")
    return path


def ssh_command(args, control, ssh, nc):
    # No user/system SSH configuration, agent, environment forwarding, hostname
    # canonicalisation, DNS host-key lookup, shell login or reused SSH connection.
    # Netcat sends the onion name to SOCKS5; it never resolves it locally.
    # OpenSSH prefixes the proxy command with exec itself. A second exec works
    # in some shells but fails in dash, before any SOCKS connection is made.
    proxy = f"{shlex.quote(nc)} -X 5 -x 127.0.0.1:{args.socks_port} %h %p"
    options = {
        "ProxyCommand": proxy,
        "BatchMode": "yes",
        "StrictHostKeyChecking": "yes",
        "UserKnownHostsFile": f'"{args.known_hosts}"',
        "GlobalKnownHostsFile": "/dev/null",
        "UpdateHostKeys": "no",
        "VerifyHostKeyDNS": "no",
        "CheckHostIP": "no",
        "CanonicalizeHostname": "no",
        "IdentitiesOnly": "yes",
        "IdentityAgent": "none",
        "AddKeysToAgent": "no",
        "PreferredAuthentications": "publickey",
        "ForwardAgent": "no",
        "ForwardX11": "no",
        "PermitLocalCommand": "no",
        "RequestTTY": "no",
        "ExitOnForwardFailure": "yes",
        "ControlPersist": "no",
        "ServerAliveInterval": "15",
        "ServerAliveCountMax": "3",
        "ConnectTimeout": "60",
    }
    command = [ssh, "-F", "/dev/null", "-T", "-S", str(control),
               "-i", str(args.identity_file), "-p", str(args.ssh_port), "-l", args.ssh_user]
    for name, value in options.items():
        command.extend(["-o", f"{name}={value}"])
    return command


def stop_child(child):
    """Stop our private process group, including encoders and the SOCKS helper."""
    if child is None:
        return
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    child.wait()


def run(args):
    publish_path = os.environ.get("TV_PRIVATE_PUBLISH_PATH", "")
    if not re.fullmatch(r"live/[A-Za-z0-9_-]{1,128}(?:\?[^\x00-\x20\x7f#]*)?", publish_path):
        raise ValueError("set TV_PRIVATE_PUBLISH_PATH to live/EVENT with URL-encoded credentials")
    if not re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", args.ssh_user):
        raise ValueError("SSH user must be an explicit dedicated account name")
    args.identity_file = private_file(args.identity_file)
    args.known_hosts = private_file(args.known_hosts)
    media = args.prepared_dir.resolve(strict=True)
    if not media.is_dir():
        raise ValueError("prepared media directory is required")
    ssh, nc = shutil.which("ssh"), shutil.which("nc")
    if not ssh or not nc or not shutil.which("ffmpeg") or not shutil.which("ffprobe"):
        raise ValueError("SSH, OpenBSD-compatible nc, FFmpeg and FFprobe are required")

    # Do not pass publish credentials to SSH or inherit an ordinary publish URL.
    clean_env = {k: v for k, v in os.environ.items()
                 if k not in ("TV_PRIVATE_PUBLISH_PATH", "TV_PUBLISH_URL")}
    stopped = False

    def on_signal(_signum, _frame):
        nonlocal stopped
        stopped = True

    old_handlers = {s: signal.signal(s, on_signal) for s in (signal.SIGINT, signal.SIGTERM)}
    verifier = master = playout = None
    try:
        # Verify locally before starting any network connection. Keep filenames,
        # tags and local directory names out of the supervisor's output.
        verifier = subprocess.Popen(["bash", str(HERE / "tv-prepare.sh"), "--verify", str(media)],
                                    env=clean_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                    start_new_session=True)
        verify_deadline = time.monotonic() + 300
        while verifier.poll() is None:
            if stopped:
                return 130
            if time.monotonic() >= verify_deadline:
                raise ValueError("local prepared media verification timed out")
            time.sleep(0.1)
        verify_status = verifier.returncode
        # A completed verifier has no surviving asynchronous work. Do not keep
        # its process-group ID for a broadcast that might last many hours.
        verifier = None
        if verify_status:
            raise ValueError("prepared media verification failed; run tv-prepare.sh --verify locally")
        if stopped:
            return 130
        # /tmp keeps the SSH control socket below Unix pathname length limits on macOS.
        with tempfile.TemporaryDirectory(prefix="sapflow-private-", dir="/tmp") as work:
            control = Path(work) / "ssh"
            command = ssh_command(args, control, ssh, nc)
            with open(os.devnull, "wb") as ssh_errors:
                master = subprocess.Popen(command + ["-M", "-N", args.onion_host],
                                          env=clean_env, stdin=subprocess.DEVNULL,
                                          stdout=subprocess.DEVNULL, stderr=ssh_errors,
                                          start_new_session=True)
                deadline = time.monotonic() + 90
                while True:
                    if stopped:
                        return 130
                    if master.poll() is not None or time.monotonic() >= deadline:
                        raise ValueError("onion SSH failed; check Tor, receiver access and the pinned host key")
                    if control.exists():
                        check = subprocess.run(command + ["-O", "check", args.onion_host],
                                               env=clean_env, stdout=subprocess.DEVNULL,
                                               stderr=subprocess.DEVNULL, timeout=3)
                        if check.returncode == 0:
                            break
                    time.sleep(0.1)

                # Reserve a candidate port, then ask the authenticated master to
                # bind it. A competing bind is a fatal error, never a fallback.
                with socket.socket() as candidate:
                    candidate.bind(("127.0.0.1", 0))
                    local_port = candidate.getsockname()[1]
                forward = f"127.0.0.1:{local_port}:127.0.0.1:{args.ingest_port}"
                bound = subprocess.run(command + ["-O", "forward", "-L", forward, args.onion_host],
                                       env=clean_env, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL, timeout=10)
                if bound.returncode or master.poll() is not None:
                    raise ValueError("private forwarding failed; no publisher was started")
                if stopped:
                    return 130
                playout_env = dict(clean_env, TV_PUBLISH_URL=f"rtmp://127.0.0.1:{local_port}/{publish_path}")
                play_command = ["bash", str(HERE / "tv-playout.sh"), "--strip-metadata", "--no-verify"]
                if args.once:
                    play_command.append("--once")
                if args.shuffle:
                    play_command.append("--shuffle")
                play_command.append(str(media))
                print("private-tv: publishing through onion SSH; no direct fallback", file=sys.stderr)
                playout = subprocess.Popen(play_command, env=playout_env, start_new_session=True)
                while not stopped:
                    if master.poll() is not None:
                        raise ValueError("onion SSH disconnected; stopping publisher")
                    result = playout.poll()
                    if result is not None:
                        return 0 if result == 0 else 1
                    time.sleep(0.1)
                return 130
    finally:
        stop_child(playout)
        stop_child(master)
        stop_child(verifier)
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--onion-host", type=onion_host, required=True)
    parser.add_argument("--ssh-user", required=True)
    parser.add_argument("--identity-file", type=Path, required=True)
    parser.add_argument("--known-hosts", type=Path, required=True)
    parser.add_argument("--socks-port", type=port, default=9050)
    parser.add_argument("--ssh-port", type=port, default=22)
    parser.add_argument("--ingest-port", type=port, default=1935)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--shuffle", action="store_true")
    parser.add_argument("prepared_dir", type=Path)
    args = parser.parse_args()
    try:
        return run(args)
    except ValueError as error:
        # Only our fixed validation messages reach here, never filesystem errors.
        print(f"private-tv: {error}", file=sys.stderr)
        return 1
    except (OSError, subprocess.TimeoutExpired):
        # Never echo untrusted filenames, command arguments or publish credentials.
        print("private-tv: stopped; verify local media, private files, Tor and pinned onion SSH access",
              file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
