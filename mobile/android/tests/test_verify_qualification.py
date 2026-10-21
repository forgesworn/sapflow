"""Artifact-verifier regressions; no Gradle, device, key or native execution."""

from contextlib import redirect_stderr
import io
from pathlib import Path
import runpy
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


if __name__ == "__main__":
    unittest.main()
