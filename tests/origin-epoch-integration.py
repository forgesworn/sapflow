#!/usr/bin/env python3
"""Bounded synthetic RTMP reconnect qualification. No origin/device management.

All destinations and recording roots are explicit runtime arguments. Credentials
come only from named environment variables and never enter retained receipts.
No network action occurs on import or during the deterministic unit tests.
"""
import argparse
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MAX_PLAYLIST = 128 * 1024
MAX_RESOURCE = 4 * 1024 * 1024
MAX_WIRE = 64 * 1024 * 1024
MAX_OUTPUT = 64 * 1024
RUN_SECONDS = 300
DECODE_CLEANUP_RESERVE_SECONDS = 60
URI = re.compile(r"[0-9a-f]{1,16}_(?:init[0-9]{0,19}|seg[0-9]{1,19}|part[0-9]{1,19})\.mp4\Z")
EVENT = re.compile(r"event-epoch-[a-z0-9][a-z0-9-]{0,48}\Z")
SHA = re.compile(r"[0-9a-f]{64}\Z")


class Failure(Exception):
    pass


def run_deadline_expired(_signum, _frame):
    raise Failure("run exceeded its absolute 300-second network/process deadline")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path, maximum=256 * 1024 * 1024):
    size = path.stat().st_size
    if not 0 < size <= maximum:
        raise Failure("file size exceeds its bound")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        consumed = 0
        while block := source.read(min(65536, maximum-consumed+1)):
            consumed += len(block)
            if consumed > maximum:
                raise Failure("growing file exceeds hash byte bound")
            digest.update(block)
    return digest.hexdigest()


def safe_uri(value):
    if len(value) > 96 or (value != "gap.mp4" and not URI.fullmatch(value)):
        raise Failure("unexpected or external media URI")
    return value


def attributes(value):
    # Our origin's subset permits no commas/escapes inside quoted attributes.
    result = {}
    for field in value.split(","):
        key, separator, item = field.partition("=")
        if not separator or key in result or not item:
            raise Failure("invalid attributes")
        if item.startswith('"'):
            if not item.endswith('"') or '"' in item[1:-1]:
                raise Failure("invalid quoted attribute")
            item = item[1:-1]
        result[key] = item
    return result


def integer(value):
    if not re.fullmatch(r"[0-9]{1,19}", value):
        raise Failure("invalid bounded integer")
    return int(value)


def duration(value):
    try:
        number = Decimal(value)
        if not number.is_finite() or not 0 < number <= 60:
            raise Failure("duration outside bounds")
        return number
    except InvalidOperation as error:
        raise Failure("invalid duration") from error


def parse_playlist(data):
    try:
        return _parse_playlist(data)
    except (ValueError, KeyError, UnicodeError, InvalidOperation) as error:
        raise Failure("invalid bounded playlist syntax") from error


