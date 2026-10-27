"""Optional authenticated channel lifecycle notices, exclusively through Tor."""
import json
from pathlib import Path
import secrets
import subprocess
import sys
import time
from urllib.parse import urlsplit


class ChannelControl:
    def __init__(self, config_file=None, socks_port=9050):
        self.config = None
        self.registered = False
        self.attempted = False
        self.session = secrets.token_hex(16)
        self.started_at = int(time.time() * 1000)
        self.socks_port = socks_port
        if config_file is None:
            return
        path = Path(config_file)
        if path.stat().st_mode & 0o077 or path.stat().st_size > 4096:
            raise ValueError("channel control configuration must be private and bounded")
        value = json.loads(path.read_text())
        if not isinstance(value, dict) or set(value) != {"origin", "token_file"}:
            raise ValueError("invalid channel control configuration")
        origin = urlsplit(value["origin"])
        if (origin.scheme != "https" or not origin.hostname or origin.username or origin.password
                or origin.path or origin.query or origin.fragment or origin.port not in (None, 443)):
            raise ValueError("channel control requires an exact HTTPS origin")
        token_path = Path(value["token_file"])
        if not token_path.is_absolute() or token_path.stat().st_mode & 0o077 or token_path.stat().st_size > 128:
            raise ValueError("channel control token must be a private absolute file")
        token = token_path.read_text().strip()
        if len(token) != 64 or any(c not in "0123456789abcdef" for c in token):
            raise ValueError("invalid channel control token")
        self.config = (value["origin"], token)

    def notify(self, action):
        if self.config is None:
            return False
        origin, token = self.config
        body = json.dumps({"action": action, "session": self.session, "startedAt": self.started_at})
        # Credentials go over stdin, never in argv, output or ordinary DNS.
        command = ["curl", "--silent", "--fail", "--proxy", f"socks5h://127.0.0.1:{self.socks_port}",
                   "--noproxy", "", "--proto", "=https", "--proto-redir", "=https",
                   "--max-redirs", "0", "--connect-timeout", "5", "--max-time", "10",
                   "--request", "POST", "--config", "-", "--data-binary", body,
                   "--output", "/dev/null", "--write-out", "%{http_code}", origin + "/tv/broadcast/control"]
        options = f'header = "Authorization: Bearer {token}"\nheader = "Content-Type: application/json"\n'
        try:
            result = subprocess.run(command, input=options, capture_output=True, text=True, timeout=12)
            return result.returncode == 0 and result.stdout == "200"
        except (OSError, subprocess.TimeoutExpired):
            return False

    def start(self):
        if self.config is not None and not self.registered:
            if self.attempted:
                self.session = secrets.token_hex(16)
            self.started_at = int(time.time() * 1000)
            self.attempted = True
            self.registered = self.notify("start")
            if not self.registered:
                print("private-tv: channel notice unavailable; receiver reconnect grace applies", file=sys.stderr)

    def end(self):
        if self.attempted:
            return self.notify("end")
        return False
