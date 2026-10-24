#!/usr/bin/env python3
"""Real AAR classes + current native SDK exercise production JVM UDP source.
Independent libsrt receives AES256 MPEG-TS, ffmpeg decodes. No Cargo/build of
substitute bindings is performed. Android hardware acceptance is separate.
"""
from pathlib import Path
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import zipfile

ROOT = Path(__file__).resolve().parents[3]
SECRET = "sapflow-public-lab-secret"


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, **kwargs)


def workspace_package_version(manifest):
    """Match native packaging's quoted workspace version without running Cargo."""
    package_table, tables, versions = False, 0, []
    for line in manifest.splitlines():
        value = line.strip()
        if value.startswith("["):
            package_table = re.fullmatch(r"\[workspace\.package]\s*(?:#.*)?", value) is not None
            tables += int(package_table)
        elif package_table and re.match(r"version\s*=", value):
            match = re.fullmatch(r'version\s*=\s*"([^"]+)"\s*(?:#.*)?', value)
            if match is None:
                raise ValueError("Expected a double-quoted workspace.package.version")
            versions.append(match[1])
    if tables != 1 or len(versions) != 1:
        raise ValueError("Expected one workspace.package.version")
    version = versions[0]
    if len(version) > 128 or re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", version) is None:
        raise ValueError("Expected a filename-safe workspace.package.version")
    return version


def main():
    version = workspace_package_version((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    aar = ROOT / f"target/native-packages/android-arm64-v8a/sapflow-{version}.aar"
    native = ROOT / "target/debug/libsapflow_ffi.dylib"
    jna = ROOT / "target/native-tooling/jna-5.19.1.jar"
    assert aar.is_file() and native.is_file() and jna.is_file(), "Actual packages/native check must be built first"
    for tool in ("kotlinc", "java", "srt-live-transmit", "ffmpeg", "ffprobe"):
        assert shutil.which(tool), f"{tool} required"
    with tempfile.TemporaryDirectory(prefix="srt-jvm-loop-", dir=ROOT / "target/mobile-android") as temporary:
        scratch = Path(temporary)
        classes, consumer = scratch / "classes.jar", scratch / "consumer.jar"
        with zipfile.ZipFile(aar) as archive:
            classes.write_bytes(archive.read("classes.jar"))
        source = ROOT / "mobile/android/app/src/main/java/dev/forgesworn/sapflow/broadcaster"
        run("kotlinc", "-language-version", "2.2", "-api-version", "2.2", "-cp", f"{classes}:{jna}",
            *[str(source / name) for name in ("EncodedSink.kt", "SrtPublishingSink.kt", "SrtDestination.kt", "SingleSampleOffer.kt", "ExplicitAttempt.kt", "SrtShutdown.kt")],
            str(ROOT / "mobile/android/qualification/SrtLoop.kt"), "-include-runtime", "-d", str(consumer))
        fixture, received = scratch / "source.flv", scratch / "received.ts"
        run("ffmpeg", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30", "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
            "-t", "6", "-c:v", "libx264", "-preset", "ultrafast", "-profile:v", "baseline", "-bf", "0", "-g", "30", "-c:a", "aac", "-ac", "1", "-f", "flv", str(fixture))
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        with received.open("wb") as output, (scratch / "receiver.log").open("w+") as log:
            receiver = subprocess.Popen(["srt-live-transmit", "-a:no", "-t:15",
                f"srt://127.0.0.1:{port}?mode=listener&latency=120&passphrase={SECRET}&pbkeylen=32&enforcedencryption=1", "file://con"], stdout=output, stderr=log)
            try:
                time.sleep(0.2)
                try:
                    result = run("java", f"-Djna.library.path={native.parent}", "-cp", f"{consumer}:{classes}:{jna}",
                        "dev.forgesworn.sapflow.broadcaster.SrtLoopKt", f"srt://127.0.0.1:{port}", str(fixture), SECRET, timeout=25)
                except subprocess.CalledProcessError as error:
                    log.flush(); log.seek(0)
                    raise RuntimeError(f"Actual JVM UDP loop failed:\n{error.stderr}\nIndependent receiver:\n{log.read()}") from error
                time.sleep(0.3)
                receiver.send_signal(signal.SIGINT)
                receiver.wait(timeout=3)
            finally:
                if receiver.poll() is None:
                    receiver.terminate()
                    try:
                        receiver.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        receiver.kill(); receiver.wait(timeout=3)
        streams = json.loads(run("ffprobe", "-v", "error", "-count_frames", "-show_streams", "-of", "json", str(received)).stdout)["streams"]
        tracks = {stream["codec_type"]: stream for stream in streams}
        assert tracks["video"]["codec_name"] == "h264" and int(tracks["video"]["nb_read_frames"]) == 180
        assert tracks["audio"]["codec_name"] == "aac" and int(tracks["audio"]["nb_read_frames"]) >= 275
        run("ffmpeg", "-v", "error", "-i", str(received), "-f", "null", "-")
        print(json.dumps({"passed": True, "transport": "actual production JVM DatagramChannel loop", "cipher": "AES256", "independent_receiver": "libsrt",
            "video_frames": int(tracks["video"]["nb_read_frames"]), "audio_frames": int(tracks["audio"]["nb_read_frames"]), "decoded": True,
            "android_physical_tested": False, "native_sdk": "real macOS native SDK with actual AAR classes", "fixture_cleanup": True, "sender": result.stdout.strip()}))


if __name__ == "__main__":
    main()