def _parse_playlist(data):
    if len(data) > MAX_PLAYLIST:
        raise Failure("playlist exceeds byte bound")
    text = data.decode("utf-8", errors="strict")
    lines = text.splitlines()
    if not lines or lines[0] != "#EXTM3U" or len(lines) > 4096:
        raise Failure("invalid playlist header/line bound")
    headers = {}
    for line in lines:
        for tag in ("TARGETDURATION", "MEDIA-SEQUENCE", "DISCONTINUITY-SEQUENCE"):
            prefix = f"#EXT-X-{tag}:"
            if line.startswith(prefix):
                if tag in headers:
                    raise Failure("duplicate playlist header")
                headers[tag] = integer(line[len(prefix):])
        if line.startswith("#EXT-X-PART-INF:"):
            if "PART-TARGET" in headers:
                raise Failure("duplicate part target")
            headers["PART-TARGET"] = duration(attributes(line.split(":", 1)[1])["PART-TARGET"])
    if not {"TARGETDURATION", "MEDIA-SEQUENCE", "DISCONTINUITY-SEQUENCE", "PART-TARGET"} <= headers.keys():
        raise Failure("missing playlist timing/sequence headers")
    if not 1 <= headers["TARGETDURATION"] <= 60:
        raise Failure("target duration outside bounds")
    msn, discontinuity = headers["MEDIA-SEQUENCE"], headers["DISCONTINUITY-SEQUENCE"]
    mapping, pending, gap = None, None, False
    segments, parts, run = [], [], []
    for line in lines[1:]:
        if line.startswith("#EXT-X-MAP:"):
            if run:
                raise Failure("MAP inserted among segment parts")
            mapping = safe_uri(attributes(line.split(":", 1)[1])["URI"])
            if "_init" not in mapping:
                raise Failure("MAP does not name initialization")
        elif line == "#EXT-X-DISCONTINUITY":
            if run:
                raise Failure("discontinuity inserted among segment parts")
            discontinuity += 1
        elif line == "#EXT-X-GAP":
            gap = True
        elif line.startswith("#EXT-X-PART:"):
            attrs = attributes(line.split(":", 1)[1])
            if mapping is None:
                raise Failure("part without initialization")
            if "_part" not in safe_uri(attrs["URI"]):
                raise Failure("part tag does not name a part")
            part = {"uri": safe_uri(attrs["URI"]), "map": mapping, "msn": msn,
                    "dseq": discontinuity, "duration": duration(attrs["DURATION"])}
            if part["duration"] > headers["PART-TARGET"]:
                raise Failure("part exceeds fixed target")
            parts.append(part)
            run.append(part)
        elif line.startswith("#EXTINF:"):
            if pending is not None:
                raise Failure("missing segment URI")
            pending = duration(line[8:].rstrip(","))
            if int(pending + Decimal("0.5")) > headers["TARGETDURATION"]:
                raise Failure("segment exceeds fixed target")
            for part in run[:-1]:
                if part["duration"] < headers["PART-TARGET"] * Decimal("0.85"):
                    raise Failure("short non-final part")
        elif line and not line.startswith("#"):
            if pending is None or mapping is None:
                raise Failure("segment without duration/initialization")
            uri = safe_uri(line)
            if not gap and "_seg" not in uri:
                raise Failure("segment tag does not name a segment")
            if (uri == "gap.mp4") != gap:
                raise Failure("gap identity mismatch")
            segments.append({"msn": msn, "dseq": discontinuity, "uri": uri, "map": mapping,
                             "duration": pending, "gap": gap})
            msn += 1
            pending, gap, run = None, False, []
        elif line.startswith("#EXT-X-PRELOAD-HINT:"):
            attrs = attributes(line.split(":", 1)[1])
            safe_uri(attrs["URI"])
            if attrs.get("TYPE") not in ("MAP", "PART"):
                raise Failure("unsupported hint type")
    if pending is not None or len(parts) + len(segments) > 512:
        raise Failure("incomplete/excessive playlist")
    for part in run[:-1]:
        if part["duration"] < headers["PART-TARGET"] * Decimal("0.85"):
            raise Failure("short non-final open part")
    return {"headers": headers, "segments": segments, "parts": parts}


