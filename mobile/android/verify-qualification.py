#!/usr/bin/env python3
"""Verify the built internal APK; does not access a device or private keys."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
from zipfile import ZipFile


EXPECTED_AAR = "4d782d501ab8c171aaf83a7dcbe70675988c32f5551c3916e1149f530286fc1a"
EXPECTED_NATIVE = "c86c5a90fe6fde856374948897e7433edcf5c9cbba3949e0745d1cde493fad7e"
POSE_APPLICATION = "dev.forgesworn.sapflow.broadcaster.QualificationPoseApplication"
POSE_MARKERS = (b"Ldev/forgesworn/sapflow/broadcaster/QualificationPoseApplication;",
                b"dev.forgesworn.sapflow.broadcaster.QUALIFICATION_POSE")
BENCHMARK_MARKER = b"Ldev/forgesworn/sapflow/broadcaster/FfiBenchmarkActivity;"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def expected_sha256(value):
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-fA-F]{64}", value) is None:
        raise ValueError("Expected SHA-256 must contain exactly 64 hexadecimal digits")
    return value.lower()


def node_block(tree, name):
    lines = tree.splitlines()
    for index, line in enumerate(lines):
        if re.search(r"E: " + re.escape(name) + r"(?:\s|$)", line):
            indentation = len(line) - len(line.lstrip())
            end = index + 1
            while end < len(lines):
                following = lines[end]
                if following.strip() and len(following) - len(following.lstrip()) <= indentation:
                    break
                end += 1
            return "\n".join(lines[index:end])
    raise ValueError("Missing manifest node: " + name)


def attribute(block, name):
    match = re.search(r":" + re.escape(name) + r"\([^)]*\)=(\S+)", block)
    return match.group(1) if match else None


def pose_markers(archive, markers=POSE_MARKERS):
    entries = [item for item in archive.infolist() if re.fullmatch(r"classes(?:[0-9]+)?\.dex", item.filename)]
    if sum(item.file_size for item in entries) > 64 * 1024 * 1024:
        raise ValueError("APK dex size exceeds verification bound")
    found = [False] * len(markers)
    for item in entries:
        data = archive.read(item)
        found = [old or marker in data for old, marker in zip(found, markers)]
    return found


def verify_release_isolation(apk, aapt2, *, expected_native=EXPECTED_NATIVE):
    expected_native = expected_sha256(expected_native)
    tree = subprocess.check_output(
        [str(aapt2), "dump", "xmltree", "--file", "AndroidManifest.xml", str(apk)], text=True)
    application = node_block(tree, "application")
    if attribute(application, "debuggable") not in (None, "false", "0x00000000"):
        raise ValueError("Release APK is debuggable")
    if POSE_APPLICATION in application or "FfiBenchmarkActivity" in application:
        raise ValueError("Qualification components present in release manifest")
    if re.search(r"E: profileable(?:\s|$)", application) and attribute(node_block(application, "profileable"), "shell") in ("true", "0xffffffff"):
        raise ValueError("Release APK permits shell profiling")
    with ZipFile(apk) as archive:
        if any(pose_markers(archive, POSE_MARKERS + (BENCHMARK_MARKER,))):
            raise ValueError("Qualification pose hook present in release dex")
        native = archive.read("lib/arm64-v8a/libsapflow_ffi.so")
    if sha256(native) != expected_native:
        raise ValueError("Release Rust library differs from the frozen package")
    return {"apk_sha256": sha256(apk.read_bytes()), "qualification_hook_absent": True,
            "offline_benchmark_absent": True, "profileable_shell": False,
            "native_sha256": sha256(native)}


def verify(apk, aar, aapt2, *, expected_aar=EXPECTED_AAR, expected_native=EXPECTED_NATIVE, expect_pose_hook=False):
    expected_aar = expected_sha256(expected_aar)
    expected_native = expected_sha256(expected_native)
    aar_digest = sha256(aar.read_bytes())
    if aar_digest != expected_aar:
        raise ValueError("AAR SHA-256 differs from the expected frozen package")
    tree = subprocess.check_output(
        [str(aapt2), "dump", "xmltree", "--file", "AndroidManifest.xml", str(apk)],
        text=True,
    )
    application = node_block(tree, "application")
    debuggable = attribute(application, "debuggable")
    if debuggable not in (None, "false", "0x00000000"):
        raise ValueError("Qualification APK is debuggable: " + str(debuggable))
    profiling = node_block(application, "profileable")
    shell = attribute(profiling, "shell")
    if shell not in ("true", "0xffffffff"):
        raise ValueError("Qualification APK does not permit shell profiling")
    if attribute(profiling, "enabled") in ("false", "0x00000000"):
        raise ValueError("Qualification APK disables shell profiling")
    if '="dev.forgesworn.sapflow.broadcaster.FfiBenchmarkActivity"' not in application:
        raise ValueError("Offline benchmark Activity missing from qualification APK")
    if expect_pose_hook and attribute(application, "name") != '"' + POSE_APPLICATION + '"':
        raise ValueError("Qualification pose Application missing from manifest")
    with ZipFile(aar) as archive:
        aar_native = archive.read("jni/arm64-v8a/libsapflow_ffi.so")
    with ZipFile(apk) as archive:
        apk_native = archive.read("lib/arm64-v8a/libsapflow_ffi.so")
        if expect_pose_hook and not all(pose_markers(archive)):
            raise ValueError("Qualification pose hook missing from dex")
    if apk_native != aar_native or sha256(apk_native) != expected_native:
        raise ValueError("Packaged Rust library differs from the frozen AAR")
    result = {
        "apk_sha256": sha256(apk.read_bytes()),
        "aar_sha256": aar_digest,
        "native_sha256": sha256(apk_native),
        "debuggable": False,
        "profileable_shell": True,
        "offline_benchmark_present": True,
        "physical_qualified": False,
    }
    if expect_pose_hook:
        result["qualification_pose_hook_present"] = True
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apk", type=Path)
    parser.add_argument("aar", type=Path)
    parser.add_argument("--aapt2", required=True, type=Path)
    parser.add_argument("--expected-aar-sha256", type=expected_sha256, default=EXPECTED_AAR)
    parser.add_argument("--expected-native-sha256", type=expected_sha256, default=EXPECTED_NATIVE)
    parser.add_argument("--expect-pose-hook", action="store_true")
    parser.add_argument("--release-apk", type=Path)
    options = parser.parse_args()
    if options.release_apk and not options.expect_pose_hook:
        parser.error("--release-apk requires --expect-pose-hook")
    result = verify(
        options.apk, options.aar, options.aapt2,
        expected_aar=options.expected_aar_sha256,
        expected_native=options.expected_native_sha256,
        expect_pose_hook=options.expect_pose_hook,
    )
    if options.release_apk:
        result["release_isolation"] = verify_release_isolation(options.release_apk, options.aapt2,
            expected_native=options.expected_native_sha256)
    print(json.dumps(result, indent=2))
