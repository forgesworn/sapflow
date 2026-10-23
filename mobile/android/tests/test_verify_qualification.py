"""Artifact-verifier regressions; no Gradle, device, key or native execution."""

from contextlib import redirect_stderr
import io
from pathlib import Path
import runpy
import struct
import tempfile
import unittest
from unittest.mock import patch
from zipfile import ZipFile


ANDROID = Path(__file__).resolve().parents[1]
ROOT = ANDROID.parents[1]
VERIFIER = runpy.run_path(str(ANDROID / "verify-qualification.py"))
MANIFEST = (Path(__file__).parent / "fixtures/qualification-manifest.txt").read_text()
NATIVE = b"\x7fELF qualification native payload fixture\x00"


class QualificationVerifierTest(unittest.TestCase):
    def setUp(self):
        output = ROOT / "target/mobile-android"
        output.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix="verifier-tests-", dir=output)
        self.addCleanup(self.directory.cleanup)
        self.apk = Path(self.directory.name) / "qualification.apk"
        self.aar = Path(self.directory.name) / "sdk.aar"
        self.write_zip(self.apk, "lib/arm64-v8a/libsapflow_ffi.so", NATIVE)
        self.write_zip(self.aar, "jni/arm64-v8a/libsapflow_ffi.so", NATIVE)
        self.aar_hash = VERIFIER["sha256"](self.aar.read_bytes())
        self.native_hash = VERIFIER["sha256"](NATIVE)

    @staticmethod
    def write_zip(path, name, data):
        with ZipFile(path, "w") as archive:
            archive.writestr(name, data)

    def verify(self, manifest=MANIFEST, **overrides):
        expected = {"expected_aar": self.aar_hash, "expected_native": self.native_hash}
        expected.update(overrides)
        with patch("subprocess.check_output", return_value=manifest) as command:
            result = VERIFIER["verify"](self.apk, self.aar, Path("aapt2"), **expected)
            command.assert_called_once_with(
                ["aapt2", "dump", "xmltree", "--file", "AndroidManifest.xml", str(self.apk)],
                text=True,
            )
            return result

    def test_explicit_hashes_accept_real_manifest_fixture_and_matching_zip_bytes(self):
        result = self.verify()
        self.assertEqual(result["aar_sha256"], self.aar_hash)
        self.assertEqual(result["native_sha256"], self.native_hash)
        self.assertFalse(result["debuggable"])
        self.assertTrue(result["profileable_shell"])
        self.assertFalse(result["physical_qualified"])

    def test_uppercase_expected_hashes_are_normalised(self):
        self.verify(expected_aar=self.aar_hash.upper(), expected_native=self.native_hash.upper())

    def test_default_baseline_identity_still_rejects_an_unrelated_package(self):
        with patch("subprocess.check_output") as command:
            with self.assertRaisesRegex(ValueError, "AAR SHA-256"):
                VERIFIER["verify"](self.apk, self.aar, Path("aapt2"))
            command.assert_not_called()

    def test_malformed_expected_hashes_fail_before_files_or_tools(self):
        values = [None, 0, "", "a" * 63, "a" * 65, "g" * 64, " " + "a" * 64, "a" * 64 + "\n"]
        with patch("subprocess.check_output") as command:
            for field in ("expected_aar", "expected_native"):
                for value in values:
                    with self.subTest(field=field, value=value):
                        with self.assertRaisesRegex(ValueError, "exactly 64 hexadecimal"):
                            self.verify(**{field: value})
            command.assert_not_called()

    def test_cli_rejects_malformed_hash(self):
        with patch("sys.argv", ["verify-qualification.py", "missing.apk", "missing.aar", "--aapt2", "aapt2", "--expected-aar-sha256", "not-a-hash"]):
            with redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaises(SystemExit) as failure:
                    runpy.run_path(str(ANDROID / "verify-qualification.py"), run_name="__main__")
        self.assertEqual(failure.exception.code, 2)
        self.assertIn("--expected-aar-sha256", stderr.getvalue())

    def test_wrong_expected_aar_fails_before_aapt(self):
        with patch("subprocess.check_output") as command:
            with self.assertRaisesRegex(ValueError, "AAR SHA-256"):
                VERIFIER["verify"](self.apk, self.aar, Path("aapt2"), expected_aar="0" * 64, expected_native=self.native_hash)
            command.assert_not_called()

    def test_wrong_expected_native_rejected_even_when_zip_payloads_match(self):
        with self.assertRaisesRegex(ValueError, "Rust library"):
            self.verify(expected_native="0" * 64)

    def test_apk_native_must_match_aar_not_just_supplied_expected_hash(self):
        changed = NATIVE + b" changed"
        self.write_zip(self.apk, "lib/arm64-v8a/libsapflow_ffi.so", changed)
        with self.assertRaisesRegex(ValueError, "Rust library"):
            self.verify(expected_native=VERIFIER["sha256"](changed))

    def test_missing_apk_native_rejected(self):
        self.write_zip(self.apk, "unrelated", b"")
        with self.assertRaises(KeyError):
            self.verify()

    def test_debuggable_manifest_rejected(self):
        changed = MANIFEST.replace("      E: application (line=28)", "      E: application (line=28)\n        A: http://schemas.android.com/apk/res/android:debuggable(0x0101000f)=true")
        with self.assertRaisesRegex(ValueError, "is debuggable"):
            self.verify(changed)

    def test_explicit_debuggable_false_accepted(self):
        changed = MANIFEST.replace("      E: application (line=28)", "      E: application (line=28)\n        A: http://schemas.android.com/apk/res/android:debuggable(0x0101000f)=false")
        self.verify(changed)

    def test_non_profileable_manifest_rejected(self):
        with self.assertRaisesRegex(ValueError, "shell profiling"):
            self.verify(MANIFEST.replace(":shell(0x01010594)=true", ":shell(0x01010594)=false"))

    def test_missing_profileable_node_rejected(self):
        with self.assertRaisesRegex(ValueError, "Missing manifest node: profileable"):
            self.verify(MANIFEST.replace("E: profileable ", "E: unrelated "))

    def test_explicitly_disabled_profileable_node_rejected(self):
        changed = MANIFEST.replace("          E: profileable (line=35)", "          E: profileable (line=35)\n            A: http://schemas.android.com/apk/res/android:enabled(0x0101000e)=false")
        with self.assertRaisesRegex(ValueError, "disables shell profiling"):
            self.verify(changed)

    def test_missing_benchmark_rejected(self):
        with self.assertRaisesRegex(ValueError, "benchmark Activity missing"):
            self.verify(MANIFEST.replace("FfiBenchmarkActivity", "SomeOtherActivity"))

    def pose_manifest(self):
        return MANIFEST.replace("      E: application (line=28)", "      E: application (line=28)\n        A: http://schemas.android.com/apk/res/android:name(0x01010003)=\"" + VERIFIER["POSE_APPLICATION"] + "\"")

    def release_manifest(self):
        return MANIFEST.replace(VERIFIER["node_block"](MANIFEST, "profileable"), "").replace("FfiBenchmarkActivity", "OtherProductionActivity")

    def dex(self, payload):
        self.write_zip(self.apk, "lib/arm64-v8a/libsapflow_ffi.so", NATIVE)
        with ZipFile(self.apk, "a") as archive:
            archive.writestr("classes.dex", payload)

    def test_requested_pose_hook_requires_real_application_and_both_dex_markers(self):
        self.dex(b"\x00".join(VERIFIER["POSE_MARKERS"]))
        self.assertTrue(self.verify(self.pose_manifest(), expect_pose_hook=True)["qualification_pose_hook_present"])
        with self.assertRaisesRegex(ValueError, "Application missing"):
            self.verify(expect_pose_hook=True)
        for marker in VERIFIER["POSE_MARKERS"]:
            self.dex(marker)
            with self.assertRaisesRegex(ValueError, "hook missing from dex"):
                self.verify(self.pose_manifest(), expect_pose_hook=True)

    def test_release_verifier_accepts_only_production_manifest_and_native(self):
        self.dex(b"production-only dex fixture")
        with patch("subprocess.check_output", return_value=self.release_manifest()):
            result = VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)
        self.assertTrue(result["qualification_hook_absent"])
        self.assertTrue(result["offline_benchmark_absent"])
        self.assertEqual(result["native_sha256"], self.native_hash)

    def test_release_refuses_pose_application_action_or_benchmark_in_any_dex(self):
        for marker in VERIFIER["POSE_MARKERS"] + (VERIFIER["BENCHMARK_MARKER"],):
            self.dex(b"production classes")
            with ZipFile(self.apk, "a") as archive:
                archive.writestr("classes2.dex", marker)
            with patch("subprocess.check_output", return_value=self.release_manifest()):
                with self.assertRaisesRegex(ValueError, "present in release dex"):
                    VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)

    def test_release_refuses_qualification_manifest_profile_or_debugging(self):
        debug = self.release_manifest().replace("      E: application (line=28)", "      E: application (line=28)\n        A: http://schemas.android.com/apk/res/android:debuggable(0x0101000f)=true")
        profiled = MANIFEST.replace("FfiBenchmarkActivity", "OtherProductionActivity")
        for manifest, reason in ((MANIFEST, "components"), (self.pose_manifest(), "components"), (profiled, "shell profiling"), (debug, "debuggable")):
            with patch("subprocess.check_output", return_value=manifest):
                with self.assertRaisesRegex(ValueError, reason):
                    VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)

    def test_release_native_hash_remains_pinned(self):
        self.dex(b"production classes")
        with patch("subprocess.check_output", return_value=self.release_manifest()):
            with self.assertRaisesRegex(ValueError, "Release Rust library"):
                VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native="0" * 64)


    def test_release_refuses_generated_native_aac_probe_markers_in_secondary_dex(self):
        for marker in VERIFIER["AAC_PROBE_MARKERS"]:
            self.dex(b"production classes")
            with ZipFile(self.apk, "a") as archive:
                archive.writestr("classes2.dex", marker)
            with patch("subprocess.check_output", return_value=self.release_manifest()):
                with self.assertRaisesRegex(ValueError, "present in release dex"):
                    VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)

    def test_release_refuses_generated_native_aac_activity_in_manifest(self):
        manifest = self.release_manifest().replace("OtherProductionActivity", "NativeAacQualificationActivity")
        with patch("subprocess.check_output", return_value=manifest):
            with self.assertRaisesRegex(ValueError, "components present in release"):
                VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)

    def test_window_reference_class_and_action_remain_release_excluded(self):
        markers = (b"Ldev/forgesworn/sapflow/broadcaster/NativeAacProgrammeWindowProbe;",
                   b"dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_WINDOW_REFERENCE")
        for marker in markers:
            self.assertIn(marker, VERIFIER["AAC_PROBE_MARKERS"])
            self.dex(b"production classes")
            with ZipFile(self.apk, "a") as archive:
                archive.writestr("classes3.dex", marker)
            with patch("subprocess.check_output", return_value=self.release_manifest()):
                with self.assertRaisesRegex(ValueError, "present in release dex"):
                    VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)

    def test_sdk_window_action_remains_release_excluded(self):
        marker = b"dev.forgesworn.sapflow.broadcaster.QUALIFY_GENERATED_AAC_SDK_WINDOW"
        self.assertIn(marker, VERIFIER["AAC_PROBE_MARKERS"])
        self.dex(b"production classes")
        with ZipFile(self.apk, "a") as archive:
            archive.writestr("classes3.dex", marker)
        with patch("subprocess.check_output", return_value=self.release_manifest()):
            with self.assertRaisesRegex(ValueError, "present in release dex"):
                VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"), expected_native=self.native_hash)