class History:
    def __init__(self):
        self.targets, self.first, self.identities = None, None, {}
        self.maps, self.part_identities, self.latest_dseq = [], {}, None

    def accept(self, playlist):
        headers = playlist["headers"]
        targets = (headers["TARGETDURATION"], headers["PART-TARGET"])
        if self.targets is not None and targets != self.targets:
            raise Failure("playlist targets changed")
        if self.first is not None and headers["MEDIA-SEQUENCE"] < self.first:
            raise Failure("media sequence decreased")
        self.targets, self.first = targets, headers["MEDIA-SEQUENCE"]
        for segment in playlist["segments"]:
            if segment["gap"]:
                continue
            key = segment["msn"]
            identity = (segment["uri"], segment["map"], segment["dseq"])
            if key in self.identities and self.identities[key] != identity:
                raise Failure("media sequence URI/MAP/discontinuity identity changed")
            self.identities[key] = identity
            if len(self.identities) > 512:
                raise Failure("history segment bound exceeded")
            if segment["map"] not in self.maps:
                if self.maps and segment["dseq"] <= self.latest_dseq:
                    raise Failure("new initialization without new discontinuity")
                self.maps.append(segment["map"])
                self.latest_dseq = segment["dseq"]
                if len(self.maps) > 16:
                    raise Failure("initialization bound exceeded")
        for part in playlist["parts"]:
            identity = (part["map"], part["msn"], part["dseq"], part["duration"])
            if self.part_identities.setdefault(part["uri"], identity) != identity:
                raise Failure("published part identity changed")
            if len(self.part_identities)>2048:
                raise Failure("history part bound exceeded")


class EpochRetention:
    """Remember the newest old segment, ignoring replayed sliding-window entries."""
    def __init__(self):
        self.latest, self.seen, self.last_map = {}, set(), None

    def observe(self, segment):
        mapping = segment["map"]
        prior = self.latest.get(mapping)
        if prior is None or segment["msn"] > prior["msn"]:
            self.latest[mapping] = segment
        if mapping in self.seen:
            return None
        if len(self.seen) >= 16:
            raise Failure("retention epoch bound exceeded")
        previous = self.latest.get(self.last_map)
        self.seen.add(mapping)
        self.last_map = mapping
        return previous


def verify_retained(fetch, media_base, resources, previous, new_map):
    for uri in (previous["map"], previous["uri"]):
        if fetch.get(media_base+uri, MAX_RESOURCE) != resources[uri]:
            raise Failure("retained epoch resource changed")
    return {"new_map": new_map, "old_map": previous["map"], "old_segment": previous["uri"],
            "old_init_sha256": sha(resources[previous["map"]]),
            "old_segment_sha256": sha(resources[previous["uri"]]), "unchanged_http_200": True}


def validate_base(url, schemes):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme not in schemes or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or parsed.query or parsed.fragment
            or any(ord(char) < 33 for char in url)):
        raise Failure("invalid destination base; credentials belong in environment variables")
    try:
        parsed.port
    except ValueError as error:
        raise Failure("invalid destination port") from error
    return url.rstrip("/")


def timing_plan(cycles, stage_seconds, gap_seconds, recording_wait_seconds):
    for value, low, high in ((cycles,1,3),(stage_seconds,12,30),(gap_seconds,0,3),(recording_wait_seconds,1,120)):
        if type(value) is not int or not low <= value <= high:
            raise Failure("run timing outside bounded integer ranges")
    stages = 3 * cycles
    planned = stages * stage_seconds + (stages-1) * gap_seconds + recording_wait_seconds + DECODE_CLEANUP_RESERVE_SECONDS
    if planned > RUN_SECONDS:
        raise Failure("planned stages, gaps, recording wait and decode/cleanup reserve exceed 300 seconds")
    return planned


def reconnect_pause(stage, gap_seconds, deadline):
    if stage == 0 or gap_seconds == 0:
        return
    if deadline-time.monotonic() <= gap_seconds:
        raise Failure("reconnect gap would exceed run deadline")
    time.sleep(gap_seconds)
    if time.monotonic() >= deadline:
        raise Failure("reconnect gap exceeded run deadline")


def publisher_command(ffmpeg, fixture, base, event, user, password, stage_seconds=12):
    if type(stage_seconds) is not int or not 12 <= stage_seconds <= 30:
        raise Failure("publisher duration outside 12 through 30 seconds")
    validate_base(base, ("rtmp", "rtmps"))
    if not EVENT.fullmatch(event):
        raise Failure("event must use the dedicated event-epoch- prefix")
    query = urllib.parse.urlencode({"user": user, "pass": password})
    return [ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-stream_loop", "-1", "-re",
            "-i", str(fixture), "-t", str(stage_seconds), "-map", "0:v:0", "-map", "0:a:0", "-c", "copy",
            "-bsf:a", "aac_adtstoasc", "-f", "flv", f"{base}/{event}?{query}"]


