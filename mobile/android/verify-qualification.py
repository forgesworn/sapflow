#!/usr/bin/env python3
"""Verify the built internal APK; does not access a device or private keys."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import struct
from urllib.parse import urlsplit
from zipfile import ZipFile


EXPECTED_AAR = "4d782d501ab8c171aaf83a7dcbe70675988c32f5551c3916e1149f530286fc1a"
EXPECTED_NATIVE = "c86c5a90fe6fde856374948897e7433edcf5c9cbba3949e0745d1cde493fad7e"
POSE_APPLICATION = "dev.forgesworn.sapflow.broadcaster.QualificationPoseApplication"
POSE_MARKERS = (b"Ldev/forgesworn/sapflow/broadcaster/QualificationPoseApplication;",
                b"dev.forgesworn.sapflow.broadcaster.QUALIFICATION_POSE")
BENCHMARK_MARKER = b"Ldev/forgesworn/sapflow/broadcaster/FfiBenchmarkActivity;"
AAC_PROBE_MARKERS = (b"Ldev/forgesworn/sapflow/broadcaster/NativeAacQualificationActivity;",
                     b"Ldev/forgesworn/sapflow/broadcaster/NativeAacQualificationProbe;",
                     b"dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC",
                     b"dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_DRAIN",
                     b"Ldev/forgesworn/sapflow/broadcaster/NativeAacLeadingProbe;",
                     b"Ldev/forgesworn/sapflow/broadcaster/NativeAacLeadingAlignment;",
                     b"dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_LEADING")
FIXTURE_CA = "res/raw/sapflow_qualification_ca.der"
FIXTURE_CONFIG = "res/xml/sapflow_qualification_network_security.xml"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def expected_sha256(value):
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-fA-F]{64}", value) is None:
        raise ValueError("Expected SHA-256 must contain exactly 64 hexadecimal digits")
    return value.lower()


EXPECTED_SECP_ARM64 = "c4cc8233878301d16f1c8590fbea9f71383bee2605db9fec5b57a62c6a8e02f0"


def verify_connector_native(apk):
    """First connector packet is ARM64 only. Inspect actual ELF bytes, not packaging declarations."""
    with ZipFile(apk) as archive:
        libs = [p for p in archive.namelist() if p.startswith("lib/") and p.endswith(".so")]
        if not libs or {p.split("/")[1] for p in libs} != {"arm64-v8a"}:
            raise ValueError("Connector qualification requires exactly the ARM64 native ABI")
        result = {}
        for name in libs:
            data = archive.read(name)
            if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
                raise ValueError("Invalid ARM64 ELF")
            if struct.unpack_from("<H", data, 18)[0] != 183:
                raise ValueError("Native ELF machine is not AArch64")
            offset = struct.unpack_from("<Q", data, 32)[0]
            size, count = struct.unpack_from("<HH", data, 54)
            if size < 56 or count > 128 or offset + size * count > len(data):
                raise ValueError("Invalid ELF program headers")
            alignments = [struct.unpack_from("<Q", data, offset + i * size + 48)[0]
                          for i in range(count) if struct.unpack_from("<I", data, offset + i * size)[0] == 1]
            if not alignments or any(x < 16384 or x & (x - 1) for x in alignments):
                raise ValueError("Native ELF lacks 16 KiB LOAD alignment")
            result[name] = {"sha256": sha256(data), "load_alignments": alignments}
        secp = result.get("lib/arm64-v8a/libsecp256k1-jni.so")
        if not secp or secp["sha256"] != EXPECTED_SECP_ARM64:
            raise ValueError("Pinned ACINQ 0.22.0 ARM64 library mismatch")
        return {"native_abis": ["arm64-v8a"], "libraries": result, "all_abi_release_qualified": False}


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


def fixture_origin(value):
    uri = urlsplit(value)
    if (re.fullmatch(r"https://localhost(?::[1-9][0-9]{0,4})?/?", value) is None
            or uri.hostname != "localhost" or uri.port is not None and not 1 <= uri.port <= 65535):
        raise ValueError("Fixture origin must be HTTPS literal localhost")
    return value.rstrip("/")


def resource_attribute(block, name):
    match = re.search(r"A: (?:[^\s=]*:)?" + re.escape(name)
                      + r"(?:\([^)]*\))?=(.*)", block)
    if not match:
        return None
    value = match.group(1).split(" (Raw:", 1)[0].strip()
    return re.sub(r"^\(type [^)]*\)", "", value)


def verify_qualification_fixture(apk, release_apk, aapt2, *, ca_sha256, api_origin, playback_origin, prefix):
    """Inspect actual binary XML, resource references, public DER and Dex isolation."""
    expected_ca = expected_sha256(ca_sha256)
    origins = {fixture_origin(api_origin), fixture_origin(playback_origin)}
    if (len(prefix) > 256 or not prefix.startswith("/") or not prefix.endswith("/")
            or ".." in prefix or "//" in prefix or re.fullmatch(r"/[A-Za-z0-9/_-]*", prefix) is None):
        raise ValueError("Invalid fixture playback prefix")
    resources = subprocess.check_output([str(aapt2), "dump", "resources", str(apk)], text=True, timeout=15)
    def resource(name):
        matches = list(re.finditer(r"resource (0x[0-9a-fA-F]+) (?:[^\s:]+:)?" + re.escape(name)
                                  + r"(?:\s|$)", resources))
        if len(matches) != 1:
            raise ValueError("Fixture resource missing: " + name)
        match = matches[0]
        block = re.split(r"\n\s*(?:resource |type )", resources[match.end():], maxsplit=1)[0]
        paths = re.findall(r"\(file\) (res/[^\s]+)", block)
        if len(paths) != 1:
            raise ValueError("Unexpected fixture resource variants")
        path = paths[0]
        if ".." in path or len(path) > 256:
            raise ValueError("Unsafe compiled fixture resource path")
        return "@" + match.group(1), path
    config_id, config_path = resource("xml/sapflow_qualification_network_security")
    ca_id, ca_path = resource("raw/sapflow_qualification_ca")
    manifest = subprocess.check_output([str(aapt2), "dump", "xmltree", "--file", "AndroidManifest.xml", str(apk)], text=True, timeout=15)
    if resource_attribute(node_block(manifest, "application"), "networkSecurityConfig") != config_id:
        raise ValueError("Qualification manifest does not use the fixture trust configuration")
    tree = subprocess.check_output([str(aapt2), "dump", "xmltree", "--file", config_path, str(apk)], text=True, timeout=15)
    for name, count in {"network-security-config": 1, "base-config": 1, "domain-config": 1,
                        "domain": 1, "trust-anchors": 2, "certificates": 2}.items():
        if len(re.findall(r"E: " + re.escape(name) + r"(?:\s|$)", tree)) != count:
            raise ValueError("Unexpected fixture trust configuration shape")
    if re.search(r"E: (?:debug-overrides|pin-set|certificateTransparency|domainEncryption)(?:\s|$)", tree):
        raise ValueError("Unexpected fixture trust override")
    base, domain = node_block(tree, "base-config"), node_block(tree, "domain-config")
    false = {"false", "0x0", "0x00000000"}
    if (resource_attribute(base, "cleartextTrafficPermitted") not in false
            or resource_attribute(domain, "cleartextTrafficPermitted") not in false
            or resource_attribute(node_block(domain, "domain"), "includeSubdomains") not in false
            or len(re.findall(r"T: ['\"]localhost['\"]", domain)) != 1
            or resource_attribute(node_block(base, "certificates"), "src") != '"system"'
            or resource_attribute(node_block(domain, "certificates"), "src") != ca_id):
        raise ValueError("Fixture trust must be CA-only for exact localhost, with no cleartext")
    with ZipFile(apk) as archive:
        ca = archive.read(ca_path)
        if not 1 <= len(ca) <= 16384 or sha256(ca) != expected_ca:
            raise ValueError("Fixture public CA DER hash mismatch")
        if not all(pose_markers(archive, tuple(x.encode() for x in origins))):
            raise ValueError("Qualification profile origins missing from actual Dex")
    release_resources = subprocess.check_output([str(aapt2), "dump", "resources", str(release_apk)], text=True, timeout=15)
    if "sapflow_qualification_" in release_resources:
        raise ValueError("Fixture resources leaked into release")
    with ZipFile(release_apk) as archive:
        if any("sapflow_qualification_" in name for name in archive.namelist()):
            raise ValueError("Fixture files leaked into release")
        if any(pose_markers(archive, tuple(x.encode() for x in origins))):
            raise ValueError("Fixture profile origin leaked into release Dex")
        for item in archive.infolist():
            if item.filename.startswith("res/") and item.file_size == len(ca) and archive.read(item) == ca:
                raise ValueError("Fixture CA leaked into release")
    return {"domain": "localhost", "include_subdomains": False, "cleartext_permitted": False,
            "ca_der_sha256": expected_ca, "qualification_origins_present": True, "prefix_build_input_validated": prefix,
            "release_fixture_profile_and_trust_absent": True}


def verify_release_isolation(apk, aapt2, *, expected_native=EXPECTED_NATIVE):
    expected_native = expected_sha256(expected_native)
    tree = subprocess.check_output(
        [str(aapt2), "dump", "xmltree", "--file", "AndroidManifest.xml", str(apk)], text=True)
    application = node_block(tree, "application")
    resources = subprocess.check_output([str(aapt2), "dump", "resources", str(apk)], text=True, timeout=15)
    if "sapflow_qualification_" in resources:
        raise ValueError("Qualification fixture resource present in release")
    if attribute(application, "debuggable") not in (None, "false", "0x00000000"):
        raise ValueError("Release APK is debuggable")
    if POSE_APPLICATION in application or "FfiBenchmarkActivity" in application or "NativeAacQualification" in application:
        raise ValueError("Qualification components present in release manifest")
    if re.search(r"E: profileable(?:\s|$)", application) and attribute(node_block(application, "profileable"), "shell") in ("true", "0xffffffff"):
        raise ValueError("Release APK permits shell profiling")
    with ZipFile(apk) as archive:
        if any("sapflow_qualification_" in name for name in archive.namelist()):
            raise ValueError("Qualification fixture resource present in release")
        if any(pose_markers(archive, POSE_MARKERS + (BENCHMARK_MARKER,) + AAC_PROBE_MARKERS)):
            raise ValueError("Qualification pose hook present in release dex")
        native = archive.read("lib/arm64-v8a/libsapflow_ffi.so")
    if sha256(native) != expected_native:
        raise ValueError("Release Rust library differs from the frozen package")
    return {"apk_sha256": sha256(apk.read_bytes()), "qualification_hook_absent": True,
            "offline_benchmark_absent": True, "native_aac_probe_absent": True, "profileable_shell": False,
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
    parser.add_argument("--expect-v4v-connector", action="store_true")
    parser.add_argument("--expected-fixture-ca-sha256", type=expected_sha256)
    parser.add_argument("--fixture-api-origin")
    parser.add_argument("--fixture-playback-origin")
    parser.add_argument("--fixture-playback-prefix")
    options = parser.parse_args()
    if options.release_apk and not options.expect_pose_hook:
        parser.error("--release-apk requires --expect-pose-hook")
    fixture = [options.expected_fixture_ca_sha256, options.fixture_api_origin,
               options.fixture_playback_origin, options.fixture_playback_prefix]
    if any(x is not None for x in fixture) and (not all(x is not None for x in fixture) or not options.release_apk):
        parser.error("Fixture verification requires all four expected public values and --release-apk")
    result = verify(
        options.apk, options.aar, options.aapt2,
        expected_aar=options.expected_aar_sha256,
        expected_native=options.expected_native_sha256,
        expect_pose_hook=options.expect_pose_hook,
    )
    if options.release_apk:
        result["release_isolation"] = verify_release_isolation(options.release_apk, options.aapt2,
            expected_native=options.expected_native_sha256)
    if options.expect_v4v_connector:
        result["connector_native"] = verify_connector_native(options.apk)
        if options.release_apk:
            result["release_isolation"]["connector_native"] = verify_connector_native(options.release_apk)
    if all(x is not None for x in fixture):
        result["qualification_fixture"] = verify_qualification_fixture(options.apk, options.release_apk, options.aapt2,
            ca_sha256=options.expected_fixture_ca_sha256, api_origin=options.fixture_api_origin,
            playback_origin=options.fixture_playback_origin, prefix=options.fixture_playback_prefix)
    print(json.dumps(result, indent=2))
