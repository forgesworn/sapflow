#!/usr/bin/env python3
"""macOS-only native decode check using media generated during this run.

No device, camera, existing recording, public listener or upload is involved.
Build the Swift helper and Sapflow first; both executable paths are explicit.
Only private scalar receipts/logs survive. All generated media is deleted.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


class HarnessDeadline(BaseException):
    """Must escape the playlist poll's retryable Exception handler."""


def independent_part_uris(epoch, raw):
    result = set()
    for line in raw.decode().splitlines():
        if line.startswith("#EXT-X-PART:"):
            attributes = epoch.attributes(line.split(":", 1)[1])
            if attributes.get("INDEPENDENT") == "YES":
                result.add(epoch.safe_uri(attributes["URI"]))
    return result


def cleanup_owned(children, scratch):
    errors = []
    for child in children:
        if child is None:
            continue
        try:
            if child.poll() is None:
                try:
                    child.terminate()
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        child.kill()
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=5)
        except BaseException as error:
            errors.append("child-stop:" + type(error).__name__)
            try:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
            except BaseException as fallback:
                errors.append("child-kill:" + type(fallback).__name__)
    stopped = True
    for child in children:
        try:
            stopped = (child is None or child.poll() is not None) and stopped
        except BaseException as error:
            stopped = False
            errors.append("child-status:" + type(error).__name__)
    try:
        shutil.rmtree(scratch)
    except BaseException as error:
        errors.append("scratch-remove:" + type(error).__name__)
    try:
        removed = not scratch.exists()
    except BaseException as error:
        removed = False
        errors.append("scratch-status:" + type(error).__name__)
    return {"ownedChildrenStopped": stopped, "ownSyntheticScratchRemoved": removed, "cleanupErrors": errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--origin-binary", required=True, type=Path)
    parser.add_argument("--reader-executable", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("this native reader check requires macOS")
    origin_binary = args.origin_binary.resolve(strict=True)
    reader_binary = args.reader_executable.resolve(strict=True)
    assert origin_binary.is_file() and reader_binary.is_file()
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(mode=0o700)  # Never replace an existing receipt directory.
    spec = importlib.util.spec_from_file_location("epoch", ROOT / "tests/origin-epoch-integration.py")
    epoch = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(epoch)

    def free_port(kind):
        with socket.socket(socket.AF_INET, kind) as channel:
            channel.bind(("127.0.0.1", 0))
            return channel.getsockname()[1]

    http, rtmp, srt = free_port(socket.SOCK_STREAM), free_port(socket.SOCK_STREAM), free_port(socket.SOCK_DGRAM)
    for _ in range(16):
        if rtmp != http:
            break
        rtmp = free_port(socket.SOCK_STREAM)
    assert rtmp != http, "could not reserve distinct HTTP and RTMP ports"
    environment = {key: value for key, value in os.environ.items() if not key.startswith("SAPFLOW_")}
    password = secrets.token_urlsafe(24)
    environment.update(SAPFLOW_HTTP_ADDR=f"127.0.0.1:{http}", SAPFLOW_RTMP_ADDR=f"127.0.0.1:{rtmp}",
                       SAPFLOW_SRT_ADDR=f"127.0.0.1:{srt}", SAPFLOW_PUBLISH_USER="synthetic",
                       SAPFLOW_PUBLISH_PASS=password, SAPFLOW_RECONNECT_GRACE="4")
    origin = publisher = None
    receipt = {
        "startedUtc": datetime.now(timezone.utc).isoformat(),
        "scope": "macOS native reader feasibility; no hardware, iPhone or playback qualification",
        "source": "lavfi testsrc2 and sine generated in this run",
        "originBinarySha256": hashlib.sha256(origin_binary.read_bytes()).hexdigest(),
        "readerBinarySha256": hashlib.sha256(reader_binary.read_bytes()).hexdigest(),
        "cameraDataUsed": False, "phoneActions": False, "recordingsEnabled": False,
        "externalCallbacks": False, "cases": [], "passed": False,
    }

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def fetch(uri, limit):
        if uri != "stream.m3u8":
            epoch.safe_uri(uri)
        url = f"http://127.0.0.1:{http}/live/event-native-generated-reader/{uri}"
        with opener.open(url, timeout=2) as response:
            data = response.read(limit + 1)
            assert len(data) <= limit
            return data

    def timed_out(_signum, _frame):
        raise HarnessDeadline("native reader harness exceeded 60 seconds")

    scratch = Path(tempfile.mkdtemp(prefix="sapflow-generated-native-reader-"))
    previous_handler = signal.signal(signal.SIGALRM, timed_out)
    signal.alarm(60)
    try:
        with (output / "origin.local.log").open("x") as log:
            origin = subprocess.Popen([str(origin_binary)], cwd=scratch, env=environment,
                                      stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        for _ in range(50):
            assert origin.poll() is None, "origin stopped during startup"
            try:
                with socket.create_connection(("127.0.0.1", rtmp), timeout=0.1):
                    break
            except OSError:
                time.sleep(0.1)
        command = ["ffmpeg", "-hide_banner", "-v", "error", "-nostdin", "-re",
                   "-f", "lavfi", "-i", "testsrc2=size=848x480:rate=30",
                   "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                   "-t", "12", "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "veryfast",
                   "-tune", "zerolatency", "-profile:v", "baseline", "-bf", "0", "-g", "30",
                   "-keyint_min", "30", "-sc_threshold", "0", "-b:v", "600k", "-maxrate", "600k",
                   "-bufsize", "600k", "-c:a", "aac", "-profile:a", "aac_low", "-ac", "1",
                   "-b:a", "96k", "-f", "flv",
                   f"rtmp://127.0.0.1:{rtmp}/live/event-native-generated-reader?user=synthetic&pass={password}"]
        with (output / "publisher.local.log").open("x") as log:
            publisher = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        chosen = None
        for _ in range(100):
            try:
                raw = fetch("stream.m3u8", 131072)
                playlist = epoch.parse_playlist(raw)
                independent = independent_part_uris(epoch, raw)
                candidates = [part for part in playlist["parts"] if part["uri"] in independent]
                if candidates:
                    chosen = candidates[-1]
                    break
            except Exception:
                pass
            assert publisher.poll() is None, "generated publisher stopped"
            time.sleep(0.1)
        assert chosen, "no independent origin part"
        initialisation = fetch(chosen["map"], 262144)
        part = fetch(chosen["uri"], 8388608)
        assert len(initialisation) + len(part) <= 8388608
        fixture = scratch / "generated-init-and-independent-part.mp4"
        fixture.write_bytes(initialisation + part)
        header = scratch / "generated-init-only.mp4"
        header.write_bytes(initialisation)
        receipt.update(fixtureSha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
                       initialisationBytes=len(initialisation), independentPartBytes=len(part))
        metadata = subprocess.run(["ffprobe", "-v", "error", "-count_frames", "-show_entries",
                                   "stream=codec_name,codec_type,width,height,nb_read_frames", "-of", "json", str(fixture)],
                                  capture_output=True, text=True, timeout=10, check=True)
        streams = json.loads(metadata.stdout)["streams"]
        assert any(track["codec_name"] == "h264" and int(track.get("nb_read_frames", "0")) >= 2 for track in streams)
        assert any(track["codec_name"] == "aac" and int(track.get("nb_read_frames", "0")) >= 2 for track in streams)
        subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-xerror", "-i", str(fixture),
                        "-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"],
                       capture_output=True, timeout=10, check=True)
        receipt["independentDecodedTracks"] = streams
        subprocess.run([str(reader_binary), "--check-pcm-timing"], capture_output=True, timeout=6, check=True)
        receipt["pcmTimingNegativeAndBoundaryChecksPassed"] = True
        for name, path, expected in [("init-and-independent-part", fixture, True), ("initialisation-without-media", header, False)]:
            started = time.monotonic()
            result = subprocess.run([str(reader_binary), "--synthetic-mp4", str(path)],
                                    capture_output=True, text=True, timeout=6)
            summary = json.loads(result.stdout)
            case = {"name": name, "exitCode": result.returncode, "elapsedSeconds": time.monotonic() - started,
                    "expectedPass": expected, "summary": summary, "stderrBytes": len(result.stderr)}
            receipt["cases"].append(case)
            assert summary["ok"] == expected and (result.returncode == 0) == expected and result.returncode != 124
            if expected:
                assert summary["videoOutputs"] >= 2 and summary["validatedPCMFrames"] >= 2048
        receipt["passed"] = True
    finally:
        try:
            signal.alarm(0)
        finally:
            try:
                cleanup = cleanup_owned([publisher, origin], scratch)
                receipt.update(cleanup)
                receipt["passed"] = (receipt["passed"] and cleanup["ownedChildrenStopped"]
                                     and cleanup["ownSyntheticScratchRemoved"] and not cleanup["cleanupErrors"])
                (output / "receipt.local.json").write_text(json.dumps(receipt, indent=2) + "\n")
            finally:
                signal.signal(signal.SIGALRM, previous_handler)
    assert receipt["passed"], "native reader check or cleanup failed"
    print(json.dumps({"passed": receipt["passed"], "receipt": str(output / "receipt.local.json")}))


if __name__ == "__main__":
    main()