def redact(text, secrets):
    for secret in sorted(set(secrets), key=len, reverse=True):
        if secret:
            text = text.replace(secret, "[REDACTED]")
            text = text.replace(urllib.parse.quote_plus(secret), "[REDACTED]")
    # ffmpeg can echo URLs with differently escaped credentials.
    return re.sub(r"(?:rtmps?|https?)://[^\s\]\"']+", "[DESTINATION]", text)


class Process:
    """Own process group, deadline and two independently bounded output pipes."""
    def __init__(self, command, seconds):
        self.started = time.monotonic()
        self.deadline = self.started + seconds
        self.child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, start_new_session=True, bufsize=0)
        self.buffers, self.overflow = [bytearray(), bytearray()], threading.Event()
        self.threads = []
        for index, pipe in enumerate((self.child.stdout, self.child.stderr)):
            thread = threading.Thread(target=self._read, args=(index, pipe), daemon=True)
            thread.start()
            self.threads.append(thread)

    def _read(self, index, pipe):
        while block := pipe.read(4096):
            available = MAX_OUTPUT - len(self.buffers[index])
            self.buffers[index].extend(block[:available])
            if len(block) > available:
                self.overflow.set()
                self.signal(signal.SIGKILL)
                return

    def signal(self, value):
        try:
            os.killpg(self.child.pid, value)
        except ProcessLookupError:
            pass

    def check_deadline(self):
        if self.overflow.is_set():
            raise Failure("owned process output exceeded limit")
        if time.monotonic() >= self.deadline:
            raise Failure("owned process exceeded deadline")

    def close(self):
        self.signal(signal.SIGTERM)
        if self.child.poll() is None:
            try:
                self.child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.signal(signal.SIGKILL)
                self.child.wait(timeout=2)
        for thread in self.threads:
            thread.join(timeout=2)
        if any(thread.is_alive() for thread in self.threads):
            self.signal(signal.SIGKILL)
            raise Failure("owned process output reader did not stop")
        for pipe in (self.child.stdout, self.child.stderr):
            pipe.close()


def start_publisher(fixture, base, event, user, password, stage_seconds, deadline):
    command = publisher_command("ffmpeg", fixture, base, event, user, password, stage_seconds)
    remaining = deadline-time.monotonic()
    if remaining <= 0:
        raise Failure("publisher would start after run deadline")
    return Process(command, min(stage_seconds+8, remaining))


def capture(command, deadline, secrets):
    process = Process(command, min(15, max(0.01, deadline-time.monotonic())))
    try:
        while process.child.poll() is None:
            process.check_deadline()
            time.sleep(0.02)
        process.check_deadline()
    finally:
        process.close()
    if process.child.returncode:
        raise Failure("decoder/probe failed: " + redact(process.buffers[1].decode(errors="replace"), secrets))
    return bytes(process.buffers[0])


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        raise Failure("HTTP redirect refused")


class Fetcher:
    def __init__(self, deadline):
        self.deadline, self.bytes = deadline, 0
        self.opener = urllib.request.build_opener(NoRedirect)

    def get(self, url, limit):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise Failure("run deadline exceeded")
        with self.opener.open(url, timeout=min(2, remaining)) as response:
            if response.headers.get("Content-Encoding", "identity") != "identity":
                raise Failure("unexpected HTTP content encoding")
            data = response.read(min(limit, MAX_WIRE-self.bytes)+1)
        if len(data) > limit or self.bytes+len(data) > MAX_WIRE:
            raise Failure("HTTP byte budget exceeded")
        self.bytes += len(data)
        return data


