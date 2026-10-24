#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Build the current host Rust SDK and run the complete iOS checkpoint suites."""
from pathlib import Path
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile

from checkpoint_test_processes import stop_owned_group

ROOT = Path(__file__).resolve().parents[2]
SOURCES = (
    "CaptureOrientation", "CaptureProfile", "RecordingStorage", "RecordingCatalogue",
    "CaptureRecordingProducer", "HardwareVideoEncoder", "VideoEncodingPolicy", "PreparedVideoSlate",
    "NativeAudioEncoder", "AudioRecordingTiming", "QuickTimeAudioPriming", "AudioShowLifecycle",
    "AudioInputPolicy", "CameraControlPolicy", "CaptureAdmission", "RecordingCaptureAdmission",
    "EncodedPipeline", "PublishingTelemetry", "RecordingCheckpointOwner", "RecordingCheckpointProbe",
)
TESTS = (("RecordingCheckpointOwnerTests", 16), ("RecordingCheckpointProbeTests", 12),
         ("RecordingDurableInputBudgetTests", 18))
TEST_COUNT = sum(count for _, count in TESTS)


def xcode_shared_frameworks(sdk):
    """Locate XCTest's private runtime in the bundle owning the selected SDK."""
    for ancestor in sdk.parents:
        if ancestor.name == "Contents":
            shared = ancestor / "SharedFrameworks"
            return shared if shared.is_dir() else None
    return None


