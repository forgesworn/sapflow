#!/usr/bin/env python3
"""Serve trusted local TV assets and fetch only the TV stream through Tor.

Open the printed loopback URL in a browser. No receiver assets or JavaScript
are executed: the installed local player is used. There is no direct fallback.
"""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import re
import signal
import threading
from urllib.parse import urlsplit

from tv_preview_policy import media_target
from tv_preview_transport import TorTransport

HERE = Path(__file__).resolve().parent
ASSETS = HERE.parent / "web/tv"
STATIC = {"/tv/": ("index.html", "text/html; charset=utf-8"),
          "/tv/licences.html": ("licences.html", "text/html; charset=utf-8"),
          "/tv/LICENSE.txt": ("LICENSE.txt", "text/plain; charset=utf-8"),
          "/tv/now-streaming.png": ("now-streaming.png", "image/png"),
          "/tv/off-air.png": ("off-air.png", "image/png"),
          "/tv/connection.mjs": ("connection.mjs", "text/javascript; charset=utf-8"),
          "/tv/playlist.mjs": ("playlist.mjs", "text/javascript; charset=utf-8"),
          "/tv/cmaf-player.mjs": ("cmaf-player.mjs", "text/javascript; charset=utf-8"),
          "/tv/tone.mjs": ("tone.mjs", "text/javascript; charset=utf-8"),
          "/tv/player.css": ("player.css", "text/css; charset=utf-8"),
          "/tv/player.mjs": ("player.mjs", "text/javascript; charset=utf-8"),
          "/tv/vendor/THIRD-PARTY-LICENCES.txt": ("vendor/THIRD-PARTY-LICENCES.txt", "text/plain; charset=utf-8")}
CSP = ("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; "
       "media-src 'self' blob:; img-src 'self'; base-uri 'none'; object-src 'none'; "
       "form-action 'none'; frame-ancestors 'none'")


def receiver_origin(value):
    if not re.fullmatch(r"https://[a-zA-Z0-9.-]+(?::[0-9]{1,5})?/?", value):
        raise argparse.ArgumentTypeError("use a receiver HTTPS origin without credentials or a path")
    p = urlsplit(value)
    if (p.scheme != "https" or not p.hostname or p.username or p.password or p.query
            or p.fragment or p.path not in ("", "/")
            or not re.fullmatch(r"[a-zA-Z0-9.-]+", p.hostname)):
        raise argparse.ArgumentTypeError("use a receiver HTTPS origin without credentials or a path")
    try:
        if p.port is not None and not 1 <= p.port <= 65535:
            raise ValueError()
    except ValueError as error:
        raise argparse.ArgumentTypeError("invalid receiver port") from error
    return value.rstrip("/")


class PreviewHandler(BaseHTTPRequestHandler):
    # Browsers may pre-open idle sockets. Bound reads/writes so shutdown can
    # join request threads even when no HTTP request has arrived.
    timeout = 5
    server_version = "SapflowPreview"
    sys_version = ""

    def log_message(self, *_args):
        pass

    def handle(self):
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def reply(self, code, body, mime="text/plain; charset=utf-8"):
        self.send_response(code)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Security-Policy", CSP)
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        expected = f"http://127.0.0.1:{self.server.server_port}"
        if (self.headers.get("Host") != expected[7:]
                or self.headers.get("Origin", expected) != expected
                or self.headers.get("Sec-Fetch-Site") == "cross-site"):
            self.reply(403, b"Forbidden\n")
            return
        if self.path in STATIC:
            name, mime = STATIC[self.path]
            self.reply(200, (self.server.assets / name).read_bytes(), mime)
            return
        try:
            if not media_target(self.path):
                self.reply(404, b"Not found\n")
                return
        except ValueError:
            self.reply(400, b"Invalid request\n")
            return
        if not self.server.slots.acquire(blocking=False):
            self.reply(503, b"Preview busy\n")
            return
        try:
            code, body, mime = self.server.transport.fetch(self.path)
            self.reply(code, body, mime)
        except (ValueError, OSError):
            self.reply(503, b"Tor preview unavailable\n")
        finally:
            self.server.slots.release()


class PreviewServer(ThreadingHTTPServer):
    daemon_threads = False
    block_on_close = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receiver", type=receiver_origin, required=True)
    parser.add_argument("--socks-port", type=int, default=9050)
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    if not 1 <= args.socks_port <= 65535 or not 0 <= args.port <= 65535:
        parser.error("ports must be in range")
    if not ASSETS.joinpath("index.html").is_file():
        parser.error("trusted local web/tv assets are required")
    try:
        transport = TorTransport(args.receiver, args.socks_port)
    except RuntimeError as error:
        parser.error(str(error))
    with PreviewServer(("127.0.0.1", args.port), PreviewHandler) as server:
        server.assets, server.transport = ASSETS, transport
        server.slots = threading.BoundedSemaphore(4)
        def stop(_signum, _frame):
            transport.stop()
            threading.Thread(target=server.shutdown, daemon=True).start()
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        print(f"Preview: http://127.0.0.1:{server.server_port}/tv/", flush=True)
        print("Only TV media uses Tor. Other browser applications are unaffected.", flush=True)
        try:
            server.serve_forever(poll_interval=0.1)
        finally:
            transport.stop()
    transport.close()
    print(f"Preview stopped: {transport.requests} media requests, {transport.connections} connections.", flush=True)


if __name__ == "__main__":
    main()