def final_file_duration(data):
    """Only read our file's bounded initialization prefix; zero means open."""
    def boxes(start, end):
        count = 0
        while start < end:
            count += 1
            if count > 64 or end-start < 8:
                raise Failure("invalid recording box count/header")
            size = int.from_bytes(data[start:start+4], "big")
            if size < 8 or start+size > end:
                raise Failure("recording initialization exceeds prefix")
            yield data[start+4:start+8], start+8, start+size
            start += size
    for kind, start, end in boxes(0, len(data)):
        if kind != b"moov":
            continue
        for child, payload, child_end in boxes(start, end):
            if child != b"mvhd":
                continue
            if payload == child_end:
                raise Failure("empty recording mvhd")
            version = data[payload]
            at, width = (payload+12, 4) if version == 0 else (payload+20, 8)
            if version not in (0,1) or at+4+width > child_end:
                raise Failure("invalid recording mvhd")
            scale = int.from_bytes(data[at:at+4], "big")
            ticks = int.from_bytes(data[at+4:at+4+width], "big")
            if not scale:
                raise Failure("zero recording timescale")
            return ticks/scale
    raise Failure("no recording movie duration")


def recording_prefix(path):
    # Read complete ftyp and moov only, never parse mdat as initialization.
    with path.open("rb") as source:
        prefix = bytearray()
        for expected in (b"ftyp", b"moov"):
            header = source.read(8)
            if len(header) < 8:
                return b""
            if header[4:] != expected:
                raise Failure("unexpected recording initialization")
            size = int.from_bytes(header[:4], "big")
            if not 8 <= size <= MAX_PLAYLIST or len(prefix)+size > MAX_PLAYLIST:
                raise Failure("recording initialization exceeds bound")
            body = source.read(size-8)
            if len(body) != size-8:
                return b""
            prefix.extend(header+body)
    return bytes(prefix)


def recording_files(directory, maximum):
    files = []
    for path in directory.iterdir():
        if (len(files) >= maximum or path.is_symlink() or not path.is_file()
                or not re.fullmatch(r"[0-9]{1,19}\.mp4",path.name)):
            raise Failure("unexpected/excessive synthetic recording entries")
        files.append(path)
    return sorted(files, key=lambda path: int(path.stem))


def copy_recording(source, destination):
    before = source.stat()
    if not 0 < before.st_size <= MAX_WIRE:
        raise Failure("recording size bound exceeded")
    copied = 0
    with source.open("rb") as incoming, destination.open("xb") as outgoing:
        while block := incoming.read(min(65536, MAX_WIRE-copied+1)):
            if copied+len(block)>MAX_WIRE:
                raise Failure("growing recording exceeded copy bound")
            outgoing.write(block)
            copied += len(block)
    after = source.stat()
    if (before.st_size,before.st_mtime_ns)!=(after.st_size,after.st_mtime_ns) or copied!=before.st_size:
        raise Failure("recording changed during bounded read-only copy")


def probe_and_decode(path, expected_width, deadline, secrets):
    metadata = json.loads(capture(["ffprobe", "-v", "error", "-show_entries",
        "stream=codec_type,codec_name,width,height,sample_rate,start_time:format=duration", "-of", "json", str(path)], deadline, secrets))
    entries = metadata.get("streams", [])
    streams = {item["codec_type"]: item for item in entries}
    if (streams.get("video", {}).get("codec_name") != "h264" or streams["video"].get("width") != expected_width
            or streams["video"].get("height") != (1280 if expected_width == 720 else 720)
            or len(entries) != 2
            or streams.get("audio", {}).get("codec_name") != "aac" or streams["audio"].get("sample_rate") != "48000"):
        raise Failure("independent probe found incorrect dimensions/tracks")
    capture(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-xerror", "-i", str(path),
             "-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"], deadline, secrets)
    return metadata