def main():
    if sys.platform != "darwin" or platform.machine() not in ("arm64", "x86_64"):
        raise RuntimeError("Requires macOS with the full Xcode developer tools")
    if os.environ.get("CARGO_TARGET_DIR") or os.environ.get("CARGO_BUILD_TARGET"):
        raise RuntimeError("Use the checkout's existing target and native host target")
    target = ROOT / "target"
    target.mkdir(exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="sapflow-checkpoint-sdk-", dir=target))
    modules = target / "ios-module-cache"
    active = None
    launching = retiring = finalising = False
    pending = None
    unknown = False
    failure = None

    def caught(number, frame):
        nonlocal pending
        pending = pending or number
        if not launching and not retiring and not finalising:
            raise InterruptedError("Signal " + str(number))

    previous = {number: signal.signal(number, caught) for number in (signal.SIGINT, signal.SIGTERM)}

    def run(label, argv, timeout, env=None):
        nonlocal active, launching, retiring, unknown
        if active is not None or unknown or pending is not None:
            raise RuntimeError("Previous owned child is not safely retired")
        owned = None
        attempted = False
        print("checkpoint-sdk: " + label, flush=True)
        try:
            with (scratch / (label + ".stdout")).open("xb") as output, (scratch / (label + ".stderr")).open("xb") as diagnostics:
                launching = True
                try:
                    attempted = True
                    process = subprocess.Popen(list(map(str, argv)), cwd=ROOT, env=env,
                        stdin=subprocess.DEVNULL, stdout=output, stderr=diagnostics, start_new_session=True)
                    owned = {"child": process, "cleanupAttempted": False}
                    active = owned
                finally:
                    launching = False
                if pending is not None:
                    raise InterruptedError("Signal during child launch")
                process.wait(timeout=timeout)
        finally:
            if owned is not None:
                retiring = True
                owned["cleanupAttempted"] = True
                try:
                    if stop_owned_group(owned, scratch, label):
                        active = None
                    else:
                        unknown = True
                except BaseException:
                    unknown = True
                    raise
                finally:
                    retiring = False
            elif attempted:
                unknown = True
            for suffix, destination in (("stdout", sys.stdout), ("stderr", sys.stderr)):
                path = scratch / (label + "." + suffix)
                if path.exists():
                    with path.open("rb") as stream:
                        shutil.copyfileobj(stream, destination.buffer)
                    destination.flush()
        if unknown or active is not None or pending is not None or owned["child"].returncode != 0:
            raise RuntimeError("Failed or unclosed child: " + label)
        return (scratch / (label + ".stdout")).read_text()

    try:
        modules.mkdir(exist_ok=True)
        sdk = Path(run("sdk-path", ["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-path"], 30).strip())
        if not sdk.is_dir() or not sdk.is_absolute():
            raise RuntimeError("xcrun did not select an installed macOS SDK")
        developer = sdk.parent.parent
        frameworks = developer / "Library/Frameworks"
        testlibs = developer / "usr/lib"
        shared = xcode_shared_frameworks(sdk)
        # Xcode 16.4's XCTest Swift support loads XCTestCore from this bundle
        # directory. Keep the selected platform's public framework search first.
        shared_rpath = ["-Xlinker", "-rpath", "-Xlinker", shared] if shared else []
        temporary = scratch / "tmp"
        temporary.mkdir()
        swift_env = {**os.environ, "SDKROOT": str(sdk), "TMPDIR": str(temporary)}
        run("rust-sdk", ["cargo", "build", "--locked", "-p", "sapflow-ffi", "--features", "bindgen", "-j", "1"], 1800)
        library = target / "debug/libsapflow_ffi.dylib"
        run("swift-bindings", [target / "debug/sapflow-bindgen", "generate", library,
            "-l", "swift", "-o", scratch, "--no-format"], 180)
        common = ["/usr/bin/xcrun", "--sdk", "macosx", "swiftc", "-swift-version", "5", "-warnings-as-errors",
            "-target", platform.machine() + "-apple-macos14.0", "-sdk", sdk, "-module-cache-path", modules,
            "-I", scratch, "-Xcc", "-fmodule-map-file=" + str(scratch / "SapflowFFI.modulemap")]
        run("sdk-compile", [*common, "-parse-as-library", "-emit-module", "-emit-object", "-module-name", "Sapflow",
            scratch / "Sapflow.swift", "-emit-module-path", scratch / "Sapflow.swiftmodule", "-o", scratch / "Sapflow.o"], 1800, swift_env)
        main_source = []
        names = []
        for test, count in TESTS:
            text = (ROOT / "mobile/ios/Tests" / (test + ".swift")).read_text()
            methods = re.findall(r"^\s*func (test\w+)\(\)", text, re.MULTILINE)
            if len(methods) != count or len(set(methods)) != count:
                raise RuntimeError("Complete expected test selection changed: " + test)
            names.extend(methods)
            main_source.extend(line for line in text.splitlines(keepends=True)
                if line.strip() != "@testable import SapflowBroadcaster")
            main_source.append("\n")
        main_source.append('let suite = XCTestSuite(name: "Real SDK checkpoint owner and cold observation")\n')
        main_source.extend("suite.addTest(" + test + ".defaultTestSuite)\n" for test, _ in TESTS)
        main_source.append("suite.run()\nguard let result = suite.testRun, result.executionCount == " +
                           str(TEST_COUNT) + ", result.hasSucceeded else { exit(1) }\n")
        (scratch / "main.swift").write_text("".join(main_source))
        run("test-compile", [*common, "-F", frameworks, "-I", testlibs, "-L", testlibs,
            "-L", library.parent, "-lsapflow_ffi", "-Xlinker", "-rpath", "-Xlinker", frameworks,
            "-Xlinker", "-rpath", "-Xlinker", testlibs, "-Xlinker", "-rpath", "-Xlinker", library.parent,
            *shared_rpath,
            *(ROOT / "mobile/ios/Sources" / (name + ".swift") for name in SOURCES),
            scratch / "main.swift", scratch / "Sapflow.o", "-o", scratch / "checkpoint-tests"], 1800, swift_env)
        text = run("test-run", [scratch / "checkpoint-tests"], 180, swift_env)
        text += (scratch / "test-run.stderr").read_text()
        for name in names:
            if len(re.findall(r"Test Case '[^\n']*\b" + re.escape(name) + r"\b[^\n']*' passed", text)) != 1:
                raise RuntimeError("Expected actual test pass once: " + name)
        if re.search(r"Executed " + str(TEST_COUNT) + r" tests?, with 0 failures", text) is None:
            raise RuntimeError("Missing complete XCTest" + str(TEST_COUNT) + " result")
        print("checkpoint-sdk: all " + str(TEST_COUNT) + " real-SDK filesystem tests passed; no native/recovery/release qualification")
    except BaseException as error:
        failure = error
        raise
    finally:
        finalising = True
        # Never unlink files while an owned child or its current group is unknown.
        try:
            if active is None and not unknown and failure is None and pending is None:
                shutil.rmtree(scratch)
            else:
                print("checkpoint-sdk: retained failed/unknown scratch: " + str(scratch), file=sys.stderr)
        finally:
            for number, handler in previous.items():
                signal.signal(number, handler)
    if pending is not None:
        raise InterruptedError("Signal during final cleanup: " + str(pending))


if __name__ == "__main__":
    main()