class ConnectorNativeVerifierTest(unittest.TestCase):
    @staticmethod
    def elf(align=16384):
        value = bytearray(120)
        value[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", value, 18, 183)
        struct.pack_into("<Q", value, 32, 64)
        struct.pack_into("<HH", value, 54, 56, 1)
        struct.pack_into("<I", value, 64, 1)
        struct.pack_into("<Q", value, 112, align)
        return bytes(value)

    def test_exact_arm64_hash_and_real_header_alignment_are_required(self):
        with tempfile.TemporaryDirectory(dir=ROOT / "target/mobile-android", prefix="connector-verifier-") as directory:
            apk = Path(directory) / "synthetic.apk"
            verify = VERIFIER["verify_connector_native"]
            good = self.elf()
            def write(data, extra=False):
                with ZipFile(apk, "w") as archive:
                    archive.writestr("lib/arm64-v8a/libsecp256k1-jni.so", data)
                    if extra:
                        archive.writestr("lib/x86_64/libsecp256k1-jni.so", data)
            with patch.dict(verify.__globals__, {"EXPECTED_SECP_ARM64": VERIFIER["sha256"](good)}):
                write(good)
                self.assertEqual(verify(apk)["native_abis"], ["arm64-v8a"])
                write(good, True)
                with self.assertRaisesRegex(ValueError, "exactly the ARM64"):
                    verify(apk)
                write(self.elf(4096))
                with self.assertRaisesRegex(ValueError, "16 KiB"):
                    verify(apk)
                write(good + b"changed")
                with self.assertRaisesRegex(ValueError, "mismatch"):
                    verify(apk)
                write(good[:80])
                with self.assertRaisesRegex(ValueError, "program headers"):
                    verify(apk)


class QualificationFixtureVerifierTest(unittest.TestCase):
    """Synthetic ZIP/tree payloads test the verifier, not X.509 issuance or TLS."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / "target/mobile-android", prefix="tls-verifier-")
        self.addCleanup(self.temp.cleanup)
        self.apk = Path(self.temp.name) / "qualification.apk"
        self.release = Path(self.temp.name) / "release.apk"
        self.ca = b"synthetic public certificate bytes for hash verification only"
        self.api, self.playback = "https://localhost:4443", "https://localhost:4444"
        self.resources = ("resource 0x7f010001 xml/sapflow_qualification_network_security\n  () (file) " + VERIFIER["FIXTURE_CONFIG"]
                          + "\nresource 0x7f020001 raw/sapflow_qualification_ca\n  () (file) " + VERIFIER["FIXTURE_CA"] + "\n")
        self.manifest = "E: manifest\n  E: application\n    A: http://schemas.android.com/apk/res/android:networkSecurityConfig(0x01010527)=@0x7f010001\n"
        self.tree = '''E: network-security-config
  E: base-config
    A: cleartextTrafficPermitted=(type 0x12)0x0
    E: trust-anchors
      E: certificates
        A: src="system"
  E: domain-config
    A: cleartextTrafficPermitted=(type 0x12)0x0
    E: domain
      A: includeSubdomains=(type 0x12)0x0
      T: "localhost"
    E: trust-anchors
      E: certificates
        A: src=@0x7f020001
'''
        self.write_qualification()
        self.write_release()

    def write_qualification(self, ca=None, dex=None):
        with ZipFile(self.apk, "w") as archive:
            archive.writestr(VERIFIER["FIXTURE_CA"], self.ca if ca is None else ca)
            archive.writestr(VERIFIER["FIXTURE_CONFIG"], b"synthetic compiled XML placeholder")
            archive.writestr("classes.dex", (self.api + " " + self.playback).encode() if dex is None else dex)

    def write_release(self, extra=None, dex=b"production code"):
        with ZipFile(self.release, "w") as archive:
            archive.writestr("classes.dex", dex)
            if extra:
                archive.writestr(*extra)

    def aapt(self, command, **_kwargs):
        if command[-1] == str(self.release):
            return ""  # No fixture resources in the production resource table.
        if command[2] == "resources":
            return self.resources
        return self.manifest if command[4] == "AndroidManifest.xml" else self.tree

    def verify(self, **overrides):
        values = dict(ca_sha256=VERIFIER["sha256"](self.ca), api_origin=self.api,
                      playback_origin=self.playback, prefix="/live/")
        values.update(overrides)
        with patch("subprocess.check_output", side_effect=self.aapt):
            return VERIFIER["verify_qualification_fixture"](self.apk, self.release, Path("aapt2"), **values)

    def test_exact_domain_resource_references_public_hash_and_variant_profile(self):
        result = self.verify()
        self.assertEqual(result["domain"], "localhost")
        self.assertFalse(result["include_subdomains"])
        self.assertTrue(result["release_fixture_profile_and_trust_absent"])

    def test_wrong_public_certificate_or_missing_ca_is_refused(self):
        self.write_qualification(ca=b"different public bytes")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            self.verify()
        with ZipFile(self.apk, "w") as archive:
            archive.writestr("classes.dex", self.api.encode())
        with self.assertRaises(KeyError):
            self.verify()

    def test_actual_compiled_resource_table_paths_survive_resource_optimisation(self):
        self.resources = self.resources.replace(VERIFIER["FIXTURE_CONFIG"], "res/A.xml").replace(VERIFIER["FIXTURE_CA"], "res/B.der")
        with ZipFile(self.apk, "w") as archive:
            archive.writestr("res/A.xml", b"compiled XML placeholder")
            archive.writestr("res/B.der", self.ca)
            archive.writestr("classes.dex", (self.api + " " + self.playback).encode())
        self.assertEqual(self.verify()["ca_der_sha256"], VERIFIER["sha256"](self.ca))

    def test_alternate_qualified_resource_cannot_hide_another_trust_configuration(self):
        self.resources += "  (en) (file) res/other.der\n"
        with self.assertRaisesRegex(ValueError, "resource variants"):
            self.verify()

    def test_manifest_or_domain_cannot_reference_another_trust_resource(self):
        self.manifest = self.manifest.replace("@0x7f010001", "@0x7f010099")
        with self.assertRaisesRegex(ValueError, "manifest"):
            self.verify()
        self.manifest = self.manifest.replace("@0x7f010099", "@0x7f010001")
        self.tree = self.tree.replace("@0x7f020001", "@0x7f020099")
        with self.assertRaisesRegex(ValueError, "CA-only"):
            self.verify()

    def test_wide_domain_cleartext_system_or_debug_trust_is_refused(self):
        original = self.tree
        mutations = [original.replace('T: "localhost"', 'T: "example.invalid"'),
                     original.replace("includeSubdomains=(type 0x12)0x0", "includeSubdomains=(type 0x12)0x1"),
                     original.replace("cleartextTrafficPermitted=(type 0x12)0x0", "cleartextTrafficPermitted=(type 0x12)0x1"),
                     original.replace("src=@0x7f020001", 'src="system"'),
                     original + "  E: debug-overrides\n"]
        for tree in mutations:
            with self.subTest(tree=tree):
                self.tree = tree
                with self.assertRaises(ValueError):
                    self.verify()

    def test_extra_trust_anchor_is_refused(self):
        self.tree += '      E: certificates\n        A: src="user"\n'
        with self.assertRaisesRegex(ValueError, "shape"):
            self.verify()

    def test_release_rejects_fixture_resources_even_with_an_unrecognised_filename(self):
        for extra in ((VERIFIER["FIXTURE_CONFIG"], b"config"), ("res/raw/other.der", self.ca)):
            self.write_release(extra=extra)
            with self.assertRaisesRegex(ValueError, "leaked into release"):
                self.verify()

    def test_release_fixture_origin_in_any_dex_is_refused(self):
        with ZipFile(self.release, "a") as archive:
            archive.writestr("classes2.dex", self.playback.encode())
        with self.assertRaisesRegex(ValueError, "origin leaked"):
            self.verify()

    def test_missing_qualification_profile_is_refused(self):
        self.write_qualification(dex=self.api.encode())
        with self.assertRaisesRegex(ValueError, "origins missing"):
            self.verify()

    def test_nonlocal_origin_bad_prefix_or_malformed_hash_is_refused(self):
        for origin in ("http://localhost", "https://example.invalid", "https://localhost.example.invalid",
                       "https://user@localhost", "https://localhost:0", "https://localhost/path"):
            with self.subTest(origin=origin), self.assertRaises(ValueError):
                self.verify(api_origin=origin)
        for prefix in ("/../", "/a//", "/a%20/", "live/"):
            with self.subTest(prefix=prefix), self.assertRaises(ValueError):
                self.verify(prefix=prefix)
        with self.assertRaises(ValueError):
            self.verify(ca_sha256="invalid")

    def test_release_base_verifier_rejects_fixture_resource_without_opt_in_flags(self):
        with ZipFile(self.apk, "w") as archive:
            archive.writestr(VERIFIER["FIXTURE_CA"], self.ca)
        with patch("subprocess.check_output", return_value=QualificationVerifierTest.release_manifest(self)):
            with self.assertRaisesRegex(ValueError, "fixture resource"):
                VERIFIER["verify_release_isolation"](self.apk, Path("aapt2"))


if __name__ == "__main__":
    unittest.main()