def parse_arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--http-base", required=True)
    parser.add_argument("--rtmp-base", required=True)
    parser.add_argument("--event", required=True)
    parser.add_argument("--publish-user-env", required=True)
    parser.add_argument("--publish-pass-env", required=True)
    parser.add_argument("--recording-root", type=Path, required=True)
    parser.add_argument("--origin-binary", type=Path, required=True)
    parser.add_argument("--expected-origin-sha256", required=True)
    parser.add_argument("--source-manifest", type=Path, required=True)
    parser.add_argument("--expected-manifest-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cycles", type=int, choices=range(1,4), default=1)
    parser.add_argument("--stage-seconds", type=int, choices=range(12,31), default=12)
    parser.add_argument("--reconnect-gap-seconds", type=int, choices=range(0,4), default=0)
    parser.add_argument("--recording-wait-seconds", type=int, choices=range(1,121), default=100)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_arguments(argv)
    planned_seconds = timing_plan(args.cycles,args.stage_seconds,args.reconnect_gap_seconds,args.recording_wait_seconds)
    http = validate_base(args.http_base, ("http", "https"))
    rtmp = validate_base(args.rtmp_base, ("rtmp", "rtmps"))
    if not EVENT.fullmatch(args.event):
        raise Failure("dedicated event-epoch- event is required")
    for expected in (args.expected_origin_sha256, args.expected_manifest_sha256):
        if not SHA.fullmatch(expected):
            raise Failure("explicit lowercase SHA256 pins required")
    if file_sha(args.origin_binary) != args.expected_origin_sha256 or file_sha(args.source_manifest) != args.expected_manifest_sha256:
        raise Failure("origin/manifest hash mismatch")
    manifest = json.loads(args.source_manifest.read_text())
    if not isinstance(manifest, dict) or not 1 <= len(manifest) <= 128:
        raise Failure("source manifest is not a bounded source hash map")
    for name, expected in manifest.items():
        if not isinstance(name,str):
            raise Failure("source manifest entry is not a path")
        path = ROOT/name
        if not isinstance(expected,str) or not SHA.fullmatch(expected) or not path.resolve().is_relative_to(ROOT) or file_sha(path,MAX_RESOURCE)!=expected:
            raise Failure("current source does not match frozen manifest")
    fixtures = {pose: ROOT/f"crates/sapflow-hls/tests/fixtures/epoch-{pose}.ts" for pose in ("portrait", "landscape")}
    for path in fixtures.values():
        if file_sha(path, MAX_RESOURCE) != manifest.get(str(path.relative_to(ROOT))):
            raise Failure("fixture does not match frozen source manifest")
    if not all(re.fullmatch(r"[A-Z_][A-Z0-9_]{0,63}", name) for name in (args.publish_user_env,args.publish_pass_env)):
        raise Failure("credential environment variable names must be explicit uppercase names")
    user, password = os.environ.get(args.publish_user_env), os.environ.get(args.publish_pass_env)
    if not user or not password or len(user)>256 or len(password)>512:
        raise Failure("named credential environment variables missing/excessive")
    if any(ord(char)<32 for char in user+password):
        raise Failure("control characters in credential environment variables")
    secrets = [user, password]
    if not all(shutil.which(binary) for binary in ("ffmpeg", "ffprobe")):
        raise Failure("ffmpeg and ffprobe are required")
    recording = args.recording_root.resolve()/"live"/args.event
    if recording.exists() or recording.is_symlink():
        raise Failure("synthetic recording event directory must not exist")
    if args.output.resolve().is_relative_to(args.recording_root.resolve()):
        raise Failure("output must be separate from the read-only origin recording root")
    args.output.mkdir(parents=False, exist_ok=False)
    scratch = args.output/"scratch"
    scratch.mkdir()
    started, deadline = time.monotonic(), time.monotonic()+RUN_SECONDS
    fetch, history = Fetcher(deadline), History()
    media_base = f"{http}/live/{args.event}/"
    receipt = {"passed": False, "event": args.event, "origin_binary_sha256": args.expected_origin_sha256,
               "origin_process_identity_verified": False, "source_manifest_sha256": args.expected_manifest_sha256,
               "fixture_sha256": {pose: file_sha(path) for pose,path in fixtures.items()},
               "harness_sha256": file_sha(Path(__file__)),
               "source_hashes": manifest,
               "cycles": args.cycles, "stage_seconds": args.stage_seconds,
               "reconnect_gap_seconds": args.reconnect_gap_seconds,
               "recording_wait_seconds": args.recording_wait_seconds,
               "planned_seconds": planned_seconds, "deadline_seconds": RUN_SECONDS,
               "decode_cleanup_reserve_seconds": DECODE_CLEANUP_RESERVE_SECONDS,
               "publishers": [], "recordings": [], "media_samples": [], "physical_acceptance": False}
    process, resources, representatives = None, {}, {}
    retention = EpochRetention()
    receipt["retention_checks"] = []
    # urllib's socket timeout limits inactivity, not a peer trickling bytes.
    # A process-local POSIX timer also bounds DNS/headers/body on the main thread.
    previous_alarm = signal.signal(signal.SIGALRM, run_deadline_expired)
    signal.setitimer(signal.ITIMER_REAL, max(0.001,deadline-time.monotonic()))
    try:
        try:
            fetch.get(media_base+"index.m3u8", MAX_PLAYLIST)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code != 404:
                raise Failure("synthetic event preflight was not 404") from None
        else:
            raise Failure("event already served; refusing to publish over it")
        for stage, pose in enumerate(["portrait", "landscape", "portrait"]*args.cycles):
            reconnect_pause(stage,args.reconnect_gap_seconds,deadline)
            started_utc = datetime.now(timezone.utc).isoformat()
            process = start_publisher(fixtures[pose],rtmp,args.event,user,password,args.stage_seconds,deadline)
            item = {"stage": stage, "pose": pose, "requested_seconds": args.stage_seconds,
                    "gap_before_seconds": args.reconnect_gap_seconds if stage else 0,
                    "process_deadline_seconds": args.stage_seconds+8, "pid": process.child.pid,
                    "started_utc": started_utc}
            receipt["publishers"].append(item)
            while True:
                process.check_deadline()
                try:
                    data = fetch.get(media_base+"stream.m3u8", MAX_PLAYLIST)
                    playlist = parse_playlist(data)
                    history.accept(playlist)
                    (args.output/f"stage-{stage}.local.m3u8").write_bytes(data)
                    for segment in playlist["segments"]:
                        if segment["gap"]:
                            continue
                        # Each segment carries its applicable MAP, not the first MAP in the playlist.
                        for uri in (segment["map"], segment["uri"]):
                            if uri not in resources:
                                if len(resources) >= 512:
                                    raise Failure("resource cache bound exceeded")
                                resources[uri] = fetch.get(media_base+uri, MAX_RESOURCE)
                        representatives.setdefault(segment["map"], segment)
                        previous = retention.observe(segment)
                        if previous is not None:
                            receipt["retention_checks"].append(verify_retained(
                                fetch, media_base, resources, previous, segment["map"]))
                except urllib.error.HTTPError as error:
                    error.close()
                    if stage != 0 or error.code not in (404,503) or time.monotonic()-process.started > 5:
                        raise Failure(f"playlist HTTP status {error.code}") from None
                if process.child.poll() is not None:
                    break
                time.sleep(0.2)
            process.close()
            item.update(exit_code=process.child.returncode, elapsed_seconds=round(time.monotonic()-process.started,3),
                        completed_utc=datetime.now(timezone.utc).isoformat(),
                        stderr=redact(process.buffers[1].decode(errors="replace"), secrets))
            if item["exit_code"] or process.overflow.is_set() or item["elapsed_seconds"] < args.stage_seconds-2:
                raise Failure("synthetic publisher did not complete its bounded requested stage")
            process = None
            print(json.dumps({"completed_stage": stage, "pose": pose}), flush=True)
        expected_poses = ["portrait", "landscape", "portrait"]
        for _ in range(1,args.cycles):
            expected_poses += ["landscape", "portrait"]
        if len(history.maps) != len(expected_poses):
            raise Failure("unexpected number of published codec epochs")
        for index, (mapping, pose) in enumerate(zip(history.maps, expected_poses)):
            segment = representatives[mapping]
            path = scratch/f"sample-{index}.mp4"
            path.write_bytes(resources[mapping]+resources[segment["uri"]])
            metadata = probe_and_decode(path, 720 if pose=="portrait" else 1280, deadline, secrets)
            receipt["media_samples"].append({"map": mapping, "segment": segment["uri"], "sha256": file_sha(path), "probe": metadata, "decode_passed": True})
            path.unlink()
        until = min(deadline, time.monotonic()+args.recording_wait_seconds)
        final = None
        while time.monotonic() < until:
            if recording.exists():
                if recording.is_symlink() or not recording.resolve().is_relative_to(args.recording_root.resolve()):
                    raise Failure("recording event escaped supplied root")
                files = recording_files(recording,len(expected_poses))
                if len(files)==len(expected_poses) and all((prefix:=recording_prefix(p)) and final_file_duration(prefix)>0 for p in files):
                    final = files
                    break
            time.sleep(0.25)
        if final is None:
            raise Failure("origin recordings did not finalise within bounded wait")
        for index,(source,pose) in enumerate(zip(final,expected_poses)):
            copy = scratch/f"recording-{index}.mp4"
            copy_recording(source,copy)
            metadata = probe_and_decode(copy,720 if pose=="portrait" else 1280,deadline,secrets)
            for stream in metadata["streams"]:
                start=Decimal(stream.get("start_time","NaN"))
                if not start.is_finite() or not 0 <= start <= Decimal("0.05"):
                    raise Failure("standalone recording origin/track offset exceeds 50ms")
            receipt["recordings"].append({"file": source.name,"bytes":copy.stat().st_size,"sha256":file_sha(copy),"probe":metadata,"decode_passed":True})
            copy.unlink()
        receipt["passed"] = True
    except Exception as error:
        receipt["failure"] = redact(f"{type(error).__name__}: {error}", secrets)
    finally:
        # Once admission stops, cleanup has its own finite process waits.
        # Do not allow the run alarm to interrupt the cleanup/receipt path.
        signal.setitimer(signal.ITIMER_REAL,0)
        signal.signal(signal.SIGALRM,previous_alarm)
        cleanup_errors = []
        if process is not None:
            try:
                process.close()
            except Exception as error:
                cleanup_errors.append(redact(type(error).__name__+": "+str(error),secrets))
            receipt["publishers"][-1].update(exit_code=process.child.poll(),
                elapsed_seconds=round(time.monotonic()-process.started,3),
                completed_utc=datetime.now(timezone.utc).isoformat(),
                stderr=redact(process.buffers[1].decode(errors="replace"),secrets))
        try:
            shutil.rmtree(scratch)
        except OSError as error:
            cleanup_errors.append(type(error).__name__)
        receipt.update(elapsed_seconds=round(time.monotonic()-started,3),http_bytes=fetch.bytes,
                       media_segments_seen=len(history.identities),codec_initializations=history.maps,
                       owned_processes_stopped=not cleanup_errors,owned_scratch_removed=not scratch.exists(),
                       origin_and_recordings_untouched=True)
        if cleanup_errors:
            receipt.update(passed=False,cleanup_errors=cleanup_errors)
        (args.output/"receipt.local.json").write_text(json.dumps(receipt,indent=2)+"\n")
    print(json.dumps({"passed":receipt["passed"],"receipt":str(args.output/"receipt.local.json")}),flush=True)
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Failure as error:
        print(json.dumps({"passed":False,"failure":str(error)}))
        raise SystemExit(1)
