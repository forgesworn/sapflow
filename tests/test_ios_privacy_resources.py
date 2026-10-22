#!/usr/bin/env python3
"""Tiny generated bundles exercise the real CLI; no Xcode or device gate."""

import copy
import importlib.util
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "mobile/ios/verify-privacy-resources.py"
SPEC = importlib.util.spec_from_file_location("sapflow_privacy_resources", SCRIPT)
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)


def profile():
    # Explicit fixture values, independent of the verifier's expected table.
    return {"NSPrivacyAccessedAPITypes": [
        {"NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategoryDiskSpace", "NSPrivacyAccessedAPITypeReasons": ["E174.1"]},
        {"NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategoryFileTimestamp", "NSPrivacyAccessedAPITypeReasons": ["C617.1", "3B52.1"]},
        {"NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategorySystemBootTime", "NSPrivacyAccessedAPITypeReasons": ["35F9.1"]},
    ]}


class PrivacyResourcesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="sapflow-privacy-resource-")
        self.addCleanup(temporary.cleanup)
        self.temporary = Path(temporary.name)
        self.app = self.temporary / "Generated.app"
        self.app.mkdir()

    def write(self, value=None, *, fmt=plistlib.FMT_XML):
        (self.app / VERIFIER.RESOURCE).write_bytes(plistlib.dumps(profile() if value is None else value, fmt=fmt))

    def rejected(self, value):
        self.write(value)
        with self.assertRaises(VERIFIER.PrivacyResourceError):
            VERIFIER.verify_app_resource(self.app)

    def cli(self, app=None):
        return subprocess.run([sys.executable, "-B", str(SCRIPT), "--app-root", str(app or self.app)], cwd=self.temporary, text=True, capture_output=True, timeout=10)

    def test_generated_xml_bundle_and_cli_from_unrelated_directory(self):
        self.write()
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        self.assertTrue(value["privacyResourceVerified"])
        self.assertFalse(value["appStoreReadinessEstablished"])
        self.assertEqual(value["collectionAndTrackingAssessment"], "Not assessed by this verifier")

    def test_binary_resource_and_order_independence(self):
        value = profile()
        value["NSPrivacyAccessedAPITypes"].reverse()
        value["NSPrivacyAccessedAPITypes"][1]["NSPrivacyAccessedAPITypeReasons"].reverse()
        self.write(value, fmt=plistlib.FMT_BINARY)
        self.assertTrue(VERIFIER.verify_app_resource(self.app)["privacyResourceVerified"])

    def test_missing_built_resource_never_falls_back_to_repo_source(self):
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_wrong_built_resource_never_falls_back_to_valid_repo_source(self):
        self.write({"NSPrivacyAccessedAPITypes": []})
        self.assertNotEqual(self.cli().returncode, 0)

    def test_bundle_copy_is_independent_of_an_altered_source(self):
        original = self.temporary / "Source.xcprivacy"
        original.write_bytes(plistlib.dumps(profile()))
        shutil.copyfile(original, self.app / VERIFIER.RESOURCE)
        original.write_bytes(b"not a manifest")
        self.assertEqual(self.cli().returncode, 0)

    def test_current_source_resource_matches_reviewed_profile(self):
        # Source-only assertion; actual built-app verification is separate.
        result = VERIFIER.verify_app_resource(ROOT / "mobile/ios")
        self.assertTrue(result["privacyResourceVerified"])

    def test_missing_category(self):
        value = profile(); value["NSPrivacyAccessedAPITypes"].pop()
        self.rejected(value)

    def test_duplicate_category_cannot_replace_a_missing_category(self):
        value = profile(); value["NSPrivacyAccessedAPITypes"][2] = copy.deepcopy(value["NSPrivacyAccessedAPITypes"][0])
        self.rejected(value)

    def test_unsupported_category(self):
        value = profile(); value["NSPrivacyAccessedAPITypes"][0]["NSPrivacyAccessedAPIType"] = "NSPrivacyAccessedAPICategoryUserDefaults"
        self.rejected(value)

    def test_wrong_or_unsupported_reason(self):
        for reason in ["85F4.1", "NOT-AN-APPLE-REASON"]:
            with self.subTest(reason=reason):
                value = profile(); value["NSPrivacyAccessedAPITypes"][0]["NSPrivacyAccessedAPITypeReasons"] = [reason]
                self.rejected(value)

    def test_both_timestamp_scopes_are_required(self):
        for reasons in [["C617.1"], ["3B52.1"], ["C617.1", "C617.1"]]:
            with self.subTest(reasons=reasons):
                value = profile(); value["NSPrivacyAccessedAPITypes"][1]["NSPrivacyAccessedAPITypeReasons"] = reasons
                self.rejected(value)

    def test_wrong_types_including_boolean_reason(self):
        for value in [[], {"NSPrivacyAccessedAPITypes": {}}, {"NSPrivacyAccessedAPITypes": [True, False, 1]}]:
            with self.subTest(value=value):
                self.rejected(value)
        for reasons in ["E174.1", True, [True], [1], {"reason": "E174.1"}]:
            with self.subTest(reasons=reasons):
                value = profile(); value["NSPrivacyAccessedAPITypes"][0]["NSPrivacyAccessedAPITypeReasons"] = reasons
                self.rejected(value)

    def test_extra_keys_do_not_create_unassessed_collection_or_tracking_claims(self):
        for key, extra in [("NSPrivacyCollectedDataTypes", []), ("NSPrivacyTracking", False), ("NSPrivacyTrackingDomains", [])]:
            with self.subTest(key=key):
                value = profile(); value[key] = extra
                self.rejected(value)
        value = profile(); value["NSPrivacyAccessedAPITypes"][0]["extra"] = False
        self.rejected(value)

    def test_duplicate_plist_keys_reject_before_normalisation(self):
        data = plistlib.dumps(profile())
        addition = b"<key>NSPrivacyAccessedAPITypes</key><array/>"
        data = data.replace(b"<dict>", b"<dict>" + addition, 1)
        (self.app / VERIFIER.RESOURCE).write_bytes(data)
        with self.assertRaises(VERIFIER.PrivacyResourceError):
            VERIFIER.verify_app_resource(self.app)

    def test_malformed_oversized_and_nonregular_resources(self):
        for data in [b"", b"not a property list", b"x" * (VERIFIER.MAX_RESOURCE_BYTES + 1)]:
            with self.subTest(bytes=len(data)):
                (self.app / VERIFIER.RESOURCE).write_bytes(data)
                self.assertNotEqual(self.cli().returncode, 0)
        (self.app / VERIFIER.RESOURCE).unlink()
        (self.app / VERIFIER.RESOURCE).mkdir()
        self.assertNotEqual(self.cli().returncode, 0)

    def test_symlink_resource_is_not_followed(self):
        original = self.temporary / "Other.xcprivacy"
        original.write_bytes(plistlib.dumps(profile()))
        (self.app / VERIFIER.RESOURCE).symlink_to(original)
        self.assertNotEqual(self.cli().returncode, 0)

    def test_malformed_xml_reports_only_bounded_validation_failure(self):
        (self.app / VERIFIER.RESOURCE).write_bytes(b'<?xml version="1.0"?><plist><dict><key>synthetic-marker</key>')
        result = self.cli()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("Traceback", result.stderr)
        self.assertNotIn("synthetic-marker", result.stderr)


if __name__ == "__main__":
    unittest.main()
